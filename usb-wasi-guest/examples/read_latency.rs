use wit_bindgen::generate;
use component::usb::{
    device::{self, DeviceHandle, UsbDevice},
    transfers::{TransferType, TransferSetup, TransferOptions},
    errors::LibusbError,
    configuration::ConfigValue,
};
use mbrman::{MBR, MBRPartitionEntry};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::time::{Duration, Instant};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use anyhow::{Result, anyhow};
use log::{debug, error, info, trace, warn};
use std::fs;
use tokio::time::timeout;

// Generate bindings for the WASI-USB interface
generate!({
    world: "guest",
    path: "../wit",
    generate_all,
});

// Custom IoSlice to restrict reads to a partition
struct IoSlice<T: Read + Seek> {
    inner: T,
    start: u64,
    end: u64,
    position: u64,
}

impl<T: Read + Seek> IoSlice<T> {
    fn new(inner: T, start: u64, end: u64) -> Result<Self> {
        if start >= end {
            return Err(anyhow!("Invalid slice range"));
        }
        Ok(IoSlice {
            inner,
            start,
            end,
            position: start,
        })
    }
}

impl<T: Read + Seek> Read for IoSlice<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.position >= self.end {
            return Ok(0);
        }
        let max_bytes = (self.end - self.position).min(buf.len() as u64) as usize;
        self.inner.seek(SeekFrom::Start(self.position))?;
        let bytes_read = self.inner.read(&mut buf[..max_bytes])?;
        self.position += bytes_read as u64;
        Ok(bytes_read)
    }
}

impl<T: Read + Seek> Seek for IoSlice<T> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(offset) => self.start + offset,
            SeekFrom::End(offset) => self.end.saturating_add(offset as u64),
            SeekFrom::Current(offset) => self.position.saturating_add_signed(offset),
        };
        if new_pos < self.start || new_pos > self.end {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Seek out of range"));
        }
        self.position = new_pos;
        self.inner.seek(SeekFrom::Start(self.position))?;
        Ok(self.position - self.start)
    }
}

// USB mass storage device wrapper
struct UsbMassStorage {
    handle: DeviceHandle,
    in_endpoint: u8,
    out_endpoint: u8,
    block_size: u32,
    seek_position: u64,
    capacity: DeviceCapacity,
    tag: u32,
}

#[derive(Default, Clone, Copy)]
struct DeviceCapacity {
    size: u64,          // Total size in bytes
    block_length: u32,  // Block size in bytes
}

impl UsbMassStorage {
    fn new(device: UsbDevice) -> Result<Self, LibusbError> {
        info!("Initializing USB mass storage device");
        debug!("Opening USB device");
        let handle = device.open()?;

        debug!("Setting configuration to 0");
        handle.set_configuration(ConfigValue::Value(0))?;

        let endpoints = vec![(0x81, 0x02)];
        let (in_endpoint, out_endpoint) = endpoints[0];
        info!("Found bulk endpoints: IN=0x{:02x}, OUT=0x{:02x}", in_endpoint, out_endpoint);

        debug!("Claiming interface 0");
        handle.claim_interface(0)?;

        let mut storage = Self {
            handle,
            in_endpoint,
            out_endpoint,
            block_size: 512,
            seek_position: 0,
            capacity: Default::default(),
            tag: 0,
        };

        debug!("Performing device reset");
        if !storage.reset()? {
            error!("Device reset failed");
            return Err(LibusbError::Other);
        }

        debug!("Testing if unit is ready");
        if !storage.test_unit_ready()? {
            error!("Device not ready");
            return Err(LibusbError::Other);
        }

        debug!("Reading device capacity");
        storage.capacity = storage.read_capacity()?;
        info!("Device capacity: {} bytes, block size: {} bytes",
              storage.capacity.size, storage.capacity.block_length);

        Ok(storage)
    }

    fn reset(&self) -> Result<bool, LibusbError> {
        trace!("Sending Mass Storage Reset command (bRequest=0xFF)");
        let setup = TransferSetup {
            bm_request_type: 0x21,
            b_request: 0xFF,
            w_value: 0,
            w_index: 0,
        };
        let opts = TransferOptions {
            endpoint: 0,
            timeout_ms: 1000,
            stream_id: 0,
            iso_packets: 0,
        };

        trace!("Creating control transfer for device reset");
        let xfer = self.handle.new_transfer(TransferType::Control, setup, 0, opts)?;
        trace!("Submitting reset transfer");
        xfer.submit_transfer(&[])?;
        trace!("Waiting for reset transfer completion");
        component::usb::transfers::await_transfer(&xfer)?;
        debug!("USB mass storage reset successful");
        Ok(true)
    }

    fn test_unit_ready(&mut self) -> Result<bool, LibusbError> {
        debug!("Sending SCSI TEST UNIT READY command");
        let cbwcb = vec![0; 6];
        match self.send_over_usb(cbwcb, None) {
            Ok(_) => {
                debug!("Device reports ready status");
                Ok(true)
            },
            Err(e) => {
                warn!("TEST UNIT READY failed: {:?}", e);
                Err(e)
            }
        }
    }

    fn read_capacity(&mut self) -> Result<DeviceCapacity, LibusbError> {
        debug!("Sending SCSI READ CAPACITY (10) command");
        let mut data = vec![0u8; 8];
        let mut command_block = BytesMut::with_capacity(10);
        command_block.put_u8(0x25);
        command_block.put_bytes(0, 9);

        trace!("Receiving capacity data from device");
        self.receive_over_usb(command_block.to_vec(), &mut data)?;

        let mut bytes = Bytes::from(data);
        let block_count = bytes.get_u32();
        let block_length = bytes.get_u32();

        let size = block_count as u64 * block_length as u64;
        debug!("Device capacity: {} blocks of {} bytes (total: {} bytes, {:.2} MB)",
               block_count + 1, block_length, size, size as f64 / 1_048_576.0);

        Ok(DeviceCapacity {
            size,
            block_length,
        })
    }

    fn send_over_usb(&mut self, cbwcb: Vec<u8>, data: Option<&[u8]>) -> Result<(), LibusbError> {
        let tag = self.increase_tag();
        let data_length = data.map(|d| d.len()).unwrap_or(0) as u32;

        trace!("Preparing CBW with tag={}, data_length={}", tag, data_length);
        trace!("Command: {:02x?}", cbwcb);

        let mut cbw = BytesMut::with_capacity(31);
        cbw.put_u32_le(0x43425355);
        cbw.put_u32_le(tag);
        cbw.put_u32_le(data_length);
        cbw.put_u8(if data.is_some() { 0 } else { 0x80 });
        cbw.put_u8(0);
        cbw.put_u8(cbwcb.len() as u8);
        cbw.put_slice(&cbwcb);
        cbw.resize(31, 0);

        let opts = TransferOptions {
            endpoint: self.out_endpoint,
            timeout_ms: 1000,
            stream_id: 0,
            iso_packets: 0,
        };

        trace!("Creating bulk transfer for CBW to endpoint 0x{:02x}", self.out_endpoint);
        let xfer = self.handle.new_transfer(
            TransferType::Bulk,
            TransferSetup {
                bm_request_type: 0x21,
                b_request: 0,
                w_value: 0,
                w_index: 0,
            },
            31,
            opts,
        )?;

        trace!("Submitting CBW transfer");
        xfer.submit_transfer(&cbw)?;
        trace!("Waiting for CBW transfer completion");
        component::usb::transfers::await_transfer(&xfer)?;
        trace!("CBW transfer completed");

        if let Some(data) = data {
            trace!("Sending {} bytes of data", data.len());
            let xfer = self.handle.new_transfer(
                TransferType::Bulk,
                TransferSetup {
                    bm_request_type: 0x21,
                    b_request: 0,
                    w_value: 0,
                    w_index: 0,
                },
                data_length,
                opts,
            )?;
            xfer.submit_transfer(data)?;
            component::usb::transfers::await_transfer(&xfer)?;
            trace!("Data transfer completed");
        }

        trace!("Receiving CSW for tag {}", tag);
        let mut csw_data = vec![0u8; 13];
        self.receive_csw(tag, &mut csw_data)?;
        trace!("CSW received successfully");

        Ok(())
    }

    fn receive_csw(&self, tag: u32, csw_data: &mut [u8]) -> Result<(), LibusbError> {
        trace!("Setting up transfer to receive CSW from endpoint 0x{:02x}", self.in_endpoint);
        let opts = TransferOptions {
            endpoint: self.in_endpoint,
            timeout_ms: 1000,
            stream_id: 0,
            iso_packets: 0,
        };
        let xfer = self.handle.new_transfer(
            TransferType::Bulk,
            TransferSetup {
                bm_request_type: 0xA1,
                b_request: 0,
                w_value: 0,
                w_index: 0,
            },
            13,
            opts,
        )?;

        trace!("Submitting CSW receive transfer");
        xfer.submit_transfer(&[])?;
        trace!("Waiting for CSW data");
        let data = component::usb::transfers::await_transfer(&xfer)?.data;
        trace!("Received {} bytes for CSW", data.len());

        if data.len() < 13 {
            error!("CSW data too short: {} bytes", data.len());
            return Err(LibusbError::Overflow);
        }

        csw_data[..13].copy_from_slice(&data[..13]);

        let csw_signature = u32::from_le_bytes(csw_data[0..4].try_into().unwrap());
        let csw_tag = u32::from_le_bytes(csw_data[4..8].try_into().unwrap());
        let csw_status = csw_data[12];

        trace!("CSW signature: 0x{:08x}, tag: {}, status: {}", csw_signature, csw_tag, csw_status);

        if csw_signature != 0x53425355 {
            error!("Invalid CSW signature: 0x{:08x}, expected 0x53425355", csw_signature);
            return Err(LibusbError::Other);
        }

        if csw_tag != tag {
            error!("CSW tag mismatch: got {}, expected {}", csw_tag, tag);
            return Err(LibusbError::Other);
        }

        if csw_status != 0 {
            error!("CSW indicates command failed: status code {}", csw_status);
            return Err(LibusbError::Other);
        }

        trace!("CSW validation successful");
        Ok(())
    }

    fn receive_over_usb(&mut self, cbwcb: Vec<u8>, data: &mut [u8]) -> Result<(), LibusbError> {
        let tag = self.increase_tag();
        let data_length = data.len() as u32;

        trace!("Preparing IN transfer with tag={}, expecting {} bytes", tag, data_length);
        trace!("Command: {:02x?}", cbwcb);

        let mut cbw = BytesMut::with_capacity(31);
        cbw.put_u32_le(0x43425355);
        cbw.put_u32_le(tag);
        cbw.put_u32_le(data_length);
        cbw.put_u8(0x80);
        cbw.put_u8(0);
        cbw.put_u8(cbwcb.len() as u8);
        cbw.put_slice(&cbwcb);
        cbw.resize(31, 0);

        let opts = TransferOptions {
            endpoint: self.out_endpoint,
            timeout_ms: 1000,
            stream_id: 0,
            iso_packets: 0,
        };

        trace!("Creating bulk transfer for CBW to endpoint 0x{:02x}", self.out_endpoint);
        let xfer = self.handle.new_transfer(
            TransferType::Bulk,
            TransferSetup {
                bm_request_type: 0x21,
                b_request: 0,
                w_value: 0,
                w_index: 0,
            },
            31,
            opts,
        )?;

        trace!("Submitting CBW transfer");
        xfer.submit_transfer(&cbw)?;
        trace!("Waiting for CBW transfer completion");
        component::usb::transfers::await_transfer(&xfer)?;
        trace!("CBW transfer completed");

        trace!("Setting up data IN transfer from endpoint 0x{:02x}, expecting {} bytes",
               self.in_endpoint, data_length);
        let opts = TransferOptions {
            endpoint: self.in_endpoint,
            timeout_ms: 1000,
            stream_id: 0,
            iso_packets: 0,
        };
        let xfer = self.handle.new_transfer(
            TransferType::Bulk,
            TransferSetup {
                bm_request_type: 0xA1,
                b_request: 0,
                w_value: 0,
                w_index: 0,
            },
            data_length,
            opts,
        )?;

        trace!("Submitting data IN transfer");
        xfer.submit_transfer(&[])?;
        trace!("Waiting for data");
        let received_data = component::usb::transfers::await_transfer(&xfer)?.data;
        trace!("Received {} bytes of data", received_data.len());

        if received_data.len() < data_length as usize {
            warn!("Received fewer bytes than requested: {} < {}", received_data.len(), data_length);
        }

        let copy_len = received_data.len().min(data_length as usize);
        data[..copy_len].copy_from_slice(&received_data[..copy_len]);

        trace!("Receiving CSW for tag {}", tag);
        let mut csw_data = vec![0u8; 13];
        self.receive_csw(tag, &mut csw_data)?;
        trace!("CSW received successfully");

        Ok(())
    }

    fn read_10(&mut self, block_address: u32, transfer_length: u16) -> Result<Vec<u8>, LibusbError> {
        trace!("Sending SCSI READ(10) command for block {} (length: {} blocks)",
              block_address, transfer_length);

        let mut command_block = BytesMut::with_capacity(10);
        command_block.put_u8(0x28);
        command_block.put_u8(0);
        command_block.put_slice(&block_address.to_be_bytes());
        command_block.put_u8(0);
        command_block.put_slice(&transfer_length.to_be_bytes());
        command_block.put_u8(0);

        let capacity = transfer_length as u64 * self.capacity.block_length as u64;
        trace!("Allocating {} bytes for read data", capacity);
        let mut data = vec![0u8; capacity as usize];

        let start = Instant::now();
        self.receive_over_usb(command_block.to_vec(), &mut data)?;
        let elapsed = start.elapsed();

        let rate_kbps = if elapsed.as_millis() > 0 {
            (capacity as f64 / 1024.0) / (elapsed.as_millis() as f64 / 1000.0)
        } else {
            0.0
        };

        trace!("Read {} blocks ({} bytes) in {:?}, {:.2} KB/s",
              transfer_length, capacity, elapsed, rate_kbps);

        Ok(data)
    }

    fn increase_tag(&mut self) -> u32 {
        self.tag = self.tag.wrapping_add(1);
        self.tag
    }
}

impl Read for UsbMassStorage {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let start_address = self.seek_position;
        let end_address = self.seek_position + buf.len() as u64;

        if end_address > self.capacity.size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Read exceeds device capacity",
            ));
        }

        let num_bytes = (end_address - start_address) as usize;
        let block_length = self.capacity.block_length as u64;

        let block_address = (start_address / block_length) as u32;
        let transfer_length = ((end_address - 1) / block_length - block_address as u64 + 1) as u16;

        let data = self.read_10(block_address, transfer_length).map_err(|e| {
            io::Error::new(io::ErrorKind::Other, format!("USB error: {:?}", e))
        })?;

        let start_index = (start_address % block_length) as usize;
        let end_index = start_index + num_bytes;
        buf.copy_from_slice(&data[start_index..end_index]);

        self.seek_position += num_bytes as u64;
        Ok(num_bytes)
    }
}

impl Seek for UsbMassStorage {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.seek_position = match pos {
            SeekFrom::Start(position) => position,
            SeekFrom::End(position) => self.capacity.size.saturating_add(position as u64),
            SeekFrom::Current(position) => self.seek_position.saturating_add_signed(position),
        };
        Ok(self.seek_position)
    }
}

// Control IN transfer for device enumeration
fn control_in(
    device: &UsbDevice,
    request: u8,
    w_value: u16,
    w_index: u16,
    len: u16,
) -> Result<Vec<u8>, LibusbError> {
    let handle = device.open()?;
    let setup = TransferSetup {
        bm_request_type: 0x80,
        b_request: request,
        w_value,
        w_index,
    };
    let opts = TransferOptions {
        endpoint: 0,
        timeout_ms: 1000,
        stream_id: 0,
        iso_packets: 0,
    };
    let xfer = handle.new_transfer(TransferType::Control, setup, len as u32, opts)?;
    xfer.submit_transfer(&[])?;
    component::usb::transfers::await_transfer(&xfer).map(|r| r.data)
}

fn main() {
    env_logger::init();
    println!("Initializing USB subsystem...");
    device::init().expect("Failed to initialize libusb");

    println!("Searching for USB devices...");
    let devs = device::list_devices().expect("Failed to list devices");
    println!("Found {} USB devices", devs.len());

    println!("Looking for Kingston DataTraveler (0951:1666)...");
    let target_dev = devs.into_iter().find(|(dev, descriptor, _)| {
        descriptor.vendor_id == 0x0951 && descriptor.product_id == 0x1666
    });

    let dev = match target_dev {
        Some(dev) => dev,
        None => {
            println!("USB drive (0951:1666) not found");
            return;
        }
    };

    let mut usb = match UsbMassStorage::new(dev.0) {
        Ok(usb) => usb,
        Err(e) => {
            println!("Failed to open USB drive: {:?}", e);
            return;
        }
    };

    let block_length = usb.capacity.block_length;
    println!("Reading MBR from device (block size: {} bytes)...", block_length);
    let mbr = match MBR::read_from(&mut usb, block_length) {
        Ok(mbr) => mbr,
        Err(e) => {
            println!("Failed to parse MBR: {:?}", e);
            return;
        }
    };

    println!("Searching for usable partitions...");
    for (idx, part) in mbr.iter() {
        if part.is_used() {
            println!("  Partition {}: Start={}, Sectors={}",
                     idx, part.starting_lba, part.sectors);
        }
    }

    let data_partition = match mbr.iter().find(|p| p.1.is_used()) {
        Some((idx, p)) => {
            println!("Using partition {}", idx);
            p
        },
        None => {
            println!("No used partition found");
            return;
        }
    };

    let slice_start = data_partition.starting_lba as u64 * block_length as u64;
    let slice_end = (data_partition.starting_lba as u64 + data_partition.sectors as u64) * block_length as u64;
    let partition_size_mb = (slice_end - slice_start) as f64 / 1_048_576.0;

    println!("Creating partition slice: offset={} bytes, size={:.2} MB",
             slice_start, partition_size_mb);
    let slice = match IoSlice::new(&mut usb, slice_start, slice_end) {
        Ok(slice) => slice,
        Err(e) => {
            println!("Failed to create slice: {:?}", e);
            return;
        }
    };

    // Latency test
    println!("Running read latency benchmark...");
    let mut durations = Vec::with_capacity(1_000_000);
    let block_size = 512; // Match device block size
    let mut buffer = vec![0u8; block_size];

    // 1) constants
    const WARMUP_ITERS: usize  = 1_000;
    const MEASURE_ITERS: usize = 1_000_000;

    // 2) warm up USB + OS caches
    for _ in 0..WARMUP_ITERS {
        usb.seek(SeekFrom::Start(0)).expect("warmup seek");
        usb.read(&mut buffer).expect("warmup read");
    }

    // 3) measure timer‐overhead once
    let timer_overhead_ns: f64 = {
        let t0 = Instant::now();
        for _ in 0..MEASURE_ITERS {
            let _ = Instant::now().elapsed();
        }
        t0.elapsed().as_nanos() as f64 / MEASURE_ITERS as f64
    };

    // 4) the real benchmark loop
    durations.clear();
    for i in 0..MEASURE_ITERS {
        // pick a pseudo‐random but repeatable offset
        let pos = (i as u64 * block_size as u64) % usb.capacity.size;
        usb.seek(SeekFrom::Start(pos)).expect("seek failed");

        let start = Instant::now();
        usb.read(&mut buffer).expect("read failed");
        let raw_ns = start.elapsed().as_nanos() as f64;

        // subtract out the Instant/elapsed overhead
        let read_only_ns = raw_ns - timer_overhead_ns;
        durations.push(read_only_ns as u64);
    }

    // Write durations to a file for analysis
    let mut file = fs::File::create("latencies_wasi.txt").expect("Failed to create file");
    for duration in durations {
        writeln!(file, "{}", duration).expect("Failed to write to file");
    }

    // Clean up
    usb.handle.close();
}