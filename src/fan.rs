use crate::config::{FanConfig, FanCurve, FanSpeed};
use crate::hardware::WirelessController;
use anyhow::{Context, Result};
use log::{debug, info, warn};
use std::collections::HashMap;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub struct FanController {
    config: FanConfig,
    curves: HashMap<String, FanCurve>,
    wireless: Arc<WirelessController>,
    stop_flag: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FanController {
    pub fn new(
        config: FanConfig,
        curves: Vec<FanCurve>,
        wireless: Arc<WirelessController>,
    ) -> Self {
        let curves_map: HashMap<String, FanCurve> =
            curves.into_iter().map(|c| (c.name.clone(), c)).collect();

        Self {
            config,
            curves: curves_map,
            wireless,
            stop_flag: Arc::new(AtomicBool::new(false)),
            thread: None,
        }
    }

    pub fn start(&mut self) {
        let config = self.config.clone();
        let curves = self.curves.clone();
        let wireless = Arc::clone(&self.wireless);
        let stop_flag = Arc::clone(&self.stop_flag);

        let thread = thread::spawn(move || {
            fan_control_thread(config, curves, wireless, stop_flag);
        });

        self.thread = Some(thread);
    }

    pub fn stop(self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread {
            let _ = thread.join();
        }
    }
}

fn fan_control_thread(
    config: FanConfig,
    curves: HashMap<String, FanCurve>,
    wireless: Arc<WirelessController>,
    stop_flag: Arc<AtomicBool>,
) {
    let update_interval = Duration::from_millis(config.update_interval_ms);
    let mut last_update = Instant::now() - update_interval;

    info!("Fan control thread started, waiting for device discovery...");

    // Wait for device discovery (max 10 seconds)
    let discovery_start = Instant::now();
    let mut discovery_complete = false;
    while !stop_flag.load(Ordering::Relaxed) && discovery_start.elapsed() < Duration::from_secs(10) {
        if wireless.has_discovered_devices() {
            let count = wireless.discovered_device_count();
            info!("Device discovery complete: found {} wireless device(s)", count);
            discovery_complete = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }

    if !discovery_complete {
        warn!("No wireless devices discovered after 10 seconds - fan control disabled");
        warn!("Ensure RX device (0416:8041) is connected for fan control to work");
        return;
    }

    info!("Starting fan speed control loop");

    while !stop_flag.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now.duration_since(last_update) < update_interval {
            thread::sleep(Duration::from_millis(100));
            continue;
        }
        last_update = now;

        // Calculate speeds for each group
        for (group_idx, group_config) in config.speeds.iter().enumerate() {
            let speeds = match calculate_fan_speeds(group_config, &curves) {
                Ok(speeds) => speeds,
                Err(err) => {
                    warn!("Fan speed calculation failed for group {}: {err}", group_idx);
                    continue;
                }
            };

            if let Err(err) = wireless.set_fan_speeds(group_idx as u8, &speeds) {
                warn!("Failed to set fan speeds for group {}: {err}", group_idx);
            }
        }

        thread::sleep(Duration::from_millis(100));
    }

    info!("Fan control thread stopped");
}

fn calculate_fan_speeds(
    fan_speeds: &[FanSpeed; 4],
    curves: &HashMap<String, FanCurve>,
) -> Result<[u8; 4]> {
    let mut pwm_values = [0u8; 4];

    for (i, fan_speed) in fan_speeds.iter().enumerate() {
        pwm_values[i] = match fan_speed {
            FanSpeed::Constant(value) => *value,
            FanSpeed::Curve(curve_name) => {
                let curve = curves
                    .get(curve_name)
                    .ok_or_else(|| anyhow::anyhow!("Curve '{}' not found", curve_name))?;

                let temp = read_temperature(&curve.temp_command)?;
                let speed_percent = interpolate_curve(&curve.curve, temp);
                let pwm = (speed_percent * 2.55) as u8;

                debug!(
                    "Fan {}: Temp {:.1}°C, Speed {:.0}%, PWM {}",
                    i, temp, speed_percent, pwm
                );

                pwm
            }
        };
    }

    Ok(pwm_values)
}

fn read_temperature(command: &str) -> Result<f32> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .context("executing temperature command")?;

    if !output.status.success() {
        anyhow::bail!(
            "temperature command failed with status {}",
            output.status
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let temp_str = stdout.split_whitespace().next().unwrap_or("0");
    let temp = temp_str
        .parse::<f32>()
        .with_context(|| format!("parsing temperature value '{temp_str}'"))?;

    if !temp.is_finite() {
        anyhow::bail!("temperature value '{temp}' is not finite");
    }

    Ok(temp)
}

fn interpolate_curve(curve: &[(f32, f32)], temp: f32) -> f32 {
    if curve.is_empty() {
        return 50.0; // Default to 50% if no curve defined
    }

    if curve.len() == 1 {
        return curve[0].1;
    }

    // Find the two points to interpolate between
    let mut sorted_curve = curve.to_vec();
    sorted_curve.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

    if temp <= sorted_curve[0].0 {
        return sorted_curve[0].1;
    }

    if temp >= sorted_curve[sorted_curve.len() - 1].0 {
        return sorted_curve[sorted_curve.len() - 1].1;
    }

    for i in 0..sorted_curve.len() - 1 {
        let (temp1, speed1) = sorted_curve[i];
        let (temp2, speed2) = sorted_curve[i + 1];

        if temp >= temp1 && temp <= temp2 {
            // Linear interpolation
            let ratio = (temp - temp1) / (temp2 - temp1);
            return speed1 + ratio * (speed2 - speed1);
        }
    }

    50.0 // Fallback
}
