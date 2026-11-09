//! JPEG XL via jxl-oxide (pure Rust).
use std::path::Path;

pub fn decode(path: &Path) -> anyhow::Result<egui::ColorImage> {
    let file = std::io::BufReader::new(std::fs::File::open(path)?);
    let decoder = jxl_oxide::integration::JxlDecoder::new(file)?;
    let img = image::DynamicImage::from_decoder(decoder)?;
    Ok(super::dynamic_to_color_image(img))
}
