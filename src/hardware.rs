use crate::media::MAX_PAYLOAD;
use anyhow::{bail, Context, Result};
use cbc::Encryptor;
use des::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
use des::Des;
use log::{debug, info, warn};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use rusb::{Device, DeviceHandle, GlobalContext};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TX_VENDOR: u16 = 0x0416;
const TX_PRODUCT: u16 = 0x8040;
const RX_VENDOR: u16 = 0x0416;
const RX_PRODUCT: u16 = 0x8041;
const LCD_VENDOR: u16 = 0x1cbe;
const LCD_PRODUCT: u16 = 0x0006;

const EP_OUT: u8 = 0x01;
const EP_IN: u8 = 0x81;

const USB_TIMEOUT: Duration = Duration::from_millis(5_000);
const LCD_WRITE_TIMEOUT: Duration = Duration::from_millis(10_000);

const DES_KEY: [u8; 8] = *b"slv3tuzx";

type DesCbc = Encryptor<Des>;

static CMD_RESET: Lazy<Vec<u8>> = Lazy::new(|| decode_command("11080000"));
static CMD_VIDEO_START: Lazy<Vec<u8>> = Lazy::new(|| decode_command("11010000"));

// Query master MAC command - 0x11 (USB_GetMac)
// Channel will be set dynamically
fn build_get_mac_command(channel: u8) -> Vec<u8> {
    let mut cmd = vec![0u8; 64];
    cmd[0] = 0x11; // USB_GetMac
    cmd[1] = channel; // Wireless channel
    cmd
}

static CMD_RX_QUERY_34: Lazy<Vec<u8>> = Lazy::new(|| decode_command("10010434"));
static CMD_RX_QUERY_37: Lazy<Vec<u8>> = Lazy::new(|| decode_command("10010437"));
static CMD_RX_LCD_MODE: Lazy<Vec<u8>> = Lazy::new(|| decode_command("10010430"));

// Fan speed control - RF data constants
const RF_SELECT: u8 = 18;
const RF_PWM_SUBCMD: u8 = 16;
const RF_DATA_SIZE: usize = 240;
const RF_CHUNK_SIZE: usize = 60;
const USB_CMD_SEND_RF: u8 = 0x10;

fn decode_command(prefix: &str) -> Vec<u8> {
    let mut bytes = hex::decode(prefix).expect("valid hex literal");
    bytes.resize(64, 0u8);
    bytes
}

pub struct PacketBuilder {
    last_timestamp: u32,
}

impl PacketBuilder {
    pub fn new() -> Self {
        Self { last_timestamp: 0 }
    }

    pub fn header(&mut self, payload_size: usize, command: u8, include_size: bool) -> Vec<u8> {
        let mut buf = vec![0u8; 504 + 8];
        buf[0] = command;
        buf[2] = 0x1A;
        buf[3] = 0x6D;

        let raw = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;
        let ts = if raw <= self.last_timestamp {
            self.last_timestamp + 1
        } else {
            raw
        };
        self.last_timestamp = ts;
        buf[4..8].copy_from_slice(&ts.to_le_bytes());

        if include_size {
            buf[8..12].copy_from_slice(&(payload_size as u32).to_be_bytes());
        }

        let cipher = DesCbc::new_from_slices(&DES_KEY, &DES_KEY)
            .expect("DES key and IV must both be 8 bytes");
        cipher
            .encrypt_padded_mut::<Pkcs7>(&mut buf, 504)
            .expect("padding")
            .to_vec()
    }
}

pub struct WirelessController {
    tx: Option<Arc<Mutex<DeviceHandle<GlobalContext>>>>,
    rx: Option<Arc<Mutex<DeviceHandle<GlobalContext>>>>,
    poll_stop: Arc<AtomicBool>,
    poll_thread: Option<JoinHandle<()>>,
    video_mode_active: Arc<AtomicBool>,
    master_mac: Arc<Mutex<[u8; 6]>>,
    discovered_devices: Arc<Mutex<Vec<DiscoveredDevice>>>,
    channel: Arc<Mutex<u8>>, // Wireless channel (default 8, can be 1-16)
}

#[derive(Debug, Clone)]
struct DiscoveredDevice {
    mac: [u8; 6],
    device_index: u8,
}

impl Clone for WirelessController {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            rx: self.rx.clone(),
            poll_stop: Arc::clone(&self.poll_stop),
            poll_thread: None, // Thread handles cannot be cloned
            video_mode_active: Arc::clone(&self.video_mode_active),
            master_mac: Arc::clone(&self.master_mac),
            discovered_devices: Arc::clone(&self.discovered_devices),
            channel: Arc::clone(&self.channel),
        }
    }
}

impl WirelessController {
    pub fn new() -> Self {
        Self {
            tx: None,
            rx: None,
            poll_stop: Arc::new(AtomicBool::new(false)),
            poll_thread: None,
            video_mode_active: Arc::new(AtomicBool::new(false)),
            master_mac: Arc::new(Mutex::new([0u8; 6])),
            discovered_devices: Arc::new(Mutex::new(Vec::new())),
            channel: Arc::new(Mutex::new(8)), // Default channel 8
        }
    }

    pub fn connect(&mut self) -> Result<()> {
        // Try to open TX device with retries
        let mut tx = None;
        let max_retries = 3;

        for attempt in 1..=max_retries {
            match open_device(TX_VENDOR, TX_PRODUCT) {
                Ok(device) => {
                    tx = Some(device);
                    break;
                }
                Err(e) if attempt < max_retries => {
                    warn!("TX device not found (attempt {}/{max_retries}): {e}", attempt);
                    thread::sleep(Duration::from_millis(1000 * attempt as u64));
                }
                Err(e) => {
                    return Err(e).context("opening wireless TX (0416:8040) - device may have disappeared from USB bus");
                }
            }
        }

        let mut tx = tx.context("TX device failed to open after retries")?;
        detach_and_configure(&mut tx, "TX")?;
        let tx_arc = Arc::new(Mutex::new(tx));

        let rx_arc = if let Some(mut rx) = open_device_optional(RX_VENDOR, RX_PRODUCT)? {
            detach_and_configure(&mut rx, "RX")?;
            Some(Arc::new(Mutex::new(rx)))
        } else {
            warn!("RX device (0416:8041) not found – telemetry disabled");
            None
        };

        self.tx = Some(tx_arc);
        self.rx = rx_arc;

        // Discover master MAC address from RX device (if available)
        self.discover_master_mac()?;

        Ok(())
    }

    /// Discovers the master MAC address and wireless channel by querying the TX device with command 0x11
    /// Tries multiple channels (1-16) to find the correct one
    /// Based on L-Connect 3's QuerryMasterMac() function (uses RFSender = TX device)
    fn discover_master_mac(&self) -> Result<()> {
        if let Some(tx) = &self.tx {
            info!("Discovering master MAC address and wireless channel...");

            // Try channels in common order: 8 (default), then 1-39
            // L-Connect 3 supports channels 1-39
            let channels_to_try: Vec<u8> = std::iter::once(8)
                .chain((1..=39).filter(|&ch| ch != 8))
                .collect();

            for channel in channels_to_try {
                let cmd = build_get_mac_command(channel);

                let handle = tx.lock();
                if handle.write_bulk(EP_OUT, &cmd, USB_TIMEOUT).is_err() {
                    drop(handle);
                    continue;
                }

                let mut response = [0u8; 64];
                let len = match handle.read_bulk(EP_IN, &mut response, Duration::from_millis(500)) {
                    Ok(len) => len,
                    Err(_) => {
                        drop(handle);
                        continue;
                    }
                };

                drop(handle);

                // Check for valid response
                if len >= 7 && response[0] == 0x11 {
                    // Master MAC is at bytes 1-6 in the response
                    let mut mac = self.master_mac.lock();
                    mac.copy_from_slice(&response[1..7]);

                    // Check if MAC is not all zeros (invalid)
                    if mac.iter().any(|&b| b != 0) {
                        *self.channel.lock() = channel;

                        info!(
                            "Discovered master MAC: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} on channel {}",
                            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5], channel
                        );

                        return Ok(());
                    }
                }
            }

            bail!("Failed to discover master MAC on any channel (tried 1-39)");
        } else {
            bail!("TX device not available - cannot discover master MAC");
        }
    }

    pub fn start_polling(&mut self) -> Result<()> {
        let tx = self
            .tx
            .as_ref()
            .cloned()
            .context("TX device must be connected before polling")?;

        let rx = self
            .rx
            .as_ref()
            .cloned()
            .context("RX device must be connected for device discovery")?;

        {
            let handle = tx.lock();
            handle
                .write_bulk(EP_OUT, &CMD_RESET, USB_TIMEOUT)
                .context("sending TX reset")?;
        }

        // Reset video mode flag since we just reset the device
        self.video_mode_active.store(false, Ordering::Release);

        // Start polling thread on RX device for device discovery (L-Connect 3 uses RFReceiver for GetDev)
        self.poll_stop.store(false, Ordering::SeqCst);
        let stop_flag = self.poll_stop.clone();
        let discovered_devices = Arc::clone(&self.discovered_devices);
        let channel = Arc::clone(&self.channel);

        self.poll_thread = Some(thread::spawn(move || {
            while !stop_flag.load(Ordering::SeqCst) {
                if let Err(err) = poll_and_discover(&rx, &discovered_devices, &channel) {
                    warn!("RX polling error: {err:?}");
                    break;
                }
                // Poll every 500ms for device list
                thread::sleep(Duration::from_millis(500));
            }
        }));

        thread::sleep(Duration::from_millis(1500));
        Ok(())
    }

    pub fn ensure_video_mode(&self) -> Result<()> {
        // Only send video mode commands once, not before every frame
        if self.video_mode_active.load(Ordering::Acquire) {
            return Ok(());
        }

        if let Some(tx) = &self.tx {
            let handle = tx.lock();
            handle
                .write_bulk(EP_OUT, &CMD_VIDEO_START, USB_TIMEOUT)
                .context("sending TX video start")?;
            thread::sleep(Duration::from_millis(2));

            // Build prep commands dynamically based on discovered devices
            let devices = self.discovered_devices.lock();
            let device_count = devices.len().max(1); // At least 1 for backward compatibility
            let current_channel = *self.channel.lock();

            for device_idx in 0..device_count {
                let prep_cmd = build_tx_prep_command(device_idx as u8, current_channel);
                handle
                    .write_bulk(EP_OUT, &prep_cmd, USB_TIMEOUT)
                    .context("sending TX prep command")?;
                thread::sleep(Duration::from_millis(1));
            }

            drop(handle);

            self.video_mode_active.store(true, Ordering::Release);
            info!("Video mode activated with {} device(s) (will not resend until reset)", device_count);
        }
        Ok(())
    }

    pub fn send_rx_sequence(&self) -> Result<()> {
        if let Some(rx) = &self.rx {
            for (cmd, capture) in [
                (&*CMD_RX_QUERY_34, true),
                (&*CMD_RX_QUERY_37, true),
                (&*CMD_RX_LCD_MODE, false),
            ] {
                {
                    let handle = rx.lock();
                    handle
                        .write_bulk(EP_OUT, cmd, USB_TIMEOUT)
                        .context("sending RX command")?;
                }
                thread::sleep(Duration::from_millis(2));
                if capture {
                    let mut buf = [0u8; 64];
                    let handle = rx.lock();
                    if let Ok(len) = handle.read_bulk(EP_IN, &mut buf, USB_TIMEOUT) {
                        debug!("RX resp: {:02x?}", &buf[..len.min(8)]);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn soft_reset(&mut self) -> bool {
        if self.tx.is_none() {
            if let Ok(mut handle) = open_device(TX_VENDOR, TX_PRODUCT) {
                if detach_and_configure(&mut handle, "TX").is_ok() {
                    self.tx = Some(Arc::new(Mutex::new(handle)));
                }
            }
        }

        if let Some(tx) = &self.tx {
            {
                let handle = tx.lock();
                if handle.write_bulk(EP_OUT, &CMD_RESET, USB_TIMEOUT).is_err() {
                    return false;
                }
            }
            // Reset video mode flag since we just reset the device
            self.video_mode_active.store(false, Ordering::Release);
            thread::sleep(Duration::from_millis(50));
            return self.ensure_video_mode().is_ok();
        }

        false
    }

    /// Check if any devices have been discovered
    pub fn has_discovered_devices(&self) -> bool {
        !self.discovered_devices.lock().is_empty()
    }

    /// Get the count of discovered devices
    pub fn discovered_device_count(&self) -> usize {
        self.discovered_devices.lock().len()
    }

    /// Set fan speeds for a specific device (0-3 fans per device)
    /// PWM values: 0-255 (0 = 0%, 128 = 50%, 255 = 100%)
    /// IMPORTANT: Unused fan slots MUST be set to 0
    pub fn set_fan_speeds(&self, device_index: u8, fan_pwm: &[u8; 4]) -> Result<()> {
        let tx = self.tx.as_ref().context("TX device not connected")?;

        // Get the device MAC for this device_index
        let devices = self.discovered_devices.lock();
        let device_mac = devices
            .iter()
            .find(|d| d.device_index == device_index)
            .map(|d| d.mac)
            .context(format!(
                "No device found with index {} (discovered {} device(s) - device discovery may still be in progress)",
                device_index,
                devices.len()
            ))?;

        let master_mac = *self.master_mac.lock();
        let current_channel = *self.channel.lock();
        drop(devices);

        // Build 240-byte RF data packet
        let mut rf_data = vec![0u8; RF_DATA_SIZE];
        rf_data[0] = RF_SELECT;           // RF_Select command (0x12 / 18)
        rf_data[1] = RF_PWM_SUBCMD;       // PWM sub-command (0x10 / 16)
        rf_data[2..8].copy_from_slice(&device_mac);    // Device MAC from discovery
        rf_data[8..14].copy_from_slice(&master_mac);   // Master MAC from query
        rf_data[14] = 0x01;               // RX type
        rf_data[15] = current_channel;    // Wireless channel (auto-detected)
        rf_data[16] = device_index;       // Device index (0-based)
        rf_data[17..21].copy_from_slice(fan_pwm);

        // Send in 4 USB packets (60 bytes each)
        let handle = tx.lock();
        for chunk_idx in 0..4 {
            let mut packet = vec![0u8; 64];
            packet[0] = USB_CMD_SEND_RF;  // USB command: Usb_SendRf
            packet[1] = chunk_idx;         // Sequence number
            packet[2] = current_channel;   // Wireless channel (auto-detected)
            packet[3] = 0x01;              // RX type

            let start = chunk_idx as usize * RF_CHUNK_SIZE;
            let end = start + RF_CHUNK_SIZE;
            packet[4..64].copy_from_slice(&rf_data[start..end]);

            handle
                .write_bulk(EP_OUT, &packet, USB_TIMEOUT)
                .context("sending fan speed RF packet")?;
            thread::sleep(Duration::from_millis(1));
        }

        debug!(
            "Set fan speeds for device {}: {:?}",
            device_index, fan_pwm
        );
        Ok(())
    }

    pub fn stop(&mut self) {
        self.poll_stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.poll_thread.take() {
            let _ = handle.join();
        }

        // Release TX interface
        if let Some(tx) = self.tx.take() {
            // Try to release even if there are other Arc references
            {
                let handle = tx.lock();
                let _ = handle.release_interface(0);
            }

            // Also try unwrap in case we're the last reference
            if let Ok(mutex) = Arc::try_unwrap(tx) {
                let handle = mutex.into_inner();
                let _ = handle.release_interface(0);
            }
        }

        // Release RX interface
        if let Some(rx) = self.rx.take() {
            // Try to release even if there are other Arc references
            {
                let handle = rx.lock();
                let _ = handle.release_interface(0);
            }

            // Also try unwrap in case we're the last reference
            if let Ok(mutex) = Arc::try_unwrap(rx) {
                let handle = mutex.into_inner();
                let _ = handle.release_interface(0);
            }
        }
    }
}

impl Drop for WirelessController {
    fn drop(&mut self) {
        // Ensure cleanup happens even if stop() wasn't called
        self.stop();
    }
}

/// Polls for device discovery using RX device
/// Based on L-Connect 3 decompiled code - uses command 0x10 to get list of all devices
/// Supports 1-12 devices dynamically
fn poll_and_discover(
    rx: &Arc<Mutex<DeviceHandle<GlobalContext>>>,
    discovered_devices: &Arc<Mutex<Vec<DiscoveredDevice>>>,
    _channel: &Arc<Mutex<u8>>,
) -> Result<()> {
    // Send GetDev command to RX device (command 0x10, page 1)
    // This is how L-Connect 3 discovers devices
    let mut cmd = vec![0u8; 64];
    cmd[0] = 0x10; // GetDev command
    cmd[1] = 0x01; // Page 1 (can support up to 10 devices per page)

    let handle = rx.lock();
    handle
        .write_bulk(EP_OUT, &cmd, USB_TIMEOUT)
        .context("sending GetDev command")?;

    // Read response - should contain device list
    // Response format from L-Connect 3:
    // [0x10, device_count, ver_info[2], device_entries[device_count * 42]]
    // Each device entry is 42 bytes with marker 0x1c at byte 41
    let mut response = [0u8; 512]; // Larger buffer for multiple devices
    match handle.read_bulk(EP_IN, &mut response, Duration::from_millis(200)) {
        Ok(len) if len >= 4 => {
            if response[0] != 0x10 {
                debug!("Unexpected response command: 0x{:02x}", response[0]);
                return Ok(());
            }

            let device_count = response[1] as usize;
            debug!("GetDev response: {} device(s) reported", device_count);

            if device_count == 0 || device_count > 12 {
                debug!("Invalid device count: {}", device_count);
                return Ok(());
            }

            let mut found_devices = Vec::new();
            let mut offset = 4; // Start after header [cmd, count, ver[2]]

            for idx in 0..device_count {
                if offset + 42 > len {
                    debug!("Response too short for device {}", idx);
                    break;
                }

                // Check for device marker at byte 41 of entry
                if response[offset + 41] == 0x1c {
                    // Extract device MAC (bytes 0-5 of entry)
                    let mut device_mac = [0u8; 6];
                    device_mac.copy_from_slice(&response[offset..offset + 6]);

                    // Extract master MAC (bytes 6-11 of entry)
                    let mut master_mac = [0u8; 6];
                    master_mac.copy_from_slice(&response[offset + 6..offset + 12]);

                    // Skip if device_type is 0xFF (master device itself)
                    let device_type = response[offset + 18];
                    if device_type == 0xFF {
                        debug!("  Device {}: Skipping master device", idx);
                        offset += 42;
                        continue;
                    }

                    debug!("  Device {}: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, Master {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, Type 0x{:02x}",
                        idx,
                        device_mac[0], device_mac[1], device_mac[2], device_mac[3], device_mac[4], device_mac[5],
                        master_mac[0], master_mac[1], master_mac[2], master_mac[3], master_mac[4], master_mac[5],
                        device_type);

                    found_devices.push(DiscoveredDevice {
                        mac: device_mac,
                        device_index: idx as u8,
                    });
                } else {
                    debug!("  Device {}: No marker at byte 41 (0x{:02x})", idx, response[offset + 41]);
                }

                offset += 42;
            }

            // Update discovered devices
            let mut devices = discovered_devices.lock();
            if !found_devices.is_empty() {
                let old_count = devices.len();
                *devices = found_devices;
                if old_count != devices.len() {
                    info!("Discovered {} wireless device(s) for fan control", devices.len());
                }
            }
        }
        Ok(len) => {
            debug!("GetDev response too short: {} bytes", len);
        }
        Err(rusb::Error::Timeout) => {
            debug!("GetDev timeout - no devices");
        }
        Err(err) => {
            debug!("GetDev error: {}", err);
        }
    }

    Ok(())
}

// Build TX prep command for video mode
// Format: 0x10 <device_idx> <channel> 0xFF [rest zeros]
fn build_tx_prep_command(device_idx: u8, channel: u8) -> Vec<u8> {
    let mut cmd = vec![0u8; 64];
    cmd[0] = 0x10; // Usb_SendRf
    cmd[1] = device_idx;
    cmd[2] = channel; // Wireless channel
    cmd[3] = 0xFF; // Prep marker
    cmd
}

fn open_device(vid: u16, pid: u16) -> Result<DeviceHandle<GlobalContext>> {
    rusb::open_device_with_vid_pid(vid, pid)
        .ok_or_else(|| anyhow::anyhow!("device {:04x}:{:04x} not found", vid, pid))
}

fn open_device_optional(vid: u16, pid: u16) -> Result<Option<DeviceHandle<GlobalContext>>> {
    Ok(rusb::open_device_with_vid_pid(vid, pid))
}

fn detach_and_configure(handle: &mut DeviceHandle<GlobalContext>, name: &str) -> Result<()> {
    match handle.kernel_driver_active(0) {
        Ok(true) => {
            handle
                .detach_kernel_driver(0)
                .with_context(|| format!("detaching kernel driver from {name}"))?;
            debug!("Detached kernel driver from {name}");
        }
        Ok(false) => {}
        Err(rusb::Error::NotSupported) => {}
        Err(e) => return Err(e).context(format!("checking kernel driver for {name}")),
    }

    match handle.set_active_configuration(1) {
        Ok(()) | Err(rusb::Error::Busy) | Err(rusb::Error::NotFound) => {}
        Err(rusb::Error::Io) => {
            // I/O error during configuration - try resetting the device
            warn!("{name} configuration failed with I/O error, attempting USB reset");
            if let Err(e) = handle.reset() {
                warn!("{name} USB reset failed: {e}");
                return Err(e).context(format!("USB reset failed for {name}"));
            }
            info!("{name} USB reset successful, retrying configuration");
            thread::sleep(Duration::from_millis(500));

            // Retry configuration after reset
            match handle.set_active_configuration(1) {
                Ok(()) | Err(rusb::Error::Busy) | Err(rusb::Error::NotFound) => {}
                Err(e) => return Err(e).context(format!("setting configuration for {name} after reset")),
            }
        }
        Err(e) => return Err(e).context(format!("setting configuration for {name}")),
    }

    // Try to claim interface, if busy try to reset first
    match handle.claim_interface(0) {
        Ok(()) => {
            let _ = handle.set_alternate_setting(0, 0);
            Ok(())
        }
        Err(rusb::Error::Busy) => {
            // Interface is busy - likely from previous crash/unclean exit
            warn!("{name} interface busy, attempting USB reset to recover");
            if let Err(e) = handle.reset() {
                warn!("{name} USB reset failed: {e}");
                return Err(e).context(format!("USB reset failed for {name}"));
            }
            info!("{name} USB reset successful");
            thread::sleep(Duration::from_millis(500));

            // Retry claim after reset
            handle
                .claim_interface(0)
                .with_context(|| format!("claiming interface 0 for {name} after reset"))?;
            let _ = handle.set_alternate_setting(0, 0);
            Ok(())
        }
        Err(e) => Err(e).context(format!("claiming interface 0 for {name}")),
    }
}

pub struct LcdDevice {
    handle: DeviceHandle<GlobalContext>,
    bus: u8,
    address: u8,
    serial: String,
    initialized: bool,
}

impl LcdDevice {
    pub fn new(device: Device<GlobalContext>) -> Result<Self> {
        let bus = device.bus_number();
        let address = device.address();

        // Get serial number before opening (opening requires mutable handle)
        let desc = device.device_descriptor().context("reading device descriptor")?;
        let serial = device.open()
            .and_then(|h| h.read_serial_number_string_ascii(&desc))
            .unwrap_or_else(|_| format!("bus{}-addr{}", bus, address));

        let mut handle = device.open().context("opening LCD device")?;
        detach_and_configure(&mut handle, "LCD")?;
        Ok(Self {
            handle,
            bus,
            address,
            serial,
            initialized: false,
        })
    }

    pub fn bus(&self) -> u8 {
        self.bus
    }

    pub fn address(&self) -> u8 {
        self.address
    }

    pub fn serial(&self) -> &str {
        &self.serial
    }

    fn send_init(&mut self, builder: &mut PacketBuilder) -> Result<()> {
        if self.initialized {
            return Ok(());
        }
        debug!(
            "LCD[bus {} addr {}] sending 0x0d init header",
            self.bus, self.address
        );
        let header = builder.header(0, 0x0D, false);
        self.handle
            .write_bulk(EP_OUT, &header, LCD_WRITE_TIMEOUT)
            .context("writing LCD init header")?;
        let mut buf = [0u8; 511];
        let _ = self.handle.read_bulk(EP_IN, &mut buf, USB_TIMEOUT);
        self.initialized = true;
        Ok(())
    }

    pub fn send_frame(&mut self, builder: &mut PacketBuilder, frame: &[u8]) -> Result<()> {
        if frame.len() > MAX_PAYLOAD {
            bail!(
                "frame payload {} exceeds LCD payload limit {}",
                frame.len(),
                MAX_PAYLOAD
            );
        }

        self.send_init(builder)?;

        let header = builder.header(frame.len(), 0x65, true);
        let mut packet = vec![0u8; 102_400];
        packet[..512].copy_from_slice(&header);
        packet[512..512 + frame.len()].copy_from_slice(frame);

        self.handle
            .write_bulk(EP_OUT, &packet, LCD_WRITE_TIMEOUT)
            .context("writing LCD frame data")?;

        let mut buf = [0u8; 511];
        let _ = self.handle.read_bulk(EP_IN, &mut buf, USB_TIMEOUT);
        Ok(())
    }
}

impl Drop for LcdDevice {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(0);
    }
}

pub fn find_lcd_devices() -> Result<Vec<Device<GlobalContext>>> {
    let devices = rusb::devices().context("enumerating USB devices")?;
    let mut list = Vec::new();
    for device in devices.iter() {
        let desc = device
            .device_descriptor()
            .context("reading device descriptor")?;
        if desc.vendor_id() == LCD_VENDOR && desc.product_id() == LCD_PRODUCT {
            list.push(device);
        }
    }
    list.sort_by_key(|dev| (dev.bus_number(), dev.address()));
    Ok(list)
}
