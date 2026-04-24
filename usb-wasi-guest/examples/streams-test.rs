// streams-test — Valideert USB 3.0 Bulk Streams via de WASI-USB host.
//
// Doel: aantonen dat `alloc_streams` + `new_transfer` met stream_id != 0
// eind-tot-eind werkt. Op gewone BOT-sticks zal `alloc_streams` doorgaans
// LIBUSB_ERROR_NOT_SUPPORTED teruggeven — dat is een geldig resultaat dat
// aantoont dat de call de host-grens haalt en een echte libusb-respons
// teruggeeft (geen stub).
//
// Gebruik:
//   streams-test <vid_hex> <pid_hex> <iface> <ep_out_hex> <ep_in_hex> [num_streams] [payload_bytes]
use wit_bindgen::generate;
generate!({ world: "guest", path: "../wit", generate_all });

use component::usb::configuration::ConfigValue;
use component::usb::device::{list_devices, UsbSpeed};
use component::usb::transfers::{await_transfer, TransferOptions, TransferSetup, TransferType};

fn parse_hex_u16(s: &str) -> u16 {
    u16::from_str_radix(s.trim_start_matches("0x"), 16).expect("bad hex u16")
}
fn parse_hex_u8(s: &str) -> u8 {
    u8::from_str_radix(s.trim_start_matches("0x"), 16).expect("bad hex u8")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 6 {
        eprintln!(
            "Usage: {} <vid> <pid> <iface> <ep_out> <ep_in> [num_streams=16] [payload=512]",
            args[0]
        );
        return;
    }
    let vid  = parse_hex_u16(&args[1]);
    let pid  = parse_hex_u16(&args[2]);
    let iface: u8 = args[3].parse().expect("iface must be u8");
    let ep_out = parse_hex_u8(&args[4]);
    let ep_in  = parse_hex_u8(&args[5]);
    let num_streams: u32 = args.get(6).and_then(|s| s.parse().ok()).unwrap_or(16);
    let payload: u32 = args.get(7).and_then(|s| s.parse().ok()).unwrap_or(512);

    println!(
        "[streams-test] target {:04x}:{:04x} iface={} ep_out=0x{:02x} ep_in=0x{:02x} \
         num_streams={} payload={}B",
        vid, pid, iface, ep_out, ep_in, num_streams, payload
    );

    // ── Fase 1: enumerate + match ─────────────────────────────────────────────
    let devices = list_devices().expect("list_devices failed");
    let (device, _desc, loc) = devices
        .into_iter()
        .find(|(_, d, _)| d.vendor_id == vid && d.product_id == pid)
        .unwrap_or_else(|| panic!("device {:04x}:{:04x} not found", vid, pid));
    println!("[streams-test] match on bus={} addr={} speed={:?}", loc.bus_number, loc.device_address, loc.speed);

    match loc.speed {
        UsbSpeed::Super | UsbSpeed::SuperPlus =>
            println!("[streams-test] ✓ SuperSpeed — streams kunnen nuttig zijn"),
        _ =>
            println!("[streams-test] ⚠  device is geen SuperSpeed — streams worden vaak geweigerd"),
    }

    // ── Fase 2: open + claim ──────────────────────────────────────────────────
    let handle = device.open().expect("open failed");
    handle.reset_device().ok();
    let _ = handle.set_configuration(ConfigValue::Value(1));
    handle.claim_interface(iface).expect("claim_interface failed");
    println!("[streams-test] ✓ opened + claimed iface {}", iface);

    // ── Fase 3: alloc_streams ─────────────────────────────────────────────────
    match handle.alloc_streams(num_streams, &[ep_out, ep_in]) {
        Ok(()) => println!(
            "[streams-test] ✓ alloc_streams OK — {} streams op EP 0x{:02x}+0x{:02x}",
            num_streams, ep_out, ep_in
        ),
        Err(e) => {
            println!(
                "[streams-test] ✗ alloc_streams faalde: {:?}  (host-grens wél bereikt)",
                e
            );
            handle.release_interface(iface).ok();
            return;
        }
    }

    // ── Fase 4: een stream-bulk transfer proberen ─────────────────────────────
    let empty = TransferSetup { bm_request_type: 0, b_request: 0, w_value: 0, w_index: 0 };
    let opts_out = TransferOptions { endpoint: ep_out, timeout_ms: 1000, stream_id: 1, iso_packets: 0 };
    let data = vec![0xA5u8; payload as usize];
    let xfer_out = handle
        .new_transfer(TransferType::Bulk, empty, payload, opts_out)
        .expect("new_transfer(OUT, stream_id=1) failed");
    let submit_ok = xfer_out.submit_transfer(&data);
    println!("[streams-test] submit bulk-stream OUT (stream_id=1, {}B) -> {:?}", payload, submit_ok);
    if submit_ok.is_ok() {
        let result = await_transfer(&xfer_out);
        println!("[streams-test] await OUT -> {:?}", result.as_ref().map(|r| r.data.len()));
    }

    // ── Fase 5: free_streams + teardown ──────────────────────────────────────
    println!("[streams-test] free_streams -> {:?}", handle.free_streams(&[ep_out, ep_in]));
    handle.release_interface(iface).ok();
    println!("[streams-test] done.");
}
