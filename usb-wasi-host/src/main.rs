// Copyright (c) 2026 IDLab Discover
// SPDX-License-Identifier: MIT

//! WASI-USB Host Runtime
//!
//! This crate implements the host-side of the WASI-USB interface, providing
//! WebAssembly modules with safe, capability-based access to USB devices.
//! It handles WIT-to-native mapping, USB transfer orchestration, and 
//! specialized interfaces for Computer Vision (UVC/YOLO).

use libusb1_sys::constants::{
    LIBUSB_TRANSFER_COMPLETED, LIBUSB_TRANSFER_TYPE_BULK, LIBUSB_TRANSFER_TYPE_CONTROL,
    LIBUSB_TRANSFER_TYPE_INTERRUPT, LIBUSB_TRANSFER_TYPE_ISOCHRONOUS,
    LIBUSB_TRANSFER_TIMED_OUT, LIBUSB_TRANSFER_CANCELLED, LIBUSB_TRANSFER_STALL,
    LIBUSB_TRANSFER_NO_DEVICE, LIBUSB_TRANSFER_OVERFLOW, LIBUSB_TRANSFER_ERROR,
};
use libusb1_sys::{
    libusb_alloc_streams, libusb_alloc_transfer, libusb_cancel_transfer, libusb_close,
    libusb_free_streams, libusb_free_transfer, libusb_submit_transfer, libusb_transfer,
    libusb_transfer_set_stream_id, libusb_unref_device,
};

use wasmtime::component::{Component, Linker, Resource, ResourceTable, ResourceTableError};
use wasmtime::{Config, Error};
use wasmtime::{Engine, Store};
use wasmtime_wasi::bindings::Command;
use wasmtime_wasi::{DirPerms, FilePerms, IoView, WasiCtx, WasiCtxBuilder, WasiView, I32Exit};

use std::env;
use log::{debug, error, info, trace, warn, LevelFilter};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::path::PathBuf;
use std::str::FromStr;
use clap::Parser;
use tokio::sync::oneshot;
use std::time::{Instant, Duration};

use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{RequestedFormat, RequestedFormatType};
use nokhwa::Camera;
use tract_onnx::prelude::*;
use image::RgbImage;

use crate::component::usb::configuration::ConfigValue;
use crate::component::usb::descriptors::{ConfigurationDescriptor, DeviceDescriptor};
use crate::component::usb::device::{
    DeviceLocation, HostDeviceHandle, HostUsbDevice, TransferOptions, TransferSetup,
    TransferType, UsbSpeed,
};
use crate::component::usb::cv::{
    Frame, Detection, HostFrameStream, HostObjectDetector,
};
use crate::component::usb::cv_ui::{HostRenderer};
use crate::component::usb::errors::LibusbError;
use crate::component::usb::transfers::{
    HostTransfer, IsoResult, IsoPacket, IsoPacketStatus,
};
use crate::component::usb::usb_hotplug::{Event, Info};

pub mod usb_backend;
pub use usb_backend::{HostUsbBackend, LibusbBackend, UsbDevice, UsbDeviceHandle};

// ── UVC class constants ───────────────────────────────────────────────────────
const USB_CLASS_VIDEO: u8 = 0x0E;
const USB_SUBCLASS_VIDEO_STREAMING: u8 = 0x02;

pub struct WebcamFrameStream {
    pub handle: UsbDeviceHandle,
    pub iface_num: u8,
    pub ep_addr: u8,
    pub packet_stride: u32,
    pub num_packets: u32,
    pub buffer_size: u32,
    pub actual_frame_size: u32,
    pub min_frame_size: usize,
    
    // Mutable state for reassembly
    pub frame_count: u32,
    pub last_fid: u8,
    pub frame_buffer: Vec<u8>,
    pub frame_started: bool,
    pub transfer_buffer: Vec<u8>,
}

pub enum FrameStream {
    Uvc(WebcamFrameStream),
}

pub struct ObjectDetector {
    pub model: SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>,
}

pub struct RendererStub;

#[derive(Debug)]
pub struct UsbTransfer {
    transfer: *mut libusb_transfer,
    completed: Arc<AtomicBool>,
    pub buffer: Option<Box<[u8]>>,
    pub buf_len: u32,
    receiver: Option<oneshot::Receiver<Result<Vec<u8>, LibusbError>>>,
    control_setup: Option<TransferSetup>,
    /// Per-packet (actual_length, status) results — populated by transfer_callback for ISO.
    pub iso_packet_results: Arc<Mutex<Option<Vec<(u32, i32)>>>>,
}

mod bindings {
    wasmtime::component::bindgen!({
        world: "host",
        path: "../wit",
        with: {
            "component:usb/transfers@0.2.1/transfer": super::UsbTransfer,
            "component:usb/device@0.2.1/usb-device": super::UsbDevice,
            "component:usb/device@0.2.1/device-handle": super::UsbDeviceHandle,
            "component:usb/cv@0.2.1/frame-stream": super::FrameStream,
            "component:usb/cv@0.2.1/object-detector": super::ObjectDetector,
            "component:usb/cv-ui@0.2.1/renderer": super::RendererStub,
        },
        async: {
            only_imports: ["await-transfer", "await-iso-transfer"]
        },
    });
}
pub(crate) use bindings::component;
pub(crate) use bindings::Host_ as Host;

// Since world is "host", it might generate a module named Host
// Or it might generate the types directly. Let's try Host:: prefix.

/// Context passed through libusb's user_data pointer to the transfer callback.
struct TransferContext {
    sender: oneshot::Sender<Result<Vec<u8>, LibusbError>>,
    completed: Arc<AtomicBool>,
    buffer: Box<[u8]>,
    /// Shared with UsbTransfer so await-iso-transfer can read per-packet results.
    iso_packet_results: Arc<Mutex<Option<Vec<(u32, i32)>>>>,
}

unsafe impl Send for UsbTransfer {}
unsafe impl Sync for UsbTransfer {}

unsafe impl Send for MyState {}
unsafe impl Sync for MyState {}

extern "system" fn iso_callback(transfer: *mut libusb1_sys::libusb_transfer) {
    unsafe {
        let completed = &*((*transfer).user_data as *const std::sync::atomic::AtomicBool);
        completed.store(true, std::sync::atomic::Ordering::Release);
    }
}

#[derive(Parser)]
#[command(name = "usb-wasi-host", about, trailing_var_arg = true)]
struct CliParser {
    #[arg(short, long)]
    component_path: PathBuf,

    #[arg(long, short = 'd')]
    usb_devices: Vec<USBDeviceIdentifier>,

    #[arg(long, short)]
    use_allow_list: bool,

    #[arg(long = "debug_level", short = 'l', default_value = "info")]
    debug_level: String,

    #[arg(long)]
    enable_yolo: bool,

    #[arg(allow_hyphen_values = true)]
    guest_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct USBDeviceIdentifier {
    vendor_id: u16,
    product_id: u16,
}

impl FromStr for USBDeviceIdentifier {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 2 {
            return Err("Invalid format. Expected vendor_id:product_id");
        }
        let vendor_id = u16::from_str_radix(parts[0], 16).map_err(|_| "Invalid vendor_id")?;
        let product_id = u16::from_str_radix(parts[1], 16).map_err(|_| "Invalid product_id")?;
        Ok(Self { vendor_id, product_id })
    }
}

#[derive(Debug, Clone)]
pub enum AllowedUSBDevices {
    Allowed(Vec<USBDeviceIdentifier>),
    Denied(Vec<USBDeviceIdentifier>),
}

impl AllowedUSBDevices {
    pub fn is_allowed(&self, device: &USBDeviceIdentifier) -> bool {
        match self {
            Self::Allowed(devices) => devices.contains(device),
            Self::Denied(devices) => !devices.contains(device),
        }
    }
}

struct MyState {
    table: ResourceTable,
    ctx: WasiCtx,
    allowed_usbdevices: AllowedUSBDevices,
    backend: Box<dyn HostUsbBackend>,
    enable_yolo: bool,
}

impl MyState {
    pub fn new(allowed_usbdevices: AllowedUSBDevices, guest_args: Vec<String>, enable_yolo: bool) -> Self {
        let mut backend = LibusbBackend::new();
        match backend.init() {
            Ok(_) => info!("Backend initialized"),
            Err(e) => error!("Failed to initialize backend: {:?}", e),
        }
        Self {
            table: ResourceTable::new(),
            ctx: WasiCtxBuilder::new()
                .inherit_stdio()
                .args(&guest_args)
                .preopened_dir(
                    env::current_dir().expect("failed to open dir"),
                    ".",
                    DirPerms::all(),
                    FilePerms::all(),
                )
                .expect("failed to open dir")
                .build(),
            allowed_usbdevices,
            backend: Box::new(backend),
            enable_yolo,
        }
    }
}

extern "system" fn transfer_callback(transfer: *mut libusb_transfer) {
    unsafe {
        let ctx_ptr = (*transfer).user_data as *mut TransferContext;
        let ctx = Box::from_raw(ctx_ptr);

        let status = (*transfer).status;
        info!("transfer_callback fired, status: {}", status);
        let result: Result<Vec<u8>, LibusbError> =
            if status == LIBUSB_TRANSFER_COMPLETED {
                let mut data_vec = Vec::new();

                if (*transfer).num_iso_packets > 0 {
                    // Isochronous: collect per-packet metadata AND flat data
                    let num_packets = (*transfer).num_iso_packets as usize;
                    let mut packet_results: Vec<(u32, i32)> = Vec::with_capacity(num_packets);

                    let mut total_actual_len = 0u32;
                    for i in 0..num_packets {
                        let desc_ptr = ((*transfer).iso_packet_desc.as_ptr() as *const libusb1_sys::libusb_iso_packet_descriptor).add(i);
                        let desc = &*desc_ptr;
                        packet_results.push((desc.actual_length, desc.status as i32));
                        total_actual_len += desc.actual_length;
                    }
                    info!("ISO transfer received {} total actual bytes", total_actual_len);

                    // Store per-packet results so await-iso-transfer can read them
                    *ctx.iso_packet_results.lock().unwrap() = Some(packet_results);

                    // Copy full buffer (stride = packet_size, not actual_length)
                    let full_len = (*transfer).length as usize;
                    info!("ISO transfer received {} bytes of data", full_len);
                    let buf_ptr = (*transfer).buffer;
                    if !buf_ptr.is_null() && full_len > 0 {
                        let data_slice = std::slice::from_raw_parts(buf_ptr, full_len);
                        data_vec = data_slice.to_vec();
                    }

                } else if (*transfer).transfer_type == LIBUSB_TRANSFER_TYPE_CONTROL {
                    let actual_len = (*transfer).actual_length as usize;
                    debug!("Control transfer completed with actual length: {}", actual_len);

                    let buf_ptr = (*transfer).buffer;
                    let bm_request_type = if !buf_ptr.is_null() { *buf_ptr } else { 0 };
                    let is_device_to_host = (bm_request_type & 0x80) != 0;

                    if is_device_to_host && actual_len > 0 {
                        if !buf_ptr.is_null() {
                            let data_slice = std::slice::from_raw_parts(buf_ptr.add(8), actual_len);
                            data_vec = data_slice.to_vec();
                            debug!("Control IN transfer data: {:?}", data_vec);
                        }
                    } else {
                        data_vec = Vec::new();
                    }

                } else {
                    // Bulk / Interrupt
                    let actual_len = (*transfer).actual_length as usize;
                    if (*transfer).endpoint & 0x80 != 0 {
                        if actual_len > 0 {
                            let buf_ptr = (*transfer).buffer;
                            if !buf_ptr.is_null() {
                                let data_slice = std::slice::from_raw_parts(buf_ptr, actual_len);
                                data_vec = data_slice.to_vec();
                            }
                        }
                    } else {
                        data_vec = std::iter::repeat(0).take(actual_len).collect();
                    }
                }

                Ok(data_vec)
            } else {
                let err = match status {
                    LIBUSB_TRANSFER_TIMED_OUT  => LibusbError::Timeout,
                    LIBUSB_TRANSFER_CANCELLED  => LibusbError::Interrupted,
                    LIBUSB_TRANSFER_STALL      => LibusbError::Pipe,
                    LIBUSB_TRANSFER_NO_DEVICE  => LibusbError::NoDevice,
                    LIBUSB_TRANSFER_OVERFLOW   => LibusbError::Overflow,
                    LIBUSB_TRANSFER_ERROR      => LibusbError::Io,
                    _                          => LibusbError::Other,
                };
                Err(err)
            };

        ctx.completed.store(true, Ordering::SeqCst);
        let _ = ctx.sender.send(result);
        libusb_free_transfer(transfer);
        // ctx drops here — buffer (Box<[u8]>) freed, Arc refcount decremented
    }
}

extern "system" fn empty_callback(_transfer: *mut libusb_transfer) {}

impl IoView for MyState {
    fn table(&mut self) -> &mut ResourceTable {
        &mut self.table
    }
}

impl WasiView for MyState {
    fn ctx(&mut self) -> &mut WasiCtx {
        &mut self.ctx
    }
}

impl LibusbError {
    pub fn from_raw(value: i32) -> Self {
        match value {
            -1  => LibusbError::Io,
            -2  => LibusbError::InvalidParam,
            -3  => LibusbError::Access,
            -4  => LibusbError::NoDevice,
            -5  => LibusbError::NotFound,
            -6  => LibusbError::Busy,
            -7  => LibusbError::Timeout,
            -8  => LibusbError::Overflow,
            -9  => LibusbError::Pipe,
            -10 => LibusbError::Interrupted,
            -11 => LibusbError::NoMem,
            -12 => LibusbError::NotSupported,
            -99 => LibusbError::Other,
            _   => LibusbError::Other,
        }
    }
}

impl UsbSpeed {
    pub fn from_raw(value: u8) -> Self {
        match value {
            0 => UsbSpeed::Unknown,
            1 => UsbSpeed::Low,
            2 => UsbSpeed::Full,
            3 => UsbSpeed::High,
            4 => UsbSpeed::Super,
            5 => UsbSpeed::SuperPlus,
            6 => UsbSpeed::SuperPlusX2,
            _ => UsbSpeed::Unknown,
        }
    }
}

impl crate::component::usb::configuration::Host for MyState {}
impl crate::component::usb::descriptors::Host for MyState {}
impl crate::component::usb::errors::Host for MyState {}

impl HostTransfer for MyState {
    fn submit_transfer(
        &mut self,
        self_: Resource<UsbTransfer>,
        data: Vec<u8>,
    ) -> Result<(), component::usb::transfers::LibusbError> {
        debug!("Submit transfer");
        let usb_transfer = self.table.get_mut(&self_).expect("Failed to get transfer");
        debug!("Transfer: {:?}", usb_transfer);
        let transfer_ptr = usb_transfer.transfer;

        if usb_transfer.completed.load(Ordering::SeqCst) {
            warn!("Transfer already completed");
            return Err(LibusbError::Busy);
        }

        unsafe {
            let transfer_type = (*transfer_ptr).transfer_type;
            debug!("Transfer type: {:?}", transfer_type);

            if transfer_type == LIBUSB_TRANSFER_TYPE_CONTROL {
                let setup_buf = (*transfer_ptr).buffer;
                if !setup_buf.is_null() {
                    let bm_request_type = usb_transfer.control_setup.unwrap().bm_request_type;
                    let direction_in = bm_request_type & 0x80 != 0;
                    if direction_in {
                        debug!("Control transfer IN");
                    } else {
                        debug!("Control transfer OUT");
                        if data.len() as u32 != usb_transfer.buf_len {
                            error!(
                                "Invalid data length for control transfer OUT: {}, expected {}",
                                data.len(), usb_transfer.buf_len
                            );
                            return Err(LibusbError::InvalidParam);
                        }
                        let buf_ptr = (*transfer_ptr).buffer;
                        if !buf_ptr.is_null() {
                            debug!("Copying data to control transfer OUT buffer");
                            std::ptr::copy_nonoverlapping(
                                data.as_ptr(),
                                setup_buf.add(8),
                                data.len(),
                            );
                        }
                    }
                }
            } else if (*transfer_ptr).endpoint & 0x80 != 0 {
                info!("IN transfer");
            } else {
                info!("OUT transfer");
                if data.len() as u32 != usb_transfer.buf_len {
                    error!(
                        "Invalid data length for OUT transfer: {}, expected {}",
                        data.len(), usb_transfer.buf_len
                    );
                    return Err(LibusbError::InvalidParam);
                }
                let buf_ptr = (*transfer_ptr).buffer;
                if !buf_ptr.is_null() {
                    debug!("Copying data to OUT transfer buffer");
                    std::ptr::copy_nonoverlapping(data.as_ptr(), buf_ptr, data.len());
                }
            }

            debug!("creating transfer context");
            let (sender, receiver) = oneshot::channel();

            let buffer_box = usb_transfer.buffer.take().expect("buffer not allocated");
            let iso_pr = usb_transfer.iso_packet_results.clone(); // clone Arc for callback

            let ctx = Box::new(TransferContext {
                sender,
                completed: usb_transfer.completed.clone(),
                buffer: buffer_box,
                iso_packet_results: iso_pr,
            });

            (*transfer_ptr).user_data = Box::into_raw(ctx) as *mut _;
            (*transfer_ptr).callback = transfer_callback;

            debug!("submitting transfer: {:?}", transfer_ptr);
            let submit_result = libusb_submit_transfer(transfer_ptr);
            if submit_result < 0 {
                error!("Failed to submit transfer: {}", LibusbError::from_raw(submit_result));
                let _ = Box::from_raw((*transfer_ptr).user_data as *mut TransferContext);
                (*transfer_ptr).callback = empty_callback;
                (*transfer_ptr).user_data = std::ptr::null_mut();
                return Err(LibusbError::from_raw(submit_result));
            } else {
                debug!("transfer submitted");
                let transfer_mut = self.table.get_mut(&self_).expect("Failed to get transfer");
                transfer_mut.receiver = Some(receiver);
            }
        }
        Ok(())
    }

    fn cancel_transfer(&mut self, self_: Resource<UsbTransfer>) -> Result<(), LibusbError> {
        let usb_transfer = self.table.get(&self_).expect("Failed to get transfer");
        let transfer_ptr = usb_transfer.transfer;
        unsafe {
            if !usb_transfer.completed.load(Ordering::SeqCst) {
                let res = libusb_cancel_transfer(transfer_ptr);
                if res < 0 {
                    return Err(LibusbError::from_raw(res));
                }
            }
        }
        Ok(())
    }

    fn drop(&mut self, self_: Resource<UsbTransfer>) -> Result<(), Error> {
        trace!("Drop transfer");
        if let Ok(transfer) = self.table.get(&self_) {
            unsafe {
                if !transfer.completed.load(Ordering::SeqCst) {
                    let _ = libusb_cancel_transfer(transfer.transfer);
                }
            }
        }
        Ok(())
    }
}

impl crate::component::usb::transfers::Host for MyState {
    async fn await_transfer(
        &mut self,
        self_: Resource<UsbTransfer>,
    ) -> Result<Vec<u8>, LibusbError> {
        info!("Awaiting transfer");
        let usb_transfer = self.table.get_mut(&self_).expect("Failed to get transfer");

        if usb_transfer.receiver.is_none() {
            error!("Transfer receiver not set");
            return Err(LibusbError::NotFound);
        }

        let receiver = usb_transfer.receiver.take().ok_or(LibusbError::NotFound)?;
        info!("Transfer receiver set");

        let result = match receiver.await {
            Ok(result) => {
                info!("Transfer result: {:?}", result);
                result
            }
            Err(_) => Err(LibusbError::Interrupted),
        };

        self.table.delete(self_).ok();
        result
    }

    async fn await_iso_transfer(
        &mut self,
        self_: Resource<UsbTransfer>,
    ) -> Result<IsoResult, LibusbError> {
        info!("Awaiting ISO transfer");
        let usb_transfer = self.table.get_mut(&self_).expect("Failed to get transfer");

        let receiver = usb_transfer.receiver.take().ok_or(LibusbError::NotFound)?;
        let iso_results_arc = usb_transfer.iso_packet_results.clone();

        // Wait for transfer_callback to fire (same mechanism as await_transfer)
        let flat_data = match receiver.await {
            Ok(Ok(data))  => data,
            Ok(Err(e))    => return Err(e),
            Err(_)        => return Err(LibusbError::Interrupted),
        };

        // Read per-packet results stored by transfer_callback
        let raw_packets = iso_results_arc
            .lock()
            .unwrap()
            .take()
            .unwrap_or_default();

        let packets: Vec<IsoPacket> = raw_packets
            .iter()
            .map(|(actual_len, status)| IsoPacket {
                actual_length: *actual_len,
                status: match *status as i32 {
                    LIBUSB_TRANSFER_COMPLETED  => IsoPacketStatus::Success,
                    LIBUSB_TRANSFER_TIMED_OUT  => IsoPacketStatus::TimedOut,
                    LIBUSB_TRANSFER_CANCELLED  => IsoPacketStatus::Cancelled,
                    LIBUSB_TRANSFER_STALL      => IsoPacketStatus::Stall,
                    LIBUSB_TRANSFER_NO_DEVICE  => IsoPacketStatus::NoDevice,
                    LIBUSB_TRANSFER_OVERFLOW   => IsoPacketStatus::Overflow,
                    _                          => IsoPacketStatus::Error,
                },
            })
            .collect();

        self.table.delete(self_).ok();
        Ok(IsoResult { data: flat_data, packets })
    }
}

impl HostUsbDevice for MyState {
    fn open(
        &mut self,
        self_: Resource<UsbDevice>,
    ) -> Result<Resource<UsbDeviceHandle>, LibusbError> {
        let usb_device = self.table.get(&self_).expect("Failed to get device");
        let handle = self.backend.open(usb_device)?;
        let resource = self.table.push(handle).or(Err(LibusbError::Other))?;
        Ok(resource)
    }

    fn get_active_configuration_descriptor(
        &mut self,
        self_: Resource<UsbDevice>,
    ) -> Result<ConfigurationDescriptor, LibusbError> {
        let usb_device = self.table.get(&self_).expect("Failed to get device");
        self.backend.get_active_configuration_descriptor(usb_device)
    }

    fn get_configuration_descriptor(
        &mut self,
        self_: Resource<UsbDevice>,
        config_index: u8,
    ) -> Result<ConfigurationDescriptor, LibusbError> {
        let usb_device = self.table.get(&self_).expect("Failed to get device");
        self.backend.get_configuration_descriptor(usb_device, config_index)
    }

    fn get_configuration_descriptor_by_value(
        &mut self,
        self_: Resource<UsbDevice>,
        config_value: u8,
    ) -> Result<
        component::usb::device::ConfigurationDescriptor,
        component::usb::device::LibusbError,
    > {
        let usb_device = self.table.get(&self_).expect("Failed to get device");
        self.backend.get_configuration_descriptor_by_value(usb_device, config_value)
    }

    fn drop(&mut self, rep: Resource<UsbDevice>) -> Result<(), Error> {
        trace!("Drop device");
        if let Ok(device) = self.table.get(&rep) {
            unsafe {
                libusb_unref_device(device.device);
            }
        }
        Ok(())
    }
}

impl HostDeviceHandle for MyState {
    fn get_configuration(&mut self, self_: Resource<UsbDeviceHandle>) -> Result<u8, LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        self.backend.get_configuration(usb_device_handle)
    }

    fn set_configuration(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        config: ConfigValue,
    ) -> Result<(), LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        self.backend.set_configuration(usb_device_handle, config)
    }

    fn claim_interface(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        ifac: u8,
    ) -> Result<(), LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        self.backend.claim_interface(usb_device_handle, ifac)
    }

    fn release_interface(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        ifac: u8,
    ) -> Result<(), LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        self.backend.release_interface(usb_device_handle, ifac)
    }

    fn set_interface_altsetting(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        ifac: u8,
        alt_setting: u8,
    ) -> Result<(), LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        self.backend.set_interface_alt_setting(usb_device_handle, ifac, alt_setting)
    }

    fn clear_halt(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        endpoint: u8,
    ) -> Result<(), LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        self.backend.clear_halt(usb_device_handle, endpoint)
    }

    fn reset_device(&mut self, self_: Resource<UsbDeviceHandle>) -> Result<(), LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        self.backend.reset_device(usb_device_handle)
    }

    fn alloc_streams(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        num_streams: u32,
        endpoints: Vec<u8>,
    ) -> Result<(), component::usb::device::LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        let num_endpoints = endpoints.len() as i32;
        let endpoints_ptr = endpoints.as_ptr() as *mut u8;
        unsafe {
            let res = libusb_alloc_streams(usb_device_handle.handle, num_streams, endpoints_ptr, num_endpoints);
            match res {
                0.. => Ok(()),
                _   => Err(LibusbError::from_raw(res)),
            }
        }
    }

    fn free_streams(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        endpoints: Vec<u8>,
    ) -> Result<(), component::usb::device::LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        let num_endpoints = endpoints.len() as i32;
        let endpoints_ptr = endpoints.as_ptr() as *mut u8;
        unsafe {
            let res = libusb_free_streams(usb_device_handle.handle, endpoints_ptr, num_endpoints);
            match res {
                0.. => Ok(()),
                _   => Err(LibusbError::from_raw(res)),
            }
        }
    }

    fn kernel_driver_active(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        ifac: u8,
    ) -> Result<bool, LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        self.backend.kernel_driver_active(usb_device_handle, ifac)
    }

    fn detach_kernel_driver(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        ifac: u8,
    ) -> Result<(), LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        self.backend.detach_kernel_driver(usb_device_handle, ifac)
    }

    fn attach_kernel_driver(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        ifac: u8,
    ) -> Result<(), LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        self.backend.attach_kernel_driver(usb_device_handle, ifac)
    }

    fn new_transfer(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        xfer_type: TransferType,
        setup: TransferSetup,
        buf_size: u32,
        opts: TransferOptions,
    ) -> Result<Resource<UsbTransfer>, component::usb::device::LibusbError> {
        info!(
            "Starting new_transfer with buf_size: {buf_size} and transfer type: {:?}",
            xfer_type
        );

        let usb_handle = self.table.get(&self_).expect("Failed to get device handle");
        debug!("Retrieved USB device handle: {:?}", usb_handle.handle);

        unsafe {
            let iso_packets = if matches!(xfer_type, TransferType::Isochronous) {
                opts.iso_packets as i32
            } else {
                0
            };
            debug!("Calculated iso_packets: {iso_packets}");

            let transfer_ptr = libusb_alloc_transfer(iso_packets);
            if transfer_ptr.is_null() {
                error!("Failed to allocate USB transfer (libusb_alloc_transfer returned null)");
                return Err(LibusbError::NoMem);
            }
            debug!("Allocated transfer pointer: {:?}", transfer_ptr);

            (*transfer_ptr).dev_handle = usb_handle.handle;
            (*transfer_ptr).endpoint = opts.endpoint;
            (*transfer_ptr).transfer_type = match xfer_type {
                TransferType::Control     => LIBUSB_TRANSFER_TYPE_CONTROL,
                TransferType::Bulk        => LIBUSB_TRANSFER_TYPE_BULK,
                TransferType::Interrupt   => LIBUSB_TRANSFER_TYPE_INTERRUPT,
                TransferType::Isochronous => LIBUSB_TRANSFER_TYPE_ISOCHRONOUS,
            };
            (*transfer_ptr).timeout = opts.timeout_ms;
            debug!(
                "Transfer configured with endpoint: {}, type: {:?}, timeout: {}ms",
                opts.endpoint, (*transfer_ptr).transfer_type, opts.timeout_ms
            );

            if opts.stream_id != 0 {
                libusb_transfer_set_stream_id(transfer_ptr, opts.stream_id);
                debug!("Stream ID set to: {}", opts.stream_id);
            }

            let total_len: u32 = if (*transfer_ptr).transfer_type == LIBUSB_TRANSFER_TYPE_CONTROL {
                8 + buf_size
            } else {
                buf_size
            };
            debug!(
                "Calculated total transfer buffer size: {}, based on transfer type: {:?}",
                total_len, (*transfer_ptr).transfer_type
            );

            let mut buffer_vec = vec![0u8; total_len as usize];

            if (*transfer_ptr).transfer_type == LIBUSB_TRANSFER_TYPE_CONTROL {
                buffer_vec[0] = setup.bm_request_type;
                buffer_vec[1] = setup.b_request;
                buffer_vec[2] = (setup.w_value & 0xFF) as u8;
                buffer_vec[3] = (setup.w_value >> 8) as u8;
                buffer_vec[4] = (setup.w_index & 0xFF) as u8;
                buffer_vec[5] = (setup.w_index >> 8) as u8;
                buffer_vec[6] = (buf_size & 0xFF) as u8;
                buffer_vec[7] = ((buf_size >> 8) & 0xFF) as u8;
                debug!(
                    "Control transfer setup filled: bm_request_type: {}, b_request: {}, \
                     w_value: {}, w_index: {}",
                    setup.bm_request_type, setup.b_request, setup.w_value, setup.w_index
                );
            }

            let buffer_box = buffer_vec.into_boxed_slice();
            (*transfer_ptr).buffer = buffer_box.as_ptr() as *mut u8;
            (*transfer_ptr).length = total_len as i32;
            debug!("Transfer buffer configured with length: {}", total_len);

            if iso_packets > 0 {
                let packet_count = iso_packets as usize;
                let base_len = buf_size / iso_packets as u32;
                let rem = buf_size % iso_packets as u32;

                for i in 0..packet_count {
                    let desc_ptr = ((*transfer_ptr).iso_packet_desc.as_mut_ptr() as *mut libusb1_sys::libusb_iso_packet_descriptor).add(i);
                    let desc = &mut *desc_ptr;
                    let packet_len = if i == packet_count - 1 {
                        base_len + rem
                    } else {
                        base_len
                    };
                    desc.length = packet_len;
                    debug!("Iso packet {} configured with length: {}", i, packet_len);
                }
                (*transfer_ptr).num_iso_packets = iso_packets;
                info!("Isochronous transfer configured with {} packets", iso_packets);
            }

            let transfer_resource = self
                .table
                .push(UsbTransfer {
                    transfer: transfer_ptr,
                    buffer: Some(buffer_box),
                    buf_len: buf_size,
                    completed: Arc::new(AtomicBool::new(false)),
                    receiver: None,
                    control_setup: Option::from(setup),
                    iso_packet_results: Arc::new(Mutex::new(None)),
                })
                .or(Err(LibusbError::Other))?;
            info!("Transfer resource created successfully");

            Ok(transfer_resource)
        }
    }

    fn close(&mut self, _self_: Resource<UsbDeviceHandle>) {
        debug!("close handle: drop will be called automatically");
    }

    fn drop(&mut self, rep: Resource<UsbDeviceHandle>) -> Result<(), Error> {
        debug!("Drop device handle: {}", rep.owned());
        if let Ok(handle) = self.table.get(&rep) {
            unsafe {
                libusb_close(handle.handle);
            }
        }
        self.table.delete(rep).expect("resource was al dada");
        Ok(())
    }
}

impl crate::component::usb::device::Host for MyState {
    fn init(&mut self) -> Result<(), crate::component::usb::device::LibusbError> {
        self.backend.init()
    }

    fn list_devices(
        &mut self,
    ) -> Result<Vec<(Resource<UsbDevice>, DeviceDescriptor, DeviceLocation)>, LibusbError> {
        let devices = self.backend.list_devices(&self.allowed_usbdevices)?;
        let mut result = Vec::with_capacity(devices.len());
        for (dev, desc, loc) in devices {
            let resource = self.table.push(dev).or(Err(LibusbError::Other))?;
            result.push((resource, desc, loc));
        }
        Ok(result)
    }
}

impl crate::component::usb::usb_hotplug::Host for MyState {
    fn enable_hotplug(&mut self) -> Result<(), LibusbError> {
        self.backend.enable_hotplug(self.allowed_usbdevices.clone())
    }

    fn poll_events(&mut self) -> Vec<(Event, Info, Resource<UsbDevice>)> {
        let events = self.backend.poll_events();
        let mut out = Vec::with_capacity(events.len());
        for (event, info, device) in events {
            let resource = self
                .table
                .push(device)
                .or(Err(LibusbError::Other))
                .unwrap();
            out.push((event, info, resource));
        }
        out
    }
}

unsafe impl Send for FrameStream {}
unsafe impl Sync for FrameStream {}


// ── UVC Helper Functions ──────────────────────────────────────────────────────

fn find_best_streaming_interface_native(
    device_ptr: *mut libusb1_sys::libusb_device,
) -> Result<(u8, u8, u8, u16), Error> {
    unsafe {
        let mut config_desc: *const libusb1_sys::libusb_config_descriptor = std::ptr::null_mut();
        let res = libusb1_sys::libusb_get_active_config_descriptor(device_ptr, &mut config_desc);
        if res < 0 {
            return Err(Error::msg(format!("Failed to get active config: {}", res)));
        }
        
        let mut best: Option<(u8, u8, u8, u16)> = None;
        let config = &*config_desc;
        debug!("  Config has {} interfaces", config.bNumInterfaces);
        
        for i in 0..config.bNumInterfaces {
            let iface = &*config.interface.add(i as usize);
            for j in 0..iface.num_altsetting {
                let iface_desc = &*iface.altsetting.add(j as usize);
                
                if iface_desc.bInterfaceClass != USB_CLASS_VIDEO
                    || iface_desc.bInterfaceSubClass != USB_SUBCLASS_VIDEO_STREAMING
                {
                    continue;
                }
                
                for k in 0..iface_desc.bNumEndpoints {
                    let ep = &*iface_desc.endpoint.add(k as usize);
                    let is_iso_in = (ep.bEndpointAddress & 0x80 != 0) && (ep.bmAttributes & 0x03 == 1);
                    if !is_iso_in {
                        continue;
                    }
                    
                    let mps = ep.wMaxPacketSize;
                    let base_size = mps & 0x7FF;
                    let multiplier = 1 + ((mps >> 11) & 0x03);
                    let effective_size = base_size * multiplier;
                    
                    debug!("    Candidate streaming endpoint: {:02x}, mps: {}, mult: {}, effective: {}", ep.bEndpointAddress, base_size, multiplier, effective_size);

                    if best.map_or(true, |(_, _, _, s)| effective_size > s) {
                        best = Some((
                            iface_desc.bInterfaceNumber,
                            iface_desc.bAlternateSetting,
                            ep.bEndpointAddress,
                            effective_size,
                        ));
                    }
                }
            }
        }
        
        libusb1_sys::libusb_free_config_descriptor(config_desc);
        best.ok_or_else(|| Error::msg("No UVC streaming interface found"))
    }
}

fn parse_payload_header(data: &[u8]) -> (usize, bool) {
    if data.len() < 2 {
        return (0, false);
    }
    let header_len = data[0] as usize;
    if header_len < 2 || header_len > data.len() {
        return (0, false);
    }
    let end_of_frame = (data[1] & 0x02) != 0;
    (header_len, end_of_frame)
}

impl crate::bindings::component::usb::cv_ui::Host for MyState {}
impl HostRenderer for MyState {
    fn new(&mut self, _title: String) -> Resource<RendererStub> {
        self.table.push(RendererStub).unwrap()
    }
    fn render(&mut self, _self_: Resource<RendererStub>, _f: Frame, detections: Vec<Detection>) -> () {
        if !detections.is_empty() {
            println!("Detections: {:?}", detections);
        }
    }
    fn drop(&mut self, rep: Resource<RendererStub>) -> wasmtime::Result<()> {
        let _ = self.table.delete(rep); Ok(())
    }
}

impl HostFrameStream for MyState {
    fn new(&mut self, index: u32) -> Resource<FrameStream> {
        info!("Creating FrameStream for camera index {}", index);
        
        // 1. First, try to see if this is a USB device we can handle via UVC
        let mut uvc_device = None;
        if let Ok(devices) = self.backend.list_devices(&self.allowed_usbdevices) {
            let uvc_devices: Vec<_> = devices.into_iter()
                .filter(|(_, desc, _)| desc.device_class == USB_CLASS_VIDEO || desc.device_class == 0xEF)
                .collect();
            
            info!("Found {} USB UVC-compatible devices", uvc_devices.len());
            for (i, (_, desc, _)) in uvc_devices.iter().enumerate() {
                info!("  USB UVC Index {}: Vendor={:04x}, Product={:04x}", i, desc.vendor_id, desc.product_id);
            }

            // Heuristic: If index > 0, it might be the first USB camera (common on Mac where 0 is integrated)
            if (index as usize) < uvc_devices.len() {
                uvc_device = Some(uvc_devices[index as usize].0.clone());
            } else if index > 0 && (index as usize - 1) < uvc_devices.len() {
                info!("Index {} not found in UVC list, but matching against UVC index {} (heuristic for Mac)", index, index - 1);
                uvc_device = Some(uvc_devices[index as usize - 1].0.clone());
            } else if uvc_devices.len() == 1 {
                // If only one USB camera is present, always try it if the index is 0 or 1?
                // Let's stick to the heuristic first.
            }
        }

        if let Some(device) = uvc_device {
            info!("Detected USB UVC device at index {}. Using raw UVC driver.", index);
            match self.backend.open(&device) {
                Ok(handle) => {
                    if let Ok((iface_num, alt_setting, ep_addr, max_packet_size)) = find_best_streaming_interface_native(device.device) {
                        info!("Found UVC interface {} with alt setting {} and endpoint {:02x}", iface_num, alt_setting, ep_addr);
                        
                        // Claim interface
                        if self.backend.claim_interface(&handle, iface_num).is_ok() {
                            // UVC Handshake (Standard GET/SET/COMMIT)
                            let timeout = 2000;
                            let mut probe = vec![0u8; 34];
                            
                            unsafe {
                                // 1. GET_CUR (Probe)
                                libusb1_sys::libusb_control_transfer(
                                    handle.handle, 0xA1, 0x81, 0x0100, iface_num as u16, probe.as_mut_ptr(), 34, timeout
                                );
                                
                                // 2. Modify probe (Format 1 = MJPEG usually, Frame 1 = Highest Resolution usually)
                                if probe.len() >= 4 {
                                    probe[2] = 2; // MJPEG (Format Index)
                                    probe[3] = 1; // Frame Index
                                    // Set frame interval (e.g., 333333 for 30fps)
                                    let interval = 333333u32;
                                    probe[4..8].copy_from_slice(&interval.to_le_bytes());
                                }
                                
                                // 3. SET_CUR (Probe)
                                libusb1_sys::libusb_control_transfer(handle.handle, 0x21, 0x01, 0x0100, iface_num as u16, probe.as_ptr() as *mut u8, probe.len() as u16, timeout);
                                
                                // 4. COMMIT_CONTROL
                                libusb1_sys::libusb_control_transfer(handle.handle, 0x21, 0x01, 0x0200, iface_num as u16, probe.as_ptr() as *mut u8, probe.len() as u16, timeout);
                            }

                            // Set Alt Setting
                            if self.backend.set_interface_alt_setting(&handle, iface_num, alt_setting).is_ok() {
                                let packet_stride = max_packet_size as u32;
                                let num_packets = 128;
                                let buffer_size = num_packets * packet_stride;
                                
                                info!("UVC Handshake successful. Stream activated.");
                                return self.table.push(FrameStream::Uvc(WebcamFrameStream {
                                    handle,
                                    iface_num,
                                    ep_addr,
                                    packet_stride,
                                    num_packets,
                                    buffer_size,
                                    actual_frame_size: 0, // Will be updated
                                    min_frame_size: 28_800,
                                    frame_count: 0,
                                    last_fid: 0,
                                    frame_buffer: Vec::with_capacity(buffer_size as usize),
                                    frame_started: false,
                                    transfer_buffer: vec![0u8; buffer_size as usize],
                                })).expect("Failed to push to table")
                            }
                        }
                    }
                }
                Err(e) => warn!("Failed to open USB device for UVC: {:?}", e),
            }
        }

        // 2. No fallback — strictly UVC
        error!("USB UVC failed or not found. Nokhwa fallback disabled as requested.");
        panic!("Failed to open USB UVC stream — check permissions or device availability");
    }

    fn read_frame(&mut self, self_: Resource<FrameStream>) -> Result<Frame, String> {
        let stream = self.table.get_mut(&self_).map_err(|e: ResourceTableError| e.to_string())?;
        
        match stream {
            FrameStream::Uvc(ref mut uvc) => {
                let timeout_ms = 2000;
                let mut attempts = 0;
                
                loop {
                    attempts += 1;
                    if attempts > 2000 {
                        return Err("Timeout waiting for UVC frame".to_string());
                    }

                    // 1. Setup and submit isochronous transfer
                    unsafe {
                        let xfer = libusb1_sys::libusb_alloc_transfer(uvc.num_packets as i32);
                        if xfer.is_null() { return Err("Failed to alloc libusb transfer".to_string()); }
                        
                        let completed = Arc::new(AtomicBool::new(false));
                        let completed_ptr = Arc::as_ptr(&completed);

                        libusb1_sys::libusb_fill_iso_transfer(
                            xfer, uvc.handle.handle, uvc.ep_addr, uvc.transfer_buffer.as_mut_ptr(), uvc.buffer_size as i32,
                            uvc.num_packets as i32, iso_callback, completed_ptr as *mut libc::c_void, timeout_ms
                        );
                        libusb1_sys::libusb_set_iso_packet_lengths(xfer, uvc.packet_stride);

                        if libusb1_sys::libusb_submit_transfer(xfer) != 0 {
                            libusb1_sys::libusb_free_transfer(xfer);
                            return Err("Failed to submit libusb transfer".to_string());
                        }

                        // 2. Wait for completion (using a small loop to handle events)
                        let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
                        while !completed.load(Ordering::Acquire) {
                            if Instant::now() > deadline {
                                libusb1_sys::libusb_cancel_transfer(xfer);
                                break;
                            }
                            let mut tv = libc::timeval { tv_sec: 0, tv_usec: 10_000 };
                            libusb1_sys::libusb_handle_events_timeout(std::ptr::null_mut(), &mut tv);
                        }

                        // 3. Collect packet lengths and free transfer
                        let mut lengths = Vec::with_capacity(uvc.num_packets as usize);
                        let mut errors = 0;
                        for i in 0..uvc.num_packets as usize {
                            let pkt_desc = ((*xfer).iso_packet_desc.as_ptr() as *const libusb1_sys::libusb_iso_packet_descriptor).add(i);
                            let status = (*pkt_desc).status;
                            if status != 0 {
                                errors += 1;
                            }
                            lengths.push((*pkt_desc).actual_length as usize);
                        }
                        if errors > 0 {
                            debug!("Isochronous transfer completed with {} packet errors", errors);
                        }
                        libusb1_sys::libusb_free_transfer(xfer);

                        // 4. Process packets to reassemble frame
                        let mut offset = 0usize;
                        for actual_len in lengths {
                            if actual_len > 0 {
                                let packet = &uvc.transfer_buffer[offset..offset + actual_len];
                                let (header_len, eof) = parse_payload_header(packet);
                                if header_len > 0 && header_len < actual_len {
                                    let fid = packet[1] & 0x01;
                                    let payload = &packet[header_len..];
                                    
                                    // Handle FID change - start of a new frame
                                    if uvc.frame_started && fid != uvc.last_fid {
                                        let frame_data = std::mem::replace(&mut uvc.frame_buffer, Vec::with_capacity(uvc.buffer_size as usize));
                                        uvc.last_fid = fid;
                                        uvc.frame_count += 1;
                                        if frame_data.len() >= uvc.min_frame_size {
                                            if let Ok(frame) = decode_and_wrap_frame(frame_data) {
                                                return Ok(frame);
                                            }
                                        }
                                    }
                                    
                                    uvc.frame_started = true;
                                    uvc.last_fid = fid;
                                    uvc.frame_buffer.extend_from_slice(payload);
                                    
                                    if eof {
                                        let frame_data = std::mem::replace(&mut uvc.frame_buffer, Vec::with_capacity(uvc.buffer_size as usize));
                                        uvc.frame_started = false;
                                        uvc.frame_count += 1;
                                        if frame_data.len() >= uvc.min_frame_size {
                                            if let Ok(frame) = decode_and_wrap_frame(frame_data) {
                                                return Ok(frame);
                                            }
                                        }
                                    }
                                }
                            }
                            offset += uvc.packet_stride as usize;
                        }
                    }
                }
            }
        }
    }

    fn drop(&mut self, rep: Resource<FrameStream>) -> Result<(), wasmtime::Error> {
        self.table.delete(rep).map_err(|e: wasmtime::component::ResourceTableError| wasmtime::Error::msg(e.to_string()))?;
        Ok(())
    }
}

impl MyState {
} // end of ResourceHostFrameStream for MyState impl

fn decode_and_wrap_frame(data: Vec<u8>) -> Result<Frame, String> {
    debug!("Decoding frame of size {} bytes", data.len());
    if data.starts_with(&[0xff, 0xd8]) {
        // MJPEG decoding
        let img = image::load_from_memory(&data).map_err(|e| format!("MJPEG decode error: {}", e))?;
        let rgb = img.to_rgb8();
        Ok(Frame {
            data: rgb.to_vec(),
            width: rgb.width(),
            height: rgb.height(),
        })
    } else {
        // YUYV fallback (Simplified: assume 640x480 if size matches 614400 bytes)
        let (w, h) = if data.len() == 640 * 480 * 2 { (640, 480) }
                    else if data.len() == 1280 * 720 * 2 { (1280, 720) }
                    else { (0, 0) };
        Ok(Frame { data, width: w, height: h })
    }
}

impl HostObjectDetector for MyState {
    fn new(&mut self, model_path: String) -> Resource<ObjectDetector> {
        let model = tract_onnx::onnx()
            .model_for_path(model_path).expect("Failed to load model")
            .with_input_fact(0, f32::fact(&[1, 3, 640, 640]).into()).expect("Failed to set input fact")
            .into_optimized().expect("Failed to optimize model")
            .into_runnable().expect("Failed to make model runnable");
        self.table.push(ObjectDetector { model }).expect("Failed to push to table")
    }

    fn detect(&mut self, self_: Resource<ObjectDetector>, f: Frame) -> Result<Vec<Detection>, String> {
        let detector = self.table.get(&self_).map_err(|e: wasmtime::component::ResourceTableError| e.to_string())?;
        
        let start = Instant::now();
        
        // Preprocessing
        let image = RgbImage::from_raw(f.width, f.height, f.data).ok_or_else(|| "Invalid frame data".to_string())?;
        let resized = image::imageops::resize(&image, 640, 640, image::imageops::FilterType::Triangle);
        
        // Convert to tensor
        let mut tensor = tract_ndarray::Array4::<f32>::zeros((1, 3, 640, 640));
        for y in 0..640 {
            for x in 0..640 {
                let pixel = resized.get_pixel(x, y);
                tensor[[0, 0, y as usize, x as usize]] = pixel[0] as f32 / 255.0;
                tensor[[0, 1, y as usize, x as usize]] = pixel[1] as f32 / 255.0;
                tensor[[0, 2, y as usize, x as usize]] = pixel[2] as f32 / 255.0;
            }
        }
        let tensor: Tensor = tensor.into();

        // Inference
        let _result = detector.model.run(tvec!(tensor.into())).map_err(|e| format!("{:?}", e))?;
        
        let duration = start.elapsed();
        if self.enable_yolo {
            info!("Inference took: {:?}", duration);
        }

        // Return empty detections for now as per requirement to focus on performance measurement
        Ok(vec![])
    }

    fn drop(&mut self, rep: Resource<ObjectDetector>) -> Result<(), wasmtime::Error> {
        self.table.delete(rep).map_err(|e: ResourceTableError| wasmtime::Error::msg(e.to_string()))?;
        Ok(())
    }
}

impl crate::component::usb::cv::Host for MyState {}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let cli = CliParser::parse();
    env_logger::Builder::new()
        .filter_module("usb_wasi_host", cli.debug_level.parse().unwrap_or(LevelFilter::Info))
        .init();

    // macOS specific nokhwa initialization for camera permissions
    nokhwa::nokhwa_initialize(|granted| {
        if granted {
            info!("Camera access granted by OS");
        } else {
            warn!("Camera access denied by OS");
        }
    });

    // Short sleep to allow the above initialization to start/register
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    info!("Starting WASM component");
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <component.wasm>", args[0]);
        std::process::exit(1);
    }

    let engine = Engine::new(
        Config::new()
            .async_support(true)
            .wasm_component_model_async(true),
    )?;
    debug!("{:?}", cli.usb_devices);

    let allowed_usbdevices = if cli.use_allow_list {
        AllowedUSBDevices::Allowed(cli.usb_devices)
    } else {
        AllowedUSBDevices::Denied(cli.usb_devices)
    };

    let mut wasi_args = vec![cli.component_path.to_string_lossy().to_string()];
    wasi_args.extend(cli.guest_args);

    let component = Component::from_file(&engine, &cli.component_path)?;
    let mut linker = Linker::new(&engine);
    Host::add_to_linker(&mut linker, |state: &mut MyState| state)?;
    wasmtime_wasi::add_to_linker_async(&mut linker)?;
    let mut store = Store::new(&engine, MyState::new(allowed_usbdevices, wasi_args, cli.enable_yolo));
    let command = Command::instantiate_async(&mut store, &component, &linker).await?;

    match command.wasi_cli_run().call_run(store).await {
        Ok(Ok(_)) => {},
        Ok(Err(_)) => error!("WASM component returned an error"),
        Err(e) => {
            if let Some(exit) = e.downcast_ref::<I32Exit>() {
                if exit.0 != 0 {
                    error!("WASM component exited with non-zero status: {}", exit.0);
                }
            } else {
                return Err(e);
            }
        }
    }

    info!("WASM component finished");
    Ok(())
}
