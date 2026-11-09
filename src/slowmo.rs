//! iPhone slow-motion playback.
//!
//! An iPhone slo-mo `.MOV` stores every frame at the capture rate (120/240 fps); Apple's
//! players slow down only a section of it. That section and rate are a Photos edit, kept in
//! a sidecar `.AAE` plist (`adjustmentFormatIdentifier` = `com.apple.video.slomo`) whose
//! `adjustmentData` is a binary plist:
//! `{ slowMotion: { rate: 0.125, regions: [ { timeRange: { start: CMTime, duration: CMTime } } ] } }`.
//! Without a sidecar we fall back to Apple's default look: 15%–85% of the clip at 30 fps.
use std::path::{Path, PathBuf};

/// Clips at or above this frame rate count as high-frame-rate captures.
pub const MIN_SLOWMO_FPS: f64 = 100.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// From the Photos edit in the `.AAE` sidecar.
    Sidecar,
    /// No sidecar: Apple-like default section.
    Default,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SlowMo {
    /// Playback speed inside the slow sections (e.g. 0.125 = 240 fps shown at 30 fps).
    pub rate: f64,
    /// Slow sections as (start, end) in seconds of media time.
    pub regions: Vec<(f64, f64)>,
    pub source: Source,
}

impl SlowMo {
    /// Speed to play at for media position `t` (seconds).
    pub fn speed_at(&self, t: f64) -> f64 {
        if self.regions.iter().any(|&(a, b)| t >= a && t < b) {
            self.rate
        } else {
            1.0
        }
    }

    /// Default when there is no sidecar: slow middle (15%–85%) at 30 fps.
    pub fn default_for(duration: f64, fps: f64) -> Option<Self> {
        if duration <= 0.0 || fps < MIN_SLOWMO_FPS {
            return None;
        }
        Some(SlowMo {
            rate: 1.0 / (fps / 30.0).round(),
            regions: vec![(duration * 0.15, duration * 0.85)],
            source: Source::Default,
        })
    }

    /// Short human description for the info panel.
    pub fn describe(&self) -> String {
        let regions: Vec<String> = self
            .regions
            .iter()
            .map(|(a, b)| format!("{a:.2}–{b:.2} s"))
            .collect();
        let src = match self.source {
            Source::Sidecar => "from .AAE",
            Source::Default => "default section",
        };
        format!("{}× at {} ({src})", fmt_rate(self.rate), regions.join(", "))
    }
}

pub fn fmt_rate(r: f64) -> String {
    let s = format!("{r:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Find the Photos sidecar for a video: `IMG_1234.AAE`, or `IMG_O1234.AAE` (the original's
/// sidecar when the clip was edited), matched case-insensitively in the same folder.
pub fn find_sidecar(video: &Path) -> Option<PathBuf> {
    let dir = video.parent()?;
    let stem = video.file_stem()?.to_str()?.to_ascii_lowercase();
    let mut wanted = vec![format!("{stem}.aae")];
    if let Some(rest) = stem.strip_prefix("img_") {
        wanted.push(format!("img_o{rest}.aae"));
    }
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| wanted.contains(&n.to_ascii_lowercase()))
        })
}

/// Parse a slo-mo `.AAE`. Returns `None` for other kinds of Photos edits.
pub fn parse_sidecar(path: &Path) -> anyhow::Result<Option<SlowMo>> {
    let outer: plist::Dictionary = plist::from_file(path)?;
    if outer
        .get("adjustmentFormatIdentifier")
        .and_then(|v| v.as_string())
        != Some("com.apple.video.slomo")
    {
        return Ok(None);
    }
    let data = outer
        .get("adjustmentData")
        .and_then(|v| v.as_data())
        .ok_or_else(|| anyhow::anyhow!("no adjustmentData"))?;
    let inner: plist::Dictionary = plist::from_bytes(data)?;
    let sm = inner
        .get("slowMotion")
        .and_then(|v| v.as_dictionary())
        .ok_or_else(|| anyhow::anyhow!("no slowMotion"))?;
    let rate = sm
        .get("rate")
        .and_then(|v| {
            v.as_real()
                .or_else(|| v.as_signed_integer().map(|i| i as f64))
        })
        .filter(|r| *r > 0.0 && *r <= 1.0)
        .ok_or_else(|| anyhow::anyhow!("bad rate"))?;
    let mut regions = Vec::new();
    for r in sm
        .get("regions")
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice())
        .unwrap_or_default()
    {
        let tr = r
            .as_dictionary()
            .and_then(|d| d.get("timeRange"))
            .and_then(|v| v.as_dictionary());
        let (Some(start), Some(dur)) = (
            tr.and_then(|d| d.get("start")).and_then(cmtime),
            tr.and_then(|d| d.get("duration")).and_then(cmtime),
        ) else {
            continue;
        };
        if dur > 0.0 {
            regions.push((start, start + dur));
        }
    }
    if regions.is_empty() {
        // Slo-mo turned off in Photos (or nothing usable): play at normal speed.
        return Ok(None);
    }
    Ok(Some(SlowMo {
        rate,
        regions,
        source: Source::Sidecar,
    }))
}

/// CMTime dictionary `{ value, timescale, flags, epoch }` → seconds (flags bit 0 = valid).
fn cmtime(v: &plist::Value) -> Option<f64> {
    let d = v.as_dictionary()?;
    let int = |k: &str| d.get(k).and_then(|v| v.as_signed_integer());
    if int("flags").unwrap_or(1) & 1 == 0 {
        return None;
    }
    let ts = int("timescale").filter(|t| *t > 0)?;
    Some(int("value")? as f64 / ts as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
    }

    #[test]
    fn parses_iphone_sidecar() {
        let s = parse_sidecar(&fixtures().join("d01_slowmo.AAE"))
            .unwrap()
            .unwrap();
        assert_eq!(s.rate, 0.125);
        assert_eq!(s.source, Source::Sidecar);
        assert_eq!(s.regions.len(), 1);
        let (a, b) = s.regions[0];
        assert!((a - 0.26875).abs() < 1e-9 && (b - 1.522916667).abs() < 1e-6);
        assert_eq!(s.speed_at(0.1), 1.0);
        assert_eq!(s.speed_at(1.0), 0.125);
        assert_eq!(s.speed_at(1.6), 1.0);
    }

    #[test]
    fn finds_sidecar_case_insensitively() {
        let dir = std::env::temp_dir().join(format!("rummv-slowmo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("IMG_1234.MOV"), b"").unwrap();
        std::fs::write(dir.join("img_o1234.aae"), b"").unwrap();
        std::fs::write(dir.join("IMG_5678.MOV"), b"").unwrap();
        assert_eq!(
            find_sidecar(&dir.join("IMG_1234.MOV")),
            Some(dir.join("img_o1234.aae"))
        );
        assert_eq!(find_sidecar(&dir.join("IMG_5678.MOV")), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn default_section_matches_apple() {
        let s = SlowMo::default_for(1.791667, 240.5).unwrap();
        assert_eq!(s.rate, 0.125);
        assert!((s.regions[0].0 - 0.26875).abs() < 1e-3);
        assert_eq!(SlowMo::default_for(10.0, 120.0).unwrap().rate, 0.25);
        assert!(SlowMo::default_for(10.0, 60.0).is_none());
    }
}
