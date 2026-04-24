use std::time::Instant;
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
use std::io::Write;

fn main() {
    device::init().expect("init failed");
    let mut devs = device::list_devices().expect("list_devices failed");
    if devs.is_empty() {
        println!("No devices.");
        return;
    }
    let handle = devs.remove(0).0.open().expect("open failed");

    // assume cfg=1, iface=1, int IN @0x81
    handle.set_configuration(ConfigValue::Value(1)).expect("set_configuration");

    // detach kernel driver if active
    if let Ok(true) = handle.kernel_driver_active(1) {
        let _ = handle.detach_kernel_driver(1);
    }

    let res = handle.claim_interface(1);
    if res.is_err() {
        println!("claim_interface failed: {:?}", res);
        return;
    }
    println!("Claimed interface 1");
    handle.set_interface_altsetting(1, 0).expect("altsetting");

    let setup = TransferSetup { bm_request_type: 0, b_request: 0, w_value: 0, w_index: 0 };
    let opts = TransferOptions { endpoint: 0x82, timeout_ms: 1_000, stream_id: 0, iso_packets: 0 };
    
    // warm up
    for _ in 0..1000 {
        let xfer = handle
            .new_transfer(TransferType::Interrupt, setup, 8, opts)
            .expect("new_transfer");
        xfer.submit_transfer(&[]).expect("submit_transfer");
        transfers::await_transfer(&xfer).expect("await_transfer");
    }
    
    // measure timer overhead (Instant::now() + elapsed())
    let timer_overhead_ns: f64 = {
        let t0 = Instant::now();
        for _ in 0..50_000 {
            let _ = Instant::now().elapsed();
        }
        t0.elapsed().as_nanos() as f64 / 50_000f64
    };
    
    let mut durations = Vec::with_capacity(50_000);

    for i in 0..50_000 {
        let start = std::time::Instant::now();
        let xfer = handle
            .new_transfer(TransferType::Interrupt, setup, 8, opts)
            .expect("new_transfer");
        xfer.submit_transfer(&[]).expect("submit_transfer");
        // start timer
        transfers::await_transfer(&xfer).expect("await_transfer");
        // stop timer
        let elapsed = start.elapsed();
        let raw_ns = elapsed.as_nanos() as f64;
        // Subtract timer overhead to isolate block-read latency
        let read_only_ns = raw_ns - timer_overhead_ns;
        durations.push(read_only_ns as u64);
    }
    
    let mut file = std::fs::File::create("latencies_wasi_interrupt.txt").expect("Failed to create file");
    for duration in durations {
        writeln!(file, "{}", duration).expect("Write failed");
    }

    handle.release_interface(1).expect("release_interface");
    handle.close();
}
