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
        println!("No USB devices found.");
        return;
    }
    let handle = devs.remove(0).0.open().expect("open failed");

    // assume cfg=1, iface=0, bulk OUT @0x01, bulk IN @0x81
    handle.set_configuration(ConfigValue::Value(1)).expect("set_configuration");

    // detach kernel driver if active
    if let Ok(true) = handle.kernel_driver_active(0) {
        let _ = handle.detach_kernel_driver(0);
    }

    handle.claim_interface(0).expect("claim_interface");

    // prepare a 0..63 pattern
    let out_data: Vec<u8> = (0..64).collect();

    // bulk‐OUT
    let opts_out = TransferOptions { endpoint: 1, timeout_ms: 5_000, stream_id: 0, iso_packets: 0 };
    let setup = TransferSetup { bm_request_type: 0, b_request: 0, w_value: 0, w_index: 0 };
    let xfer_out = handle
        .new_transfer(TransferType::Bulk, setup, out_data.len() as u32, opts_out)
        .expect("alloc xfer_out");
    xfer_out.submit_transfer(&out_data).expect("submit out");

    // bulk‐IN
    let opts_in = TransferOptions { endpoint: 0x81, timeout_ms: 5_000, stream_id: 0, iso_packets: 0 };
    let xfer_in = handle
        .new_transfer(TransferType::Bulk, setup, out_data.len() as u32, opts_in)
        .expect("alloc xfer_in");
    xfer_in.submit_transfer(&[]).expect("submit in");

    let in_data = transfers::await_transfer(&xfer_in).expect("await in").data;
    println!("Received {} bytes: {:?}", in_data.len(), in_data);

    handle.release_interface(0).expect("release_interface");
    handle.close();
}
