//! Background image decoding with prefetch of neighbours, and a small texture cache.
use crate::decode::{self, Decoded};
use crate::media::ImageFormat;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub struct Loaded {
    pub frames: Vec<(egui::TextureHandle, Duration)>,
    pub size: egui::Vec2,
    pub orig_size: [usize; 2],
    pub info: Vec<(String, String)>,
}

pub enum Slot {
    Loading,
    Ready(Arc<Loaded>),
    Failed(String),
}

type Job = (PathBuf, ImageFormat);
type JobResult = (PathBuf, Result<Decoded, String>);

#[derive(Default)]
struct QueueState {
    pending: VecDeque<Job>,
    /// Paths a worker is decoding right now (so we don't queue them twice).
    inflight: HashSet<PathBuf>,
}

struct Queue {
    jobs: Mutex<QueueState>,
    cv: Condvar,
}

pub struct ImageCache {
    queue: Arc<Queue>,
    results: crossbeam_channel::Receiver<JobResult>,
    slots: HashMap<PathBuf, Slot>,
    wanted: HashSet<PathBuf>,
    max_side: Arc<AtomicUsize>,
}

impl ImageCache {
    pub fn new(ctx: egui::Context, threads: usize) -> Self {
        let queue = Arc::new(Queue {
            jobs: Mutex::new(QueueState::default()),
            cv: Condvar::new(),
        });
        let (tx, rx) = crossbeam_channel::unbounded();
        let max_side = Arc::new(AtomicUsize::new(8192));
        for i in 0..threads {
            let queue = queue.clone();
            let tx = tx.clone();
            let ctx = ctx.clone();
            let max_side = max_side.clone();
            std::thread::Builder::new()
                .name(format!("rummv-decode-{i}"))
                .spawn(move || {
                    loop {
                        let (path, fmt) = {
                            let mut q = queue.jobs.lock().unwrap();
                            loop {
                                if let Some(j) = q.pending.pop_front() {
                                    q.inflight.insert(j.0.clone());
                                    break j;
                                }
                                q = queue.cv.wait(q).unwrap();
                            }
                        };
                        let ms = max_side.load(Ordering::Relaxed);
                        let res = std::panic::catch_unwind(|| decode::decode(&path, fmt, ms))
                            .map_err(|_| "decoder panicked".to_string())
                            .and_then(|r| r.map_err(|e| format!("{e:#}")));
                        queue.jobs.lock().unwrap().inflight.remove(&path);
                        if tx.send((path, res)).is_err() {
                            return;
                        }
                        ctx.request_repaint();
                    }
                })
                .expect("spawn decoder");
        }
        Self {
            queue,
            results: rx,
            slots: HashMap::new(),
            wanted: HashSet::new(),
            max_side,
        }
    }

    pub fn set_max_side(&self, side: usize) {
        self.max_side.store(side.max(256), Ordering::Relaxed);
    }

    /// Declare which images should be resident, highest priority first
    /// (current, next, prev, next+1, ...). Everything else is evicted.
    pub fn want(&mut self, items: &[(PathBuf, ImageFormat)]) {
        self.wanted = items.iter().map(|(p, _)| p.clone()).collect();
        self.slots.retain(|p, _| self.wanted.contains(p));
        let mut q = self.queue.jobs.lock().unwrap();
        // Drop stale requests; re-add (in priority order) whatever is still wanted.
        q.pending.clear();
        for (p, f) in items {
            let needs_job = match self.slots.get(p) {
                None => true,
                Some(Slot::Loading) => !q.inflight.contains(p),
                _ => false,
            };
            if needs_job {
                self.slots.insert(p.clone(), Slot::Loading);
                q.pending.push_back((p.clone(), *f));
            }
        }
        drop(q);
        self.queue.cv.notify_all();
    }

    /// Force a reload (file changed on disk / was rewritten).
    pub fn invalidate(&mut self, path: &Path) {
        self.slots.remove(path);
    }

    /// Move finished decodes onto the GPU. Call every frame.
    pub fn pump(&mut self, ctx: &egui::Context) {
        while let Ok((path, res)) = self.results.try_recv() {
            if !self.wanted.contains(&path) {
                continue;
            }
            if let Some(Slot::Ready(_)) = self.slots.get(&path) {
                continue; // duplicate decode
            }
            let slot = match res {
                Ok(d) => Slot::Ready(Arc::new(upload(ctx, &path, d))),
                Err(e) => Slot::Failed(e),
            };
            self.slots.insert(path, slot);
        }
    }

    pub fn get(&self, path: &Path) -> Option<&Slot> {
        self.slots.get(path)
    }
}

fn upload(ctx: &egui::Context, path: &Path, d: Decoded) -> Loaded {
    let size = d.size();
    let opts = egui::TextureOptions {
        magnification: egui::TextureFilter::Linear,
        minification: egui::TextureFilter::Linear,
        wrap_mode: egui::TextureWrapMode::ClampToEdge,
        // Mipmaps keep heavily downscaled photos from shimmering/aliasing.
        mipmap_mode: Some(egui::TextureFilter::Linear),
    };
    let name = path.to_string_lossy();
    let frames = d
        .frames
        .into_iter()
        .enumerate()
        .map(|(i, f)| {
            (
                ctx.load_texture(format!("{name}#{i}"), f.image, opts),
                f.delay,
            )
        })
        .collect();
    Loaded {
        frames,
        size: egui::vec2(size[0] as f32, size[1] as f32),
        orig_size: d.orig_size,
        info: d.info,
    }
}
