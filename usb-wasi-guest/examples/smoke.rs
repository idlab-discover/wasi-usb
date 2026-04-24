use wit_bindgen::generate;

generate!({
    world: "guest",
    path: "../wit",
    generate_all,
});

use component::usb::{
    device,
};

use component::usb::transfers::TransferType;
use crate::component::usb;
use crate::component::usb::transfers;
use crate::component::usb::transfers::{TransferOptions, TransferSetup};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    println!("=== usb smoke-test (blocking) ===");

    // 1. Initialise libusb backend
    device::init().expect("libusb initialisation failed");

    // 2. List devices
    let devices = device::list_devices().expect("list_devices failed");
    if devices.is_empty() {
        println!("No USB devices found.");
        return;
    }
    println!("Found {} devices", devices.len());

    let dev = &devices[0];
    println!("Opening first device …");
    let handle = dev.0.open().expect("open() failed");

    // 3. Query active configuration
    match handle.get_configuration() {
        Ok(cfg) => println!("Active configuration: {}", cfg),
        Err(e)  => println!("get_configuration error: {:?}", e),
    }

    // 4. Claim interface 0 if present
    let _ = handle.claim_interface(0).expect("claim_interface failed");

    // 5. Simple 18-byte GET_DESCRIPTOR(Device) control IN transfer
    let setup = TransferSetup {
        bm_request_type: 0x80,        // D2H | standard | device
        b_request: 6,                 // GET_DESCRIPTOR
        w_value: 0x0100,              // descriptor type 1 (Device), index 0
        w_index: 0,
    };
    let opts = TransferOptions {
        endpoint: 0,                  // control EP0
        timeout_ms: 1_000,
        stream_id: 0,
        iso_packets: 0,
    };
    let xfer = handle
        .new_transfer(TransferType::Control, setup, 18, opts)
        .expect("new_transfer failed");

    xfer.submit_transfer(&*Vec::new()).expect("submit failed");
    match transfers::await_transfer(&xfer) {
        Ok(result) => println!("Device descriptor bytes: {:02X?}", result.data),
        Err(e)     => println!("Transfer failed: {:?}", e),
    }

    // 6. Release IF 0 and close handle
    let _ = handle.release_interface(0);
    
    // wait for 5 seconds
    println!("Waiting for 5 seconds …");
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    println!("Smoke-test finished.");
}