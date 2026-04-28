//! Target-independent SCSI layer on top of [`BulkTransport`].
//!
//! `ScsiDevice<T>` compiles identically for native (T = `NativeTransport`,
//! condition C3) and wasm32-wasip2 (T = `WasmTransport`, condition C5 added
//! in F3). Only the transport implementation changes; the SCSI logic is
//! shared. This is the key "one source, two runtimes" claim of the thesis.
//!
//! Supported SCSI commands:
//! - TEST UNIT READY (0x00) — validate device is ready
//! - READ CAPACITY (10) (0x25) — discover block geometry
//! - READ (10) (0x28) — sequential block reads for W-bulk workload
//!
//! The implementation intentionally avoids higher-level filesystem crates so
//! we measure **raw block-I/O overhead**, not filesystem parsing overhead.

use anyhow::{anyhow, Result};

use crate::bulk_only::BulkTransport;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Block geometry returned by SCSI READ CAPACITY (10).
#[derive(Debug, Clone, Copy)]
pub struct Capacity {
    /// Bytes per logical block (typically 512).
    pub block_size: u32,
    /// Total number of logical blocks on the device.
    pub num_blocks: u32,
    /// Total capacity in bytes (`block_size * num_blocks`).
    pub total_bytes: u64,
}

/// SCSI device wrapper. Owns a `BulkTransport` implementation and exposes
/// typed SCSI commands for the W-bulk benchmark workload.
///
/// # Type parameter
/// `T: BulkTransport` — the underlying transport. Use `NativeTransport` for
/// native builds (C3) and `WasmTransport` (F3) for wasm32-wasip2 (C5).
pub struct ScsiDevice<T: BulkTransport> {
    transport: T,
    /// Block geometry discovered at init time via READ CAPACITY (10).
    pub capacity: Capacity,
}

impl<T: BulkTransport> ScsiDevice<T> {
    /// Initialise the SCSI device: run TEST UNIT READY + READ CAPACITY (10)
    /// to validate the device and discover the block geometry.
    pub fn new(mut transport: T) -> Result<Self> {
        // SCSI TEST UNIT READY — 6-byte CDB, no data phase
        transport
            .command_out(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00], None)
            .map_err(|e| anyhow!("TEST UNIT READY failed: {e}"))?;

        // SCSI READ CAPACITY (10) — 10-byte CDB, 8-byte response
        let data = transport
            .command_in(
                &[0x25, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
                8,
            )
            .map_err(|e| anyhow!("READ CAPACITY (10) failed: {e}"))?;

        if data.len() < 8 {
            return Err(anyhow!(
                "READ CAPACITY (10) returned {} bytes (expected 8)",
                data.len()
            ));
        }

        // Response: [returned_lba (4 BE bytes)] [block_length (4 BE bytes)]
        let last_lba =
            u32::from_be_bytes(data[0..4].try_into().unwrap());
        let block_size =
            u32::from_be_bytes(data[4..8].try_into().unwrap());

        if block_size == 0 {
            return Err(anyhow!("Device reports block_size = 0"));
        }

        let num_blocks = last_lba.saturating_add(1);
        let total_bytes = num_blocks as u64 * block_size as u64;

        Ok(Self {
            transport,
            capacity: Capacity {
                block_size,
                num_blocks,
                total_bytes,
            },
        })
    }

    // -----------------------------------------------------------------------
    // SCSI READ (10) — core W-bulk workload operation
    // -----------------------------------------------------------------------

    /// SCSI READ (10): read `count` logical blocks starting at LBA `lba`.
    ///
    /// Returns `count * block_size` raw bytes. The returned buffer is *not*
    /// parsed — the benchmark checks only its size and optionally hashes it
    /// for integrity validation.
    ///
    /// **Timing note**: callers should start their `Instant::now()` *before*
    /// this call and stop it *after* it returns to capture the full USB RTT.
    pub fn read_blocks(&mut self, lba: u32, count: u16) -> Result<Vec<u8>> {
        let transfer_len = count as u32 * self.capacity.block_size;

        // SCSI READ (10) — 10-byte CDB (SBC-3 §5.8)
        #[rustfmt::skip]
        let cdb: [u8; 10] = [
            0x28,                                           // OPERATION CODE
            0x00,                                           // flags (DPO=0, FUA=0)
            (lba >> 24) as u8, (lba >> 16) as u8,          // LOGICAL BLOCK ADDRESS (MSB)
            (lba >>  8) as u8,  lba         as u8,          // LOGICAL BLOCK ADDRESS (LSB)
            0x00,                                           // GROUP NUMBER
            (count >> 8) as u8, count as u8,                // TRANSFER LENGTH
            0x00,                                           // CONTROL
        ];

        self.transport
            .command_in(&cdb, transfer_len)
            .map_err(|e| anyhow!("READ (10) lba={lba} count={count}: {e}"))
    }

    // -----------------------------------------------------------------------
    // SCSI WRITE (10) — write workload
    // -----------------------------------------------------------------------

    /// SCSI WRITE (10): write `count` logical blocks starting at LBA `lba`.
    ///
    /// The caller supplies `data` which must be exactly `count * block_size`
    /// bytes.  The data is sent as-is to the device; no filesystem parsing
    /// is performed.
    ///
    /// ⚠️ **Data safety**: this overwrites whatever is stored at `lba` on the
    /// device.  Use only on a dedicated test device or a scratch LBA range.
    /// For a safe read-then-write-back pattern use [`ScsiDevice::readwrite_blocks`].
    ///
    /// Returns `Ok(())` on success.
    pub fn write_blocks(&mut self, lba: u32, count: u16, data: &[u8]) -> Result<()> {
        let expected = count as usize * self.capacity.block_size as usize;
        if data.len() != expected {
            return Err(anyhow!(
                "write_blocks: data length {} ≠ expected {} (count={count} block_size={})",
                data.len(), expected, self.capacity.block_size
            ));
        }

        // SCSI WRITE (10) — 10-byte CDB (SBC-3 §5.26)
        #[rustfmt::skip]
        let cdb: [u8; 10] = [
            0x2A,                                           // OPERATION CODE
            0x00,                                           // flags (DPO=0, FUA=0)
            (lba >> 24) as u8, (lba >> 16) as u8,          // LOGICAL BLOCK ADDRESS (MSB)
            (lba >>  8) as u8,  lba         as u8,          // LOGICAL BLOCK ADDRESS (LSB)
            0x00,                                           // GROUP NUMBER
            (count >> 8) as u8, count as u8,                // TRANSFER LENGTH
            0x00,                                           // CONTROL
        ];

        self.transport
            .command_out(&cdb, Some(data))
            .map_err(|e| anyhow!("WRITE (10) lba={lba} count={count}: {e}"))
    }

    // -----------------------------------------------------------------------
    // SCSI READ-then-WRITE (safe write benchmark)
    // -----------------------------------------------------------------------

    /// Read `count` blocks at `lba`, then write the same data back.
    ///
    /// This is the safest write-benchmark pattern: it never changes the
    /// on-disk content, so the device filesystem stays intact.  It measures
    /// the **combined** RTT of one read + one write and returns the bytes
    /// read (for checksum verification).
    ///
    /// **Note**: the returned duration in the caller measures both transfers.
    /// If you want write-only throughput, use [`write_blocks`] with a pre-built
    /// buffer of known content.
    pub fn readwrite_blocks(&mut self, lba: u32, count: u16) -> Result<Vec<u8>> {
        let data = self.read_blocks(lba, count)?;
        self.write_blocks(lba, count, &data)?;
        Ok(data)
    }

    // -----------------------------------------------------------------------
    // Convenience getters
    // -----------------------------------------------------------------------

    /// Bytes per logical block (shortcut for `self.capacity.block_size`).
    pub fn block_size(&self) -> u32 {
        self.capacity.block_size
    }

    /// Total number of logical blocks (shortcut for `self.capacity.num_blocks`).
    pub fn num_blocks(&self) -> u32 {
        self.capacity.num_blocks
    }
}
