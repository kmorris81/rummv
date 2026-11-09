//! Filmstrip thumbnails using the freedesktop thumbnail cache (~/.cache/thumbnails), which
//! Dolphin/KDE share. Missing thumbnails are generated and written back to the cache.
use crate::decode;
use crate::media::{ImageFormat, MediaKind};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

pub const SIDE: u32 = 256;

pub enum Thumb {
    Pending,
    Ready(egui::TextureHandle),
    Failed,
}

struct Queue {
    jobs: Mutex<VecDeque<(PathBuf, MediaKind)>>,
    cv: Condvar,
}

pub struct Thumbnailer {
    queue: Arc<Queue>,
    results: crossbeam_channel::Receiver<(PathBuf, Option<image::RgbaImage>)>,
    thumbs: HashMap<PathBuf, Thumb>,
}

impl Thumbnailer {
    pub fn new(ctx: egui::Context, threads: usize) -> Self {
        let queue = Arc::new(Queue {
            jobs: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
        });
        let (tx, rx) = crossbeam_channel::unbounded();
        for i in 0..threads {
            let queue = queue.clone();
            let tx = tx.clone();
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name(format!("mview-thumb-{i}"))
                .spawn(move || {
                    loop {
                        let (path, kind) = {
                            let mut q = queue.jobs.lock().unwrap();
                            loop {
                                if let Some(j) = q.pop_front() {
                                    break j;
                                }
                                q = queue.cv.wait(q).unwrap();
                            }
                        };
                        let img = std::panic::catch_unwind(|| load_or_generate(&path, kind))
                            .ok()
                            .flatten();
                        if tx.send((path, img)).is_err() {
                            return;
                        }
                        ctx.request_repaint();
                    }
                })
                .expect("spawn thumbnailer");
        }
        Self {
            queue,
            results: rx,
            thumbs: HashMap::new(),
        }
    }

    /// Request thumbnails for these entries (in priority order); stale queue entries are dropped.
    pub fn request(&mut self, items: &[(PathBuf, MediaKind)]) {
        let mut q = self.queue.jobs.lock().unwrap();
        q.retain(|(p, _)| items.iter().any(|(ip, _)| ip == p));
        for (p, k) in items {
            if !self.thumbs.contains_key(p) {
                self.thumbs.insert(p.clone(), Thumb::Pending);
                q.push_back((p.clone(), *k));
            } else if matches!(self.thumbs.get(p), Some(Thumb::Pending))
                && !q.iter().any(|(qp, _)| qp == p)
            {
                // was dropped from the queue earlier; it may or may not be in flight, requeue
                q.push_back((p.clone(), *k));
            }
        }
        drop(q);
        self.queue.cv.notify_all();
    }

    pub fn pump(&mut self, ctx: &egui::Context) {
        while let Ok((path, img)) = self.results.try_recv() {
            let t = match img {
                Some(img) => {
                    let (w, h) = img.dimensions();
                    let ci = egui::ColorImage::from_rgba_unmultiplied(
                        [w as usize, h as usize],
                        img.as_raw(),
                    );
                    Thumb::Ready(ctx.load_texture(
                        format!("thumb:{}", path.display()),
                        ci,
                        egui::TextureOptions::LINEAR,
                    ))
                }
                None => Thumb::Failed,
            };
            self.thumbs.insert(path, t);
        }
    }

    pub fn get(&self, path: &Path) -> Option<&Thumb> {
        self.thumbs.get(path)
    }

    pub fn forget(&mut self, path: &Path) {
        self.thumbs.remove(path);
    }
}

fn file_uri(path: &Path) -> Option<String> {
    url::Url::from_file_path(path).ok().map(|u| u.to_string())
}

fn mtime_secs(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

fn cache_root() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("thumbnails"))
}

/// Path of the cached thumbnail for `path` in the given size dir (normal/large/x-large).
pub fn cache_path(path: &Path, size_dir: &str) -> Option<PathBuf> {
    let uri = file_uri(path)?;
    let hash = format!("{:x}", md5::compute(uri.as_bytes()));
    Some(cache_root()?.join(size_dir).join(format!("{hash}.png")))
}

fn load_or_generate(path: &Path, kind: MediaKind) -> Option<image::RgbaImage> {
    let mtime = mtime_secs(path);
    for dir in ["large", "x-large", "normal"] {
        if let Some(p) = cache_path(path, dir)
            && let Some(img) = read_cached(&p, mtime)
        {
            return Some(img);
        }
    }
    let img = match kind {
        MediaKind::Image(fmt) => generate_image(path, fmt)?,
        MediaKind::Video => generate_video(path)?,
    };
    if let (Some(dst), Some(uri), Some(mt)) = (cache_path(path, "large"), file_uri(path), mtime) {
        // Don't thumbnail the thumbnail cache itself.
        if !path.starts_with(cache_root().unwrap_or_default())
            && let Err(e) = write_cached(&dst, &img, &uri, mt)
        {
            log::debug!("write thumb {}: {e}", dst.display());
        }
    }
    Some(img)
}

/// Read a cached PNG, validating `Thumb::MTime` against the source file per the spec.
fn read_cached(p: &Path, mtime: Option<u64>) -> Option<image::RgbaImage> {
    let file = std::fs::File::open(p).ok()?;
    let dec = png::Decoder::new(std::io::BufReader::new(file));
    let reader = dec.read_info().ok()?;
    let info = reader.info();
    let cached_mtime = info
        .uncompressed_latin1_text
        .iter()
        .find(|t| t.keyword == "Thumb::MTime")
        .and_then(|t| t.text.trim().parse::<u64>().ok());
    if let (Some(a), Some(b)) = (cached_mtime, mtime)
        && a != b
    {
        return None; // stale
    }
    drop(reader);
    image::open(p).ok().map(|i| i.into_rgba8())
}

fn write_cached(dst: &Path, img: &image::RgbaImage, uri: &str, mtime: u64) -> anyhow::Result<()> {
    let dir = dst.parent().ok_or_else(|| anyhow::anyhow!("no parent"))?;
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    // Unique per write: two workers may race on the same thumbnail.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dst.with_extension(format!("mview-{}-{seq}.tmp", std::process::id()));
    {
        let f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        let mut enc = png::Encoder::new(f, img.width(), img.height());
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.add_text_chunk("Thumb::URI".into(), uri.into())?;
        enc.add_text_chunk("Thumb::MTime".into(), mtime.to_string())?;
        enc.add_text_chunk("Software".into(), "mview".into())?;
        let mut w = enc.write_header()?;
        w.write_image_data(img.as_raw())?;
    }
    std::fs::rename(&tmp, dst)?;
    Ok(())
}

fn generate_image(path: &Path, fmt: ImageFormat) -> Option<image::RgbaImage> {
    decode::thumbnail(path, fmt, SIDE).ok()
}

/// Video thumbnail via ffmpegthumbnailer (what KDE uses) or ffmpeg, whichever is on PATH.
fn generate_video(path: &Path) -> Option<image::RgbaImage> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("mview-thumb-{}-{seq}.png", std::process::id()));
    let side = SIDE.to_string();
    let scale = format!("scale={side}:{side}:force_original_aspect_ratio=decrease");
    let attempts: Vec<Vec<std::ffi::OsString>> = vec![
        // ffmpegthumbnailer (what KDE uses)
        vec![
            "ffmpegthumbnailer".into(),
            "-s".into(),
            side.clone().into(),
            "-q".into(),
            "8".into(),
            "-t".into(),
            "10%".into(),
            "-i".into(),
            path.into(),
            "-o".into(),
            tmp.clone().into(),
        ],
        // ffmpeg: a representative frame a few seconds in
        vec![
            "ffmpeg".into(),
            "-nostdin".into(),
            "-loglevel".into(),
            "error".into(),
            "-y".into(),
            "-ss".into(),
            "3".into(),
            "-i".into(),
            path.into(),
            "-frames:v".into(),
            "1".into(),
            "-vf".into(),
            format!("thumbnail,{scale}").into(),
            tmp.clone().into(),
        ],
        // very short clips: first frame
        vec![
            "ffmpeg".into(),
            "-nostdin".into(),
            "-loglevel".into(),
            "error".into(),
            "-y".into(),
            "-i".into(),
            path.into(),
            "-frames:v".into(),
            "1".into(),
            "-vf".into(),
            scale.clone().into(),
            tmp.clone().into(),
        ],
    ];
    let mut img = None;
    for args in attempts {
        let _ = std::fs::remove_file(&tmp);
        let ran = std::process::Command::new(&args[0])
            .args(&args[1..])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        // Tools can exit 0 without writing a frame (e.g. seeking past the end): verify output.
        if ran && let Ok(i) = image::open(&tmp) {
            img = Some(i.into_rgba8());
            break;
        }
    }
    let _ = std::fs::remove_file(&tmp);
    img
}
