/// Xbox One S controller reader — polls interrupt-IN endpoint and renders button state.
use wit_bindgen::generate;
generate!({ world: "guest", path: "../wit", generate_all });

use component::usb::device::list_devices;
use component::usb::transfers::{await_transfer, TransferOptions, TransferSetup, TransferType};

fn parse_state(data: &[u8]) -> String {
    if data.len() < 18 {
        return "<short packet>".into();
    }
    let lt = byteorder::LittleEndian::read_u16(&data[6..]) as f32 / 1023.0;
    let rt = byteorder::LittleEndian::read_u16(&data[8..]) as f32 / 1023.0;
    let lx = (byteorder::LittleEndian::read_i16(&data[10..]) as f32 + 0.5) / 32767.5;
    let ly = (byteorder::LittleEndian::read_i16(&data[12..]) as f32 + 0.5) / 32767.5;
    let rx = (byteorder::LittleEndian::read_i16(&data[14..]) as f32 + 0.5) / 32767.5;
    let ry = (byteorder::LittleEndian::read_i16(&data[16..]) as f32 + 0.5) / 32767.5;

    let a = (data[4] & 0x10) != 0;
    let b = (data[4] & 0x20) != 0;
    let x = (data[4] & 0x40) != 0;
    let y = (data[4] & 0x80) != 0;

    format!(
        "LX:{:+.2} LY:{:+.2} RX:{:+.2} RY:{:+.2} LT:{:.2} RT:{:.2} | A:{} B:{} X:{} Y:{}",
        lx, ly, rx, ry, lt, rt, a as u8, b as u8, x as u8, y as u8
    )
}

// byteorder is not a dep of usb-wasi-guest so we inline a minimal version
mod byteorder {
    pub struct LittleEndian;
    impl LittleEndian {
        pub fn read_u16(buf: &[u8]) -> u16 { u16::from_le_bytes([buf[0], buf[1]]) }
        pub fn read_i16(buf: &[u8]) -> i16 { i16::from_le_bytes([buf[0], buf[1]]) }
    }
}

fn main() {
    use std::io::Write;

    let devices = list_devices().expect("list_devices failed");
    let (device, _d, _l) = devices
        .into_iter()
        .find(|(_, d, _)| d.vendor_id == 0x045e && d.product_id == 0x02ea)
        .expect("Xbox One S controller (045e:02ea) not found");

    let handle = device.open().expect("open failed");
    handle.claim_interface(0).expect("claim interface 0 failed");

    let empty = TransferSetup { bm_request_type: 0, b_request: 0, w_value: 0, w_index: 0 };
    let out_opts = TransferOptions { endpoint: 0x02, timeout_ms: 1000, stream_id: 0, iso_packets: 0 };

    // Send init packet
    let init = handle.new_transfer(TransferType::Interrupt, empty.clone(), 5, out_opts)
        .expect("new_transfer(init) failed");
    init.submit_transfer(&[0x05, 0x20, 0x00, 0x01, 0x00]).ok();
    await_transfer(&init).ok();

    println!("Connected. Press Ctrl-C to exit.");

    let in_opts = TransferOptions { endpoint: 0x81, timeout_ms: 0, stream_id: 0, iso_packets: 0 };
    loop {
        let xfer = handle.new_transfer(TransferType::Interrupt, empty.clone(), 64, in_opts.clone())
            .expect("new_transfer failed");
        xfer.submit_transfer(&[]).expect("submit failed");
        if let Ok(result) = await_transfer(&xfer) {
            if result.data.len() >= 18 {
                print!("\r{}", parse_state(&result.data));
                std::io::stdout().flush().ok();
            }
        }
    }
}
