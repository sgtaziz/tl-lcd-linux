use crate::config::{config_identity, AppConfig, ConfigKey, MediaType};
use crate::fan::FanController;
use crate::hardware::{find_lcd_devices, LcdDevice, PacketBuilder, WirelessController};
use crate::media::{prepare_media_asset, MediaAsset, SensorAsset};
use anyhow::Result;
use libc::geteuid;
use log::{debug, error, info, warn};
use parking_lot::Mutex;
use rusb::{Device, GlobalContext};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

const CONFIG_POLL_INTERVAL: Duration = Duration::from_secs(2);
const DEVICE_POLL_INTERVAL: Duration = Duration::from_secs(1);
const ACTIVE_SLEEP: Duration = Duration::from_millis(1);
const IDLE_SLEEP: Duration = Duration::from_millis(200);

pub struct ServiceManager {
    config_path: PathBuf,
    config_mtime: Option<SystemTime>,
    config: Option<AppConfig>,
    media_assets: HashMap<usize, MediaAsset>,
    targets: HashMap<usize, ActiveTarget>,
    wireless: WirelessController,
    packet_builder: PacketBuilder,
    fan_controller: Option<FanController>,
    last_config_check: Instant,
    last_device_scan: Instant,
    running: bool,
}

impl ServiceManager {
    pub fn new(config_path: PathBuf) -> Result<Self> {
        Ok(Self {
            config_path,
            config_mtime: None,
            config: None,
            media_assets: HashMap::new(),
            targets: HashMap::new(),
            wireless: WirelessController::new(),
            packet_builder: PacketBuilder::new(),
            fan_controller: None,
            last_config_check: Instant::now() - CONFIG_POLL_INTERVAL,
            last_device_scan: Instant::now() - DEVICE_POLL_INTERVAL,
            running: true,
        })
    }

    pub fn run(&mut self) -> Result<()> {
        info!("=====================================================================");
        info!("LIANLI LCD SERVICE (Rust)");
        info!("=====================================================================");
        if unsafe { geteuid() } != 0 {
            warn!("Root privileges recommended for USB access.");
        }

        self.load_config(true)?;
        self.ensure_wireless()?;
        let _ = self.wireless.send_rx_sequence();
        self.start_fan_control();

        while self.running {
            let now = Instant::now();

            if now.duration_since(self.last_config_check) >= CONFIG_POLL_INTERVAL {
                self.last_config_check = now;
                if self.load_config(false)? {
                    self.last_device_scan = Instant::now() - DEVICE_POLL_INTERVAL;
                    // Reload fan control on config change
                    self.start_fan_control();
                }
            }

            if now.duration_since(self.last_device_scan) >= DEVICE_POLL_INTERVAL {
                self.last_device_scan = Instant::now();
                self.refresh_targets();
            }

            self.stream_targets();

            thread::sleep(if self.targets.is_empty() {
                IDLE_SLEEP
            } else {
                ACTIVE_SLEEP
            });
        }

        self.shutdown();
        Ok(())
    }

    fn shutdown(&mut self) {
        for target in self.targets.values_mut() {
            target.stop();
        }
        self.targets.clear();

        if let Some(fan_controller) = self.fan_controller.take() {
            info!("Stopping fan controller...");
            fan_controller.stop();
        }

        self.wireless.stop();
    }

    fn start_fan_control(&mut self) {
        // Stop existing fan controller if any
        if let Some(controller) = self.fan_controller.take() {
            info!("Stopping existing fan controller for reload...");
            controller.stop();
        }

        let (fan_config, fan_curves) = if let Some(cfg) = &self.config {
            match (&cfg.fans, &cfg.fan_curves) {
                (Some(fans), curves) => (fans.clone(), curves.clone()),
                (None, _) => {
                    info!("No fan configuration found in config");
                    return;
                }
            }
        } else {
            return;
        };

        info!("Starting fan control with {} curve(s)", fan_curves.len());
        let wireless = Arc::new(self.wireless.clone());
        let mut controller = FanController::new(fan_config, fan_curves, wireless);
        controller.start();
        self.fan_controller = Some(controller);
    }

    fn ensure_wireless(&mut self) -> Result<()> {
        loop {
            match self.wireless.connect() {
                Ok(()) => {
                    self.wireless.start_polling()?;
                    info!("Wireless links active");
                    return Ok(());
                }
                Err(err) => {
                    info!("[wireless] waiting for TX/RX devices: {err}");
                    thread::sleep(Duration::from_secs(2));
                }
            }
        }
    }

    fn recover_wireless(&mut self) -> bool {
        if self.wireless.soft_reset() {
            return true;
        }
        warn!("Wireless soft-reset failed; reinitialising");
        self.wireless.stop();
        self.ensure_wireless().is_ok()
    }

    fn load_config(&mut self, force: bool) -> Result<bool> {
        let metadata = match std::fs::metadata(&self.config_path) {
            Ok(meta) => meta,
            Err(err) => {
                error!(
                    "unable to access config {}: {err}",
                    self.config_path.display()
                );
                return Ok(false);
            }
        };

        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if !force && self.config_mtime == Some(modified) {
            return Ok(false);
        }

        match AppConfig::load(&self.config_path) {
            Ok(cfg) => {
                self.config = Some(cfg);
            }
            Err(err) => {
                error!("failed to load {}: {err}", self.config_path.display());
                return Ok(false);
            }
        };
        self.config_mtime = Some(modified);
        self.packet_builder = PacketBuilder::new();
        self.prepare_media_assets();
        Ok(true)
    }

    fn prepare_media_assets(&mut self) {
        self.media_assets.clear();
        if let Some(cfg) = &self.config {
            for (idx, device) in cfg.lcds.iter().enumerate() {
                let cfg_key = config_identity(device);
                match prepare_media_asset(device, cfg.default_fps) {
                    Ok(asset) => {
                        self.media_assets.insert(idx, asset);
                        let device_id = device.device_id();
                        match device.media_type {
                            MediaType::Image => info!("Prepared image for LCD[{device_id}]"),
                            MediaType::Video => info!("Prepared video for LCD[{device_id}]"),
                            MediaType::Gif => info!("Prepared GIF for LCD[{device_id}]"),
                            MediaType::Color => {
                                info!("Prepared color frame for LCD[{device_id}]")
                            }
                            MediaType::Sensor => info!(
                                "Prepared sensor for LCD[{device_id}]: {}",
                                device
                                    .sensor
                                    .as_ref()
                                    .map(|s| s.label.as_str())
                                    .unwrap_or("<unknown>")
                            ),
                        }
                    }
                    Err(err) => warn!("Skipping LCD[{}] media: {err}", cfg_key),
                }
            }
        }
    }

    fn refresh_targets(&mut self) {
        if self.media_assets.is_empty() {
            return;
        }

        let devices = match find_lcd_devices() {
            Ok(devs) => devs,
            Err(err) => {
                warn!("failed to enumerate LCD devices: {err}");
                return;
            }
        };

        // Build list of (Device, serial_number) for matching
        let mut device_info: Vec<(Device<GlobalContext>, String)> = Vec::new();
        for device in devices {
            let desc = match device.device_descriptor() {
                Ok(d) => d,
                Err(_) => continue,
            };
            let serial = device
                .open()
                .and_then(|h| h.read_serial_number_string_ascii(&desc))
                .unwrap_or_else(|_| {
                    format!("bus{}-addr{}", device.bus_number(), device.address())
                });
            device_info.push((device, serial));
        }

        let mut new_targets = HashMap::new();

        if let Some(cfg) = &self.config {
            for (cfg_idx, device_cfg) in cfg.lcds.iter().enumerate() {
                let asset = match self.media_assets.get(&cfg_idx) {
                    Some(asset) => asset,
                    None => {
                        if let Some(mut existing) = self.targets.remove(&cfg_idx) {
                            existing.stop();
                        }
                        continue;
                    }
                };

                // Find matching device by serial or index
                let matched_device = if let Some(serial) = &device_cfg.serial {
                    // Match by serial number (preferred)
                    device_info.iter().find(|(_, s)| s == serial).map(|(d, _)| d)
                } else if let Some(index) = device_cfg.index {
                    // Match by index (legacy)
                    device_info.get(index).map(|(d, _)| d)
                } else {
                    None
                };

                let device = match matched_device {
                    Some(dev) => dev.clone(),
                    None => {
                        if let Some(mut existing) = self.targets.remove(&cfg_idx) {
                            info!("[devices] LCD[{}] detached", device_cfg.device_id());
                            existing.stop();
                        }
                        continue;
                    }
                };

                let cfg_key = config_identity(device_cfg);
                if let Some(mut existing) = self.targets.remove(&cfg_idx) {
                    if existing.matches(&device, &cfg_key) {
                        new_targets.insert(cfg_idx, existing);
                        continue;
                    } else {
                        existing.stop();
                    }
                }

                match LcdDevice::new(device) {
                    Ok(lcd) => {
                        info!(
                            "[devices] LCD[{}] attached (serial: {}, orientation: {:.0}°)",
                            device_cfg.device_id(),
                            lcd.serial(),
                            device_cfg.orientation
                        );
                        let target = ActiveTarget::new(cfg_idx, cfg_key, lcd, asset);
                        new_targets.insert(cfg_idx, target);
                    }
                    Err(err) => {
                        warn!(
                            "[devices] LCD[{}] unavailable during attach: {err}",
                            device_cfg.device_id()
                        );
                    }
                }
            }
        }

        for (idx, mut target) in self.targets.drain() {
            if !new_targets.contains_key(&idx) {
                target.stop();
            }
        }

        self.targets = new_targets;
    }

    fn stream_targets(&mut self) {
        if self.targets.is_empty() {
            return;
        }

        let now = Instant::now();
        let ids: Vec<usize> = self.targets.keys().cloned().collect();
        for idx in ids {
            if let Some(target) = self.targets.get_mut(&idx) {
                if !target.should_send(now) {
                    continue;
                }

                match target.send_frame(&self.wireless, &mut self.packet_builder) {
                    Ok(true) => {
                        if target.frame_counter % 30 == 0 {
                            debug!(
                                "LCD[{}] streamed {} frames",
                                target.index, target.frame_counter
                            );
                        }
                    }
                    Ok(false) => {}
                    Err(SendError::Usb(err)) => {
                        self.handle_usb_error(idx, err);
                        break;
                    }
                    Err(SendError::Other(err)) => {
                        warn!("LCD[{}] media error: {err}", target.index);
                        let mut removed = self.targets.remove(&idx).unwrap();
                        removed.stop();
                        break;
                    }
                }
            }
        }
    }

    fn handle_usb_error(&mut self, index: usize, err: rusb::Error) {
        if let Some(mut target) = self.targets.remove(&index) {
            warn!("LCD[{}] USB error ({:?}): {err}", index, err);
            target.stop();
        }
        if matches!(err, rusb::Error::Timeout)
            && self.recover_wireless()
        {
            info!("Wireless link recovered");
        }
    }
}

struct ActiveTarget {
    index: usize,
    key: ConfigKey,
    lcd: LcdDevice,
    media: MediaRuntime,
    next_due: Option<Instant>,
    frame_counter: u64,
}

impl ActiveTarget {
    fn new(index: usize, key: ConfigKey, lcd: LcdDevice, asset: &MediaAsset) -> Self {
        Self {
            index,
            key,
            lcd,
            media: MediaRuntime::from_asset(asset),
            next_due: None,
            frame_counter: 0,
        }
    }

    fn matches(&self, device: &Device<GlobalContext>, key: &ConfigKey) -> bool {
        device.bus_number() == self.lcd.bus()
            && device.address() == self.lcd.address()
            && key == &self.key
    }

    fn should_send(&self, now: Instant) -> bool {
        match &self.media {
            MediaRuntime::Static { sent, .. } => !*sent,
            MediaRuntime::Video { .. } | MediaRuntime::Sensor { .. } => match self.next_due {
                Some(due) => now >= due,
                None => true,
            },
        }
    }

    fn send_frame(
        &mut self,
        wireless: &WirelessController,
        builder: &mut PacketBuilder,
    ) -> Result<bool, SendError> {
        let frame = match self.media.next_frame_bytes() {
            Some(bytes) => bytes,
            None => return Ok(false),
        };

        wireless.ensure_video_mode().map_err(SendError::Other)?;
        self.lcd
            .send_frame(builder, frame)
            .map_err(|err| match err.downcast::<rusb::Error>() {
                Ok(usb) => SendError::Usb(usb),
                Err(other) => SendError::Other(other),
            })?;

        self.media.advance_schedule(&mut self.next_due);
        self.frame_counter += 1;
        Ok(true)
    }

    fn stop(&mut self) {}
}

enum MediaRuntime {
    Static {
        frame: ArcFrame,
        sent: bool,
    },
    Video {
        frames: ArcFrames,
        durations: ArcDurations,
        cursor: usize,
        start: Option<Instant>,
        elapsed: Duration,
        last_duration: Duration,
    },
    Sensor {
        renderer: Arc<AsyncSensorRenderer>,
        cached_frame: Vec<u8>,
        next_frame_time: Instant,
    },
}

/// Asynchronously renders sensor frames in a background thread to avoid blocking video playback
struct AsyncSensorRenderer {
    asset: Arc<SensorAsset>,
    current_frame: Arc<Mutex<Vec<u8>>>,
    stop_flag: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl AsyncSensorRenderer {
    fn new(asset: Arc<SensorAsset>) -> Self {
        // Render initial frame
        let initial = match asset.render_frame() {
            Ok(frame) => frame,
            Err(err) => {
                warn!("sensor initial render failed: {err}");
                asset.blank_frame()
            }
        };

        let current_frame = Arc::new(Mutex::new(initial));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let update_interval = asset.update_interval();

        // Spawn background rendering thread
        let asset_clone = Arc::clone(&asset);
        let frame_clone = Arc::clone(&current_frame);
        let stop_clone = Arc::clone(&stop_flag);

        let thread = thread::spawn(move || {
            while !stop_clone.load(Ordering::Relaxed) {
                thread::sleep(update_interval);

                if stop_clone.load(Ordering::Relaxed) {
                    break;
                }

                // Render new frame in background
                match asset_clone.render_frame() {
                    Ok(new_frame) => {
                        *frame_clone.lock() = new_frame;
                    }
                    Err(err) => {
                        warn!("sensor background render failed: {err}");
                    }
                }
            }
        });

        Self {
            asset,
            current_frame,
            stop_flag,
            thread: Some(thread),
        }
    }

    fn get_frame(&self) -> Vec<u8> {
        self.current_frame.lock().clone()
    }

    fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for AsyncSensorRenderer {
    fn drop(&mut self) {
        self.stop();
    }
}

type ArcFrame = Arc<Vec<u8>>;
type ArcFrames = Arc<Vec<Vec<u8>>>;
type ArcDurations = Arc<Vec<Duration>>;

impl MediaRuntime {
    fn from_asset(asset: &MediaAsset) -> Self {
        match asset {
            MediaAsset::Static { frame } => Self::Static {
                frame: Arc::clone(frame),
                sent: false,
            },
            MediaAsset::Video {
                frames,
                frame_durations,
            } => Self::Video {
                frames: Arc::clone(frames),
                durations: Arc::clone(frame_durations),
                cursor: 0,
                start: None,
                elapsed: Duration::default(),
                last_duration: Duration::default(),
            },
            MediaAsset::Sensor { asset } => {
                let renderer = Arc::new(AsyncSensorRenderer::new(Arc::clone(asset)));
                let update_interval = asset.update_interval();
                let cached_frame = renderer.get_frame();
                Self::Sensor {
                    renderer,
                    cached_frame,
                    next_frame_time: Instant::now() + update_interval,
                }
            }
        }
    }

    fn next_frame_bytes(&mut self) -> Option<&[u8]> {
        match self {
            MediaRuntime::Static { frame, sent } => {
                if *sent {
                    None
                } else {
                    *sent = true;
                    Some(frame.as_slice())
                }
            }
            MediaRuntime::Video {
                frames,
                durations,
                cursor,
                last_duration,
                ..
            } => {
                if frames.is_empty() {
                    None
                } else {
                    let idx = *cursor % frames.len();
                    *cursor += 1;
                    let duration = durations
                        .get(idx)
                        .copied()
                        .unwrap_or_else(|| Duration::from_millis(33));
                    *last_duration = duration;
                    Some(frames[idx].as_slice())
                }
            }
            MediaRuntime::Sensor {
                renderer,
                cached_frame,
                next_frame_time,
                ..
            } => {
                let now = Instant::now();
                // Check if it's time to fetch the latest pre-rendered frame
                if now >= *next_frame_time {
                    *cached_frame = renderer.get_frame();
                    *next_frame_time = now + renderer.asset.update_interval();
                }
                Some(cached_frame.as_slice())
            }
        }
    }

    fn advance_schedule(&mut self, next_due: &mut Option<Instant>) {
        match self {
            MediaRuntime::Static { .. } => {
                *next_due = None;
            }
            MediaRuntime::Video {
                durations,
                cursor,
                start,
                elapsed,
                last_duration,
                ..
            } => {
                let base = start.get_or_insert_with(Instant::now);
                let frame_delay = (*last_duration).max(Duration::from_millis(10));
                *elapsed += frame_delay;
                *next_due = Some(*base + *elapsed);
                if !durations.is_empty() && *cursor % durations.len() == 0 {
                    *start = Some(Instant::now());
                    *elapsed = Duration::default();
                }
            }
            MediaRuntime::Sensor { next_frame_time, .. } => {
                *next_due = Some(*next_frame_time);
            }
        }
    }
}

enum SendError {
    Usb(rusb::Error),
    Other(anyhow::Error),
}
