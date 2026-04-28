//! WASI wit-bindgen-backed Bulk-Only-Transport implementation (condition C5).
//!
//! Uses the `component:usb` WIT interface directly — no rusb wrapper layer.
//! This is the "raw-WIT" path that the thesis uses to isolate the overhead
//! of the WASI USB runtime itself.
//!
//! The same `mass_storage::ScsiDevice<WasmTransport>` is compiled from the
//! same source as `ScsiDevice<NativeTransport>` (C3), so any measured
//! difference between C3 and C5 is pure runtime overhead: wasm sandbox,
//! WIT marshalling, wasmtime call-overhead.
//!
//! # WIT path
//! `"../wit"` is resolved relative to `usb-bench-rs/Cargo.toml`
//! (= `CARGO_MANIFEST_DIR`), pointing to `wasi-usb/wit/`.

use anyhow::{anyhow, Result};

use super::BulkTransport;

// ---------------------------------------------------------------------------
// WIT bindings — imported from the central bindings module
// ---------------------------------------------------------------------------
// `wit_bindgen::generate!` is called *once* in `crate::bindings` so that
// every binary has exactly one `component-type` custom section.  All wasm
// transport modules (this one, w_ctrl, w_int, w_iso) import from there.

use crate::bindings::component::usb::configuration::ConfigValue;
use crate::bindings::component::usb::device::{list_devices, DeviceHandle};
use crate::bindings::component::usb::transfers::{
    await_transfer, TransferOptions, TransferSetup, TransferType,
};

// ---------------------------------------------------------------------------
// Public struct
// ---------------------------------------------------------------------------

/// WASI-backed USB Bulk-Only-Transport device.
///
/// Created via [`WasmTransport::open`]. Uses the `component:usb` WIT interface
/// for all USB I/O. Implements [`BulkTransport`] identically to
/// [`super::super::bulk_only::native::NativeTransport`] — the SCSI layer sees
/// no difference.
pub struct WasmTransport {
    handle: DeviceHandle,
    bulk_in: u8,
    bulk_out: u8,
    tag: u32,
    interface_number: u8,
}

impl WasmTransport {
    /// Find, open, and claim a USB device identified by *VID:PID* via the
    /// WASI USB WIT interface.
    ///
    /// Matches the first device whose `vendor_id` and `product_id` match the
    /// given values, selects the first interface with class=0x08 /
    /// protocol=0x50 (USB Mass Storage Bulk-Only Transport), claims it, then
    /// issues a class-specific BOT Reset control transfer.
    pub fn open(vid: u16, pid: u16) -> Result<Self> {
        let devices = list_devices()
            .map_err(|e| anyhow!("list_devices failed: {e:?}"))?;

        for (device, desc, _loc) in devices {
            if desc.vendor_id != vid || desc.product_id != pid {
                continue;
            }

            let config = device
                .get_active_configuration_descriptor()
                .map_err(|e| anyhow!("get_active_configuration_descriptor: {e:?}"))?;

            // Find mass-storage BOT interface (class=0x08, protocol=0x50)
            let interface_number = config
                .interfaces
                .iter()
                .find(|iface| {
                    iface.interface_class == 0x08 && iface.interface_protocol == 0x50
                })
                .map(|iface| iface.interface_number)
                .unwrap_or(0);

            let handle = device
                .open()
                .map_err(|e| anyhow!("device.open: {e:?}"))?;

            // Ensure the correct configuration is active
            let active = handle.get_configuration().unwrap_or(0);
            if active != config.configuration_value {
                let _ =
                    handle.set_configuration(ConfigValue::Value(config.configuration_value));
            }

            handle
                .claim_interface(interface_number)
                .map_err(|e| anyhow!("claim_interface({interface_number}): {e:?}"))?;

            // Find bulk IN/OUT endpoints on the claimed interface
            let (bulk_in, bulk_out) = {
                let iface = config
                    .interfaces
                    .iter()
                    .find(|i| i.interface_number == interface_number)
                    .ok_or_else(|| anyhow!("interface {interface_number} missing from config"))?;

                let mut in_ep = 0u8;
                let mut out_ep = 0u8;
                for ep in &iface.endpoints {
                    let is_in = (ep.endpoint_address & 0x80) != 0;
                    let is_bulk = (ep.attributes & 0x03) == 0x02;
                    if is_bulk {
                        if is_in {
                            in_ep = ep.endpoint_address;
                        } else {
                            out_ep = ep.endpoint_address;
                        }
                    }
                }
                (in_ep, out_ep)
            };

            // BOT Reset — class-specific control transfer (§3.1 of BOT spec)
            // Ignore errors: some devices don't implement it but work anyway.
            let reset_setup = TransferSetup {
                bm_request_type: 0x21, // Class, Interface, OUT
                b_request: 0xFF,       // BOT Bulk-Only Mass Storage Reset
                w_value: 0,
                w_index: interface_number as u16,
            };
            let reset_opts = TransferOptions {
                endpoint: 0,
                timeout_ms: 1_000,
                stream_id: 0,
                iso_packets: 0,
            };
            if let Ok(xfer) = handle.new_transfer(
                TransferType::Control,
                reset_setup,
                0,
                reset_opts,
            ) {
                let _ = xfer.submit_transfer(&[]);
                let _ = await_transfer(&xfer);
            }

            return Ok(Self {
                handle,
                bulk_in,
                bulk_out,
                tag: 1,
                interface_number,
            });
        }

        Err(anyhow!("Device {:04x}:{:04x} not found via WASI USB", vid, pid))
    }

    // -----------------------------------------------------------------------
    // Private helpers (mirror NativeTransport helpers, adapted for WIT API)
    // -----------------------------------------------------------------------

    fn next_tag(&mut self) -> u32 {
        let t = self.tag;
        self.tag = self.tag.wrapping_add(1);
        t
    }

    /// Build a 31-byte Command Block Wrapper (§5.1 of USB-MS BOT spec).
    fn build_cbw(tag: u32, transfer_len: u32, direction_in: bool, cbwcb: &[u8]) -> [u8; 31] {
        debug_assert!(!cbwcb.is_empty() && cbwcb.len() <= 16);
        let mut cbw = [0u8; 31];
        cbw[0..4].copy_from_slice(&0x4342_5355u32.to_le_bytes()); // 'USBC'
        cbw[4..8].copy_from_slice(&tag.to_le_bytes());
        cbw[8..12].copy_from_slice(&transfer_len.to_le_bytes());
        cbw[12] = if direction_in { 0x80 } else { 0x00 };
        cbw[13] = 0; // bCBWLUN = 0
        cbw[14] = cbwcb.len() as u8;
        cbw[15..15 + cbwcb.len()].copy_from_slice(cbwcb);
        cbw
    }

    /// Validate a 13-byte Command Status Wrapper (§5.2 of USB-MS BOT spec).
    fn validate_csw(csw_bytes: &[u8], expected_tag: u32) -> Result<()> {
        if csw_bytes.len() < 13 {
            return Err(anyhow!(
                "CSW too short: {} bytes (expected 13)",
                csw_bytes.len()
            ));
        }
        let sig = u32::from_le_bytes(csw_bytes[0..4].try_into().unwrap());
        if sig != 0x5342_5355 {
            return Err(anyhow!(
                "Invalid CSW signature: {sig:#010x} (expected 0x53425355)"
            ));
        }
        let tag = u32::from_le_bytes(csw_bytes[4..8].try_into().unwrap());
        if tag != expected_tag {
            return Err(anyhow!(
                "CSW tag mismatch: got {tag}, expected {expected_tag}"
            ));
        }
        if csw_bytes[12] != 0 {
            return Err(anyhow!("CSW failure status: {}", csw_bytes[12]));
        }
        Ok(())
    }

    fn bulk_out_opts(&self) -> TransferOptions {
        TransferOptions {
            endpoint: self.bulk_out,
            timeout_ms: 10_000,
            stream_id: 0,
            iso_packets: 0,
        }
    }

    fn bulk_in_opts(&self) -> TransferOptions {
        TransferOptions {
            endpoint: self.bulk_in,
            timeout_ms: 10_000,
            stream_id: 0,
            iso_packets: 0,
        }
    }

    fn empty_setup() -> TransferSetup {
        TransferSetup {
            bm_request_type: 0,
            b_request: 0,
            w_value: 0,
            w_index: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// BulkTransport implementation
// ---------------------------------------------------------------------------

impl BulkTransport for WasmTransport {
    /// Send CBW (IN direction), receive data, validate CSW.
    fn command_in(&mut self, cbwcb: &[u8], transfer_len: u32) -> Result<Vec<u8>> {
        let tag = self.next_tag();
        let cbw = Self::build_cbw(tag, transfer_len, true, cbwcb);

        // 1. Send Command Block Wrapper
        let xfer_cbw = self
            .handle
            .new_transfer(
                TransferType::Bulk,
                Self::empty_setup(),
                31,
                self.bulk_out_opts(),
            )
            .map_err(|e| anyhow!("new_transfer (CBW): {e:?}"))?;
        xfer_cbw
            .submit_transfer(&cbw)
            .map_err(|e| anyhow!("submit CBW: {e:?}"))?;
        await_transfer(&xfer_cbw).map_err(|e| anyhow!("await CBW: {e:?}"))?;

        // 2. Receive data phase
        let data = if transfer_len > 0 {
            let xfer_data = self
                .handle
                .new_transfer(
                    TransferType::Bulk,
                    Self::empty_setup(),
                    transfer_len,
                    self.bulk_in_opts(),
                )
                .map_err(|e| anyhow!("new_transfer (data): {e:?}"))?;
            xfer_data
                .submit_transfer(&[])
                .map_err(|e| anyhow!("submit data IN: {e:?}"))?;
            await_transfer(&xfer_data)
                .map_err(|e| anyhow!("await data IN: {e:?}"))?
                .data
        } else {
            vec![]
        };

        // 3. Receive Command Status Wrapper
        let xfer_csw = self
            .handle
            .new_transfer(
                TransferType::Bulk,
                Self::empty_setup(),
                13,
                self.bulk_in_opts(),
            )
            .map_err(|e| anyhow!("new_transfer (CSW): {e:?}"))?;
        xfer_csw
            .submit_transfer(&[])
            .map_err(|e| anyhow!("submit CSW: {e:?}"))?;
        let csw_bytes = await_transfer(&xfer_csw)
            .map_err(|e| anyhow!("await CSW: {e:?}"))?
            .data;

        Self::validate_csw(&csw_bytes, tag)?;
        Ok(data)
    }

    /// Send CBW (OUT direction), optionally write data, validate CSW.
    fn command_out(&mut self, cbwcb: &[u8], data: Option<&[u8]>) -> Result<()> {
        let tag = self.next_tag();
        let transfer_len = data.map(|d| d.len() as u32).unwrap_or(0);
        let cbw = Self::build_cbw(tag, transfer_len, false, cbwcb);

        // 1. Send Command Block Wrapper
        let xfer_cbw = self
            .handle
            .new_transfer(
                TransferType::Bulk,
                Self::empty_setup(),
                31,
                self.bulk_out_opts(),
            )
            .map_err(|e| anyhow!("new_transfer (CBW): {e:?}"))?;
        xfer_cbw
            .submit_transfer(&cbw)
            .map_err(|e| anyhow!("submit CBW: {e:?}"))?;
        await_transfer(&xfer_cbw).map_err(|e| anyhow!("await CBW: {e:?}"))?;

        // 2. Send data phase (optional)
        if let Some(d) = data {
            let xfer_data = self
                .handle
                .new_transfer(
                    TransferType::Bulk,
                    Self::empty_setup(),
                    d.len() as u32,
                    self.bulk_out_opts(),
                )
                .map_err(|e| anyhow!("new_transfer (data out): {e:?}"))?;
            xfer_data
                .submit_transfer(d)
                .map_err(|e| anyhow!("submit data OUT: {e:?}"))?;
            await_transfer(&xfer_data).map_err(|e| anyhow!("await data OUT: {e:?}"))?;
        }

        // 3. Receive Command Status Wrapper
        let xfer_csw = self
            .handle
            .new_transfer(
                TransferType::Bulk,
                Self::empty_setup(),
                13,
                self.bulk_in_opts(),
            )
            .map_err(|e| anyhow!("new_transfer (CSW): {e:?}"))?;
        xfer_csw
            .submit_transfer(&[])
            .map_err(|e| anyhow!("submit CSW: {e:?}"))?;
        let csw_bytes = await_transfer(&xfer_csw)
            .map_err(|e| anyhow!("await CSW: {e:?}"))?
            .data;

        Self::validate_csw(&csw_bytes, tag)
    }
}

// ---------------------------------------------------------------------------
// Drop: release the claimed interface
// ---------------------------------------------------------------------------

impl Drop for WasmTransport {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(self.interface_number);
    }
}
