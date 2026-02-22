use super::common::{apply_orientation, encode_jpeg, MediaError, LCD_HEIGHT, LCD_WIDTH};
use image::imageops::FilterType;
use image::{ImageBuffer, Rgb};
use std::path::Path;

pub fn load_image_frame(path: &Path, orientation: f32) -> Result<Vec<u8>, MediaError> {
    let rgb = image::open(path)?.to_rgb8();
    let resized = image::imageops::resize(&rgb, LCD_WIDTH, LCD_HEIGHT, FilterType::Lanczos3);
    let oriented = apply_orientation(resized, orientation);
    encode_jpeg(&oriented)
}

pub fn build_color_frame(rgb: [u8; 3]) -> Vec<u8> {
    let image = ImageBuffer::from_pixel(LCD_WIDTH, LCD_HEIGHT, Rgb(rgb));
    encode_jpeg(&image).expect("encoding color frame should not fail")
}
