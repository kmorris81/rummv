//! Embedded libmpv player. mpv renders each frame into an OpenGL texture that we own;
//! egui draws that texture like any image (so zoom/pan work on video too).
//!
//! One mpv core + render context lives for the whole session. Switching files is a
//! `loadfile`, leaving a video is `stop`. That avoids repeatedly creating/destroying GPU
//! interop resources.
use crate::slowmo::{self, SlowMo};
use glow::HasContext;
use libmpv2::{
    Format, Mpv,
    events::Event,
    render::{OpenGLInitParams, RenderContext, RenderParam, RenderParamApiType},
};
use std::ffi::{CString, c_void};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

type GetProc = Arc<dyn Fn(&std::ffi::CStr) -> *const c_void + Send + Sync>;

fn get_proc_address(ctx: &GetProc, name: &str) -> *mut c_void {
    match CString::new(name) {
        Ok(c) => ctx(&c) as *mut c_void,
        Err(_) => std::ptr::null_mut(),
    }
}

#[derive(Debug, Default, Clone)]
pub struct VideoState {
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    pub volume: f64,
    pub mute: bool,
    /// Display size (after aspect ratio / rotation).
    pub size: [i32; 2],
    pub loaded: bool,
    pub hwdec_current: String,
}

struct GlTarget {
    fbo: glow::Framebuffer,
    tex: glow::Texture,
    tex_id: egui::TextureId,
    size: [i32; 2],
}

pub struct VideoPlayer {
    mpv: &'static Mpv,
    render: RenderContext<'static>,
    gl: Arc<glow::Context>,
    target: Option<GlTarget>,
    frame_ready: Arc<AtomicBool>,
    pub state: VideoState,
    current: Option<PathBuf>,
    hw_enabled: bool,
    hwdec_value: String,
    /// Set if the current file failed with hwdec on and we retried in software.
    sw_fallback_used: bool,
    pub error: Option<String>,
    /// One-shot user-facing messages (fallback notices etc).
    pub toasts: Vec<String>,
    /// True if the user has something drawn this session (texture holds a frame).
    has_frame: bool,
    /// Play iPhone slow-motion sections slowly (user setting).
    slowmo_enabled: bool,
    /// Slow-motion plan for the current file, once known.
    slowmo: Option<SlowMo>,
    slowmo_checked: bool,
    /// Last `speed` we set on mpv.
    speed: f64,
}

pub struct VideoInit {
    pub volume: f64,
    pub mute: bool,
    pub hw_enabled: bool,
    pub hwdec_value: String,
    pub loop_video: bool,
    pub slowmo: bool,
    pub extra_options: std::collections::BTreeMap<String, String>,
}

impl VideoPlayer {
    pub fn new(cc: &eframe::CreationContext<'_>, init: VideoInit) -> anyhow::Result<Self> {
        let gl = cc
            .gl
            .clone()
            .ok_or_else(|| anyhow::anyhow!("glow backend required"))?;
        let get_proc: GetProc = cc
            .get_proc_address
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no GL get_proc_address"))?;

        // libmpv requires LC_NUMERIC=C for parsing floats.
        unsafe {
            libc_setlocale_numeric_c();
        }
        let hwdec = if init.hw_enabled {
            init.hwdec_value.clone()
        } else {
            "no".into()
        };
        let mpv = Mpv::with_initializer(|i| {
            i.set_option("vo", "libmpv")?;
            i.set_option("hwdec", hwdec.as_str())?;
            i.set_option("idle", "yes")?;
            i.set_option("keep-open", "yes")?;
            i.set_option("loop-file", if init.loop_video { "inf" } else { "no" })?;
            i.set_option("volume", init.volume)?;
            i.set_option("mute", if init.mute { "yes" } else { "no" })?;
            i.set_option("input-default-bindings", "no")?;
            i.set_option("input-vo-keyboard", "no")?;
            i.set_option("osc", "no")?;
            // RUMMV_MPV_LOG=v (or debug, info...) prints mpv's own log to stderr for diagnosis.
            match std::env::var("RUMMV_MPV_LOG") {
                Ok(level) if !level.is_empty() => {
                    i.set_option("terminal", "yes")?;
                    i.set_option("msg-level", format!("all={level}").as_str())?;
                }
                _ => i.set_option("terminal", "no")?,
            }
            i.set_option("config", "no")?;
            i.set_option("ytdl", "no")?;
            i.set_option("audio-client-name", "rummv")?;
            i.set_option("sub-auto", "fuzzy")?;
            i.set_option("hr-seek", "yes")?;
            // Don't let mpv_render_context_render block the UI thread waiting for vsync timing.
            i.set_option("video-timing-offset", 0.0)?;
            // The render API only offers mpv's legacy `gpu` renderer, whose default HDR->SDR
            // curve blows out HLG/PQ footage (e.g. iPhone video). Hable is much closer to what
            // standalone mpv (gpu-next) produces. Only affects HDR; `[mpv]` can override it.
            i.set_option("tone-mapping", "hable")?;
            for (k, v) in &init.extra_options {
                if let Err(e) = i.set_option(k, v.as_str()) {
                    log::warn!("mpv option {k}={v}: {e}");
                }
            }
            Ok(())
        })
        .map_err(|e| anyhow::anyhow!("mpv init: {e}"))?;
        // One player per process; leaking gives the render context a 'static borrow.
        let mpv: &'static Mpv = Box::leak(Box::new(mpv));
        {
            let (name, id) = ("hwdec-current", 1);
            let _ = mpv.observe_property(name, Format::String, id);
        }

        let mut params: Vec<RenderParam<GetProc>> = vec![
            RenderParam::ApiType(RenderParamApiType::OpenGl),
            RenderParam::InitParams(OpenGLInitParams {
                get_proc_address,
                ctx: get_proc,
            }),
        ];
        // Needed for VA-API interop under Wayland (Intel/AMD); harmless elsewhere.
        if let Some(wl) = wayland_display(cc) {
            params.push(RenderParam::WaylandDisplay(wl));
        }
        let mut render = mpv
            .create_render_context(params)
            .map_err(|e| anyhow::anyhow!("mpv render context: {e}"))?;

        let frame_ready = Arc::new(AtomicBool::new(true));
        {
            let ready = frame_ready.clone();
            let ctx = cc.egui_ctx.clone();
            render.set_update_callback(move || {
                ready.store(true, Ordering::Release);
                ctx.request_repaint();
            });
        }

        Ok(Self {
            mpv,
            render,
            gl,
            target: None,
            frame_ready,
            state: VideoState {
                volume: init.volume,
                mute: init.mute,
                ..Default::default()
            },
            current: None,
            hw_enabled: init.hw_enabled,
            hwdec_value: init.hwdec_value,
            sw_fallback_used: false,
            error: None,
            toasts: Vec::new(),
            has_frame: false,
            slowmo_enabled: init.slowmo,
            slowmo: None,
            slowmo_checked: false,
            speed: 1.0,
        })
    }

    pub fn load(&mut self, path: &Path) {
        self.error = None;
        self.has_frame = false;
        self.state.loaded = false;
        self.state.position = 0.0;
        self.state.duration = 0.0;
        self.state.size = [0, 0];
        self.reset_slowmo();
        if self.sw_fallback_used {
            // Previous file needed software decode; give hardware another chance on this one.
            self.sw_fallback_used = false;
            self.apply_hwdec();
        }
        self.current = Some(path.to_path_buf());
        let p = path.to_string_lossy();
        if let Err(e) = self.mpv.command("loadfile", &[&p, "replace"]) {
            self.error = Some(format!("loadfile: {e}"));
        }
        let _ = self.mpv.set_property("pause", false);
    }

    pub fn stop(&mut self) {
        if self.current.take().is_some() {
            let _ = self.mpv.command("stop", &[]);
        }
        self.state.loaded = false;
        self.has_frame = false;
        self.error = None;
        self.reset_slowmo();
    }

    fn reset_slowmo(&mut self) {
        if self.slowmo.take().is_some() {
            let _ = self.mpv.set_property("audio-pitch-correction", true);
        }
        self.slowmo_checked = false;
        self.set_speed(1.0);
    }

    fn set_speed(&mut self, speed: f64) {
        if speed != self.speed {
            log::debug!(
                "speed {} -> {speed} at {:.3}s",
                self.speed,
                self.state.position
            );
            self.speed = speed;
            let _ = self.mpv.set_property("speed", speed);
        }
    }

    /// Slow-motion plan for the current video (whether or not it's enabled).
    pub fn slowmo(&self) -> Option<&SlowMo> {
        self.slowmo.as_ref()
    }
    pub fn slowmo_enabled(&self) -> bool {
        self.slowmo_enabled
    }
    pub fn set_slowmo_enabled(&mut self, on: bool) {
        self.slowmo_enabled = on;
    }

    /// Work out whether the loaded file is an iPhone slow-motion clip.
    fn detect_slowmo(&self) -> Option<SlowMo> {
        let path = self.current.as_ref()?;
        if let Some(aae) = slowmo::find_sidecar(path) {
            match slowmo::parse_sidecar(&aae) {
                Ok(plan) => return plan,
                Err(e) => log::warn!("{}: {e}", aae.display()),
            }
        }
        // No usable Photos edit: Apple's default for its own high-frame-rate captures,
        // unless the clip is tagged to play at full frame rate (iOS 18+ 120 fps video).
        let key = |k: &str| self.prop_string(&format!("metadata/by-key/com.apple.quicktime.{k}"));
        if key("make").as_deref() != Some("Apple")
            || key("full-frame-rate-playback-intent").as_deref() == Some("1")
        {
            return None;
        }
        SlowMo::default_for(self.state.duration, self.prop_f64("container-fps")?)
    }

    pub fn is_active(&self) -> bool {
        self.current.is_some()
    }

    pub fn toggle_pause(&mut self) {
        let _ = self.mpv.command("cycle", &["pause"]);
    }
    pub fn seek_relative(&mut self, secs: f64) {
        let _ = self
            .mpv
            .command("seek", &[&format!("{secs}"), "relative+exact"]);
    }
    pub fn seek_absolute(&mut self, secs: f64) {
        let _ = self
            .mpv
            .command("seek", &[&format!("{secs:.3}"), "absolute+exact"]);
    }
    pub fn frame_step(&mut self, forward: bool) {
        let _ = self.mpv.command(
            if forward {
                "frame-step"
            } else {
                "frame-back-step"
            },
            &[],
        );
    }
    pub fn set_volume(&mut self, v: f64) {
        let v = v.clamp(0.0, 130.0);
        self.state.volume = v;
        let _ = self.mpv.set_property("volume", v);
        if self.state.mute && v > 0.0 {
            self.set_mute(false);
        }
    }
    pub fn set_mute(&mut self, m: bool) {
        self.state.mute = m;
        let _ = self.mpv.set_property("mute", m);
    }
    pub fn set_loop(&mut self, on: bool) {
        let _ = self
            .mpv
            .set_property("loop-file", if on { "inf" } else { "no" });
    }

    pub fn hw_enabled(&self) -> bool {
        self.hw_enabled
    }
    pub fn toggle_hw(&mut self) {
        self.hw_enabled = !self.hw_enabled;
        self.sw_fallback_used = false;
        self.apply_hwdec();
    }
    fn apply_hwdec(&mut self) {
        let v = if self.hw_enabled && !self.sw_fallback_used {
            self.hwdec_value.as_str()
        } else {
            "no"
        };
        // Changing hwdec at runtime re-initialises the decoder in place (no reload needed).
        let _ = self.mpv.set_property("hwdec", v);
    }

    /// String property for the info overlay.
    pub fn prop_string(&self, name: &str) -> Option<String> {
        self.mpv
            .get_property::<String>(name)
            .ok()
            .filter(|s| !s.is_empty())
    }
    pub fn prop_f64(&self, name: &str) -> Option<f64> {
        self.mpv.get_property::<f64>(name).ok()
    }

    /// Drain mpv events and refresh cached state. Call once per frame.
    pub fn poll(&mut self) {
        while let Some(ev) = self.mpv.wait_event(0.0) {
            match ev {
                Ok(Event::FileLoaded) => self.state.loaded = true,
                Ok(Event::PropertyChange {
                    name: "hwdec-current",
                    change: libmpv2::events::PropertyData::Str(s),
                    ..
                }) => self.state.hwdec_current = s.to_string(),
                Ok(Event::EndFile(reason)) => {
                    // 4 == MPV_END_FILE_REASON_ERROR
                    if reason == 4 && self.current.is_some() {
                        self.on_load_error("playback error".into());
                    }
                }
                Err(e) => {
                    if self.current.is_some() && !self.state.loaded {
                        self.on_load_error(e.to_string());
                    } else {
                        log::debug!("mpv event error: {e}");
                    }
                }
                _ => {}
            }
        }
        if self.current.is_none() {
            return;
        }
        self.state.position = self.prop_f64("time-pos").unwrap_or(0.0);
        self.state.duration = self.prop_f64("duration").unwrap_or(0.0);
        self.state.paused = self.mpv.get_property::<bool>("pause").unwrap_or(false);
        let w = self.mpv.get_property::<i64>("dwidth").unwrap_or(0) as i32;
        let h = self.mpv.get_property::<i64>("dheight").unwrap_or(0) as i32;
        self.state.size = [w, h];

        if self.state.loaded && !self.slowmo_checked && self.state.duration > 0.0 {
            self.slowmo_checked = true;
            self.slowmo = self.detect_slowmo();
            if let Some(s) = &self.slowmo {
                log::info!("slow motion: {}", s.describe());
                // Slowed audio drops in pitch, like Apple's players.
                let _ = self.mpv.set_property("audio-pitch-correction", false);
            }
        }
        let speed = match &self.slowmo {
            Some(s) if self.slowmo_enabled => s.speed_at(self.state.position),
            _ => 1.0,
        };
        self.set_speed(speed);
    }

    fn on_load_error(&mut self, msg: String) {
        if self.hw_enabled && !self.sw_fallback_used {
            // Zero-copy path failed: retry this file with CPU decoding.
            self.sw_fallback_used = true;
            self.apply_hwdec();
            self.toasts
                .push("Hardware decoding failed, retrying with CPU decoding".into());
            if let Some(p) = self.current.clone() {
                let _ = self
                    .mpv
                    .command("loadfile", &[&p.to_string_lossy(), "replace"]);
                let _ = self.mpv.set_property("pause", false);
            }
        } else {
            self.error = Some(format!("Can't play this file: {msg}"));
        }
    }

    /// Render the newest frame (if any) into our texture and return it for drawing.
    /// Must be called from `App::ui` (GL context is current there).
    pub fn render_frame(
        &mut self,
        frame: &mut eframe::Frame,
    ) -> Option<(egui::TextureId, egui::Vec2)> {
        self.current.as_ref()?;
        let [w, h] = self.state.size;
        if w <= 0 || h <= 0 {
            return None;
        }
        self.ensure_target(frame, [w, h]);
        let target = self.target.as_ref()?;
        if self.frame_ready.swap(false, Ordering::AcqRel) {
            let flags = self.render.update().unwrap_or(0);
            // MPV_RENDER_UPDATE_FRAME == 1
            if flags & 1 != 0 || !self.has_frame {
                let fbo_id = target.fbo.0.get() as i32;
                if self.render.render::<GetProc>(fbo_id, w, h, false).is_ok() {
                    self.has_frame = true;
                }
                unsafe {
                    // Hand GL back to egui in a known state.
                    self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                    self.gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
                    self.gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
                }
            }
        }
        self.has_frame
            .then(|| (target.tex_id, egui::vec2(w as f32, h as f32)))
    }

    fn ensure_target(&mut self, frame: &mut eframe::Frame, size: [i32; 2]) {
        let gl = &self.gl;
        if let Some(t) = &self.target
            && t.size == size
        {
            return;
        }
        unsafe {
            let (fbo, tex, tex_id) = match self.target.take() {
                Some(t) => (t.fbo, t.tex, t.tex_id),
                None => {
                    let tex = gl.create_texture().expect("create texture");
                    let fbo = gl.create_framebuffer().expect("create fbo");
                    let tex_id = frame.register_native_glow_texture(tex);
                    (fbo, tex, tex_id)
                }
            };
            // Reallocate storage in place: the egui TextureId stays valid.
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                size[0],
                size[1],
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(tex),
                0,
            );
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            self.target = Some(GlTarget {
                fbo,
                tex,
                tex_id,
                size,
            });
        }
        self.has_frame = false;
        self.frame_ready.store(true, Ordering::Release);
    }

    pub fn shutdown(&mut self, gl: Option<&glow::Context>) {
        let _ = self.mpv.command("stop", &[]);
        if let (Some(gl), Some(t)) = (gl, self.target.take()) {
            unsafe {
                gl.delete_framebuffer(t.fbo);
                gl.delete_texture(t.tex);
            }
        }
    }
}

fn wayland_display(cc: &eframe::CreationContext<'_>) -> Option<*const c_void> {
    use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
    match cc.display_handle().ok()?.as_raw() {
        RawDisplayHandle::Wayland(h) => Some(h.display.as_ptr() as *const c_void),
        _ => None,
    }
}

unsafe extern "C" {
    fn setlocale(category: i32, locale: *const std::ffi::c_char) -> *mut std::ffi::c_char;
}

/// LC_NUMERIC = 4 on glibc and musl.
unsafe fn libc_setlocale_numeric_c() {
    unsafe {
        setlocale(4, c"C".as_ptr());
    }
}
