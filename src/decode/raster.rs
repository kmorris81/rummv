//! Formats handled by the `image` crate, including animated GIF / WebP / APNG.
use super::{Frame, dynamic_to_color_image, rgba_to_color_image, still};
use image::{AnimationDecoder, ImageDecoder, ImageFormat as F, ImageReader};
use std::io::BufReader;
use std::path::Path;
use std::time::Duration;

type Out = (Vec<Frame>, Vec<(String, String)>);

pub fn decode(path: &Path) -> anyhow::Result<Out> {
    // Trust the content over the extension (misnamed .jpg that is really PNG etc.).
    let mut reader = ImageReader::open(path)?.with_guessed_format()?;
    reader.no_limits();
    let fmt = reader.format();
    match fmt {
        Some(F::Gif) => {
            let d =
                image::codecs::gif::GifDecoder::new(BufReader::new(std::fs::File::open(path)?))?;
            return animated(d.into_frames(), "GIF");
        }
        Some(F::WebP) => {
            let d =
                image::codecs::webp::WebPDecoder::new(BufReader::new(std::fs::File::open(path)?))?;
            if d.has_animation() {
                return animated(d.into_frames(), "WebP");
            }
        }
        Some(F::Png) => {
            let d =
                image::codecs::png::PngDecoder::new(BufReader::new(std::fs::File::open(path)?))?;
            if d.is_apng()? {
                return animated(d.apng()?.into_frames(), "APNG");
            }
        }
        _ => {}
    }
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    let info = fmt
        .map(|f| vec![("Format".to_string(), format!("{f:?}"))])
        .unwrap_or_default();
    Ok((vec![still(dynamic_to_color_image(img))], info))
}

fn animated(frames: image::Frames<'_>, name: &str) -> anyhow::Result<Out> {
    let mut out = Vec::new();
    for f in frames {
        let f = f?;
        let (num, den) = f.delay().numer_denom_ms();
        let ms = num.checked_div(den).unwrap_or(100);
        // Browsers clamp tiny delays (0/10ms) to 100ms; match that.
        let delay = Duration::from_millis(if ms <= 10 { 100 } else { ms as u64 });
        let buf = f.into_buffer();
        let (w, h) = buf.dimensions();
        out.push(Frame {
            image: rgba_to_color_image(w, h, buf.as_raw()),
            delay,
        });
    }
    if out.len() == 1 {
        out[0].delay = Duration::ZERO;
    }
    Ok((out, vec![("Format".into(), name.into())]))
}
