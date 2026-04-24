/// USB ping — sends a vendor-specific OUT transfer and waits for the IN echo.
/// Usage: ping <vid_hex> <pid_hex> <ep_out_hex> <ep_in_hex> [payload_bytes]
use wit_bindgen::generate;
generate!({ world: "guest", path: "../wit", generate_all });

use component::usb::device::list_devices;
use component::usb::transfers::{await_transfer, TransferOptions, TransferSetup, TransferType};

fn parse_hex_u16(s: &str) -> u16 {
    u16::from_str_radix(s.trim_start_matches("0x"), 16).expect("bad hex u16")
}
fn parse_hex_u8(s: &str) -> u8 {
    u8::from_str_radix(s.trim_start_matches("0x"), 16).expect("bad hex u8")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!("Usage: ping <vid> <pid> <ep_out> <ep_in> [payload_bytes=8]");
        return;
    }
    let vid = parse_hex_u16(&args[1]);
    let pid = parse_hex_u16(&args[2]);
    let ep_out = parse_hex_u8(&args[3]);
    let ep_in = parse_hex_u8(&args[4]);
    let payload: usize = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(8);

    let devices = list_devices().expect("list_devices failed");
    let (device, _d, loc) = devices
        .into_iter()
        .find(|(_, d, _)| d.vendor_id == vid && d.product_id == pid)
        .unwrap_or_else(|| panic!("device {:04x}:{:04x} not found", vid, pid));
    println!("Found device on bus={} addr={}", loc.bus_number, loc.device_address);

    let handle = device.open().expect("open failed");

    let empty = TransferSetup { bm_request_type: 0, b_request: 0, w_value: 0, w_index: 0 };
    let out_opts = TransferOptions { endpoint: ep_out, timeout_ms: 1000, stream_id: 0, iso_packets: 0 };
    let in_opts  = TransferOptions { endpoint: ep_in,  timeout_ms: 1000, stream_id: 0, iso_packets: 0 };

    let data = vec![0xA5u8; payload];

    // OUT
    let xfer_out = handle.new_transfer(TransferType::Bulk, empty.clone(), payload as u32, out_opts)
        .expect("new_transfer(OUT) failed");
    xfer_out.submit_transfer(&data).expect("submit(OUT) failed");
    await_transfer(&xfer_out).expect("await(OUT) failed");
    println!("Sent {} bytes", payload);

    // IN
    let xfer_in = handle.new_transfer(TransferType::Bulk, empty, payload as u32, in_opts)
        .expect("new_transfer(IN) failed");
    xfer_in.submit_transfer(&[]).expect("submit(IN) failed");
    let result = await_transfer(&xfer_in).expect("await(IN) failed");
    println!("Received {} bytes: {:02x?}", result.data.len(), &result.data[..result.data.len().min(16)]);
}
