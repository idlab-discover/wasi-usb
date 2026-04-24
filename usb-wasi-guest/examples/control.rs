/// Control transfer example — sends a GET_DESCRIPTOR (Device) request to an Arduino Nano 33 BLE.
use wit_bindgen::generate;
generate!({ world: "guest", path: "../wit", generate_all });

use component::usb::device::list_devices;
use component::usb::transfers::{await_transfer, TransferOptions, TransferSetup, TransferType};

fn main() {
    let devices = list_devices().expect("list_devices failed");
    let (device, _desc, _loc) = devices
        .into_iter()
        .find(|(_, d, _)| d.vendor_id == 0x2341 && d.product_id == 0x8057)
        .expect("Arduino (0x2341:0x8057) not found");

    let handle = device.open().expect("open failed");

    // GET_DESCRIPTOR (Device) — bmRequestType=0x80, bRequest=0x06, wValue=0x0100
    let setup = TransferSetup {
        bm_request_type: 0x80,
        b_request: 0x06,
        w_value: 0x0100,
        w_index: 0,
    };
    let opts = TransferOptions { endpoint: 0, timeout_ms: 1000, stream_id: 0, iso_packets: 0 };

    let xfer = handle.new_transfer(TransferType::Control, setup, 64, opts)
        .expect("new_transfer failed");
    xfer.submit_transfer(&[]).expect("submit failed");

    let result = await_transfer(&xfer).expect("await failed");
    println!("Device Descriptor ({} bytes): {:02x?}", result.data.len(), result.data);
}
