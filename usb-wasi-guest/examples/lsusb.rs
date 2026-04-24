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

/// Issue a Control-IN transfer and return the received data.
///
/// Always uses bmRequestType = 0x80 (IN | standard | device).
fn control_in(
    handle: &device::DeviceHandle,
    request: u8,
    w_value: u16,
    w_index: u16,
    len: u16,
) -> Result<Vec<u8>, component::usb::errors::LibusbError> {
    let setup = TransferSetup {
        bm_request_type: 0x80,
        b_request: request,
        w_value,
        w_index,
    };
    let opts = TransferOptions {
        endpoint: 0,          // EP-0
        timeout_ms: 1_000,
        stream_id: 0,
        iso_packets: 0,
    };
    let xfer = handle
        .new_transfer(TransferType::Control, setup, len as u32, opts)
        .expect("new_transfer failed");

    // OUT buffer is empty for IN requests
    xfer.submit_transfer(&[]).expect("submit failed");
    transfers::await_transfer(&xfer).map(|r| r.data)
}

/// Decode UTF-16LE bytes from a string-descriptor into Rust UTF-8.
fn decode_usb_string(buf: &[u8]) -> String {
    if buf.len() < 2 || buf[1] != 0x03 {
        return "<bad string>".into();
    }
    // bytes 2.. are UTF-16LE code units
    let utf16: Vec<u16> = buf[2..]
        .chunks(2)
        .filter_map(|b| if b.len() == 2 {
            Some(u16::from_le_bytes([b[0], b[1]]))
        } else { None })
        .collect();
    String::from_utf16(&utf16).unwrap_or_else(|_| "<utf16 error>".into())
}

fn main() {
    //----------------------------------------
    // 1: initialise backend
    //----------------------------------------
        device::init().expect("libusb init failed");

    //----------------------------------------
    // 2: list and iterate devices
    //----------------------------------------
    let devs = device::list_devices().expect("list_devices failed");
    if devs.is_empty() {
        println!("No USB devices found.");
        return;
    }
    println!("Found {} device(s)\n", devs.len());

    for (idx, dev) in devs.iter().enumerate() {
        println!("── Device #{} ──", idx);

        //------------------------------------
        // open – skip if permission denied
        //------------------------------------
        let handle = match dev.0.open() {
            Ok(h) => h,
            Err(e) => {
                println!("  <cannot open>  {:?}", e);
                continue;
            }
        };

        //------------------------------------
        // fetch the 18-byte device descriptor
        //------------------------------------
        let ddesc = match control_in(&handle, 0x06, 0x0100, 0, 18) {
            Ok(buf) => buf,
            Err(e) => {
                println!("  GET_DESCRIPTOR(Device) failed: {:?}", e);
                handle.close();
                continue;
            }
        };
        if ddesc.len() != 18 {
            println!("  Bad descriptor length {}", ddesc.len());
            handle.close();
            continue;
        }
        // parse full device descriptor fields
        let usb_version    = u16::from_le_bytes([ddesc[2], ddesc[3]]);
        let dev_class      = ddesc[4];
        let dev_subclass   = ddesc[5];
        let dev_protocol   = ddesc[6];
        let max_packet0    = ddesc[7];
        let dev_release    = u16::from_le_bytes([ddesc[12], ddesc[13]]);
        let mfg_idx        = ddesc[14];
        let prod_idx       = ddesc[15];
        let serial_idx     = ddesc[16];
        let num_cfg        = ddesc[17];

        // print descriptor details
        println!("  bcdUSB             {:04x}", usb_version);
        println!("  Device Class       {:#04x}", dev_class);
        println!("  Subclass           {:#04x}", dev_subclass);
        println!("  Protocol           {:#04x}", dev_protocol);
        println!("  MaxPacketSize0     {}", max_packet0);
        println!("  bcdDevice          {:04x}", dev_release);
        println!("  iManufacturer idx  {}", mfg_idx);
        println!("  iProduct idx       {}", prod_idx);
        println!("  iSerialNumber idx  {}", serial_idx);
        println!("  NumConfigurations  {}", num_cfg);

        let vid = u16::from_le_bytes([ddesc[8],  ddesc[9]]);
        let pid = u16::from_le_bytes([ddesc[10], ddesc[11]]);
        println!("  VID:PID            {:04x}:{:04x}", vid, pid);

        //------------------------------------
        // fetch manufacturer / product strings (if any)
        //------------------------------------
        let mut mfg = None;
        if mfg_idx != 0 {
            if let Ok(buf) = control_in(&handle, 0x06, 0x0300 | mfg_idx as u16, 0x0409, 255) {
                mfg = Some(decode_usb_string(&buf));
            }
        }
        let mut prod = None;
        if prod_idx != 0 {
            if let Ok(buf) = control_in(&handle, 0x06, 0x0300 | prod_idx as u16, 0x0409, 255) {
                prod = Some(decode_usb_string(&buf));
            }
        }
        if mfg.is_some() || prod.is_some() {
            println!(
                "  Strings            {} {}",
                mfg.as_deref().unwrap_or(""),
                prod.as_deref().unwrap_or("")
            );
        }

        //------------------------------------
        // Optionally list each configuration’s total length and endpoints
        //------------------------------------
        for cfg in 0..num_cfg {
            // first fetch the 9‐byte header
            if let Ok(buf9) = control_in(&handle, 0x06, 0x0200 | cfg as u16, 0, 9) {
                if buf9.len() == 9 {
                    let total_len = u16::from_le_bytes([buf9[2], buf9[3]]);
                    let num_ifaces = buf9[4];
                    println!(
                        "  [cfg {:02}] total_len={}  interfaces={}",
                        cfg, total_len, num_ifaces
                    );
                    // now fetch the full block
                    if let Ok(full) =
                        control_in(&handle, 0x06, 0x0200 | cfg as u16, 0, total_len)
                    {
                        // parse sub‐descriptors starting at offset 9
                        let mut i = 9;
                        while i + 1 < full.len() {
                            let len = full[i] as usize;
                            if i + len > full.len() { break; }
                            let dtype = full[i + 1];
                            match dtype {
                                4 => {
                                    // interface descriptor
                                    let iface = full[i + 2];
                                    let alt   = full[i + 3];
                                    let epcnt = full[i + 4];
                                    println!(
                                        "    Interface {} alt {} endpoints={}",
                                        iface, alt, epcnt
                                    );
                                }
                                5 => {
                                    // endpoint descriptor
                                    let addr    = full[i + 2];
                                    let attrs   = full[i + 3];
                                    let mx      = u16::from_le_bytes([full[i + 4], full[i + 5]]);
                                    let interval= full[i + 6];
                                    println!(
                                        "      Endpoint addr=0x{:02x} attrs=0x{:02x} max_pkt={} interval={}",
                                        addr, attrs, mx, interval
                                    );
                                }
                                _ => {}
                            }
                            i += len.max(1);
                        }
                    }
                }
            }
        }

        //------------------------------------
        // close handle
        //------------------------------------
        handle.close();
        println!();
    }
}