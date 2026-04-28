// Copyright (c) 2026 IDLab Discover
// SPDX-License-Identifier: MIT

//! WASI-USB Host Runtime
//!
//! This crate implements the host-side of the WASI-USB interface, providing
//! WebAssembly modules with safe, capability-based access to USB devices.
//! It handles WIT-to-native mapping and USB transfer orchestration.
//! Architecture: dumb host, smart guest — no UVC, MJPEG, or ML in the host.

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

use wasmtime::component::{Component, Linker, Resource, ResourceTable};
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

use crate::component::usb::configuration::ConfigValue;
use crate::component::usb::descriptors::{ConfigurationDescriptor, DeviceDescriptor};
use crate::component::usb::device::{
    DeviceLocation, HostDeviceHandle, HostUsbDevice, TransferOptions, TransferSetup,
    TransferType, UsbSpeed,
};
use crate::component::usb::errors::LibusbError;
use crate::component::usb::transfers::{
    HostTransfer, TransferResult, IsoPacket, IsoPacketStatus,
};
use crate::component::usb::usb_hotplug::{Event, Info};

pub mod usb_backend;
pub use usb_backend::{HostUsbBackend, LibusbBackend, UsbDevice, UsbDeviceHandle};

pub mod instrument;
use instrument::CallTrace;

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
        },
        async: {
            only_imports: ["await-transfer"]
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
}

impl MyState {
    pub fn new(allowed_usbdevices: AllowedUSBDevices, guest_args: Vec<String>) -> Self {
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
        }
    }
}

extern "system" fn transfer_callback(transfer: *mut libusb_transfer) {
    unsafe {
        let ctx_ptr = (*transfer).user_data as *mut TransferContext;
        let ctx = Box::from_raw(ctx_ptr);

        let status = (*transfer).status;
        debug!("transfer_callback fired, status: {}", status);
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
                    debug!("ISO transfer received {} total actual bytes", total_actual_len);

                    // Store per-packet results so await-iso-transfer can read them
                    *ctx.iso_packet_results.lock().unwrap() = Some(packet_results);

                    // Copy full buffer (stride = packet_size, not actual_length)
                    let full_len = (*transfer).length as usize;
                    debug!("ISO transfer received {} bytes of data", full_len);
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
        let _xfer_type_raw = unsafe { (*usb_transfer.transfer).transfer_type };
        let _xfer_dir = unsafe { (*usb_transfer.transfer).endpoint & 0x80 != 0 };
        let _t = CallTrace::enter("submit_transfer").detail(&format!(
            "xfer_type={} len={} dir={}",
            match _xfer_type_raw {
                0 => "Control", 1 => "Isochronous", 2 => "Bulk", 3 => "Interrupt", _ => "Unknown"
            },
            usb_transfer.buf_len,
            if _xfer_dir { "In" } else { "Out" },
        ));
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
                debug!("IN transfer");
            } else {
                debug!("OUT transfer");
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
        // `await_transfer` already calls `table.delete` on successful completion,
        // so this delete only succeeds for transfers that were never awaited.
        if let Ok(transfer) = self.table.delete(self_) {
            unsafe {
                if transfer.completed.load(Ordering::SeqCst) {
                    // Callback already fired and called libusb_free_transfer.
                    // Don't free again.
                } else if transfer.receiver.is_some() {
                    // Transfer was submitted and the callback hasn't fired yet.
                    // Cancel it; the callback will call libusb_free_transfer.
                    let _ = libusb_cancel_transfer(transfer.transfer);
                } else {
                    // Transfer was allocated (new_transfer) but never submitted.
                    // No callback will ever fire; free it ourselves.
                    libusb_free_transfer(transfer.transfer);
                }
            }
        }
        Ok(())
    }
}

impl crate::component::usb::transfers::Host for MyState {
    /// Wait for a transfer to complete and return the result.
    ///
    /// For isochronous transfers the `packets` field of `TransferResult` is
    /// populated from the per-packet results stored by `transfer_callback`.
    /// For all other transfer types `packets` is empty.
    async fn await_transfer(
        &mut self,
        self_: Resource<UsbTransfer>,
    ) -> Result<TransferResult, LibusbError> {
        debug!("Awaiting transfer");
        let _t = CallTrace::enter("await_transfer");
        let usb_transfer = self.table.get_mut(&self_).expect("Failed to get transfer");

        if usb_transfer.receiver.is_none() {
            error!("Transfer receiver not set");
            return Err(LibusbError::NotFound);
        }

        let receiver = usb_transfer.receiver.take().ok_or(LibusbError::NotFound)?;
        let iso_results_arc = usb_transfer.iso_packet_results.clone();

        let data = match receiver.await {
            Ok(Ok(data)) => {
                debug!("Transfer completed, {} bytes", data.len());
                data
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(LibusbError::Interrupted),
        };

        // Read per-packet results for isochronous transfers (empty for others).
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
                    LIBUSB_TRANSFER_COMPLETED => IsoPacketStatus::Success,
                    LIBUSB_TRANSFER_TIMED_OUT => IsoPacketStatus::TimedOut,
                    LIBUSB_TRANSFER_CANCELLED => IsoPacketStatus::Cancelled,
                    LIBUSB_TRANSFER_STALL     => IsoPacketStatus::Stall,
                    LIBUSB_TRANSFER_NO_DEVICE => IsoPacketStatus::NoDevice,
                    LIBUSB_TRANSFER_OVERFLOW  => IsoPacketStatus::Overflow,
                    _                         => IsoPacketStatus::Error,
                },
            })
            .collect();

        self.table.delete(self_).ok();
        Ok(TransferResult { data, packets })
    }
}

impl HostUsbDevice for MyState {
    fn open(
        &mut self,
        self_: Resource<UsbDevice>,
    ) -> Result<Resource<UsbDeviceHandle>, LibusbError> {
        let usb_device = self.table.get(&self_).expect("Failed to get device");
        let _t = CallTrace::enter("open_device");
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
        debug!(
            "Starting new_transfer with buf_size: {buf_size} and transfer type: {:?}",
            xfer_type
        );
        let _t = CallTrace::enter("new_transfer").detail(&format!(
            "xfer_type={:?} buf_size={} ep={:#04x} iso_pkts={}",
            xfer_type, buf_size, opts.endpoint, opts.iso_packets,
        ));

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
                debug!("Isochronous transfer configured with {} packets", iso_packets);
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
            debug!("Transfer resource created successfully");

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
        let _t = CallTrace::enter("list_devices");
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

// ── Hotplug ───────────────────────────────────────────────────────────────────
// UVC handshake, frame reassembly, and protocol-specific logic are guest concerns.
// The host provides only generic USB primitives (dumb-host / smart-guest).

// ── Hotplug ───────────────────────────────────────────────────────────────────
#[tokio::main]
async fn main() -> Result<(), Error> {
    let cli = CliParser::parse();

    // Logging setup:
    // - If RUST_LOG is set, honour it exactly (supports wasi_usb_trace=info etc.)
    // - Otherwise fall back to --debug_level filter for the usb_wasi_host module.
    {
        let mut builder = env_logger::Builder::from_default_env();
        if std::env::var("RUST_LOG").is_err() {
            builder.filter_module(
                "usb_wasi_host",
                cli.debug_level.parse().unwrap_or(LevelFilter::Info),
            );
        }
        builder.init();
    }

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
    let mut store = Store::new(&engine, MyState::new(allowed_usbdevices, wasi_args));
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
