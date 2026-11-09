//! Metadata for the info overlay.
use std::path::Path;

/// A short, human-friendly EXIF summary (empty if the file has none).
pub fn exif_summary(path: &Path) -> Vec<(String, String)> {
    let Ok(file) = std::fs::File::open(path) else {
        return vec![];
    };
    let mut r = std::io::BufReader::new(file);
    let Ok(exif) = exif::Reader::new().read_from_container(&mut r) else {
        return vec![];
    };
    use exif::{In, Tag};
    let get = |t: Tag| {
        exif.get_field(t, In::PRIMARY)
            .map(|f| {
                f.display_value()
                    .with_unit(&exif)
                    .to_string()
                    .trim_matches('"')
                    .trim()
                    .to_string()
            })
            .filter(|s| !s.is_empty())
    };
    let mut out = Vec::new();
    let camera = match (get(Tag::Make), get(Tag::Model)) {
        (Some(m), Some(md)) if md.starts_with(&m) => Some(md),
        (Some(m), Some(md)) => Some(format!("{m} {md}")),
        (a, b) => a.or(b),
    };
    let mut push = |k: &str, v: Option<String>| {
        if let Some(v) = v {
            out.push((k.to_string(), v));
        }
    };
    push("Camera", camera);
    push("Lens", get(Tag::LensModel));
    push(
        "Taken",
        get(Tag::DateTimeOriginal).or_else(|| get(Tag::DateTime)),
    );
    push("Exposure", get(Tag::ExposureTime));
    push("Aperture", get(Tag::FNumber));
    push("ISO", get(Tag::PhotographicSensitivity));
    push("Focal length", get(Tag::FocalLength));
    push("Flash", get(Tag::Flash));
    out
}

pub fn human_size(bytes: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

pub fn fmt_time(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "--:--".into();
    }
    let s = secs as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats() {
        assert_eq!(super::human_size(512), "512 B");
        assert_eq!(super::human_size(1536), "1.5 KiB");
        assert_eq!(super::fmt_time(65.4), "1:05");
        assert_eq!(super::fmt_time(3725.0), "1:02:05");
    }
}
