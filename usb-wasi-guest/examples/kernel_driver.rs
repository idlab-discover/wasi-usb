use wit_bindgen::generate;
generate!({ world: "guest", path: "../wit", generate_all });

use component::usb::{ device, configuration::ConfigValue };

fn main() {
    device::init().expect("init failed");
    let mut devs = device::list_devices().expect("list_devices failed");
    if devs.is_empty() { return; }
    let handle = devs.remove(0).0.open().expect("open failed");

    // set configuration 1
    handle
        .set_configuration(ConfigValue::Value(1))
        .expect("set_configuration failed");

    let ifac = 0;
    // check kernel driver
    let active = handle.kernel_driver_active(ifac).expect("kernel_driver_active failed");
    println!("kernel driver active on ifac {}: {}", ifac, active);

    // detach then re-attach
    handle
        .detach_kernel_driver(ifac)
        .expect("detach_kernel_driver failed");
    println!("detached kernel driver");

    handle
        .attach_kernel_driver(ifac)
        .expect("attach_kernel_driver failed");
    println!("re-attached kernel driver");

    handle.close();
}
