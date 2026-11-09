//! Photoshop PSD: flattened composite.
use std::path::Path;

pub fn decode(path: &Path) -> anyhow::Result<egui::ColorImage> {
    let bytes = std::fs::read(path)?;
    let psd = psd::Psd::from_bytes(&bytes).map_err(|e| anyhow::anyhow!("psd: {e}"))?;
    Ok(super::rgba_to_color_image(
        psd.width(),
        psd.height(),
        &psd.rgba(),
    ))
}
