//! Camera RAW via rawler: prefer the embedded full-size JPEG preview (fast, camera-rendered
//! colours), fall back to a full demosaic/develop.
use rawler::decoders::RawDecodeParams;
use std::path::Path;

pub fn decode(path: &Path) -> anyhow::Result<(egui::ColorImage, &'static str)> {
    let params = RawDecodeParams::default();
    if let Ok(img) = rawler::analyze::extract_preview_pixels(path, &params) {
        // Ignore tiny embedded thumbnails; those aren't good enough as a main view.
        if img.width().max(img.height()) >= 1000 {
            return Ok((super::dynamic_to_color_image(img), "embedded preview"));
        }
    }
    let img = rawler::analyze::extract_full_pixels(path, &params)?;
    Ok((super::dynamic_to_color_image(img), "developed"))
}

pub fn thumbnail(path: &Path) -> anyhow::Result<image::DynamicImage> {
    let params = RawDecodeParams::default();
    rawler::analyze::extract_thumbnail_pixels(path, &params)
        .or_else(|_| rawler::analyze::extract_preview_pixels(path, &params))
        .map_err(Into::into)
}
