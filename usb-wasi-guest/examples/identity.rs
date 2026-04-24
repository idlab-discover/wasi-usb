/// Identity — trivial guest that lists USB devices and exits.
/// (Originally a passthrough for the wac composition pipeline; simplified to a standalone command.)
use wit_bindgen::generate;
generate!({ world: "guest", path: "../wit", generate_all });

use component::usb::device::list_devices;

fn main() {
    let devices = list_devices().expect("list_devices failed");
    println!("identity: {} device(s) visible", devices.len());
    for (_dev, desc, loc) in devices {
        println!(
            "  {:04x}:{:04x}  bus={} addr={}",
            desc.vendor_id, desc.product_id, loc.bus_number, loc.device_address
        );
    }
}
