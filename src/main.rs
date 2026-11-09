//! rummv — image & video viewer. `rummv <file|folder>`
mod actions;
mod app;
mod cache;
mod config;
mod decode;
mod media;
mod meta;
mod playlist;
mod slowmo;
mod thumbs;
mod video;

use std::path::PathBuf;

const USAGE: &str = "\
rummv — image & video viewer

USAGE:
    rummv [FILE | FOLDER]

Opens FILE and lets you step through every image/video in its folder with ←/→.
With a FOLDER (or nothing: the current directory) it starts at the first file.
Press F1 inside the viewer for all keyboard shortcuts.

Config: ~/.config/rummv/config.toml";

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let mut target: Option<PathBuf> = None;
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("-h" | "--help") => {
                println!("{USAGE}");
                return Ok(());
            }
            Some("-V" | "--version") => {
                println!("rummv {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            Some("--") => {}
            _ => {
                if target.is_none() {
                    target = Some(parse_target(arg));
                }
            }
        }
    }
    let target =
        target.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    anyhow::ensure!(
        target.exists(),
        "{}: no such file or directory",
        target.display()
    );

    let cfg = config::Config::load();
    let playlist = playlist::Playlist::open(&target, cfg.show_hidden, cfg.watch_folder)?;

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("rummv")
        .with_app_id("rummv")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([320.0, 240.0])
        .with_drag_and_drop(true)
        // X11 window icon; on Wayland Plasma takes it from rummv.desktop (Icon=rummv).
        .with_icon(std::sync::Arc::new(eframe::icon_data::from_png_bytes(
            include_bytes!("../assets/rummv.png"),
        )?));
    if cfg.start_fullscreen {
        viewport = viewport.with_fullscreen(true);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        glow_options: eframe::egui_glow::GlowConfiguration {
            vsync: true,
            hardware_acceleration: eframe::egui_glow::HardwareAcceleration::Required,
            ..Default::default()
        },
        depth_buffer: 0,
        multisampling: 0,
        ..Default::default()
    };
    eframe::run_native(
        "rummv",
        options,
        Box::new(move |cc| Ok(Box::new(app::RummvApp::new(cc, cfg, playlist)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Accept plain paths and file:// URIs (some launchers pass %u).
fn parse_target(arg: std::ffi::OsString) -> PathBuf {
    if let Some(s) = arg.to_str()
        && s.starts_with("file://")
        && let Ok(u) = url::Url::parse(s)
        && let Ok(p) = u.to_file_path()
    {
        return p;
    }
    PathBuf::from(arg)
}
