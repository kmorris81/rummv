//! Apple icon image (.icns): show the largest icon in the family that decodes.
use std::path::Path;

pub fn decode(path: &Path) -> anyhow::Result<egui::ColorImage> {
    let file = std::io::BufReader::new(std::fs::File::open(path)?);
    let family = icns::IconFamily::read(file)?;
    let mut types = family.available_icons();
    types.sort_by_key(|t| std::cmp::Reverse(t.pixel_width() * t.pixel_height()));
    let mut last_err = None;
    for t in types {
        match family.get_icon_with_type(t) {
            Ok(img) => {
                let rgba = img.convert_to(icns::PixelFormat::RGBA);
                return Ok(super::rgba_to_color_image(
                    rgba.width(),
                    rgba.height(),
                    rgba.data(),
                ));
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(match last_err {
        Some(e) => anyhow::anyhow!("icns: {e}"),
        None => anyhow::anyhow!("icns: no icons in file"),
    })
}
