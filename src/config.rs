use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::to_string;
use std::collections::HashSet;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    Image,
    Video,
    Color,
    Gif,
    Sensor,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DeviceConfig {
    /// Device index (legacy, less reliable). Use `serial` instead for stable identification.
    #[serde(default)]
    pub index: Option<usize>,
    /// Device serial number (preferred). Use this for stable device identification.
    pub serial: Option<String>,
    #[serde(rename = "type")]
    pub media_type: MediaType,
    pub path: Option<PathBuf>,
    pub fps: Option<f32>,
    pub rgb: Option<[u8; 3]>,
    #[serde(default)]
    pub orientation: f32,
    #[serde(default)]
    pub sensor: Option<SensorDescriptor>,
}

impl DeviceConfig {
    pub fn device_id(&self) -> String {
        if let Some(serial) = &self.serial {
            format!("serial:{}", serial)
        } else if let Some(index) = self.index {
            format!("index:{}", index)
        } else {
            "unknown".to_string()
        }
    }

    pub fn validate(&self) -> Result<()> {
        // Must have either index or serial
        if self.index.is_none() && self.serial.is_none() {
            bail!("device config requires either 'index' or 'serial' field");
        }

        let device_id = self.device_id();

        match self.media_type {
            MediaType::Image | MediaType::Video | MediaType::Gif => {
                let path = self
                    .path
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("LCD[{device_id}] requires a media path"))?;
                if !path.exists() {
                    bail!(
                        "LCD[{device_id}] media path '{}' does not exist",
                        path.display()
                    );
                }
            }
            MediaType::Color => {
                if self.rgb.is_none() {
                    bail!("LCD[{device_id}] color entry requires an 'rgb' field");
                }
            }
            MediaType::Sensor => {
                let descriptor = self.sensor.as_ref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "LCD[{device_id}] sensor configuration missing 'sensor' section"
                    )
                })?;
                descriptor.validate()?;
            }
        }

        if let Some(fps) = self.fps {
            if fps <= 0.0 {
                bail!("LCD[{device_id}] fps must be positive");
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SensorRange {
    pub max: Option<f32>,
    pub color: [u8; 3],
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SensorSourceConfig {
    Constant { value: f32 },
    Command { cmd: String },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SensorDescriptor {
    pub label: String,
    pub unit: String,
    pub source: SensorSourceConfig,
    #[serde(default = "SensorDescriptor::default_text_color")]
    pub text_color: [u8; 3],
    #[serde(default = "SensorDescriptor::default_background_color")]
    pub background_color: [u8; 3],
    #[serde(default = "SensorDescriptor::default_gauge_background")]
    pub gauge_background_color: [u8; 3],
    #[serde(default = "SensorDescriptor::default_ranges")]
    pub gauge_ranges: Vec<SensorRange>,
    #[serde(default = "SensorDescriptor::default_update_ms")]
    pub update_interval_ms: u64,
    #[serde(default = "SensorDescriptor::default_gauge_start_angle")]
    pub gauge_start_angle: f32,
    #[serde(default = "SensorDescriptor::default_gauge_sweep_angle")]
    pub gauge_sweep_angle: f32,
    #[serde(default = "SensorDescriptor::default_gauge_outer_radius")]
    pub gauge_outer_radius: f32,
    #[serde(default = "SensorDescriptor::default_gauge_thickness")]
    pub gauge_thickness: f32,
    #[serde(default = "SensorDescriptor::default_bar_corner_radius")]
    pub bar_corner_radius: f32,
    #[serde(default = "SensorDescriptor::default_value_font_size")]
    pub value_font_size: f32,
    #[serde(default = "SensorDescriptor::default_unit_font_size")]
    pub unit_font_size: f32,
    #[serde(default = "SensorDescriptor::default_label_font_size")]
    pub label_font_size: f32,
    pub font_path: Option<PathBuf>,
    #[serde(default = "SensorDescriptor::default_decimal_places")]
    pub decimal_places: u8,
    #[serde(default = "SensorDescriptor::default_value_offset")]
    pub value_offset: i32,
    #[serde(default = "SensorDescriptor::default_unit_offset")]
    pub unit_offset: i32,
    #[serde(default = "SensorDescriptor::default_label_offset")]
    pub label_offset: i32,
}

impl SensorDescriptor {
    fn default_text_color() -> [u8; 3] {
        [255, 255, 255]
    }

    fn default_background_color() -> [u8; 3] {
        [0, 0, 0]
    }

    fn default_gauge_background() -> [u8; 3] {
        [60, 60, 60]
    }

    fn default_ranges() -> Vec<SensorRange> {
        vec![
            SensorRange {
                max: Some(50.0),
                color: [0, 200, 0],
            },
            SensorRange {
                max: Some(80.0),
                color: [220, 140, 0],
            },
            SensorRange {
                max: None,
                color: [220, 0, 0],
            },
        ]
    }

    fn default_update_ms() -> u64 {
        1_000
    }

    fn default_gauge_start_angle() -> f32 {
        90.0
    }

    fn default_gauge_sweep_angle() -> f32 {
        330.0
    }

    fn default_gauge_outer_radius() -> f32 {
        180.0
    }

    fn default_gauge_thickness() -> f32 {
        40.0
    }

    fn default_bar_corner_radius() -> f32 {
        0.0
    }

    fn default_value_font_size() -> f32 {
        72.0
    }

    fn default_unit_font_size() -> f32 {
        32.0
    }

    fn default_label_font_size() -> f32 {
        28.0
    }

    fn default_decimal_places() -> u8 {
        0
    }

    fn default_value_offset() -> i32 {
        0
    }

    fn default_unit_offset() -> i32 {
        60
    }

    fn default_label_offset() -> i32 {
        -60
    }

    pub fn validate(&self) -> Result<()> {
        match &self.source {
            SensorSourceConfig::Constant { value } => {
                if !value.is_finite() {
                    bail!("sensor constant value must be finite");
                }
                if *value < 0.0 || *value > 100.0 {
                    bail!("sensor constant value must be between 0 and 100");
                }
            }
            SensorSourceConfig::Command { cmd } => {
                if cmd.trim().is_empty() {
                    bail!("sensor command must not be empty");
                }
            }
        }

        if self.update_interval_ms == 0 {
            bail!("sensor update_interval_ms must be greater than zero");
        }

        if self.gauge_sweep_angle <= 0.0 || self.gauge_sweep_angle > 360.0 {
            bail!("sensor gauge_sweep_angle must be within (0, 360] degree range");
        }

        if self.gauge_thickness <= 0.0 {
            bail!("sensor gauge_thickness must be positive");
        }

        if self.gauge_outer_radius <= self.gauge_thickness + 5.0 {
            bail!("sensor gauge_outer_radius must exceed gauge_thickness by at least 5");
        }

        if self.value_font_size <= 0.0 || self.unit_font_size <= 0.0 || self.label_font_size <= 0.0
        {
            bail!("sensor font sizes must be greater than zero");
        }

        if self.bar_corner_radius < 0.0 {
            bail!("sensor bar_corner_radius must be non-negative");
        }

        if self.decimal_places > 10 {
            bail!("sensor decimal_places must be 10 or less");
        }

        if let Some(path) = &self.font_path {
            if !path.exists() {
                bail!("sensor font_path '{}' does not exist", path.display());
            }
        }

        let mut last_max = -f32::INFINITY;
        for range in &self.gauge_ranges {
            if let Some(max) = range.max {
                if max < last_max {
                    bail!("sensor gauge ranges must be sorted by max value");
                }
                if !(0.0..=100.0).contains(&max) {
                    bail!("sensor gauge range max must be between 0 and 100");
                }
            }
            last_max = range.max.unwrap_or(100.0);
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FanCurve {
    /// Curve name for reference
    pub name: String,
    /// Command to get temperature value
    pub temp_command: String,
    /// Fan curve points: [(temp_celsius, speed_percent)]
    pub curve: Vec<(f32, f32)>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum FanSpeed {
    /// Constant speed (0-255)
    Constant(u8),
    /// Reference to a named curve
    Curve(String),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FanConfig {
    /// Speed configuration for each group
    /// Each group is an array of 4 fan speeds (max 4 fans per group)
    /// Supports flat array for single group (backward compatibility) or array of arrays for multiple groups
    #[serde(deserialize_with = "deserialize_fan_speeds")]
    pub speeds: Vec<[FanSpeed; 4]>,
    /// Update interval in milliseconds (for curve-based fans)
    #[serde(default = "FanConfig::default_update_interval")]
    pub update_interval_ms: u64,
}

impl FanConfig {
    fn default_update_interval() -> u64 {
        1000
    }
}

/// Custom deserializer to support both flat array (backward compat) and array of arrays
fn deserialize_fan_speeds<'de, D>(deserializer: D) -> Result<Vec<[FanSpeed; 4]>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    use std::fmt;

    struct FanSpeedsVisitor;

    impl<'de> Visitor<'de> for FanSpeedsVisitor {
        type Value = Vec<[FanSpeed; 4]>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("an array of 4 fan speeds or an array of arrays")
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut result = Vec::new();

            // Try to read the first element to determine format
            if let Some(first) = seq.next_element::<serde_json::Value>()? {
                if first.is_array() {
                    // Array of arrays format - multiple groups
                    let first_group: [FanSpeed; 4] = serde_json::from_value(first)
                        .map_err(|e| de::Error::custom(format!("Invalid fan speed array: {}", e)))?;
                    result.push(first_group);

                    while let Some(group) = seq.next_element::<[FanSpeed; 4]>()? {
                        result.push(group);
                    }
                } else {
                    // Flat array format - single group (backward compatibility)
                    let mut speeds = vec![first];
                    while let Some(val) = seq.next_element::<serde_json::Value>()? {
                        speeds.push(val);
                    }

                    if speeds.len() != 4 {
                        return Err(de::Error::custom(format!(
                            "Expected 4 fan speeds, got {}",
                            speeds.len()
                        )));
                    }

                    let group: [FanSpeed; 4] = speeds
                        .into_iter()
                        .map(|v| serde_json::from_value(v))
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|e| de::Error::custom(format!("Invalid fan speed: {}", e)))?
                        .try_into()
                        .map_err(|_| de::Error::custom("Failed to convert to array"))?;

                    result.push(group);
                }
            }

            Ok(result)
        }
    }

    deserializer.deserialize_seq(FanSpeedsVisitor)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppConfig {
    #[serde(default = "AppConfig::default_fps")]
    pub default_fps: f32,
    #[serde(alias = "devices")] // Backward compatibility
    pub lcds: Vec<DeviceConfig>,
    #[serde(default)]
    pub fan_curves: Vec<FanCurve>,
    #[serde(default)]
    pub fans: Option<FanConfig>,
}

impl AppConfig {
    fn default_fps() -> f32 {
        30.0
    }

    pub fn load(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let reader = BufReader::new(file);
        let mut cfg: AppConfig = serde_json::from_reader(reader)
            .with_context(|| format!("parsing {}", path.display()))?;

        if cfg.lcds.is_empty() {
            bail!("configuration must contain at least one LCD entry");
        }

        let base_dir = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        // Check for duplicate identifiers (serial or index)
        let mut seen_identifiers = HashSet::new();
        for device in &mut cfg.lcds {
            let identifier = if let Some(serial) = &device.serial {
                format!("serial:{}", serial)
            } else if let Some(index) = device.index {
                format!("index:{}", index)
            } else {
                // Will be caught by validate() later
                continue;
            };

            if !seen_identifiers.insert(identifier.clone()) {
                bail!(
                    "duplicate device identifier '{}' in configuration",
                    identifier
                );
            }

            if let Some(existing) = &device.path {
                if existing.is_relative() {
                    device.path = Some(base_dir.join(existing));
                }
            }

            if let Some(sensor) = &mut device.sensor {
                if let Some(font_path) = &sensor.font_path {
                    if font_path.is_relative() {
                        sensor.font_path = Some(base_dir.join(font_path));
                    }
                }
            }

            device.validate()?;
        }

        if cfg.default_fps <= 0.0 {
            bail!("default_fps must be greater than zero");
        }

        for device in &mut cfg.lcds {
            let normalized = (device.orientation % 360.0 + 360.0) % 360.0;
            let snapped = ((normalized + 45.0) / 90.0).floor() * 90.0;
            device.orientation = snapped % 360.0;
        }

        Ok(cfg)
    }
}

pub type ConfigKey = String;

pub fn config_identity(cfg: &DeviceConfig) -> ConfigKey {
    to_string(cfg).unwrap_or_else(|_| cfg.device_id())
}
