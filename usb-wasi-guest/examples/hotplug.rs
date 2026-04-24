use wit_bindgen::generate;
use crate::component::usb::{device, usb_hotplug};
use crate::component::usb::device::init;

generate!({
    world: "guest",
    path: "../wit",
    generate_all,
});

fn main() {
    init().expect("Could not init backend");
    // 1. Turn hot-plug on (safe to call repeatedly).
    if let Err(err) = usb_hotplug::enable_hotplug() {
        eprintln!("Hot-plug not available: {:?}", err);
        return;
    }
    println!("Hot-plug enabled – attach or remove a USB device to test.");

    // 2. Show the initial device count so we know the backend is alive.
    match device::list_devices() {
        Ok(list) => println!("Initially {} devices present.", list.len()),
        Err(e) => {
            eprintln!("Could not list devices: {:?}", e);
            return;
        }
    }

    for _ in 0..60 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        println!("Waiting for events...");

        // poll_events now gives Vec<(Event, Info)> directly
        for (event, info, _) in usb_hotplug::poll_events() {
            match event {
                usb_hotplug::Event::ARRIVED => println!(
                    "ARRIVED bus {:03} addr {:03} {:04x}:{:04x}",
                    info.bus, info.address, info.vendor, info.product
                ),
                usb_hotplug::Event::LEFT => println!(
                    "LEFT    bus {:03} addr {:03} {:04x}:{:04x}",
                    info.bus, info.address, info.vendor, info.product
                ),
                _ => todo!(),
            }
        }
    }

    println!("Done – no more polling.");
}