//! Persistent settings: ~/.config/mview/config.toml
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FitMode {
    /// Shrink to fit the window, never upscale beyond 100%.
    #[default]
    Fit,
    /// Fill the window (crop overflow).
    Fill,
    /// 1:1 pixels.
    Actual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 0..=130 (mpv volume scale).
    pub volume: f64,
    pub mute: bool,
    /// mpv `hwdec` value used when hardware decoding is on (zero-copy interop).
    pub hwdec: String,
    /// Start with hardware decoding enabled. Ctrl+H toggles at runtime.
    pub hw_decode: bool,
    pub fit_mode: FitMode,
    pub start_fullscreen: bool,
    pub watch_folder: bool,
    pub show_hidden: bool,
    pub show_filmstrip: bool,
    pub show_info: bool,
    /// Slideshow interval for images, seconds.
    pub slideshow_secs: f32,
    /// Loop videos forever (true) or stop at end (false).
    pub loop_video: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            volume: 70.0,
            mute: false,
            hwdec: "auto-safe".into(),
            hw_decode: true,
            fit_mode: FitMode::Fit,
            start_fullscreen: false,
            watch_folder: true,
            show_hidden: false,
            show_filmstrip: false,
            show_info: false,
            slideshow_secs: 4.0,
            loop_video: true,
        }
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("mview").join("config.toml"))
    }

    pub fn load() -> Self {
        let Some(p) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&p) {
            Ok(s) => toml::from_str(&s).unwrap_or_else(|e| {
                log::warn!("bad config {}: {e}; using defaults", p.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let Some(p) = Self::path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match toml::to_string_pretty(self) {
            Ok(s) => {
                // Write atomically so a crash never leaves a truncated config.
                let tmp = p.with_extension("toml.tmp");
                if std::fs::write(&tmp, s).is_ok() {
                    let _ = std::fs::rename(&tmp, &p);
                }
            }
            Err(e) => log::warn!("config serialize: {e}"),
        }
    }
}
