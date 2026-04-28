//! Native rusb-backed Bulk-Only-Transport implementation (condition C3).
//!
//! Exposes `NativeTransport` which implements `super::BulkTransport` using
//! `rusb::DeviceHandle<GlobalContext>`. This file is only compiled for
//! non-wasm targets (`#[cfg(not(target_family = "wasm"))]` in the parent
//! module).

use std::time::Duration;

use anyhow::{anyhow, Result};
use rusb::{DeviceHandle, Direction, GlobalContext, TransferType, UsbContext};

use super::BulkTransport;

const TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Public struct
// ---------------------------------------------------------------------------

/// rusb-backed USB Bulk-Only-Transport device (Bulk-Only-Transfer, BOT).
///
/// Created via [`NativeTransport::open`]. On drop the claimed interface is
/// released so the kernel driver can re-attach if needed.
pub struct NativeTransport {
    handle: DeviceHandle<GlobalContext>,
    bulk_in: u8,
    bulk_out: u8,
    tag: u32,
    interface_number: u8,
}

impl NativeTransport {
    /// Find, open, and claim a USB device identified by *VID:PID*.
    ///
    /// Selects the first interface with class 0x08 (mass storage) and
    /// protocol 0x50 (Bulk-Only Transport). Falls back to interface 0 if no
    /// mass-storage interface descriptor is found.
    ///
    /// Performs a BOT-Reset control request after claiming so the device is in
    /// a known state for the first transfer.
    pub fn open(vid: u16, pid: u16) -> Result<Self> {
        let ctx = GlobalContext::default();
        let devices = ctx.devices().map_err(|e| anyhow!("list_devices: {e}"))?;

        for device in devices.iter() {
            let desc = device.device_descriptor()?;
            if desc.vendor_id() != vid || desc.product_id() != pid {
                continue;
            }

            // Find mass-storage BOT interface (class=0x08, protocol=0x50)
            let config = device.config_descriptor(0)?;
            let interface_number = config
                .interfaces()
                .find_map(|iface| {
                    let d = iface.descriptors().next()?;
                    if d.class_code() == 0x08 && d.protocol_code() == 0x50 {
                        Some(iface.number())
                    } else {
                        None
                    }
                })
                .unwrap_or(0);

            let handle = device.open()?;
            // Allow OS to detach any kernel driver that holds the interface.
            let _ = handle.set_auto_detach_kernel_driver(true);
            handle.claim_interface(interface_number)?;

            // Re-read config to get endpoint descriptors for the claimed interface.
            let config = device.config_descriptor(0)?;
            let (bulk_in, bulk_out) =
                find_bulk_endpoints(&config, interface_number).ok_or_else(|| {
                    anyhow!("No bulk endpoints on interface {interface_number}")
                })?;

            // BOT Reset (class-specific control request, §3.1 of USB-MS BOT spec)
            let rt = rusb::request_type(
                Direction::Out,
                rusb::RequestType::Class,
                rusb::Recipient::Interface,
            );
            // Ignore errors; some devices don't support BOT reset but work anyway.
            let _ = handle.write_control(
                rt,
                0xFF,
                0,
                interface_number as u16,
                &[],
                TIMEOUT,
            );

            return Ok(Self {
                handle,
                bulk_in,
                bulk_out,
                tag: 1,
                interface_number,
            });
        }

        Err(anyhow!("Device {:04x}:{:04x} not found", vid, pid))
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    fn next_tag(&mut self) -> u32 {
        let t = self.tag;
        self.tag = self.tag.wrapping_add(1);
        t
    }

    /// Construct and write a 31-byte CBW to the bulk-OUT endpoint.
    fn write_cbw(
        &self,
        tag: u32,
        transfer_len: u32,
        direction_in: bool,
        cbwcb: &[u8],
    ) -> Result<()> {
        debug_assert!(!cbwcb.is_empty() && cbwcb.len() <= 16);

        let mut cbw = [0u8; 31];
        // dCBWSignature = 'USBC' (little-endian)
        cbw[0..4].copy_from_slice(&0x4342_5355u32.to_le_bytes());
        cbw[4..8].copy_from_slice(&tag.to_le_bytes());
        cbw[8..12].copy_from_slice(&transfer_len.to_le_bytes());
        cbw[12] = if direction_in { 0x80 } else { 0x00 };
        cbw[13] = 0; // bCBWLUN = 0
        cbw[14] = cbwcb.len() as u8;
        cbw[15..15 + cbwcb.len()].copy_from_slice(cbwcb);

        let n = self
            .handle
            .write_bulk(self.bulk_out, &cbw, TIMEOUT)
            .map_err(|e| anyhow!("CBW write_bulk: {e}"))?;
        if n != 31 {
            return Err(anyhow!("CBW short write: {n}/31 bytes"));
        }
        Ok(())
    }

    /// Read the 13-byte CSW from the bulk-IN endpoint and validate it.
    fn read_csw(&self, expected_tag: u32) -> Result<()> {
        let mut csw = [0u8; 13];
        let mut total = 0;
        while total < 13 {
            let n = self
                .handle
                .read_bulk(self.bulk_in, &mut csw[total..], TIMEOUT)
                .map_err(|e| anyhow!("CSW read_bulk: {e}"))?;
            if n == 0 {
                return Err(anyhow!("CSW read returned 0 bytes (stall?)"));
            }
            total += n;
        }

        let sig = u32::from_le_bytes(csw[0..4].try_into().unwrap());
        if sig != 0x5342_5355 {
            return Err(anyhow!("Invalid CSW signature: {sig:#010x} (expected 0x53425355)"));
        }
        let tag = u32::from_le_bytes(csw[4..8].try_into().unwrap());
        if tag != expected_tag {
            return Err(anyhow!(
                "CSW tag mismatch: got {tag}, expected {expected_tag}"
            ));
        }
        if csw[12] != 0 {
            return Err(anyhow!("CSW failure status: {}", csw[12]));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// BulkTransport implementation
// ---------------------------------------------------------------------------

impl BulkTransport for NativeTransport {
    fn command_in(&mut self, cbwcb: &[u8], transfer_len: u32) -> Result<Vec<u8>> {
        let tag = self.next_tag();
        self.write_cbw(tag, transfer_len, true, cbwcb)?;

        // Read data phase
        let mut data = vec![0u8; transfer_len as usize];
        let mut total = 0;
        while total < transfer_len as usize {
            let n = self
                .handle
                .read_bulk(self.bulk_in, &mut data[total..], TIMEOUT)
                .map_err(|e| anyhow!("data read_bulk: {e}"))?;
            if n == 0 {
                break; // short transfer — device finished early
            }
            total += n;
        }
        data.truncate(total);

        self.read_csw(tag)?;
        Ok(data)
    }

    fn command_out(&mut self, cbwcb: &[u8], data: Option<&[u8]>) -> Result<()> {
        let tag = self.next_tag();
        let transfer_len = data.map(|d| d.len() as u32).unwrap_or(0);
        self.write_cbw(tag, transfer_len, false, cbwcb)?;

        if let Some(d) = data {
            let mut offset = 0;
            while offset < d.len() {
                let n = self
                    .handle
                    .write_bulk(self.bulk_out, &d[offset..], TIMEOUT)
                    .map_err(|e| anyhow!("data write_bulk: {e}"))?;
                if n == 0 {
                    return Err(anyhow!("write_bulk returned 0 — device stalled?"));
                }
                offset += n;
            }
        }

        self.read_csw(tag)
    }
}

// ---------------------------------------------------------------------------
// Drop: release the claimed interface
// ---------------------------------------------------------------------------

impl Drop for NativeTransport {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(self.interface_number);
    }
}

// ---------------------------------------------------------------------------
// Helper: find bulk-IN and bulk-OUT endpoint addresses
// ---------------------------------------------------------------------------

fn find_bulk_endpoints(
    config: &rusb::ConfigDescriptor,
    interface_number: u8,
) -> Option<(u8, u8)> {
    for iface in config.interfaces() {
        if iface.number() != interface_number {
            continue;
        }
        for desc in iface.descriptors() {
            let mut in_ep = None;
            let mut out_ep = None;
            for ep in desc.endpoint_descriptors() {
                if ep.transfer_type() == TransferType::Bulk {
                    match ep.direction() {
                        Direction::In => in_ep = Some(ep.address()),
                        Direction::Out => out_ep = Some(ep.address()),
                    }
                }
            }
            if let (Some(i), Some(o)) = (in_ep, out_ep) {
                return Some((i, o));
            }
        }
    }
    None
}
