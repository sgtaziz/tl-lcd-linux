pub mod common;
pub mod image;
pub mod sensor;
pub mod video;

pub use common::{MediaError, MAX_PAYLOAD};
pub use sensor::SensorAsset;

use crate::config::{DeviceConfig, MediaType};
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;

pub enum MediaAsset {
    Static {
        frame: Arc<Vec<u8>>,
    },
    Video {
        frames: Arc<Vec<Vec<u8>>>,
        frame_durations: Arc<Vec<Duration>>,
    },
    Sensor {
        asset: Arc<SensorAsset>,
    },
}

pub fn prepare_media_asset(cfg: &DeviceConfig, default_fps: f32) -> Result<MediaAsset, MediaError> {
    match cfg.media_type {
        MediaType::Image => {
            let path = cfg.path.as_ref().expect("validated path");
            let frame = image::load_image_frame(path, cfg.orientation)?;
            Ok(MediaAsset::Static {
                frame: Arc::new(frame),
            })
        }
        MediaType::Color => {
            let rgb = cfg.rgb.expect("validated rgb");
            let frame = image::build_color_frame(rgb);
            Ok(MediaAsset::Static {
                frame: Arc::new(frame),
            })
        }
        MediaType::Video => {
            let desired_fps = cfg.fps.unwrap_or(default_fps);
            if desired_fps <= 0.0 {
                return Err(MediaError::InvalidFps);
            }
            let path = cfg.path.as_ref().expect("validated path");
            let (frames, durations) =
                video::build_video_frames(path, desired_fps, cfg.orientation)?;
            Ok(MediaAsset::Video {
                frames: Arc::new(frames),
                frame_durations: Arc::new(durations),
            })
        }
        MediaType::Gif => {
            let path = cfg.path.as_ref().expect("validated path");
            let (frames, durations) = video::build_gif_frames(path, cfg.orientation)?;
            Ok(MediaAsset::Video {
                frames: Arc::new(frames),
                frame_durations: Arc::new(durations),
            })
        }
        MediaType::Sensor => {
            let descriptor = cfg.sensor.as_ref().expect("validated sensor config");
            let asset = SensorAsset::new(descriptor, cfg.orientation)?;
            Ok(MediaAsset::Sensor { asset })
        }
    }
}
