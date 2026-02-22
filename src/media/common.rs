use image::imageops::{rotate180, rotate270, rotate90};
use image::RgbImage;
use thiserror::Error;

pub const LCD_WIDTH: u32 = 400;
pub const LCD_HEIGHT: u32 = 400;
pub const JPEG_QUALITY: u8 = 90;
pub const MAX_PAYLOAD: usize = 102_400 - 512;

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("ffmpeg failed: {0}")]
    Ffmpeg(String),
    #[error("generated frame ({size} bytes) exceeds LCD payload limit")]
    PayloadTooLarge { size: usize },
    #[error("video or animation produced no frames")]
    EmptyVideo,
    #[error("invalid fps value")]
    InvalidFps,
    #[error("sensor error: {0}")]
    Sensor(String),
}

pub fn encode_jpeg(image: &RgbImage) -> Result<Vec<u8>, MediaError> {
    let mut buf = Vec::new();
    {
        let mut encoder =
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, JPEG_QUALITY);
        encoder.encode_image(image)?;
    }
    if buf.len() > MAX_PAYLOAD {
        return Err(MediaError::PayloadTooLarge { size: buf.len() });
    }
    Ok(buf)
}

pub fn apply_orientation(image: RgbImage, orientation: f32) -> RgbImage {
    let norm = ((orientation % 360.0) + 360.0) % 360.0;
    if (norm - 0.0).abs() < 0.5 || (norm - 360.0).abs() < 0.5 {
        image
    } else if (norm - 90.0).abs() < 0.5 {
        rotate90(&image)
    } else if (norm - 180.0).abs() < 0.5 {
        rotate180(&image)
    } else if (norm - 270.0).abs() < 0.5 {
        rotate270(&image)
    } else {
        let nearest = ((norm + 45.0) / 90.0).floor() as i32 & 3;
        match nearest {
            1 => rotate90(&image),
            2 => rotate180(&image),
            3 => rotate270(&image),
            _ => image,
        }
    }
}
