//! Media kind detection (extension first, magic-byte sniff as fallback).
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    /// Anything the `image` crate decodes (JPEG, PNG, GIF, WebP, TIFF, EXR, ...).
    Raster,
    Heif,
    Jxl,
    Svg,
    Psd,
    Icns,
    Raw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Image(ImageFormat),
    Video,
}

impl MediaKind {
    pub fn is_video(self) -> bool {
        matches!(self, MediaKind::Video)
    }
}

const RASTER: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "apng", "gif", "webp", "bmp", "dib", "ico", "cur", "tif",
    "tiff", "tga", "dds", "hdr", "exr", "pnm", "pbm", "pgm", "ppm", "pam", "ff", "qoi",
];
const HEIF: &[&str] = &["heic", "heif", "hif", "avif", "heics", "avifs"];
const JXL: &[&str] = &["jxl"];
const SVG: &[&str] = &["svg", "svgz"];
const PSD: &[&str] = &["psd"];
const ICNS: &[&str] = &["icns"];
const RAW: &[&str] = &[
    "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "dng", "raf", "orf", "rw2", "pef",
    "srw", "x3f", "3fr", "erf", "kdc", "dcr", "mrw", "iiq", "mos", "rwl", "mef", "ari",
];
const VIDEO: &[&str] = &[
    "mp4", "m4v", "mkv", "webm", "mov", "qt", "avi", "wmv", "flv", "f4v", "mpg", "mpeg", "mpe",
    "m1v", "m2v", "m2ts", "mts", "ts", "vob", "ogv", "3gp", "3g2", "divx", "xvid", "asf", "rm",
    "rmvb", "y4m", "mxf", "nut", "dv", "h264", "h265", "hevc", "av1", "ivf", "fli", "flc", "ogm",
];

pub fn kind_from_ext(path: &Path) -> Option<MediaKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let e = ext.as_str();
    let has = |list: &[&str]| list.contains(&e);
    Some(if has(RASTER) {
        MediaKind::Image(ImageFormat::Raster)
    } else if has(HEIF) {
        MediaKind::Image(ImageFormat::Heif)
    } else if has(JXL) {
        MediaKind::Image(ImageFormat::Jxl)
    } else if has(SVG) {
        MediaKind::Image(ImageFormat::Svg)
    } else if has(PSD) {
        MediaKind::Image(ImageFormat::Psd)
    } else if has(ICNS) {
        MediaKind::Image(ImageFormat::Icns)
    } else if has(RAW) {
        MediaKind::Image(ImageFormat::Raw)
    } else if has(VIDEO) {
        MediaKind::Video
    } else {
        return None;
    })
}

/// Sniff the first bytes of the file. Only used for unknown/missing extensions.
pub fn kind_from_magic(path: &Path) -> Option<MediaKind> {
    use std::io::Read;
    let mut buf = [0u8; 64];
    let n = std::fs::File::open(path).ok()?.read(&mut buf).ok()?;
    let buf = &buf[..n];
    if jxl_magic(buf) {
        return Some(MediaKind::Image(ImageFormat::Jxl));
    }
    let t = infer::get(buf)?;
    let mime = t.mime_type();
    match mime {
        "image/heif" | "image/avif" | "image/heic" => Some(MediaKind::Image(ImageFormat::Heif)),
        "image/vnd.adobe.photoshop" => Some(MediaKind::Image(ImageFormat::Psd)),
        "image/x-icns" => Some(MediaKind::Image(ImageFormat::Icns)),
        "image/jxl" => Some(MediaKind::Image(ImageFormat::Jxl)),
        m if m.starts_with("image/") => {
            // infer reports CR2/NEF etc. as image/x-canon-cr2 ...
            if m.contains("x-canon")
                || m.contains("x-nikon")
                || m.contains("x-sony")
                || m.contains("x-adobe-dng")
                || m.contains("x-fuji")
                || m.contains("x-olympus")
                || m.contains("x-panasonic")
            {
                Some(MediaKind::Image(ImageFormat::Raw))
            } else {
                Some(MediaKind::Image(ImageFormat::Raster))
            }
        }
        m if m.starts_with("video/") => Some(MediaKind::Video),
        _ => None,
    }
}

fn jxl_magic(b: &[u8]) -> bool {
    b.starts_with(&[0xFF, 0x0A]) || b.starts_with(&[0, 0, 0, 0x0C, b'J', b'X', b'L', b' '])
}

/// Detect a path's media kind; `None` means "not media, skip it".
pub fn detect(path: &Path) -> Option<MediaKind> {
    match path.extension() {
        Some(_) => kind_from_ext(path).or_else(|| {
            // Known non-media extensions are skipped cheaply; unknown ones get sniffed.
            if is_known_non_media(path) {
                None
            } else {
                kind_from_magic(path)
            }
        }),
        None => kind_from_magic(path),
    }
}

fn is_known_non_media(path: &Path) -> bool {
    const SKIP: &[&str] = &[
        "txt",
        "md",
        "pdf",
        "doc",
        "docx",
        "zip",
        "tar",
        "gz",
        "xz",
        "zst",
        "7z",
        "rar",
        "json",
        "xml",
        "html",
        "htm",
        "rs",
        "py",
        "sh",
        "exe",
        "iso",
        "nfo",
        "srt",
        "ass",
        "ssa",
        "vtt",
        "sub",
        "idx",
        "xmp",
        "pp3",
        "dop",
        "thm",
        "lrv",
        "aae",
        "db",
        "ini",
        "log",
        "mp3",
        "flac",
        "ogg",
        "opus",
        "wav",
        "m4a",
        "aac",
        "wma",
        "part",
        "crdownload",
        "torrent",
        "kra",
        "xcf",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| SKIP.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn ext_detection() {
        let k = |s: &str| kind_from_ext(&PathBuf::from(s));
        assert_eq!(k("a.JPG"), Some(MediaKind::Image(ImageFormat::Raster)));
        assert_eq!(k("a.heic"), Some(MediaKind::Image(ImageFormat::Heif)));
        assert_eq!(k("a.AVIF"), Some(MediaKind::Image(ImageFormat::Heif)));
        assert_eq!(k("a.jxl"), Some(MediaKind::Image(ImageFormat::Jxl)));
        assert_eq!(k("a.svg"), Some(MediaKind::Image(ImageFormat::Svg)));
        assert_eq!(k("a.psd"), Some(MediaKind::Image(ImageFormat::Psd)));
        assert_eq!(k("a.icns"), Some(MediaKind::Image(ImageFormat::Icns)));
        assert_eq!(k("a.CR3"), Some(MediaKind::Image(ImageFormat::Raw)));
        assert_eq!(k("a.mkv"), Some(MediaKind::Video));
        assert_eq!(k("a.txt"), None);
        assert_eq!(k("noext"), None);
    }

    #[test]
    fn skips_audio_and_sidecars() {
        assert!(detect(&PathBuf::from("/nonexistent/x.mp3")).is_none());
        assert!(detect(&PathBuf::from("/nonexistent/x.xmp")).is_none());
    }
}
