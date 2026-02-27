use libusb1_sys::constants::{
    LIBUSB_TRANSFER_COMPLETED, LIBUSB_TRANSFER_TYPE_BULK, LIBUSB_TRANSFER_TYPE_CONTROL,
    LIBUSB_TRANSFER_TYPE_INTERRUPT, LIBUSB_TRANSFER_TYPE_ISOCHRONOUS,
};
use libusb1_sys::{
    libusb_alloc_streams, libusb_alloc_transfer, libusb_cancel_transfer, libusb_close,
    libusb_free_streams, libusb_free_transfer, libusb_submit_transfer, libusb_transfer,
    libusb_transfer_set_stream_id, libusb_unref_device,
};

use wasmtime::component::*;
use wasmtime::{Config, Error};
use wasmtime::{Engine, Store};
use wasmtime_wasi::bindings::Command;
use wasmtime_wasi::{DirPerms, FilePerms, IoView, WasiCtx, WasiCtxBuilder, WasiView};

use std::env;
use log::{debug, error, info, trace, warn, LevelFilter};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::path::PathBuf;
use std::str::FromStr;
use clap::Parser;
use tokio::sync::oneshot;

use crate::component::usb::configuration::ConfigValue;
use crate::component::usb::descriptors::{ConfigurationDescriptor, DeviceDescriptor};
use crate::component::usb::device::{DeviceLocation, HostDeviceHandle, HostUsbDevice, TransferOptions, TransferSetup, TransferType, UsbSpeed};
use crate::component::usb::errors::LibusbError;
use crate::component::usb::transfers::{HostTransfer, Transfer};
use crate::component::usb::usb_hotplug::{Event, Info};

pub mod usb_backend;
pub use usb_backend::{HostUsbBackend, LibusbBackend, UsbDevice, UsbDeviceHandle};


#[derive(Debug)]
pub struct UsbTransfer {
    transfer: *mut libusb_transfer,
    completed: Arc<AtomicBool>,
    pub buffer: Option<Box<[u8]>>,
    pub buf_len: u32,
    receiver: Option<oneshot::Receiver<Result<Vec<u8>, LibusbError>>>,
    control_setup: Option<TransferSetup>
}

bindgen!({
    world: "host",
    path: "../wit",
    with: {
        "component:usb/transfers/transfer": UsbTransfer,
        "component:usb/device/usb-device": UsbDevice,
        "component:usb/device/device-handle": UsbDeviceHandle,
    },
    async: {
        only_imports: ["await-transfer"]
    },
});

// Context struct for transfer callback
struct TransferContext {
    sender: oneshot::Sender<Result<Vec<u8>, LibusbError>>,
    completed: Arc<AtomicBool>,
    buffer: Box<[u8]>,
}

// Safety: Ensure that the usage of `*mut libusb_device` is thread-safe.

unsafe impl Send for UsbTransfer {}
unsafe impl Sync for UsbTransfer {}

unsafe impl Send for MyState {}
unsafe impl Sync for MyState {}

#[derive(Parser)]
#[command(name = "usb-wasi-host", about, trailing_var_arg = true)]
struct CliParser {
    #[arg(short, long)]
    component_path: PathBuf,

    #[arg(long, short = 'd')]
    usb_devices: Vec<USBDeviceIdentifier>,

    #[arg(long, short)]
    use_allow_list: bool,

    // set the debug level
    #[arg(long = "debug_level", short = 'l', default_value = "info")]
    debug_level: String,

    // trailing args to pass to the guest component
    #[arg(allow_hyphen_values = true)]
    guest_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct USBDeviceIdentifier {
    vendor_id: u16,
    product_id: u16
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
    Denied(Vec<USBDeviceIdentifier>)
}

impl AllowedUSBDevices {
    pub fn is_allowed(&self, device: &USBDeviceIdentifier) -> bool {
        match self {
            Self::Allowed(devices) => devices.contains(device),
            Self::Denied(devices) => !devices.contains(device)
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
        // Initialize backend (context, etc.) immediately to be ready
        match backend.init() {
            Ok(_) => info!("Backend initialized"),
            Err(e) => error!("Failed to initialize backend: {:?}", e),
        }
        
        Self {
            table: ResourceTable::new(),
            ctx: WasiCtxBuilder::new()
                .inherit_stdio()
                .args(&guest_args)
                .preopened_dir(env::current_dir().expect("failed to open dir"), ".", DirPerms::all(), FilePerms::all()).expect("failed to open dir")
                .build(),
            allowed_usbdevices,
            backend: Box::new(backend),
        }
    }
}


extern "system" fn transfer_callback(transfer: *mut libusb_transfer) {
    unsafe {
        // Reconstruct the context
        let ctx_ptr = (*transfer).user_data as *mut TransferContext;
        let ctx = Box::from_raw(ctx_ptr);
        // Determine transfer status and prepare result
        let status = (*transfer).status;
        let result: Result<Vec<u8>, LibusbError> =
            if status == LIBUSB_TRANSFER_COMPLETED {
                // Transfer completed successfully
                let mut data_vec = Vec::new();
                if (*transfer).num_iso_packets > 0 {
                    // Isochronous transfer: combine data from all packets
                    let num_packets = (*transfer).num_iso_packets as usize;
                    let mut total_len: usize = 0;
                    for i in 0..num_packets {
                        let desc = (*transfer).iso_packet_desc.as_ptr().add(i);
                        total_len += (*desc).actual_length as usize;
                    }
                    let buf_ptr = (*transfer).buffer;
                    if !buf_ptr.is_null() && total_len > 0 {
                        let data_slice = std::slice::from_raw_parts(buf_ptr, total_len);
                        data_vec = data_slice.to_vec();
                    }
                } else if (*transfer).transfer_type == LIBUSB_TRANSFER_TYPE_CONTROL {
                    // Control transfer
                    // For control IN (device-to-host): skip setup packet (first 8 bytes)
                    let actual_len = (*transfer).actual_length as usize;
                    debug!("Control transfer completed with actual length: {}", actual_len);
                    
                    // Extract request type from the setup packet
                    let buf_ptr = (*transfer).buffer;
                    let bm_request_type = if !buf_ptr.is_null() { *buf_ptr } else { 0 };
                    let is_device_to_host = (bm_request_type & 0x80) != 0;
                    
                    if is_device_to_host && actual_len > 0 {
                        // For IN transfers, return the data after the setup packet
                        if !buf_ptr.is_null() {
                            // Get the data portion (skipping 8-byte setup)
                            let data_slice = std::slice::from_raw_parts(buf_ptr.add(8), actual_len);
                            data_vec = data_slice.to_vec();
                            debug!("Control IN transfer data: {:?}", data_vec);
                        }
                    } else {
                        // For OUT transfers, no data to return
                        data_vec = Vec::new();
                    }
                } else {
                    // Bulk/Interrupt transfer
                    let actual_len = (*transfer).actual_length as usize;
                    if (*transfer).endpoint & 0x80 != 0 {
                        // IN transfer: copy received data
                        if actual_len > 0 {
                            let buf_ptr = (*transfer).buffer;
                            if !buf_ptr.is_null() {
                                let data_slice = std::slice::from_raw_parts(buf_ptr, actual_len);
                                data_vec = data_slice.to_vec();
                            }
                        }
                    } else {
                        // OUT transfer: no data to return
                        data_vec = Vec::new();
                    }
                }
                Ok(data_vec)
            } else {
                // Transfer did not complete successfully, map status to LibusbError
                let err = match status {
                    LIBUSB_TRANSFER_TIMED_OUT => LibusbError::Timeout,
                    LIBUSB_TRANSFER_CANCELLED => LibusbError::Interrupted,
                    LIBUSB_TRANSFER_STALL => LibusbError::Pipe,
                    LIBUSB_TRANSFER_NO_DEVICE => LibusbError::NoDevice,
                    LIBUSB_TRANSFER_OVERFLOW => LibusbError::Overflow,
                    LIBUSB_TRANSFER_ERROR => LibusbError::Io,
                    _ => LibusbError::Other,
                };
                Err(err)
            };
        // Mark as completed
        ctx.completed.store(true, Ordering::SeqCst);
        // Send result (if receiver still exists)
        let _ = ctx.sender.send(result);
        // Free the libusb transfer struct
        libusb_free_transfer(transfer);
        // Box::from_raw has taken ownership of ctx, dropping it here will free buffer
        // (Buffer is inside ctx.buffer as Box<[u8]> and will be dropped automatically)
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
    /// Convert a raw `libusb_error` integer value to a `LibusbError` variant.
    pub fn from_raw(value: i32) -> Self {
        match value {
            -1 => LibusbError::Io,
            -2 => LibusbError::InvalidParam,
            -3 => LibusbError::Access,
            -4 => LibusbError::NoDevice,
            -5 => LibusbError::NotFound,
            -6 => LibusbError::Busy,
            -7 => LibusbError::Timeout,
            -8 => LibusbError::Overflow,
            -9 => LibusbError::Pipe,
            -10 => LibusbError::Interrupted,
            -11 => LibusbError::NoMem,
            -12 => LibusbError::NotSupported,
            -99 => LibusbError::Other,
            _ => LibusbError::Other, // Default to `Other` for unknown error codes
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

impl component::usb::configuration::Host for MyState {}
impl component::usb::descriptors::Host for MyState {}
impl component::usb::errors::Host for MyState {}

impl HostTransfer for MyState {
    fn submit_transfer(
        &mut self,
        self_: Resource<Transfer>,
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
                        // control transfer IN
                        debug!("Control transfer IN");
                    } else {
                        debug!("Control transfer OUT");
                        // control transfer out
                        if data.len() as u32 != usb_transfer.buf_len {
                            error!(
                                "Invalid data length for control transfer OUT: {}, expected {}",
                                data.len(),
                                usb_transfer.buf_len
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
                // IN transfer
                info!("IN transfer");
            } else {
                info!("OUT transfer");
                // OUT transfer
                if data.len() as u32 != usb_transfer.buf_len {
                    error!(
                        "Invalid data length for OUT transfer: {}, expected {}",
                        data.len(),
                        usb_transfer.buf_len
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
            let ctx = Box::new(TransferContext {
                sender,
                completed: usb_transfer.completed.clone(),
                buffer: buffer_box,
            });

            (*transfer_ptr).user_data = Box::into_raw(ctx) as *mut _;
            (*transfer_ptr).callback = transfer_callback;

            debug!("submitting transfer: {:?}", transfer_ptr);
            let submit_result = libusb_submit_transfer(transfer_ptr);
            if submit_result < 0 {
                error!(
                    "Failed to submit transfer: {}",
                    LibusbError::from_raw(submit_result)
                );
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
                } else {
                    // If the transfer is already completed, we can safely drop it
                    // without calling `libusb_cancel_transfer`.
                }
            }
        }
        Ok(())
    }
}

impl component::usb::transfers::Host for MyState {
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

        // Remove the transfer from the resource table to free memory
        self.table.delete(self_).ok();

        result
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
            let res = libusb_alloc_streams(
                usb_device_handle.handle,
                num_streams,
                endpoints_ptr,
                num_endpoints,
            );
            match res {
                0.. => Ok(()),
                _ => Err(LibusbError::from_raw(res)),
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
                _ => Err(LibusbError::from_raw(res)),
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
    ) -> Result<
        Resource<component::usb::device::Transfer>,
        component::usb::device::LibusbError,
    > {
        info!(
            "Starting new_transfer with buf_size: {buf_size} and transfer type: {:?}",
            xfer_type
        );

        let usb_handle = self.table.get(&self_).expect("Failed to get device handle");
        debug!("Retrieved USB device handle: {:?}", usb_handle.handle);

        unsafe {
            let iso_packets =
                if matches!(xfer_type, TransferType::Isochronous) {
                    opts.iso_packets as i32
                } else {
                    0
                };
            debug!("Calculated iso_packets: {iso_packets}");

            let transfer_ptr = libusb_alloc_transfer(iso_packets);
            if transfer_ptr.is_null() {
                log::error!(
                    "Failed to allocate USB transfer (libusb_alloc_transfer returned null)"
                );
                return Err(LibusbError::NoMem);
            }
            debug!("Allocated transfer pointer: {:?}", transfer_ptr);

            (*transfer_ptr).dev_handle = usb_handle.handle;
            (*transfer_ptr).endpoint = opts.endpoint;
            (*transfer_ptr).transfer_type = match xfer_type {
                TransferType::Control => LIBUSB_TRANSFER_TYPE_CONTROL,
                TransferType::Bulk => LIBUSB_TRANSFER_TYPE_BULK,
                TransferType::Interrupt => LIBUSB_TRANSFER_TYPE_INTERRUPT,
                TransferType::Isochronous => LIBUSB_TRANSFER_TYPE_ISOCHRONOUS,
            };
            (*transfer_ptr).timeout = opts.timeout_ms;
            debug!(
                "Transfer configured with endpoint: {}, type: {:?}, timeout: {}ms",
                opts.endpoint,
                (*transfer_ptr).transfer_type,
                opts.timeout_ms
            );

            if opts.stream_id != 0 {
                libusb_transfer_set_stream_id(transfer_ptr, opts.stream_id);
                debug!("Stream ID set to: {}", opts.stream_id);
            }

            let total_len: u32 = if (*transfer_ptr).transfer_type == LIBUSB_TRANSFER_TYPE_CONTROL {
                8 + buf_size
                // buf_size
            } else {
                buf_size
            };
            debug!(
                "Calculated total transfer buffer size: {}, based on transfer type: {:?}",
                total_len,
                (*transfer_ptr).transfer_type
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
                    "Control transfer setup filled: bm_request_type: {}, b_request: {}, w_value: {}, w_index: {}",
                    setup.bm_request_type,
                    setup.b_request,
                    setup.w_value,
                    setup.w_index
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
                    let desc = (*transfer_ptr).iso_packet_desc.as_mut_ptr().add(i);
                    let packet_len = if i == packet_count - 1 {
                        base_len + rem
                    } else {
                        base_len
                    };
                    (*desc).length = packet_len;
                    debug!("Iso packet {} configured with length: {}", i, packet_len);
                }

                (*transfer_ptr).num_iso_packets = iso_packets;
                info!(
                    "Isochronous transfer configured with {} packets",
                    iso_packets
                );
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
                })
                .or(Err(LibusbError::Other))?;
            info!("Transfer resource created successfully");

            Ok(transfer_resource)
        }
    }

    fn close(&mut self, self_: Resource<UsbDeviceHandle>) {
        debug!("close handle: does not do anything as drop will be automatically called");
        //
        // if (!self_.owned()) {
        //     return Ok(())
        // }
        //
        // if let Ok(handle) = self.table.get(&self_) {
        //     unsafe {
        //         libusb_close(handle.handle);
        //     }
        //     self.table.delete(self_).expect("resource was al dada");
        // }
        
    }

    fn drop(&mut self, rep: Resource<UsbDeviceHandle>) -> Result<(), Error> {
        debug!("Drop device handle: {}", rep.owned());
        if let Ok(handle) = self.table.get(&rep) {
            unsafe {
                libusb_close(handle.handle);
            }
            self.table.delete(rep).expect("resource was al dada");
        }
        Ok(())
    }
}

impl component::usb::device::Host for MyState {
    fn init(&mut self) -> Result<(), component::usb::device::LibusbError> {
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

impl component::usb::usb_hotplug::Host for MyState {
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
                .or(Err(LibusbError::Other)) // Wrapping error in case push fails, but push returns Result<Resource, TableError>
                .unwrap(); // Should handle this better
             out.push((event, info, resource));
        }
        out
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let cli = CliParser::parse();
    // Initialize the logger
    env_logger::Builder::new()
        .filter_module("usb_wasi_host", cli.debug_level.parse().unwrap_or(LevelFilter::Info))
        .init();
    
    info!("Starting WASM component");
    // Compile the `Component` that is being run for the application.
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <path_to_component>", args[0]);
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
    
    // Construct the argument list for the WASI guest (argv[0] is the program name)
    let mut wasi_args = vec![cli.component_path.to_string_lossy().to_string()];
    wasi_args.extend(cli.guest_args);

    let component = Component::from_file(&engine, &cli.component_path)?;
    let mut linker = Linker::new(&engine);
    Host_::add_to_linker(&mut linker, |state: &mut MyState| state)?;
    wasmtime_wasi::add_to_linker_async(&mut linker)?;
    let mut store = Store::new(&engine, MyState::new(allowed_usbdevices, wasi_args));
    let command = Command::instantiate_async(&mut store, &component, &linker).await?;
    match command.wasi_cli_run().call_run(store).await? {
        Ok(_) => {},
        Err(_) => error!("WASM component returned an error"),
    }
    info!("WASM component finished");
    Ok(())
}

