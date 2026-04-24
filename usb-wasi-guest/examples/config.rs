use wit_bindgen::generate;
generate!({ world: "guest", path: "../wit", generate_all });

use component::usb::device;
use component::usb::descriptors::ConfigurationDescriptor;

fn main() {
    device::init().expect("init failed");
    let devs = device::list_devices().expect("list_devices failed");
    if devs.is_empty() {
        println!("No devices.");
        return;
    }
    let dev = &devs[0].0;
    // by index
    let cfg0 = dev
        .get_configuration_descriptor(0)
        .expect("get cfg#0 failed");
    println!("cfg[0] via index: {:?}", cfg0);
    // by value
    let cfg1 = dev
        .get_configuration_descriptor_by_value(cfg0.configuration_value)
        .expect("get cfg by value failed");
    println!("cfg via value {}: {:?}", cfg0.configuration_value, cfg1);
}
