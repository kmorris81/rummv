//! SVG / SVGZ via resvg, rasterized large enough to stay sharp when zoomed.
use std::path::Path;

pub fn decode(path: &Path, max_side: usize) -> anyhow::Result<egui::ColorImage> {
    let data = std::fs::read(path)?;
    let mut opt = resvg::usvg::Options {
        resources_dir: path.parent().map(Path::to_path_buf),
        ..Default::default()
    };
    opt.fontdb_mut().load_system_fonts();
    let tree = resvg::usvg::Tree::from_data(&data, &opt)?;
    let size = tree.size();
    // Rasterize so the long side is at least 2048px (icons) but within the texture limit.
    let long = size.width().max(size.height()).max(1.0);
    let target = (long.max(2048.0)).min(max_side as f32);
    let scale = target / long;
    let w = ((size.width() * scale).ceil() as u32).max(1);
    let h = ((size.height() * scale).ceil() as u32).max(1);
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(w, h).ok_or_else(|| anyhow::anyhow!("pixmap {w}x{h}"))?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia is premultiplied.
    Ok(egui::ColorImage::from_rgba_premultiplied(
        [w as usize, h as usize],
        pixmap.data(),
    ))
}
