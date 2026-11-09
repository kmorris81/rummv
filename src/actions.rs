//! File actions. Anything that modifies or deletes a file is only ever invoked after the
//! user confirms it in the UI (see `app.rs`, `Confirm`).
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Command, Stdio};

pub fn trash(path: &Path) -> anyhow::Result<()> {
    trash::delete(path).map_err(|e| anyhow::anyhow!("move to trash: {e}"))
}

pub fn delete_permanently(path: &Path) -> anyhow::Result<()> {
    std::fs::remove_file(path).map_err(|e| anyhow::anyhow!("delete: {e}"))
}

/// Reveal the file in the file manager (Dolphin implements org.freedesktop.FileManager1).
pub fn show_in_file_manager(path: &Path) -> anyhow::Result<()> {
    let uri = url::Url::from_file_path(path)
        .map_err(|_| anyhow::anyhow!("bad path"))?
        .to_string();
    let ok = Command::new("dbus-send")
        .args([
            "--session",
            "--dest=org.freedesktop.FileManager1",
            "--type=method_call",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1.ShowItems",
            &format!("array:string:{uri}"),
            "string:",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        let dir = path.parent().unwrap_or(path);
        Command::new("xdg-open").arg(dir).spawn()?;
    }
    Ok(())
}

pub fn open_with_default(path: &Path) -> anyhow::Result<()> {
    Command::new("xdg-open")
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Rotation saving

/// How a rotation would be persisted; shown to the user in the confirmation dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SavePlan {
    /// JPEG: only the EXIF Orientation tag is changed (no recompression).
    JpegExif,
    /// Re-encode pixels with the image crate in the same format.
    Reencode(image::ImageFormat),
    Unsupported(String),
}

impl SavePlan {
    pub fn describe(&self) -> String {
        match self {
            SavePlan::JpegExif => {
                "Only the EXIF orientation tag will be changed. The image data is not re-encoded."
                    .into()
            }
            SavePlan::Reencode(f) => format!(
                "The image will be decoded, rotated and RE-ENCODED as {f:?}. Embedded metadata may be lost{}.",
                if *f == image::ImageFormat::WebP {
                    ", and lossy WebP becomes lossless (larger file)"
                } else {
                    ""
                }
            ),
            SavePlan::Unsupported(why) => why.clone(),
        }
    }
}

pub fn plan_rotation_save(path: &Path) -> SavePlan {
    let fmt = image::ImageReader::open(path)
        .ok()
        .and_then(|r| r.with_guessed_format().ok())
        .and_then(|r| r.format());
    use image::ImageFormat as F;
    match fmt {
        Some(F::Jpeg) => SavePlan::JpegExif,
        Some(f @ (F::Png | F::Bmp | F::Tiff | F::Tga | F::Qoi | F::Pnm | F::WebP)) => {
            if is_animated(path, f) {
                SavePlan::Unsupported("Saving rotation of animated images isn't supported.".into())
            } else {
                SavePlan::Reencode(f)
            }
        }
        Some(F::Gif) => SavePlan::Unsupported("Saving rotation of GIFs isn't supported.".into()),
        _ => SavePlan::Unsupported(
            "Saving rotation isn't supported for this format (HEIC/AVIF/JXL/RAW/PSD/SVG/EXR...)."
                .into(),
        ),
    }
}

fn is_animated(path: &Path, f: image::ImageFormat) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let r = std::io::BufReader::new(file);
    match f {
        image::ImageFormat::Png => image::codecs::png::PngDecoder::new(r)
            .and_then(|d| d.is_apng())
            .unwrap_or(false),
        image::ImageFormat::WebP => image::codecs::webp::WebPDecoder::new(r)
            .map(|d| d.has_animation())
            .unwrap_or(false),
        _ => false,
    }
}

/// Persist `quarter_turns_cw` (1..=3) of clockwise rotation. Only call after confirmation.
pub fn save_rotation(path: &Path, quarter_turns_cw: u8) -> anyhow::Result<()> {
    let q = quarter_turns_cw % 4;
    if q == 0 {
        return Ok(());
    }
    match plan_rotation_save(path) {
        SavePlan::JpegExif => jpeg_set_orientation(path, q),
        SavePlan::Reencode(fmt) => reencode_rotated(path, fmt, q),
        SavePlan::Unsupported(why) => anyhow::bail!(why),
    }
}

fn reencode_rotated(path: &Path, fmt: image::ImageFormat, q: u8) -> anyhow::Result<()> {
    use image::ImageDecoder;
    let mut reader = image::ImageReader::open(path)?.with_guessed_format()?;
    reader.no_limits();
    let mut dec = reader.into_decoder()?;
    let orient = dec
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(dec)?;
    img.apply_orientation(orient);
    let img = match q {
        1 => img.rotate90(),
        2 => img.rotate180(),
        _ => img.rotate270(),
    };
    let tmp = tmp_sibling(path);
    let res = img.save_with_format(&tmp, fmt);
    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    replace_preserving_perms(&tmp, path)
}

fn tmp_sibling(path: &Path) -> std::path::PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.rummv-{}.tmp", std::process::id()))
}

fn replace_preserving_perms(tmp: &Path, dst: &Path) -> anyhow::Result<()> {
    if let Ok(meta) = std::fs::metadata(dst) {
        let _ = std::fs::set_permissions(tmp, meta.permissions());
    }
    std::fs::rename(tmp, dst)?;
    Ok(())
}

// ---- EXIF orientation algebra -------------------------------------------------------
// Represent an orientation as "optionally mirror horizontally, then rotate r×90° CW".
fn orient_to_fr(o: u16) -> (bool, u8) {
    match o {
        2 => (true, 0),
        3 => (false, 2),
        4 => (true, 2),
        5 => (true, 3),
        6 => (false, 1),
        7 => (true, 1),
        8 => (false, 3),
        _ => (false, 0),
    }
}
fn fr_to_orient(f: bool, r: u8) -> u16 {
    match (f, r % 4) {
        (false, 0) => 1,
        (true, 0) => 2,
        (false, 2) => 3,
        (true, 2) => 4,
        (true, 3) => 5,
        (false, 1) => 6,
        (true, 1) => 7,
        (false, 3) => 8,
        _ => unreachable!(),
    }
}
pub fn compose_orientation(current: u16, quarter_turns_cw: u8) -> u16 {
    let (f, r) = orient_to_fr(current);
    fr_to_orient(f, r + quarter_turns_cw)
}

/// Update (or add) the EXIF Orientation of a JPEG without touching image data.
fn jpeg_set_orientation(path: &Path, q: u8) -> anyhow::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let mut data = Vec::new();
    f.read_to_end(&mut data)?;
    match find_jpeg_orientation(&data) {
        Some(loc) => {
            let cur = if loc.big_endian {
                u16::from_be_bytes([data[loc.offset], data[loc.offset + 1]])
            } else {
                u16::from_le_bytes([data[loc.offset], data[loc.offset + 1]])
            };
            let new = compose_orientation(cur, q);
            let bytes = if loc.big_endian {
                new.to_be_bytes()
            } else {
                new.to_le_bytes()
            };
            // In-place 2-byte patch: the file size and all other bytes stay identical.
            f.seek(SeekFrom::Start(loc.offset as u64))?;
            f.write_all(&bytes)?;
            f.sync_all()?;
            Ok(())
        }
        None => {
            if has_exif_app1(&data) {
                // EXIF exists but has no Orientation tag; inserting a tag into an existing IFD
                // would mean rewriting offsets. Re-encode would lose quality, so refuse.
                anyhow::bail!(
                    "This JPEG's EXIF block has no orientation tag; can't rotate losslessly."
                );
            }
            // No EXIF at all: insert a minimal APP1 segment right after SOI.
            let app1 = minimal_exif_app1(compose_orientation(1, q));
            let mut out = Vec::with_capacity(data.len() + app1.len());
            out.extend_from_slice(&data[..2]);
            out.extend_from_slice(&app1);
            out.extend_from_slice(&data[2..]);
            drop(f);
            let tmp = tmp_sibling(path);
            std::fs::write(&tmp, &out)?;
            replace_preserving_perms(&tmp, path)
        }
    }
}

struct OrientLoc {
    offset: usize,
    big_endian: bool,
}

fn jpeg_segments(data: &[u8]) -> impl Iterator<Item = (u8, usize, usize)> + '_ {
    // yields (marker, payload_start, payload_len) up to SOS
    let mut i = 2usize;
    std::iter::from_fn(move || {
        if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
            return None;
        }
        while i + 4 <= data.len() {
            if data[i] != 0xFF {
                return None;
            }
            let marker = data[i + 1];
            if marker == 0xFF {
                i += 1;
                continue;
            }
            if marker == 0xDA || marker == 0xD9 {
                return None;
            }
            let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
            if len < 2 || i + 2 + len > data.len() {
                return None;
            }
            let start = i + 4;
            i += 2 + len;
            return Some((marker, start, len - 2));
        }
        None
    })
}

fn has_exif_app1(data: &[u8]) -> bool {
    jpeg_segments(data).any(|(m, s, l)| m == 0xE1 && l >= 6 && &data[s..s + 6] == b"Exif\0\0")
}

fn find_jpeg_orientation(data: &[u8]) -> Option<OrientLoc> {
    let (_, s, l) = jpeg_segments(data)
        .find(|&(m, s, l)| m == 0xE1 && l >= 14 && &data[s..s + 6] == b"Exif\0\0")?;
    let tiff = s + 6;
    let end = s + l;
    let be = match &data[tiff..tiff + 2] {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let rd16 = |o: usize| -> Option<u16> {
        let b = data.get(o..o + 2)?;
        Some(if be {
            u16::from_be_bytes([b[0], b[1]])
        } else {
            u16::from_le_bytes([b[0], b[1]])
        })
    };
    let rd32 = |o: usize| -> Option<u32> {
        let b = data.get(o..o + 4)?;
        Some(if be {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        })
    };
    let ifd0 = tiff + rd32(tiff + 4)? as usize;
    let n = rd16(ifd0)? as usize;
    for k in 0..n {
        let e = ifd0 + 2 + k * 12;
        if e + 12 > end {
            return None;
        }
        if rd16(e)? == 0x0112 && rd16(e + 2)? == 3 {
            return Some(OrientLoc {
                offset: e + 8,
                big_endian: be,
            });
        }
    }
    None
}

fn minimal_exif_app1(orientation: u16) -> Vec<u8> {
    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"MM\0\x2A");
    tiff.extend_from_slice(&8u32.to_be_bytes()); // IFD0 offset
    tiff.extend_from_slice(&1u16.to_be_bytes()); // 1 entry
    tiff.extend_from_slice(&0x0112u16.to_be_bytes());
    tiff.extend_from_slice(&3u16.to_be_bytes()); // SHORT
    tiff.extend_from_slice(&1u32.to_be_bytes()); // count
    tiff.extend_from_slice(&orientation.to_be_bytes());
    tiff.extend_from_slice(&[0, 0]);
    tiff.extend_from_slice(&0u32.to_be_bytes()); // no next IFD
    let mut seg = vec![0xFF, 0xE1];
    let len = (2 + 6 + tiff.len()) as u16;
    seg.extend_from_slice(&len.to_be_bytes());
    seg.extend_from_slice(b"Exif\0\0");
    seg.extend_from_slice(&tiff);
    seg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_algebra() {
        assert_eq!(compose_orientation(1, 1), 6);
        assert_eq!(compose_orientation(6, 1), 3);
        assert_eq!(compose_orientation(3, 1), 8);
        assert_eq!(compose_orientation(8, 1), 1);
        assert_eq!(compose_orientation(2, 2), 4);
        for o in 1..=8 {
            assert_eq!(compose_orientation(o, 4), o);
            assert_eq!(compose_orientation(compose_orientation(o, 1), 3), o);
        }
    }

    #[test]
    fn jpeg_exif_roundtrip() {
        let dir = std::env::temp_dir().join(format!("rummv-rot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.jpg");
        let img = image::RgbImage::from_fn(8, 4, |x, _| image::Rgb([x as u8 * 30, 0, 0]));
        img.save(&p).unwrap();
        let before = std::fs::read(&p).unwrap();
        assert!(find_jpeg_orientation(&before).is_none());
        save_rotation(&p, 1).unwrap(); // inserts APP1
        let d = std::fs::read(&p).unwrap();
        let loc = find_jpeg_orientation(&d).unwrap();
        assert_eq!(u16::from_be_bytes([d[loc.offset], d[loc.offset + 1]]), 6);
        let len = d.len();
        save_rotation(&p, 1).unwrap(); // in-place patch
        let d2 = std::fs::read(&p).unwrap();
        assert_eq!(d2.len(), len);
        assert_eq!(u16::from_be_bytes([d2[loc.offset], d2[loc.offset + 1]]), 3);
        // image crate sees the orientation and the dims flip when applied
        let (w, h) = {
            use image::ImageDecoder;
            let mut dec = image::ImageReader::open(&p)
                .unwrap()
                .into_decoder()
                .unwrap();
            assert_eq!(
                dec.orientation().unwrap(),
                image::metadata::Orientation::Rotate180
            );
            dec.dimensions()
        };
        assert_eq!((w, h), (8, 4));

        let png = dir.join("b.png");
        img.save(&png).unwrap();
        assert_eq!(
            plan_rotation_save(&png),
            SavePlan::Reencode(image::ImageFormat::Png)
        );
        save_rotation(&png, 1).unwrap();
        assert_eq!(image::image_dimensions(&png).unwrap(), (4, 8));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
