use crate::wasm_host::bindings::component::usb::descriptors::{DeviceDescriptor, ConfigurationDescriptor, InterfaceDescriptor, EndpointDescriptor};
use crate::wasm_host::bindings::component::usb::device::{DeviceLocation, UsbSpeed};
use crate::wasm_host::bindings::component::usb::errors::LibusbError;
use crate::wasm_host::bindings::component::usb::usb_hotplug::{Event, Info};
use crate::wasm_host::bindings::component::usb::configuration::{ConfigValue};

use libusb1_sys::{
    libusb_context, libusb_device, libusb_device_handle, libusb_init,
    libusb_get_device_list, libusb_free_device_list, libusb_get_device_descriptor,
    libusb_get_bus_number, libusb_get_device_address, libusb_get_port_number, libusb_get_device_speed,
    libusb_ref_device, libusb_unref_device, libusb_open, libusb_close,
    libusb_get_configuration, libusb_set_configuration, libusb_claim_interface, libusb_release_interface,
    libusb_set_interface_alt_setting, libusb_clear_halt, libusb_reset_device,
    libusb_set_auto_detach_kernel_driver,
    libusb_kernel_driver_active, libusb_detach_kernel_driver, libusb_attach_kernel_driver,
    libusb_has_capability, libusb_hotplug_callback_handle, libusb_hotplug_register_callback,
    libusb_handle_events_timeout_completed,
};
use libusb1_sys::constants::{
    LIBUSB_CAP_HAS_HOTPLUG, LIBUSB_HOTPLUG_EVENT_DEVICE_ARRIVED, LIBUSB_HOTPLUG_EVENT_DEVICE_LEFT,
    LIBUSB_HOTPLUG_MATCH_ANY, LIBUSB_HOTPLUG_NO_FLAGS,
};

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::collections::VecDeque;
use std::thread;
use log::{info, error};
use libc::timeval;
use once_cell::sync::Lazy;

pub enum AllowedUSBDevices {
    All,
    Allow(Vec<USBDeviceIdentifier>),
    Denied(Vec<USBDeviceIdentifier>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct USBDeviceIdentifier {
    pub vendor_id: u16,
    pub product_id: u16,
}

impl AllowedUSBDevices {
    pub fn is_allowed(&self, id: &USBDeviceIdentifier) -> bool {
        match self {
            AllowedUSBDevices::All => true,
            AllowedUSBDevices::Allow(list) => list.contains(id),
            AllowedUSBDevices::Denied(list) => !list.contains(id),
        }
    }
}

static HOTPLUG_QUEUE: Lazy<Mutex<VecDeque<(Event, Info, UsbDevice)>>> = Lazy::new(|| Mutex::new(VecDeque::new()));

pub struct UsbDevice { pub device: *mut libusb1_sys::libusb_device }
unsafe impl Send for UsbDevice {}
unsafe impl Sync for UsbDevice {}

pub struct UsbDeviceHandle { pub handle: *mut libusb1_sys::libusb_device_handle }
unsafe impl Send for UsbDeviceHandle {}
unsafe impl Sync for UsbDeviceHandle {}

pub struct UsbTransfer { 
    pub transfer: *mut libusb1_sys::libusb_transfer,
    pub receiver: Option<tokio::sync::oneshot::Receiver<Result<Vec<u8>, LibusbError>>>,
}
unsafe impl Send for UsbTransfer {}
unsafe impl Sync for UsbTransfer {}

pub trait HostUsbBackend: Send + Sync {
    fn init(&mut self) -> Result<(), LibusbError>;
    fn list_devices(&mut self, allowed: &AllowedUSBDevices) -> Result<Vec<(UsbDevice, DeviceDescriptor, DeviceLocation)>, LibusbError>;
    fn open(&mut self, device: &UsbDevice) -> Result<UsbDeviceHandle, LibusbError>;
    fn get_configuration(&mut self, handle: &UsbDeviceHandle) -> Result<u8, LibusbError>;
    fn set_configuration(&mut self, handle: &UsbDeviceHandle, config: ConfigValue) -> Result<(), LibusbError>;
    fn claim_interface(&mut self, handle: &UsbDeviceHandle, ifac: u8) -> Result<(), LibusbError>;
    fn release_interface(&mut self, handle: &UsbDeviceHandle, ifac: u8) -> Result<(), LibusbError>;
    fn set_interface_alt_setting(&mut self, handle: &UsbDeviceHandle, ifac: u8, alt: u8) -> Result<(), LibusbError>;
    fn clear_halt(&mut self, handle: &UsbDeviceHandle, endpoint: u8) -> Result<(), LibusbError>;
    fn reset_device(&mut self, handle: &UsbDeviceHandle) -> Result<(), LibusbError>;
    fn kernel_driver_active(&mut self, handle: &UsbDeviceHandle, ifac: u8) -> Result<bool, LibusbError>;
    fn detach_kernel_driver(&mut self, handle: &UsbDeviceHandle, ifac: u8) -> Result<(), LibusbError>;
    fn attach_kernel_driver(&mut self, handle: &UsbDeviceHandle, ifac: u8) -> Result<(), LibusbError>;
    fn get_active_configuration_descriptor(&mut self, device: &UsbDevice) -> Result<ConfigurationDescriptor, LibusbError>;
    fn get_configuration_descriptor(&mut self, device: &UsbDevice, index: u8) -> Result<ConfigurationDescriptor, LibusbError>;
    fn get_configuration_descriptor_by_value(&mut self, device: &UsbDevice, value: u8) -> Result<ConfigurationDescriptor, LibusbError>;
    fn alloc_streams(&mut self, handle: &UsbDeviceHandle, num: u32, endpoints: &[u8]) -> Result<(), LibusbError>;
    fn free_streams(&mut self, handle: &UsbDeviceHandle, endpoints: &[u8]) -> Result<(), LibusbError>;
    fn new_transfer(&mut self, handle: &UsbDeviceHandle, t: crate::wasm_host::bindings::component::usb::transfers::TransferType, setup: crate::wasm_host::bindings::component::usb::transfers::TransferSetup, size: u32, opts: crate::wasm_host::bindings::component::usb::transfers::TransferOptions) -> Result<UsbTransfer, LibusbError>;
    fn submit_transfer(&mut self, xfer: &UsbTransfer, data: Vec<u8>) -> Result<(), LibusbError>;
    fn cancel_transfer(&mut self, xfer: &UsbTransfer) -> Result<(), LibusbError>;
    fn await_transfer(&mut self, xfer: &UsbTransfer) -> Result<Vec<u8>, LibusbError>;
    fn await_iso_transfer(&mut self, xfer: &UsbTransfer) -> Result<crate::wasm_host::bindings::component::usb::transfers::IsoResult, LibusbError>;
}

pub struct LibusbBackend {
    context: Option<*mut libusb_context>,
    event_loop_flag: Option<Arc<AtomicBool>>,
    event_thread: Option<thread::JoinHandle<()>>,
}

unsafe impl Send for LibusbBackend {}
unsafe impl Sync for LibusbBackend {}

impl LibusbBackend {
    pub fn new() -> Self { Self { context: None, event_loop_flag: None, event_thread: None } }
}

impl LibusbError {
    pub fn from_i32(n: i32) -> Self {
        match n {
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
            _ => LibusbError::Other,
        }
    }
}

impl UsbSpeed {
    pub fn from_u8(n: u8) -> Self {
        match n {
            1 => UsbSpeed::Low,
            2 => UsbSpeed::Full,
            3 => UsbSpeed::High,
            4 => UsbSpeed::Super,
            5 => UsbSpeed::SuperPlus,
            _ => UsbSpeed::Unknown,
        }
    }
}

extern "system" fn hotplug_cb(_: *mut libusb_context, dev: *mut libusb1_sys::libusb_device, ev: libusb1_sys::libusb_hotplug_event, _user_data: *mut std::ffi::c_void) -> std::os::raw::c_int {
    unsafe {
        let mut desc = std::mem::MaybeUninit::<libusb1_sys::libusb_device_descriptor>::uninit();
        if libusb_get_device_descriptor(dev, desc.as_mut_ptr()) < 0 { return 0; }
        let d = desc.assume_init();
        let info = Info {
            bus: libusb_get_bus_number(dev),
            address: libusb_get_device_address(dev),
            vendor: d.idVendor,
            product: d.idProduct,
        };
        let event = match ev {
            LIBUSB_HOTPLUG_EVENT_DEVICE_ARRIVED => Event::ARRIVED,
            LIBUSB_HOTPLUG_EVENT_DEVICE_LEFT => Event::LEFT,
            _ => return 0,
        };
        libusb_ref_device(dev); 
        HOTPLUG_QUEUE.lock().unwrap().push_back((event, info, UsbDevice{ device: dev }));
        0
    }
}

impl HostUsbBackend for LibusbBackend {
    fn init(&mut self) -> Result<(), LibusbError> {
        if self.context.is_some() { return Ok(()); }
        unsafe {
            let mut ctx: *mut libusb_context = std::ptr::null_mut();
            let res = libusb_init(&mut ctx);
            if res < 0 { return Err(LibusbError::from_i32(res)); }
            self.context = Some(ctx);
            let flag = Arc::new(AtomicBool::new(true));
            self.event_loop_flag = Some(flag.clone());
            let ctx_num = ctx as usize;
            self.event_thread = Some(thread::spawn(move || {
                let tv = timeval { tv_sec: 0, tv_usec: 20_000 };
                while flag.load(Ordering::SeqCst) {
                    libusb_handle_events_timeout_completed(ctx_num as *mut libusb_context, &tv, std::ptr::null_mut());
                }
            }));
            Ok(())
        }
    }

    fn list_devices(&mut self, allowed: &AllowedUSBDevices) -> Result<Vec<(UsbDevice, DeviceDescriptor, DeviceLocation)>, LibusbError> {
        unsafe {
            let mut list: *mut *mut libusb1_sys::libusb_device = std::ptr::null_mut();
            let cnt = libusb_get_device_list(self.context.unwrap(), &mut list as *mut _ as *mut _);
            if cnt < 0 { return Err(LibusbError::from_i32(cnt as i32)); }
            let mut res = Vec::new();
            for i in 0..cnt {
                let dev = *list.add(i as usize);
                let mut d = std::mem::MaybeUninit::<libusb1_sys::libusb_device_descriptor>::uninit();
                if libusb_get_device_descriptor(dev, d.as_mut_ptr()) < 0 { continue; }
                let d = d.assume_init();
                if !allowed.is_allowed(&USBDeviceIdentifier { vendor_id: d.idVendor, product_id: d.idProduct }) { continue; }
                libusb_ref_device(dev);
                res.push((
                    UsbDevice { device: dev },
                    DeviceDescriptor {
                        length: d.bLength, descriptor_type: d.bDescriptorType, usb_version_bcd: d.bcdUSB,
                        device_class: d.bDeviceClass, device_subclass: d.bDeviceSubClass, device_protocol: d.bDeviceProtocol,
                        max_packet_size0: d.bMaxPacketSize0, vendor_id: d.idVendor, product_id: d.idProduct,
                        device_version_bcd: d.bcdDevice, manufacturer_index: d.iManufacturer, product_index: d.iProduct,
                        serial_number_index: d.iSerialNumber, num_configurations: d.bNumConfigurations,
                    },
                    DeviceLocation {
                        bus_number: libusb_get_bus_number(dev), device_address: libusb_get_device_address(dev),
                        port_number: libusb_get_port_number(dev), speed: UsbSpeed::from_u8(libusb_get_device_speed(dev) as u8)
                    }
                ));
            }
            libusb_free_device_list(list, 1);
            Ok(res)
        }
    }

    fn open(&mut self, d: &UsbDevice) -> Result<UsbDeviceHandle, LibusbError> {
        unsafe { let mut h: *mut libusb1_sys::libusb_device_handle = std::ptr::null_mut(); let rc = libusb_open(d.device, &mut h); if rc < 0 { return Err(LibusbError::from_i32(rc)); } Ok(UsbDeviceHandle { handle: h }) }
    }

    fn get_configuration(&mut self, h: &UsbDeviceHandle) -> Result<u8, LibusbError> {
        unsafe { let mut c: i32 = 0; let res = libusb1_sys::libusb_get_configuration(h.handle, &mut c); if res < 0 { Err(LibusbError::from_i32(res)) } else { Ok(c as u8) } }
    }
    fn set_configuration(&mut self, h: &UsbDeviceHandle, c: ConfigValue) -> Result<(), LibusbError> {
        unsafe { let v = match c { ConfigValue::Value(x) => x as i32, ConfigValue::Unconfigured => 0 }; let res = libusb1_sys::libusb_set_configuration(h.handle, v); if res < 0 { Err(LibusbError::from_i32(res)) } else { Ok(()) } }
    }
    fn claim_interface(&mut self, h: &UsbDeviceHandle, i: u8) -> Result<(), LibusbError> {
        unsafe { let res = libusb1_sys::libusb_claim_interface(h.handle, i as i32); if res < 0 { Err(LibusbError::from_i32(res)) } else { Ok(()) } }
    }
    fn release_interface(&mut self, h: &UsbDeviceHandle, i: u8) -> Result<(), LibusbError> {
        unsafe { let res = libusb1_sys::libusb_release_interface(h.handle, i as i32); if res < 0 { Err(LibusbError::from_i32(res)) } else { Ok(()) } }
    }
    fn set_interface_alt_setting(&mut self, h: &UsbDeviceHandle, i: u8, a: u8) -> Result<(), LibusbError> {
        unsafe { let res = libusb1_sys::libusb_set_interface_alt_setting(h.handle, i as i32, a as i32); if res < 0 { Err(LibusbError::from_i32(res)) } else { Ok(()) } }
    }
    fn clear_halt(&mut self, h: &UsbDeviceHandle, e: u8) -> Result<(), LibusbError> {
        unsafe { let res = libusb1_sys::libusb_clear_halt(h.handle, e); if res < 0 { Err(LibusbError::from_i32(res)) } else { Ok(()) } }
    }
    fn reset_device(&mut self, h: &UsbDeviceHandle) -> Result<(), LibusbError> {
        unsafe { let res = libusb1_sys::libusb_reset_device(h.handle); if res < 0 { Err(LibusbError::from_i32(res)) } else { Ok(()) } }
    }
    fn kernel_driver_active(&mut self, h: &UsbDeviceHandle, i: u8) -> Result<bool, LibusbError> {
        unsafe { let res = libusb1_sys::libusb_kernel_driver_active(h.handle, i as i32); match res { 0 => Ok(false), 1.. => Ok(true), _ => Err(LibusbError::from_i32(res)) } }
    }
    fn detach_kernel_driver(&mut self, h: &UsbDeviceHandle, i: u8) -> Result<(), LibusbError> {
        unsafe { let res = libusb1_sys::libusb_detach_kernel_driver(h.handle, i as i32); if res < 0 { Err(LibusbError::from_i32(res)) } else { Ok(()) } }
    }
    fn attach_kernel_driver(&mut self, h: &UsbDeviceHandle, i: u8) -> Result<(), LibusbError> {
        unsafe { let res = libusb1_sys::libusb_attach_kernel_driver(h.handle, i as i32); if res < 0 { Err(LibusbError::from_i32(res)) } else { Ok(()) } }
    }
    fn get_active_configuration_descriptor(&mut self, d: &UsbDevice) -> Result<ConfigurationDescriptor, LibusbError> {
        unsafe { let mut c: *const libusb1_sys::libusb_config_descriptor = std::ptr::null(); let res = libusb1_sys::libusb_get_active_config_descriptor(d.device, &mut c); if res < 0 { Err(LibusbError::from_i32(res)) } else { let desc = generate_config_descriptor(&*c); libusb1_sys::libusb_free_config_descriptor(c); Ok(desc) } }
    }
    fn get_configuration_descriptor(&mut self, d: &UsbDevice, i: u8) -> Result<ConfigurationDescriptor, LibusbError> {
        unsafe { let mut c: *const libusb1_sys::libusb_config_descriptor = std::ptr::null(); let res = libusb1_sys::libusb_get_config_descriptor(d.device, i, &mut c); if res < 0 { Err(LibusbError::from_i32(res)) } else { let desc = generate_config_descriptor(&*c); libusb1_sys::libusb_free_config_descriptor(c); Ok(desc) } }
    }
    fn get_configuration_descriptor_by_value(&mut self, d: &UsbDevice, v: u8) -> Result<ConfigurationDescriptor, LibusbError> {
        unsafe { let mut c: *const libusb1_sys::libusb_config_descriptor = std::ptr::null(); let res = libusb1_sys::libusb_get_config_descriptor_by_value(d.device, v, &mut c); if res < 0 { Err(LibusbError::from_i32(res)) } else { let desc = generate_config_descriptor(&*c); libusb1_sys::libusb_free_config_descriptor(c); Ok(desc) } }
    }
    fn alloc_streams(&mut self, h: &UsbDeviceHandle, n: u32, e: &[u8]) -> Result<(), LibusbError> { Ok(()) }
    fn free_streams(&mut self, h: &UsbDeviceHandle, e: &[u8]) -> Result<(), LibusbError> { Ok(()) }
    fn new_transfer(&mut self, h: &UsbDeviceHandle, t: crate::wasm_host::bindings::component::usb::transfers::TransferType, s: crate::wasm_host::bindings::component::usb::transfers::TransferSetup, sz: u32, o: crate::wasm_host::bindings::component::usb::transfers::TransferOptions) -> Result<UsbTransfer, LibusbError> { Err(LibusbError::Other) }
    fn submit_transfer(&mut self, x: &UsbTransfer, d: Vec<u8>) -> Result<(), LibusbError> { Ok(()) }
    fn cancel_transfer(&mut self, x: &UsbTransfer) -> Result<(), LibusbError> { Ok(()) }
    fn await_transfer(&mut self, x: &UsbTransfer) -> Result<Vec<u8>, LibusbError> { Ok(vec![]) }
    fn await_iso_transfer(&mut self, x: &UsbTransfer) -> Result<crate::wasm_host::bindings::component::usb::transfers::IsoResult, LibusbError> { Err(LibusbError::Other) }
}

fn generate_config_descriptor(_c: &libusb1_sys::libusb_config_descriptor) -> ConfigurationDescriptor {
    ConfigurationDescriptor {
        length: 9, descriptor_type: 2, total_length: 0,
        configuration_value: 0, configuration_index: 0, attributes: 0, max_power: 0, interfaces: vec![]
    }
}
