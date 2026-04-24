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
use crate::component::usb::transfers;

fn main() {
    // ...existing code: init and list devices...
    device::init().expect("libusb init failed");
    let mut devs = device::list_devices().expect("list_devices failed");
    if devs.is_empty() {
        println!("No USB devices found.");
        return;
    }
    // open first device
    let handle = devs.remove(0).0.open().expect("open failed");

    // prepare two Control-IN transfers for configuration 0 and 1
    let opts = TransferOptions {
        endpoint: 0, timeout_ms: 1_000, stream_id: 0, iso_packets: 0,
    };
    let setup0 = TransferSetup { bm_request_type: 0x80, b_request: 0x06, w_value: 0x0200, w_index: 0, };
    let setup1 = TransferSetup { bm_request_type: 0x80, b_request: 0x06, w_value: 0x0201, w_index: 0, };

    // allocate both transfers (9-byte descriptors)
    let xfer0 = handle.new_transfer(TransferType::Control, setup0, 9, opts).expect("alloc xfer0");
    let xfer1 = handle.new_transfer(TransferType::Control, setup1, 9, opts).expect("alloc xfer1");

    // submit both before awaiting
    xfer0.submit_transfer(&[]).expect("submit xfer0");
    xfer1.submit_transfer(&[]).expect("submit xfer1");

    // now await both results
    let buf0 = transfers::await_transfer(&xfer0).expect("await xfer0").data;
    let buf1 = transfers::await_transfer(&xfer1).expect("await xfer1").data;

    println!("Config[0] descriptor bytes: {:?}", buf0);
    println!("Config[1] descriptor bytes: {:?}", buf1);

    handle.close();
}
