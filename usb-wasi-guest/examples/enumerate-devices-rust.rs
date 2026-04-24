use wit_bindgen::generate;
generate!({ world: "guest", path: "../wit", generate_all });

use component::usb::device::list_devices;

fn main() {
    let devices = list_devices().expect("Failed to list devices");
    for (_device, descriptor, _location) in devices {
        println!(
            "{:#06x}:{:#06x} (Manufacturer Index: {}, Product Index: {})",
            descriptor.vendor_id,
            descriptor.product_id,
            descriptor.manufacturer_index,
            descriptor.product_index,
        );
    }
}
