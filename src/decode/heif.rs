//! HEIC / HEIF / AVIF via libheif (rotation & mirroring from the file are applied).
use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};
use std::path::Path;

pub fn decode(path: &Path) -> anyhow::Result<egui::ColorImage> {
    let lib = LibHeif::new();
    let ctx = HeifContext::read_from_file(
        path.to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF8 path"))?,
    )?;
    let handle = ctx.primary_image_handle()?;
    let img = lib.decode(&handle, ColorSpace::Rgb(RgbChroma::Rgba), None)?;
    let planes = img.planes();
    let p = planes
        .interleaved
        .ok_or_else(|| anyhow::anyhow!("no interleaved plane"))?;
    let (w, h) = (p.width as usize, p.height as usize);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        rgba.extend_from_slice(&p.data[row * p.stride..row * p.stride + w * 4]);
    }
    Ok(super::rgba_to_color_image(w as u32, h as u32, &rgba))
}
