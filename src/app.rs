//! The egui application: input, layout, overlays, confirmation dialogs.
use crate::actions;
use crate::cache::{ImageCache, Loaded, Slot};
use crate::config::{Config, FitMode};
use crate::media::MediaKind;
use crate::meta;
use crate::playlist::Playlist;
use crate::thumbs::{Thumb, Thumbnailer};
use crate::video::{VideoInit, VideoPlayer};
use egui::{
    Align2, Color32, CornerRadius, Event, FontId, Key, Modifiers, PointerButton, Pos2, Rect, Sense,
    Stroke, Vec2, pos2, vec2,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const BG: Color32 = Color32::from_rgb(16, 16, 16);
const OVERLAY_BG: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 170);
const ACCENT: Color32 = Color32::from_rgb(90, 160, 255);
const UI_HIDE_AFTER: f64 = 2.5;
const FILMSTRIP_H: f32 = 108.0;
const THUMB_W: f32 = 120.0;
const PREFETCH: isize = 2;

/// Something that would modify or delete a file. Never executed without the user confirming.
#[derive(Clone)]
enum ConfirmAction {
    Trash,
    DeletePermanently,
    SaveRotation { quarter_turns: u8 },
}

#[derive(Clone)]
struct Confirm {
    action: ConfirmAction,
    path: PathBuf,
    title: String,
    body: String,
    confirm_label: &'static str,
    danger: bool,
    first_frame: bool,
}

#[derive(Default)]
struct View {
    /// Explicit zoom (physical px per image px). `None` = follow the fit mode.
    zoom: Option<f32>,
    pan: Vec2,
    /// Quarter turns clockwise (view only).
    rotation: u8,
}

struct Toast {
    text: String,
    until: f64,
}

pub struct MviewApp {
    cfg: Config,
    playlist: Playlist,
    cache: ImageCache,
    thumbs: Thumbnailer,
    video: Option<VideoPlayer>,
    video_init_error: Option<String>,

    shown: Option<PathBuf>,
    view: View,
    anim_frame: usize,
    anim_next: f64,
    slideshow: Option<f64>,
    last_video_pos: f64,

    confirm: Option<Confirm>,
    toasts: Vec<Toast>,
    show_help: bool,
    last_activity: f64,
    last_pointer: Option<Pos2>,
    wheel_nav_accum: f32,
    seek_drag: bool,
    sized_window: bool,
    filmstrip_scroll_to_current: bool,
    title: String,
    loading_since: f64,
}

impl MviewApp {
    pub fn new(cc: &eframe::CreationContext<'_>, cfg: Config, playlist: Playlist) -> Self {
        let ctx = &cc.egui_ctx;
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = BG;
        visuals.window_fill = Color32::from_rgb(32, 32, 34);
        ctx.set_visuals(visuals);

        let (video, video_init_error) = match VideoPlayer::new(
            cc,
            VideoInit {
                volume: cfg.volume,
                mute: cfg.mute,
                hw_enabled: cfg.hw_decode,
                hwdec_value: cfg.hwdec.clone(),
                loop_video: cfg.loop_video,
            },
        ) {
            Ok(v) => (Some(v), None),
            Err(e) => {
                log::error!("video disabled: {e:#}");
                (None, Some(format!("{e:#}")))
            }
        };

        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        Self {
            cache: ImageCache::new(ctx.clone(), (threads / 2).clamp(2, 4)),
            thumbs: Thumbnailer::new(ctx.clone(), 2),
            cfg,
            playlist,
            video,
            video_init_error,
            shown: None,
            view: View::default(),
            anim_frame: 0,
            anim_next: 0.0,
            slideshow: None,
            last_video_pos: 0.0,
            confirm: None,
            toasts: Vec::new(),
            show_help: false,
            last_activity: 0.0,
            last_pointer: None,
            wheel_nav_accum: 0.0,
            seek_drag: false,
            sized_window: false,
            filmstrip_scroll_to_current: true,
            title: String::new(),
            loading_since: 0.0,
        }
    }

    fn toast(&mut self, ctx: &egui::Context, text: impl Into<String>) {
        let now = ctx.input(|i| i.time);
        self.toasts.push(Toast {
            text: text.into(),
            until: now + 3.0,
        });
    }

    fn current(&self) -> Option<(PathBuf, MediaKind)> {
        self.playlist.current().map(|e| (e.path.clone(), e.kind))
    }

    // ------------------------------------------------------------------ navigation

    fn navigate(&mut self, delta: isize) {
        self.playlist.step(delta);
        self.filmstrip_scroll_to_current = true;
    }

    /// React to the current entry changing: reset view, start/stop video, prefetch.
    fn sync_current(&mut self, ctx: &egui::Context) {
        let cur = self.current();
        let cur_path = cur.as_ref().map(|c| c.0.clone());
        if cur_path != self.shown {
            self.shown = cur_path.clone();
            self.view = View::default();
            self.anim_frame = 0;
            self.anim_next = 0.0;
            self.last_video_pos = 0.0;
            self.loading_since = ctx.input(|i| i.time);
            if let Some(s) = &mut self.slideshow {
                *s = ctx.input(|i| i.time) + self.cfg.slideshow_secs as f64;
            }
            match &cur {
                Some((p, MediaKind::Video)) => {
                    if let Some(v) = &mut self.video {
                        v.load(p);
                    }
                }
                _ => {
                    if let Some(v) = &mut self.video {
                        v.stop();
                    }
                }
            }
            let title = match &cur {
                Some((p, _)) => format!(
                    "{} — mview",
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                ),
                None => "mview".into(),
            };
            if title != self.title {
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
                self.title = title;
            }
        }

        // Prefetch: current first, then alternating next/prev.
        let mut want = Vec::new();
        if !self.playlist.is_empty() {
            let mut offsets = vec![0isize];
            for d in 1..=PREFETCH {
                offsets.push(d);
                offsets.push(-d);
            }
            for d in offsets {
                if let Some(i) = self.playlist.offset_index(d) {
                    let e = &self.playlist.entries[i];
                    if let MediaKind::Image(fmt) = e.kind
                        && !want.iter().any(|(p, _)| p == &e.path)
                    {
                        want.push((e.path.clone(), fmt));
                    }
                }
            }
        }
        self.cache.want(&want);
    }

    // ------------------------------------------------------------------ input

    fn handle_input(&mut self, ctx: &egui::Context) {
        let events = ctx.input(|i| i.events.clone());
        let now = ctx.input(|i| i.time);
        for ev in &events {
            match ev {
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    self.last_activity = now;
                    self.on_key(ctx, *key, *modifiers);
                }
                Event::PointerButton {
                    button: PointerButton::Extra1,
                    pressed: true,
                    ..
                } => self.navigate(-1),
                Event::PointerButton {
                    button: PointerButton::Extra2,
                    pressed: true,
                    ..
                } => self.navigate(1),
                _ => {}
            }
        }

        // Drag-and-drop a file/folder onto the window: open it.
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if let Some(p) = dropped.first() {
            match Playlist::open(p, self.cfg.show_hidden, self.cfg.watch_folder) {
                Ok(pl) => {
                    self.playlist = pl;
                    self.filmstrip_scroll_to_current = true;
                }
                Err(e) => self.toast(ctx, format!("Can't open {}: {e}", p.display())),
            }
        }
    }

    fn on_key(&mut self, ctx: &egui::Context, key: Key, m: Modifiers) {
        let is_video = matches!(self.current(), Some((_, MediaKind::Video)));
        let ctrl = m.ctrl || m.command;
        let shift = m.shift;
        match key {
            // Navigation / seeking
            Key::ArrowLeft | Key::ArrowRight => {
                let dir = if key == Key::ArrowRight { 1.0 } else { -1.0 };
                if shift {
                    if is_video && let Some(v) = &mut self.video {
                        v.seek_relative(dir * if ctrl { 30.0 } else { 5.0 });
                    }
                } else if !ctrl {
                    self.navigate(dir as isize);
                }
            }
            Key::PageDown => self.navigate(1),
            Key::PageUp => self.navigate(-1),
            Key::Home => {
                self.playlist.first();
                self.filmstrip_scroll_to_current = true;
            }
            Key::End => {
                self.playlist.last();
                self.filmstrip_scroll_to_current = true;
            }
            Key::Period if is_video => {
                if let Some(v) = &mut self.video {
                    v.frame_step(true);
                }
            }
            Key::Comma if is_video => {
                if let Some(v) = &mut self.video {
                    v.frame_step(false);
                }
            }
            Key::Space => {
                if is_video {
                    if let Some(v) = &mut self.video {
                        v.toggle_pause();
                    }
                } else {
                    self.toggle_slideshow(ctx);
                }
            }
            // Volume
            Key::ArrowUp | Key::ArrowDown => {
                if let Some(v) = &mut self.video {
                    let step = if key == Key::ArrowUp { 5.0 } else { -5.0 };
                    v.set_volume(v.state.volume + step);
                    self.cfg.volume = v.state.volume;
                    self.cfg.mute = v.state.mute;
                    let msg = format!("Volume {:.0}%", v.state.volume);
                    self.toast(ctx, msg);
                }
            }
            Key::M => {
                if let Some(v) = &mut self.video {
                    let m = !v.state.mute;
                    v.set_mute(m);
                    self.cfg.mute = m;
                    self.toast(ctx, if m { "Muted" } else { "Unmuted" });
                }
            }
            // Zoom & fit
            Key::Num0 => {
                self.cfg.fit_mode = FitMode::Fit;
                self.view.zoom = None;
                self.view.pan = Vec2::ZERO;
            }
            Key::Num1 => {
                self.view.zoom = Some(1.0);
                self.view.pan = Vec2::ZERO;
            }
            Key::F if !ctrl => {
                self.cfg.fit_mode = FitMode::Fill;
                self.view.zoom = None;
                self.view.pan = Vec2::ZERO;
            }
            Key::Plus | Key::Equals => self.zoom_by(ctx, 1.25, None),
            Key::Minus => self.zoom_by(ctx, 0.8, None),
            // Rotation (view only)
            Key::R if !ctrl => {
                self.view.rotation = (self.view.rotation + if shift { 3 } else { 1 }) % 4;
                self.view.pan = Vec2::ZERO;
            }
            // File actions
            Key::Delete => {
                if let Some((p, _)) = self.current() {
                    self.ask_delete(p, shift);
                }
            }
            Key::S if ctrl => self.ask_save_rotation(ctx),
            Key::C if ctrl => {
                if let Some((p, _)) = self.current() {
                    ctx.copy_text(p.to_string_lossy().into_owned());
                    self.toast(ctx, "Path copied");
                }
            }
            Key::E if ctrl => {
                if let Some((p, _)) = self.current()
                    && let Err(e) = actions::show_in_file_manager(&p)
                {
                    self.toast(ctx, format!("{e}"));
                }
            }
            Key::O if !ctrl => {
                if let Some((p, _)) = self.current() {
                    if let Some(v) = &mut self.video
                        && v.is_active()
                        && !v.state.paused
                    {
                        v.toggle_pause();
                    }
                    if let Err(e) = actions::open_with_default(&p) {
                        self.toast(ctx, format!("{e}"));
                    }
                }
            }
            Key::H if ctrl => {
                if let Some(v) = &mut self.video {
                    v.toggle_hw();
                    self.cfg.hw_decode = v.hw_enabled();
                    let msg = if v.hw_enabled() {
                        "Hardware decoding on"
                    } else {
                        "Hardware decoding off (CPU)"
                    };
                    self.toast(ctx, msg);
                }
            }
            Key::L if !ctrl => {
                self.cfg.loop_video = !self.cfg.loop_video;
                if let Some(v) = &mut self.video {
                    v.set_loop(self.cfg.loop_video);
                }
                let msg = if self.cfg.loop_video {
                    "Loop on"
                } else {
                    "Loop off"
                };
                self.toast(ctx, msg);
            }
            // UI toggles
            Key::I => self.cfg.show_info = !self.cfg.show_info,
            Key::T => {
                self.cfg.show_filmstrip = !self.cfg.show_filmstrip;
                self.filmstrip_scroll_to_current = true;
            }
            Key::F1 | Key::Questionmark => self.show_help = !self.show_help,
            Key::F5 => {
                self.playlist.rescan();
                if let Some((p, _)) = self.current() {
                    self.cache.invalidate(&p);
                }
                self.shown = None;
            }
            Key::F11 => self.toggle_fullscreen(ctx),
            Key::Enter if m.alt => self.toggle_fullscreen(ctx),
            Key::Escape => {
                if self.show_help {
                    self.show_help = false;
                } else if self.slideshow.is_some() {
                    self.slideshow = None;
                    self.toast(ctx, "Slideshow stopped");
                } else if ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                } else {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Key::Q if !ctrl => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Key::W if ctrl => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            _ => {}
        }
    }

    fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
    }

    fn toggle_slideshow(&mut self, ctx: &egui::Context) {
        if self.slideshow.is_some() {
            self.slideshow = None;
            self.toast(ctx, "Slideshow stopped");
        } else {
            self.slideshow = Some(ctx.input(|i| i.time) + self.cfg.slideshow_secs as f64);
            self.toast(
                ctx,
                format!(
                    "Slideshow ({}s) — Space/Esc to stop",
                    self.cfg.slideshow_secs
                ),
            );
        }
    }

    // ------------------------------------------------------------------ confirmations

    fn ask_delete(&mut self, path: PathBuf, permanent: bool) {
        let name = file_name(&path);
        self.confirm = Some(if permanent {
            Confirm {
                action: ConfirmAction::DeletePermanently,
                title: "Delete permanently?".into(),
                body: format!("“{name}” will be permanently deleted.\nThis cannot be undone."),
                confirm_label: "Delete permanently",
                danger: true,
                path,
                first_frame: true,
            }
        } else {
            Confirm {
                action: ConfirmAction::Trash,
                title: "Move to Trash?".into(),
                body: format!(
                    "“{name}” will be moved to the Trash.\nYou can restore it from the Trash in Dolphin."
                ),
                confirm_label: "Move to Trash",
                danger: false,
                path,
                first_frame: true,
            }
        });
    }

    fn ask_save_rotation(&mut self, ctx: &egui::Context) {
        let Some((path, kind)) = self.current() else {
            return;
        };
        if !matches!(kind, MediaKind::Image(_)) {
            self.toast(ctx, "Rotation can only be saved for images");
            return;
        }
        if self.view.rotation == 0 {
            self.toast(ctx, "Nothing to save — rotate with R / Shift+R first");
            return;
        }
        let plan = actions::plan_rotation_save(&path);
        if let actions::SavePlan::Unsupported(why) = &plan {
            self.toast(ctx, why.clone());
            return;
        }
        let deg = self.view.rotation as u32 * 90;
        self.confirm = Some(Confirm {
            action: ConfirmAction::SaveRotation {
                quarter_turns: self.view.rotation,
            },
            title: "Save rotation to file?".into(),
            body: format!(
                "“{}” will be modified: rotated {deg}° clockwise.\n\n{}",
                file_name(&path),
                plan.describe()
            ),
            confirm_label: "Save",
            danger: matches!(plan, actions::SavePlan::Reencode(_)),
            path,
            first_frame: true,
        });
    }

    /// Shows the modal; executes the action only on explicit confirmation.
    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut c) = self.confirm.take() else {
            return;
        };
        // Keyboard: Enter / Y confirm, Esc / N cancel. Consumed so nothing else reacts.
        let (yes, no) = ctx.input_mut(|i| {
            let yes = i.consume_key(Modifiers::NONE, Key::Enter)
                || i.consume_key(Modifiers::NONE, Key::Y);
            let no = i.consume_key(Modifiers::NONE, Key::Escape)
                || i.consume_key(Modifiers::NONE, Key::N);
            (yes, no)
        });
        let mut decided: Option<bool> = if yes {
            Some(true)
        } else if no {
            Some(false)
        } else {
            None
        };

        let resp = egui::Modal::new(egui::Id::unique("confirm")).show(ctx, |ui| {
            ui.set_max_width(440.0);
            ui.heading(&c.title);
            ui.add_space(8.0);
            ui.label(&c.body);
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(c.path.display().to_string())
                    .small()
                    .weak(),
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let cancel = ui.button("Cancel  (Esc)");
                if c.first_frame {
                    cancel.request_focus();
                    c.first_frame = false;
                }
                if cancel.clicked() {
                    decided = Some(false);
                }
                let label = egui::RichText::new(format!("{}  (Enter)", c.confirm_label));
                let btn = if c.danger {
                    egui::Button::new(label.color(Color32::WHITE))
                        .fill(Color32::from_rgb(170, 40, 40))
                } else {
                    egui::Button::new(label)
                };
                if ui.add(btn).clicked() {
                    decided = Some(true);
                }
            });
        });
        if resp.should_close() && decided.is_none() {
            decided = Some(false); // click outside
        }
        match decided {
            Some(true) => self.run_confirmed(ctx, c),
            Some(false) => {}
            None => self.confirm = Some(c),
        }
    }

    fn run_confirmed(&mut self, ctx: &egui::Context, c: Confirm) {
        // Make sure the path is still the current one before touching anything.
        let still_current = self.current().map(|(p, _)| p == c.path).unwrap_or(false);
        match c.action {
            ConfirmAction::Trash | ConfirmAction::DeletePermanently => {
                // Release the file from mpv first.
                if let Some(v) = &mut self.video {
                    v.stop();
                }
                let res = if matches!(c.action, ConfirmAction::Trash) {
                    actions::trash(&c.path)
                } else {
                    actions::delete_permanently(&c.path)
                };
                match res {
                    Ok(()) => {
                        if still_current {
                            self.playlist.remove_current();
                        } else {
                            self.playlist.rescan();
                        }
                        self.cache.invalidate(&c.path);
                        self.thumbs.forget(&c.path);
                        self.shown = None;
                        let what = if matches!(c.action, ConfirmAction::Trash) {
                            "Moved to Trash"
                        } else {
                            "Deleted"
                        };
                        self.toast(ctx, format!("{what}: {}", file_name(&c.path)));
                    }
                    Err(e) => {
                        self.shown = None; // reload video if it was one
                        self.toast(ctx, format!("{e:#}"));
                    }
                }
            }
            ConfirmAction::SaveRotation { quarter_turns } => {
                match actions::save_rotation(&c.path, quarter_turns) {
                    Ok(()) => {
                        self.cache.invalidate(&c.path);
                        self.thumbs.forget(&c.path);
                        self.shown = None;
                        self.toast(ctx, "Rotation saved");
                    }
                    Err(e) => self.toast(ctx, format!("Couldn't save: {e:#}")),
                }
            }
        }
    }

    // ------------------------------------------------------------------ zoom helpers

    fn content_size(&self, base: Vec2) -> Vec2 {
        if self.view.rotation % 2 == 1 {
            vec2(base.y, base.x)
        } else {
            base
        }
    }

    /// Effective zoom (physical pixels per media pixel).
    fn effective_zoom(
        &self,
        ctx: &egui::Context,
        rect: Rect,
        media_px: Vec2,
        is_video: bool,
    ) -> f32 {
        if let Some(z) = self.view.zoom {
            return z;
        }
        let ppp = ctx.pixels_per_point();
        let s = self.content_size(media_px);
        if s.x <= 0.0 || s.y <= 0.0 {
            return 1.0;
        }
        let fx = rect.width() * ppp / s.x;
        let fy = rect.height() * ppp / s.y;
        match self.cfg.fit_mode {
            // Like Windows Photos: small images are not blown up, videos always fill the window.
            FitMode::Fit => {
                let f = fx.min(fy);
                if is_video { f } else { f.min(1.0) }
            }
            FitMode::Fill => fx.max(fy),
            FitMode::Actual => 1.0,
        }
    }

    fn zoom_by(&mut self, ctx: &egui::Context, factor: f32, anchor: Option<Pos2>) {
        // Needs last frame's geometry; stored via memory.
        let Some((rect, media_px, is_video)) =
            ctx.data(|d| d.get_temp::<(Rect, Vec2, bool)>(egui::Id::unique("geom")))
        else {
            return;
        };
        let old = self.effective_zoom(ctx, rect, media_px, is_video);
        let new = (old * factor).clamp(0.01, 64.0);
        let anchor = anchor.unwrap_or(rect.center());
        let center = rect.center() + self.view.pan;
        let offset = anchor - center;
        let new_center = anchor - offset * (new / old);
        self.view.pan = new_center - rect.center();
        self.view.zoom = Some(new);
    }

    // ------------------------------------------------------------------ drawing

    fn draw_media(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame, rect: Rect) {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        let Some((path, kind)) = self.current() else {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                format!("No images or videos in\n{}", self.playlist.dir.display()),
                FontId::proportional(18.0),
                Color32::GRAY,
            );
            return;
        };

        // Resolve what to draw: (texture, media pixel size)
        let mut tex: Option<(egui::TextureId, Vec2)> = None;
        let mut message: Option<String> = None;
        let mut loading = false;
        let is_video = kind.is_video();
        let mut loaded_img: Option<Arc<Loaded>> = None;

        if is_video {
            match &mut self.video {
                Some(v) => {
                    if let Some(e) = &v.error {
                        message = Some(e.clone());
                    }
                    tex = v.render_frame(frame);
                    if tex.is_none() && message.is_none() {
                        loading = true;
                    }
                }
                None => {
                    message = Some(format!(
                        "Video playback unavailable: {}",
                        self.video_init_error
                            .as_deref()
                            .unwrap_or("libmpv failed to initialise")
                    ))
                }
            }
        } else {
            match self.cache.get(&path) {
                Some(Slot::Ready(l)) => {
                    loaded_img = Some(l.clone());
                }
                Some(Slot::Failed(e)) => message = Some(format!("Can't open this file:\n{e}")),
                _ => loading = true,
            }
        }

        if let Some(l) = &loaded_img {
            // Advance animation.
            if l.frames.len() > 1 {
                if self.anim_next == 0.0 {
                    self.anim_next = now + l.frames[0].1.as_secs_f64();
                }
                while now >= self.anim_next {
                    self.anim_frame = (self.anim_frame + 1) % l.frames.len();
                    self.anim_next += l.frames[self.anim_frame].1.as_secs_f64().max(0.01);
                    if self.anim_next < now - 1.0 {
                        self.anim_next = now; // fell far behind (window hidden); resync
                    }
                }
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                    (self.anim_next - now).max(0.0),
                ));
            }
            let f = &l.frames[self.anim_frame.min(l.frames.len() - 1)];
            tex = Some((f.0.id(), l.size));
        }

        // While loading, show the cached thumbnail scaled up (instant feedback).
        let mut placeholder = false;
        if tex.is_none()
            && loading
            && let Some(Thumb::Ready(t)) = self.thumbs.get(&path)
        {
            tex = Some((t.id(), t.size_vec2()));
            placeholder = true;
        }

        let Some((tex_id, tex_px)) = tex else {
            if let Some(msg) = message {
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    msg,
                    FontId::proportional(16.0),
                    Color32::LIGHT_RED,
                );
            } else if loading && now - self.loading_since > 0.15 {
                let r = Rect::from_center_size(rect.center(), vec2(32.0, 32.0));
                ui.put(r, egui::Spinner::new().size(32.0));
            }
            if loading {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
            self.thumbs.request(&[(path.clone(), kind)]);
            return;
        };

        // Geometry. For a placeholder thumbnail use the real image size if we can guess it.
        let media_px = if placeholder {
            // fit thumbnail like the final image would be (best effort: upscale to fit)
            let s = self.content_size(tex_px);
            let ppp = ctx.pixels_per_point();
            let f = (rect.width() * ppp / s.x).min(rect.height() * ppp / s.y);
            tex_px * f.min(4.0)
        } else {
            tex_px
        };
        ctx.data_mut(|d| d.insert_temp(egui::Id::unique("geom"), (rect, media_px, is_video)));
        let zoom = self.effective_zoom(&ctx, rect, media_px, is_video);
        let ppp = ctx.pixels_per_point();
        let disp = self.content_size(media_px) * zoom / ppp;

        // Interaction: drag to pan, wheel to zoom, Ctrl+wheel to navigate, double-click fullscreen.
        let resp = ui.interact(rect, egui::Id::unique("media"), Sense::click_and_drag());
        if resp.dragged_by(PointerButton::Primary) {
            self.view.pan += resp.drag_delta();
            if self.view.zoom.is_none() {
                self.view.zoom = Some(zoom);
            }
        }
        if resp.double_clicked() {
            self.toggle_fullscreen(&ctx);
        }
        if resp.clicked_by(PointerButton::Middle) {
            self.view.zoom = None;
            self.view.pan = Vec2::ZERO;
        }
        if resp.hovered() && self.confirm.is_none() {
            let hover = ctx.input(|i| i.pointer.hover_pos());
            let wheel: Vec<(egui::MouseWheelUnit, Vec2, Modifiers)> = ctx.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        Event::MouseWheel {
                            unit,
                            delta,
                            modifiers,
                            ..
                        } => Some((*unit, *delta, *modifiers)),
                        _ => None,
                    })
                    .collect()
            });
            for (unit, delta, mods) in wheel {
                let lines = match unit {
                    egui::MouseWheelUnit::Line => delta.y,
                    egui::MouseWheelUnit::Point => delta.y / 50.0,
                    egui::MouseWheelUnit::Page => delta.y * 3.0,
                };
                if mods.ctrl || mods.command {
                    self.wheel_nav_accum += lines;
                    while self.wheel_nav_accum >= 1.0 {
                        self.wheel_nav_accum -= 1.0;
                        self.navigate(-1);
                    }
                    while self.wheel_nav_accum <= -1.0 {
                        self.wheel_nav_accum += 1.0;
                        self.navigate(1);
                    }
                } else if lines != 0.0 {
                    self.zoom_by(&ctx, 1.15f32.powf(lines), hover);
                }
            }
            // Touchpad pinch
            let pinch: f32 = ctx.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| {
                        if let Event::Zoom(z) = e {
                            Some(*z)
                        } else {
                            None
                        }
                    })
                    .product()
            });
            if pinch != 1.0 {
                self.zoom_by(&ctx, pinch, hover);
            }
        }

        // Re-read after interaction changes, clamp pan so the image can't be lost off-screen.
        let zoom = self.effective_zoom(&ctx, rect, media_px, is_video);
        let disp = if self.view.zoom.is_some() {
            self.content_size(media_px) * zoom / ppp
        } else {
            disp
        };
        let max_pan = ((disp - rect.size()) * 0.5).max(Vec2::ZERO);
        self.view.pan = self.view.pan.clamp(-max_pan, max_pan);
        let img_rect = Rect::from_center_size(rect.center() + self.view.pan, disp);

        if resp.dragged() {
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if max_pan != Vec2::ZERO && resp.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Grab);
        }

        paint_rotated(ui.painter_at(rect), tex_id, img_rect, self.view.rotation);

        // Zoom indicator when not fitted.
        if self.view.zoom.is_some() && !placeholder {
            let txt = format!("{:.0}%", zoom * 100.0);
            // Top-centre, below the title bar (the info panel lives on the right).
            let galley =
                ui.painter()
                    .layout_no_wrap(txt, FontId::proportional(13.0), Color32::WHITE);
            let pos = rect.center_top() + vec2(-galley.size().x / 2.0, 46.0);
            let r = Rect::from_min_size(pos, galley.size()).expand(4.0);
            ui.painter().rect_filled(r, 4.0, OVERLAY_BG);
            ui.painter()
                .galley(r.min + vec2(4.0, 4.0), galley, Color32::WHITE);
        }

        // First media of the session: size the window to it (unless fullscreen/maximized).
        if !self.sized_window && !placeholder {
            self.sized_window = true;
            self.size_window_to(&ctx, media_px);
        }
    }

    fn size_window_to(&self, ctx: &egui::Context, media_px: Vec2) {
        let (fs, maxed, monitor) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.fullscreen.unwrap_or(false),
                v.maximized.unwrap_or(false),
                v.monitor_size,
            )
        });
        if fs || maxed {
            return;
        }
        let Some(monitor) = monitor else { return };
        let ppp = ctx.pixels_per_point();
        let mut size = self.content_size(media_px) / ppp;
        if self.cfg.show_filmstrip {
            size.y += FILMSTRIP_H;
        }
        let max = monitor * 0.85;
        let scale = (max.x / size.x).min(max.y / size.y).min(1.0);
        let size = (size * scale).max(vec2(640.0, 420.0)).min(monitor);
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        let pos = ((monitor - size) * 0.5).to_pos2();
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
    }

    fn draw_top_bar(&self, ui: &egui::Ui, rect: Rect, visible: f32) {
        if visible <= 0.0 {
            return;
        }
        let Some((path, kind)) = self.current() else {
            return;
        };
        let painter = ui.painter_at(rect);
        let bar = Rect::from_min_size(rect.min, vec2(rect.width(), 36.0));
        painter.rect_filled(bar, 0.0, OVERLAY_BG.gamma_multiply(visible));
        let text_color = Color32::WHITE.gamma_multiply(visible);
        let mut left = file_name(&path);
        if self.slideshow.is_some() {
            left.push_str("   ▶ slideshow");
        }
        painter.text(
            bar.left_center() + vec2(12.0, 0.0),
            Align2::LEFT_CENTER,
            left,
            FontId::proportional(15.0),
            text_color,
        );
        let mut right = format!("{} / {}", self.playlist.index + 1, self.playlist.len());
        if let MediaKind::Video = kind
            && let Some(v) = &self.video
        {
            if !v.state.hwdec_current.is_empty() && v.state.hwdec_current != "no" {
                right = format!("HW:{}   {right}", v.state.hwdec_current);
            } else if v.is_active() {
                right = format!("CPU   {right}");
            }
        }
        painter.text(
            bar.right_center() - vec2(12.0, 0.0),
            Align2::RIGHT_CENTER,
            right,
            FontId::proportional(14.0),
            text_color,
        );
    }

    /// Bottom video controls. Returns true if the pointer is over them.
    fn draw_video_controls(&mut self, ui: &mut egui::Ui, rect: Rect, visible: f32) -> bool {
        let Some(v) = &mut self.video else {
            return false;
        };
        if !v.is_active() || visible <= 0.0 {
            return false;
        }
        let h = 54.0;
        let bar = Rect::from_min_max(pos2(rect.min.x, rect.max.y - h), rect.max);
        let painter = ui.painter_at(bar);
        painter.rect_filled(bar, 0.0, OVERLAY_BG.gamma_multiply(visible));
        let white = Color32::WHITE.gamma_multiply(visible);
        let st = v.state.clone();

        // Seek bar
        let seek = Rect::from_min_max(
            pos2(bar.min.x + 12.0, bar.min.y + 8.0),
            pos2(bar.max.x - 12.0, bar.min.y + 20.0),
        );
        let sresp = ui.interact(seek, egui::Id::unique("seek"), Sense::click_and_drag());
        let track = Rect::from_center_size(seek.center(), vec2(seek.width(), 4.0));
        painter.rect_filled(track, 2.0, Color32::from_gray(90).gamma_multiply(visible));
        let frac = if st.duration > 0.0 {
            (st.position / st.duration).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let filled = Rect::from_min_max(
            track.min,
            pos2(track.min.x + track.width() * frac, track.max.y),
        );
        painter.rect_filled(filled, 2.0, ACCENT.gamma_multiply(visible));
        let knob_r = if sresp.hovered() || sresp.dragged() {
            7.0
        } else {
            5.0
        };
        painter.circle_filled(pos2(filled.max.x, track.center().y), knob_r, white);
        if st.duration > 0.0 {
            if let Some(p) = sresp.interact_pointer_pos() {
                if sresp.is_pointer_button_down_on() || sresp.clicked() {
                    let f = ((p.x - track.min.x) / track.width()).clamp(0.0, 1.0) as f64;
                    v.seek_absolute(f * st.duration);
                    self.seek_drag = true;
                }
            } else {
                self.seek_drag = false;
            }
            if sresp.hovered()
                && let Some(hp) = sresp.hover_pos()
            {
                let f = ((hp.x - track.min.x) / track.width()).clamp(0.0, 1.0) as f64;
                let tip = meta::fmt_time(f * st.duration);
                painter.text(
                    pos2(hp.x, seek.min.y - 10.0),
                    Align2::CENTER_BOTTOM,
                    tip,
                    FontId::proportional(12.0),
                    white,
                );
            }
        }

        // Buttons row
        let row_y = bar.min.y + 38.0;
        let play_rect = Rect::from_center_size(pos2(bar.min.x + 26.0, row_y), vec2(28.0, 24.0));
        let presp = ui.interact(play_rect, egui::Id::unique("play"), Sense::click());
        paint_play_pause(&painter, play_rect.center(), st.paused, white);
        if presp.clicked() {
            v.toggle_pause();
        }
        let time = format!(
            "{} / {}",
            meta::fmt_time(st.position),
            meta::fmt_time(st.duration)
        );
        painter.text(
            pos2(play_rect.max.x + 10.0, row_y),
            Align2::LEFT_CENTER,
            time,
            FontId::monospace(13.0),
            white,
        );

        // Volume slider (right)
        let vol_w = 110.0;
        let vol = Rect::from_min_max(
            pos2(bar.max.x - 12.0 - vol_w, row_y - 8.0),
            pos2(bar.max.x - 12.0, row_y + 8.0),
        );
        let vresp = ui.interact(vol, egui::Id::unique("vol"), Sense::click_and_drag());
        let vtrack = Rect::from_center_size(vol.center(), vec2(vol.width(), 4.0));
        painter.rect_filled(vtrack, 2.0, Color32::from_gray(90).gamma_multiply(visible));
        let vf = if st.mute {
            0.0
        } else {
            (st.volume / 100.0).clamp(0.0, 1.0) as f32
        };
        painter.rect_filled(
            Rect::from_min_max(
                vtrack.min,
                pos2(vtrack.min.x + vtrack.width() * vf, vtrack.max.y),
            ),
            2.0,
            white,
        );
        if let Some(p) = vresp.interact_pointer_pos()
            && vresp.is_pointer_button_down_on()
        {
            let f = ((p.x - vtrack.min.x) / vtrack.width()).clamp(0.0, 1.0) as f64;
            v.set_volume(f * 100.0);
            self.cfg.volume = v.state.volume;
            self.cfg.mute = v.state.mute;
        }
        let mute_rect = Rect::from_center_size(pos2(vol.min.x - 18.0, row_y), vec2(26.0, 24.0));
        let mresp = ui.interact(mute_rect, egui::Id::unique("mute"), Sense::click());
        paint_speaker(
            &painter,
            mute_rect.center(),
            st.mute || st.volume <= 0.0,
            st.volume,
            white,
        );
        if mresp.clicked() {
            let m = !st.mute;
            v.set_mute(m);
            self.cfg.mute = m;
        }

        ui.input(|i| i.pointer.hover_pos())
            .is_some_and(|p| bar.contains(p))
            || self.seek_drag
    }

    fn draw_filmstrip(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_rgb(10, 10, 10));
        let n = self.playlist.len();
        if n == 0 {
            return;
        }
        let mut builder = egui::UiBuilder::new().max_rect(rect);
        builder = builder.layout(egui::Layout::left_to_right(egui::Align::Center));
        let mut child = ui.new_child(builder);
        let scroll_to = self
            .filmstrip_scroll_to_current
            .then_some(self.playlist.index);
        self.filmstrip_scroll_to_current = false;
        let item_w = THUMB_W + 6.0;
        let mut clicked: Option<usize> = None;
        let mut requests: Vec<(PathBuf, MediaKind)> = Vec::new();

        let mut area = egui::ScrollArea::horizontal()
            .id_salt("filmstrip")
            .auto_shrink([false, false]);
        if let Some(i) = scroll_to {
            // Centre the current item.
            let target = (i as f32 * item_w + item_w / 2.0 - rect.width() / 2.0).max(0.0);
            area = area.horizontal_scroll_offset(target);
        }
        area.show_viewport(&mut child, |ui, viewport| {
            let total = vec2(n as f32 * item_w, rect.height() - 12.0);
            let (resp_rect, _) = ui.allocate_exact_size(total, Sense::hover());
            let first = ((viewport.min.x / item_w).floor().max(0.0)) as usize;
            let last = (((viewport.max.x / item_w).ceil()) as usize).min(n);
            // request visible first, then a margin on each side
            let margin = 8;
            let lo = first.saturating_sub(margin);
            let hi = (last + margin).min(n);
            for i in (first..last).chain(lo..first).chain(last..hi) {
                let e = &self.playlist.entries[i];
                requests.push((e.path.clone(), e.kind));
            }
            for i in first..last {
                let e = &self.playlist.entries[i];
                let cell = Rect::from_min_size(
                    pos2(
                        resp_rect.min.x + i as f32 * item_w + 3.0,
                        resp_rect.min.y + 3.0,
                    ),
                    vec2(THUMB_W, total.y - 6.0),
                );
                let id = egui::Id::unique(("thumb", i));
                let r = ui.interact(cell, id, Sense::click());
                if r.clicked() {
                    clicked = Some(i);
                }
                let selected = i == self.playlist.index;
                let p = ui.painter();
                p.rect_filled(
                    cell,
                    4.0,
                    Color32::from_gray(if r.hovered() { 40 } else { 24 }),
                );
                match self.thumbs.get(&e.path) {
                    Some(Thumb::Ready(t)) => {
                        let s = t.size_vec2();
                        let f = (cell.width() / s.x).min(cell.height() / s.y);
                        let ir = Rect::from_center_size(cell.center(), s * f);
                        p.image(
                            t.id(),
                            ir,
                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    Some(Thumb::Failed) => {
                        p.text(
                            cell.center(),
                            Align2::CENTER_CENTER,
                            "?",
                            FontId::proportional(20.0),
                            Color32::GRAY,
                        );
                    }
                    _ => {}
                }
                if e.kind.is_video() {
                    p.text(
                        cell.left_bottom() + vec2(5.0, -4.0),
                        Align2::LEFT_BOTTOM,
                        "▶",
                        FontId::proportional(14.0),
                        Color32::WHITE,
                    );
                }
                if selected {
                    p.rect_stroke(
                        cell,
                        4.0,
                        Stroke::new(2.5, ACCENT),
                        egui::StrokeKind::Inside,
                    );
                }
                r.on_hover_text(file_name(&e.path));
            }
        });
        self.thumbs.request(&requests);
        if let Some(i) = clicked {
            self.playlist.go_to(i);
        }
    }

    fn draw_info(&self, ui: &egui::Ui, rect: Rect) {
        let Some((path, kind)) = self.current() else {
            return;
        };
        let mut rows: Vec<(String, String)> = Vec::new();
        rows.push(("Name".into(), file_name(&path)));
        rows.push((
            "Folder".into(),
            path.parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        ));
        rows.push((
            "Position".into(),
            format!("{} of {}", self.playlist.index + 1, self.playlist.len()),
        ));
        if let Ok(md) = std::fs::metadata(&path) {
            rows.push(("Size".into(), meta::human_size(md.len())));
            if let Ok(t) = md.modified() {
                rows.push(("Modified".into(), fmt_system_time(t)));
            }
        }
        match kind {
            MediaKind::Image(_) => {
                if let Some(Slot::Ready(l)) = self.cache.get(&path) {
                    rows.push((
                        "Dimensions".into(),
                        format!("{} × {}", l.orig_size[0], l.orig_size[1]),
                    ));
                    rows.extend(l.info.iter().cloned());
                }
            }
            MediaKind::Video => {
                if let Some(v) = &self.video {
                    let s = |k: &str| v.prop_string(k);
                    if let (Some(w), Some(h)) = (s("width"), s("height")) {
                        rows.push(("Resolution".into(), format!("{w} × {h}")));
                    }
                    if let Some(fps) = v.prop_f64("container-fps") {
                        rows.push(("Frame rate".into(), format!("{fps:.3} fps")));
                    }
                    if let Some(c) = s("video-codec") {
                        rows.push(("Video codec".into(), c));
                    }
                    if let Some(c) = s("audio-codec-name") {
                        rows.push(("Audio codec".into(), c));
                    }
                    if let Some(f) = s("file-format") {
                        rows.push(("Container".into(), f));
                    }
                    if let Some(b) = v.prop_f64("video-bitrate") {
                        rows.push(("Video bitrate".into(), format!("{:.1} Mb/s", b / 1e6)));
                    }
                    rows.push(("Duration".into(), meta::fmt_time(v.state.duration)));
                    let hw = if v.state.hwdec_current.is_empty() {
                        "no".into()
                    } else {
                        v.state.hwdec_current.clone()
                    };
                    rows.push(("hwdec-current".into(), hw));
                    rows.push((
                        "HW decode setting".into(),
                        if v.hw_enabled() {
                            self.cfg.hwdec.clone()
                        } else {
                            "off".into()
                        },
                    ));
                }
            }
        }
        let w = 340.0;
        let area_rect =
            Rect::from_min_size(pos2(rect.max.x - w - 12.0, rect.min.y + 46.0), vec2(w, 0.0));
        egui::Area::new(egui::Id::unique("info"))
            .fixed_pos(area_rect.min)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(OVERLAY_BG)
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.set_width(w - 20.0);
                        egui::Grid::new("info_grid")
                            .num_columns(2)
                            .spacing([10.0, 4.0])
                            .show(ui, |ui| {
                                for (k, v) in rows {
                                    ui.label(egui::RichText::new(k).color(Color32::GRAY));
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(v).color(Color32::WHITE),
                                        )
                                        .wrap(),
                                    );
                                    ui.end_row();
                                }
                            });
                    });
            });
    }

    fn draw_help(&self, ctx: &egui::Context) {
        const KEYS: &[(&str, &str)] = &[
            ("← / →, PgUp/PgDn", "Previous / next file (wraps)"),
            ("Ctrl+Wheel, mouse back/fwd", "Previous / next file"),
            ("Home / End", "First / last file"),
            ("Space", "Play/pause video · slideshow for images"),
            ("Shift+← / →", "Seek ±5 s"),
            ("Ctrl+Shift+← / →", "Seek ±30 s"),
            (", / .", "Frame step back / forward"),
            ("↑ / ↓,  M", "Volume,  mute"),
            ("L", "Toggle video looping"),
            ("Wheel / pinch", "Zoom at cursor"),
            ("Drag,  middle-click", "Pan,  reset zoom"),
            ("0 / 1 / F,  + / −", "Fit / 100% / fill,  zoom"),
            ("R / Shift+R", "Rotate view CW / CCW"),
            ("Ctrl+S", "Save rotation to file (asks first)"),
            ("Del / Shift+Del", "Trash / delete permanently (asks first)"),
            ("Ctrl+C", "Copy file path"),
            ("Ctrl+E", "Show in file manager"),
            ("O", "Open with default app"),
            ("I / T", "Info panel / filmstrip"),
            ("Ctrl+H", "Toggle hardware video decoding"),
            ("F5", "Reload folder"),
            ("F11, double-click", "Fullscreen"),
            ("Esc / Q", "Leave fullscreen or quit / quit"),
        ];
        egui::Window::new("Keyboard shortcuts")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .vscroll(true)
            .max_height(ctx.content_rect().height() - 60.0)
            .show(ctx, |ui| {
                egui::Grid::new("help")
                    .num_columns(2)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        for (k, d) in KEYS {
                            ui.label(egui::RichText::new(*k).monospace().color(ACCENT));
                            ui.label(*d);
                            ui.end_row();
                        }
                    });
                ui.add_space(6.0);
                ui.label(egui::RichText::new("F1 / ? / Esc to close").weak());
            });
    }

    fn draw_toasts(&mut self, ui: &egui::Ui, rect: Rect) {
        let now = ui.input(|i| i.time);
        self.toasts.retain(|t| t.until > now);
        let mut y = rect.max.y - 80.0;
        for t in self.toasts.iter().rev().take(3) {
            let galley = ui.painter().layout_no_wrap(
                t.text.clone(),
                FontId::proportional(14.0),
                Color32::WHITE,
            );
            let r =
                Rect::from_center_size(pos2(rect.center().x, y), galley.size() + vec2(24.0, 14.0));
            ui.painter()
                .rect_filled(r, 6.0, Color32::from_rgba_premultiplied(20, 20, 20, 225));
            ui.painter()
                .galley(r.min + vec2(12.0, 7.0), galley, Color32::WHITE);
            y -= r.height() + 6.0;
        }
        if let Some(t) = self.toasts.iter().map(|t| t.until).reduce(f64::min) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64((t - now).max(0.0)));
        }
    }

    fn tick_slideshow(&mut self, ctx: &egui::Context) {
        let Some(next) = self.slideshow else { return };
        let now = ctx.input(|i| i.time);
        let is_video = matches!(self.current(), Some((_, MediaKind::Video)));
        if is_video {
            // Let a video play once through, then advance.
            if let Some(v) = &self.video {
                let pos = v.state.position;
                let ended = v.state.duration > 0.0
                    && (pos + 0.25 >= v.state.duration
                        || (pos + 0.5 < self.last_video_pos && self.last_video_pos > 1.0));
                self.last_video_pos = pos;
                if ended {
                    self.navigate(1);
                }
            }
        } else if now >= next {
            self.navigate(1);
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(next - now));
        }
    }
}

impl eframe::App for MviewApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.input(|i| self.cache.set_max_side(i.max_texture_side));

        if self.playlist.poll_changes() {
            self.filmstrip_scroll_to_current = true;
        }
        self.cache.pump(&ctx);
        self.thumbs.pump(&ctx);
        if let Some(v) = &mut self.video {
            v.poll();
            for t in std::mem::take(&mut v.toasts) {
                self.toasts.push(Toast {
                    text: t,
                    until: ctx.input(|i| i.time) + 4.0,
                });
            }
        }

        if self.confirm.is_none() && !self.show_help {
            self.handle_input(&ctx);
        } else if self.show_help {
            let close = ctx.input(|i| {
                i.key_pressed(Key::Escape)
                    || i.key_pressed(Key::F1)
                    || i.key_pressed(Key::Questionmark)
            });
            if close {
                self.show_help = false;
            }
        }
        self.tick_slideshow(&ctx);
        self.sync_current(&ctx);

        // Auto-hide overlay chrome after inactivity.
        let now = ctx.input(|i| i.time);
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        if pointer != self.last_pointer || ctx.input(|i| i.pointer.any_down()) {
            self.last_activity = now;
            self.last_pointer = pointer;
        }
        let paused_video = self
            .video
            .as_ref()
            .is_some_and(|v| v.is_active() && v.state.paused);
        let idle = now - self.last_activity;
        let chrome_visible = if idle < UI_HIDE_AFTER || paused_video || self.confirm.is_some() {
            1.0
        } else {
            (1.0 - (idle - UI_HIDE_AFTER) as f32 / 0.3).max(0.0)
        };
        if chrome_visible > 0.0 && !paused_video {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                (UI_HIDE_AFTER - idle).max(0.03),
            ));
        }
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if fullscreen && chrome_visible <= 0.0 {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG))
            .show(ui, |ui| {
                let full = ui.max_rect();
                let (media_rect, strip_rect) = if self.cfg.show_filmstrip {
                    let split = full.max.y - FILMSTRIP_H;
                    (
                        Rect::from_min_max(full.min, pos2(full.max.x, split)),
                        Some(Rect::from_min_max(pos2(full.min.x, split), full.max)),
                    )
                } else {
                    (full, None)
                };
                self.draw_media(ui, frame, media_rect);
                self.draw_top_bar(ui, media_rect, chrome_visible);
                let over_controls = self.draw_video_controls(ui, media_rect, chrome_visible);
                if over_controls {
                    self.last_activity = now;
                }
                if let Some(r) = strip_rect {
                    self.draw_filmstrip(ui, r);
                }
                if self.cfg.show_info {
                    self.draw_info(ui, media_rect);
                }
                self.draw_toasts(ui, media_rect);
            });

        if self.show_help {
            self.draw_help(&ctx);
        }
        self.confirm_dialog(&ctx);
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {}

    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        if let Some(v) = &mut self.video {
            self.cfg.volume = v.state.volume;
            self.cfg.mute = v.state.mute;
            v.shutdown(gl);
        }
        self.cfg.save();
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BG.to_normalized_gamma_f32()
    }
}

/// Vector icons (the default fonts lack media glyphs).
fn paint_play_pause(p: &egui::Painter, c: Pos2, paused: bool, color: Color32) {
    if paused {
        let pts = vec![
            c + vec2(-6.0, -8.0),
            c + vec2(9.0, 0.0),
            c + vec2(-6.0, 8.0),
        ];
        p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
    } else {
        p.rect_filled(
            Rect::from_min_size(c + vec2(-7.0, -8.0), vec2(5.0, 16.0)),
            1.0,
            color,
        );
        p.rect_filled(
            Rect::from_min_size(c + vec2(2.0, -8.0), vec2(5.0, 16.0)),
            1.0,
            color,
        );
    }
}

fn paint_speaker(p: &egui::Painter, c: Pos2, muted: bool, volume: f64, color: Color32) {
    let o = c + vec2(-5.0, 0.0);
    let body = vec![
        o + vec2(-5.0, -3.0),
        o + vec2(-1.0, -3.0),
        o + vec2(4.0, -7.0),
        o + vec2(4.0, 7.0),
        o + vec2(-1.0, 3.0),
        o + vec2(-5.0, 3.0),
    ];
    p.add(egui::Shape::convex_polygon(body, color, Stroke::NONE));
    let stroke = Stroke::new(1.6, color);
    if muted {
        let x = o + vec2(10.0, 0.0);
        p.line_segment([x + vec2(-3.0, -3.0), x + vec2(3.0, 3.0)], stroke);
        p.line_segment([x + vec2(-3.0, 3.0), x + vec2(3.0, -3.0)], stroke);
        return;
    }
    let arcs = if volume < 34.0 {
        1
    } else if volume < 67.0 {
        2
    } else {
        3
    };
    for k in 0..arcs {
        let r = 4.0 + k as f32 * 3.5;
        let pts: Vec<Pos2> = (0..=8)
            .map(|i| {
                let a = -0.9 + 1.8 * i as f32 / 8.0;
                o + vec2(4.0 + r * a.cos(), r * a.sin())
            })
            .collect();
        p.add(egui::Shape::line(pts, stroke));
    }
}

/// Paint a texture into `rect` rotated by `q` quarter turns clockwise.
fn paint_rotated(painter: egui::Painter, tex: egui::TextureId, rect: Rect, q: u8) {
    let base = [
        pos2(0.0, 0.0),
        pos2(1.0, 0.0),
        pos2(1.0, 1.0),
        pos2(0.0, 1.0),
    ];
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    let mut mesh = egui::Mesh::with_texture(tex);
    for (i, c) in corners.iter().enumerate() {
        let uv = base[(i + 4 - q as usize % 4) % 4];
        mesh.vertices.push(egui::epaint::Vertex {
            pos: *c,
            uv,
            color: Color32::WHITE,
        });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn fmt_system_time(t: std::time::SystemTime) -> String {
    match jiff::Timestamp::try_from(t) {
        Ok(ts) => ts
            .to_zoned(jiff::tz::TimeZone::system())
            .strftime("%Y-%m-%d %H:%M:%S")
            .to_string(),
        Err(_) => "?".into(),
    }
}
