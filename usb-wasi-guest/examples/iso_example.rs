use wit_bindgen::generate;
generate!({
    world: "guest",
    path: "../wit",
    generate_all,
});

use component::usb::{
    device,
    transfers::{TransferType, TransferSetup, TransferOptions},
};
use crate::component::usb::configuration::ConfigValue;
use crate::component::usb::transfers;

fn main() {
    // ...existing code: init + list...
    device::init().expect("init failed");
    let mut devs = device::list_devices().expect("list_devices failed");
    if devs.is_empty() {
        println!("No devices.");
        return;
    }
    let handle = devs.remove(0).0.open().expect("open failed");

    // assume cfg=1, iface=2, iso IN @0x82
    handle.set_configuration(ConfigValue::Value(1)).expect("set_configuration");

    // detach kernel driver if active
    if let Ok(true) = handle.kernel_driver_active(2) {
        let _ = handle.detach_kernel_driver(2);
    }

    handle.claim_interface(2).expect("claim_interface");
    handle.set_interface_altsetting(2, 1).expect("altsetting");

    // request two 512-byte packets
    let setup = TransferSetup { bm_request_type: 0, b_request: 0, w_value: 0, w_index: 0 };
    let opts = TransferOptions { endpoint: 0x82, timeout_ms: 5000, stream_id: 0, iso_packets: 2 };
    let buf_size = 512 * 2;

    let xfer = handle
        .new_transfer(TransferType::Isochronous, setup, buf_size, opts)
        .expect("new_transfer");
    xfer.submit_transfer(&[]).expect("submit_transfer");

    let buf = transfers::await_transfer(&xfer).expect("await_transfer").data;
    println!("Isochronous total bytes: {}", buf.len());

    handle.release_interface(2).expect("release_interface");
    handle.close();
}
