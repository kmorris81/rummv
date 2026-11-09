//! Decoders for every supported still/animated image format.
//! All decoders produce straight-alpha RGBA8 frames, already EXIF-oriented.
mod heif;
mod jxl;
mod psd;
mod raster;
mod raw;
mod svg;

use crate::media::ImageFormat;
use std::path::Path;
use std::time::Duration;

pub struct Frame {
    pub image: egui::ColorImage,
    /// Display time for animations (zero for stills).
    pub delay: Duration,
}

pub struct Decoded {
    pub frames: Vec<Frame>,
    /// Original pixel size (before any downscale to fit the GPU texture limit).
    pub orig_size: [usize; 2],
    /// Human-readable details for the info overlay.
    pub info: Vec<(String, String)>,
}

impl Decoded {
    pub fn size(&self) -> [usize; 2] {
        self.frames.first().map(|f| f.image.size).unwrap_or([0, 0])
    }
}

/// Cap total RGBA bytes held for one animation (GPU memory).
const MAX_ANIM_BYTES: usize = 1 << 30;

pub fn decode(path: &Path, fmt: ImageFormat, max_side: usize) -> anyhow::Result<Decoded> {
    let (mut frames, mut info) = match fmt {
        ImageFormat::Raster => raster::decode(path)?,
        ImageFormat::Heif => (vec![still(heif::decode(path)?)], vec![]),
        ImageFormat::Jxl => (vec![still(jxl::decode(path)?)], vec![]),
        ImageFormat::Svg => (vec![still(svg::decode(path, max_side.min(4096))?)], vec![]),
        ImageFormat::Psd => (vec![still(psd::decode(path)?)], vec![]),
        ImageFormat::Raw => {
            let (img, how) = raw::decode(path)?;
            (vec![still(img)], vec![("RAW".to_string(), how.to_string())])
        }
    };
    anyhow::ensure!(!frames.is_empty(), "no frames decoded");
    let first = frames[0].image.size;
    let orig_size = first;

    // Bound memory for huge animations.
    let per = first[0] * first[1] * 4;
    if per > 0 && frames.len() * per > MAX_ANIM_BYTES {
        let keep = (MAX_ANIM_BYTES / per).max(1);
        info.push((
            "Note".into(),
            format!("animation truncated to {keep} of {} frames", frames.len()),
        ));
        frames.truncate(keep);
    }

    // Downscale anything larger than the GPU's max texture side.
    if first[0] > max_side || first[1] > max_side {
        for f in &mut frames {
            f.image = downscale(&f.image, max_side);
        }
        let s = frames[0].image.size;
        info.push((
            "Displayed at".into(),
            format!("{}×{} (GPU texture limit)", s[0], s[1]),
        ));
    }
    if frames.len() > 1 {
        let total: Duration = frames.iter().map(|f| f.delay).sum();
        info.push((
            "Animation".into(),
            format!("{} frames, {:.2}s", frames.len(), total.as_secs_f32()),
        ));
    }
    info.extend(crate::meta::exif_summary(path));
    Ok(Decoded {
        frames,
        orig_size,
        info,
    })
}

pub(crate) fn still(image: egui::ColorImage) -> Frame {
    Frame {
        image,
        delay: Duration::ZERO,
    }
}

pub(crate) fn rgba_to_color_image(w: u32, h: u32, rgba: &[u8]) -> egui::ColorImage {
    egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba)
}

pub(crate) fn dynamic_to_color_image(img: image::DynamicImage) -> egui::ColorImage {
    use image::DynamicImage as D;
    let rgba = match img {
        // HDR/float sources are linear; map to sRGB so they don't look dark.
        D::ImageRgb32F(_) | D::ImageRgba32F(_) => linear_float_to_srgb8(img.into_rgba32f()),
        other => other.into_rgba8(),
    };
    let (w, h) = rgba.dimensions();
    rgba_to_color_image(w, h, rgba.as_raw())
}

fn linear_float_to_srgb8(img: image::Rgba32FImage) -> image::RgbaImage {
    let (w, h) = img.dimensions();
    let enc = |v: f32| -> u8 {
        let v = v.clamp(0.0, 1.0);
        let s = if v <= 0.0031308 {
            v * 12.92
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        };
        (s * 255.0 + 0.5) as u8
    };
    let mut out = image::RgbaImage::new(w, h);
    for (o, i) in out.pixels_mut().zip(img.pixels()) {
        o.0 = [
            enc(i.0[0]),
            enc(i.0[1]),
            enc(i.0[2]),
            (i.0[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
        ];
    }
    out
}

fn downscale(img: &egui::ColorImage, max_side: usize) -> egui::ColorImage {
    let [w, h] = img.size;
    let scale = max_side as f64 / w.max(h) as f64;
    let nw = ((w as f64 * scale).round() as u32).max(1);
    let nh = ((h as f64 * scale).round() as u32).max(1);
    let src = image::RgbaImage::from_raw(w as u32, h as u32, img.as_raw().to_vec())
        .expect("ColorImage is RGBA8");
    let dst = image::imageops::resize(&src, nw, nh, image::imageops::FilterType::Triangle);
    rgba_to_color_image(nw, nh, dst.as_raw())
}

/// Decode a small thumbnail (longest side ≤ `side`) for any image format.
pub fn thumbnail(path: &Path, fmt: ImageFormat, side: u32) -> anyhow::Result<image::RgbaImage> {
    let img: image::DynamicImage = match fmt {
        ImageFormat::Raw => raw::thumbnail(path)?,
        _ => {
            let d = decode(path, fmt, 2048)?;
            let f = &d.frames[0].image;
            image::DynamicImage::ImageRgba8(
                image::RgbaImage::from_raw(f.size[0] as u32, f.size[1] as u32, f.as_raw().to_vec())
                    .expect("rgba"),
            )
        }
    };
    Ok(img.thumbnail(side, side).into_rgba8())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{self, MediaKind};
    use std::path::PathBuf;

    fn fixtures() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
    }

    /// (file, expected size, expected frame count)
    const CASES: &[(&str, [usize; 2], usize)] = &[
        ("a01.jpg", [640, 400], 1),
        ("a03.png", [300, 200], 1),
        ("a04.gif", [120, 80], 3),
        ("a05_anim.webp", [120, 80], 3),
        ("a06.webp", [640, 400], 1),
        ("a07.tiff", [640, 400], 1),
        ("a08.bmp", [640, 400], 1),
        ("a09.tga", [640, 400], 1),
        ("a10.exr", [640, 400], 1),
        ("a11.heic", [640, 400], 1),
        ("a12.avif", [640, 400], 1),
        ("a13.jxl", [300, 200], 1),
        ("a15.psd", [640, 400], 1),
        ("a18_anim.png", [300, 200], 3),
        ("c_misnamed.jpg", [300, 200], 1),
    ];

    #[test]
    fn decodes_every_fixture() {
        let dir = fixtures();
        let mut failures = Vec::new();
        for (name, size, frames) in CASES {
            let p = dir.join(name);
            let Some(MediaKind::Image(fmt)) = media::detect(&p) else {
                failures.push(format!("{name}: not detected as image"));
                continue;
            };
            match decode(&p, fmt, 16384) {
                Ok(d) => {
                    if d.size() != *size || d.frames.len() != *frames {
                        failures.push(format!(
                            "{name}: got {:?} x{} frames",
                            d.size(),
                            d.frames.len()
                        ));
                    }
                }
                Err(e) => failures.push(format!("{name}: {e:#}")),
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn svg_is_rasterized_large_and_capped() {
        let p = fixtures().join("a14.svg");
        let d = decode(&p, ImageFormat::Svg, 16384).unwrap();
        assert_eq!(d.size(), [2048, 1024]);
        let d = decode(&p, ImageFormat::Svg, 1000).unwrap();
        assert_eq!(d.size()[0], 1000);
    }

    #[test]
    fn downscales_to_texture_limit() {
        let d = decode(&fixtures().join("a01.jpg"), ImageFormat::Raster, 320).unwrap();
        assert_eq!(d.size(), [320, 200]);
        assert_eq!(d.orig_size, [640, 400]);
    }

    #[test]
    fn exif_orientation_applied() {
        let tmp = std::env::temp_dir().join(format!("mview-orient-{}.jpg", std::process::id()));
        std::fs::copy(fixtures().join("a01.jpg"), &tmp).unwrap();
        crate::actions::save_rotation(&tmp, 1).unwrap();
        let d = decode(&tmp, ImageFormat::Raster, 16384).unwrap();
        std::fs::remove_file(&tmp).unwrap();
        assert_eq!(d.size(), [400, 640]);
    }

    #[test]
    fn videos_and_sniffing() {
        let dir = fixtures();
        for v in ["b01.mp4", "b02.webm", "b03.mkv", "noext_video"] {
            assert_eq!(media::detect(&dir.join(v)), Some(MediaKind::Video), "{v}");
        }
        assert_eq!(media::detect(&dir.join("notes.txt")), None);
    }

    #[test]
    fn thumbnails() {
        let t = thumbnail(&fixtures().join("a11.heic"), ImageFormat::Heif, 256).unwrap();
        assert_eq!(t.dimensions(), (256, 160));
    }
}
