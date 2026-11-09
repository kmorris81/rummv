//! The folder's media list: natural sort (like Dolphin), wrap-around navigation, live updates.
use crate::media::{self, MediaKind};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub kind: MediaKind,
}

pub struct Playlist {
    pub dir: PathBuf,
    pub entries: Vec<Entry>,
    pub index: usize,
    show_hidden: bool,
    _watcher: Option<notify::RecommendedWatcher>,
    events: Option<crossbeam_channel::Receiver<()>>,
}

/// Case-insensitive natural compare of file names, ties broken case-sensitively for stability.
pub fn natural_cmp(a: &Path, b: &Path) -> Ordering {
    let an = a
        .file_name()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    let bn = b
        .file_name()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    natord::compare_ignore_case(&an, &bn).then_with(|| natord::compare(&an, &bn))
}

pub fn scan_dir(dir: &Path, show_hidden: bool) -> Vec<Entry> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<Entry> = rd
        .filter_map(|e| e.ok())
        .filter(|e| {
            // follow symlinks for the is-file check
            std::fs::metadata(e.path())
                .map(|m| m.is_file())
                .unwrap_or(false)
        })
        .map(|e| e.path())
        .filter(|p| {
            show_hidden
                || !p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('.'))
        })
        .filter_map(|p| media::detect(&p).map(|kind| Entry { path: p, kind }))
        .collect();
    v.sort_by(|a, b| natural_cmp(&a.path, &b.path));
    v
}

impl Playlist {
    /// `target` may be a file (start on it) or a directory (start on the first entry).
    pub fn open(target: &Path, show_hidden: bool, watch: bool) -> anyhow::Result<Self> {
        let target = std::fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
        let (dir, start) = if target.is_dir() {
            (target.clone(), None)
        } else {
            let dir = target
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            (dir, Some(target.clone()))
        };
        let mut entries = scan_dir(&dir, show_hidden);
        let mut index = 0;
        if let Some(start) = &start {
            match entries.iter().position(|e| &e.path == start) {
                Some(i) => index = i,
                None => {
                    // Opened explicitly (e.g. hidden file or odd extension): include it anyway.
                    let kind = media::detect(start).unwrap_or(MediaKind::Video);
                    entries.push(Entry {
                        path: start.clone(),
                        kind,
                    });
                    entries.sort_by(|a, b| natural_cmp(&a.path, &b.path));
                    index = entries.iter().position(|e| &e.path == start).unwrap_or(0);
                }
            }
        }
        let (watcher, events) = if watch {
            start_watcher(&dir)
        } else {
            (None, None)
        };
        Ok(Self {
            dir,
            entries,
            index,
            show_hidden,
            _watcher: watcher,
            events,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.index)
    }

    /// Index `delta` steps away from the current one, wrapping around.
    pub fn offset_index(&self, delta: isize) -> Option<usize> {
        let n = self.entries.len() as isize;
        if n == 0 {
            return None;
        }
        Some((self.index as isize + delta).rem_euclid(n) as usize)
    }

    pub fn step(&mut self, delta: isize) {
        if let Some(i) = self.offset_index(delta) {
            self.index = i;
        }
    }
    pub fn first(&mut self) {
        self.index = 0;
    }
    pub fn last(&mut self) {
        self.index = self.entries.len().saturating_sub(1);
    }
    pub fn go_to(&mut self, i: usize) {
        if i < self.entries.len() {
            self.index = i;
        }
    }

    /// Remove an entry (after deleting it) keeping the cursor on the "next" file.
    pub fn remove_current(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        self.entries.remove(self.index);
        if self.index >= self.entries.len() {
            self.index = 0; // wrap like navigation does
        }
    }

    /// Non-blocking: returns true if the folder changed and the list was rebuilt.
    pub fn poll_changes(&mut self) -> bool {
        let Some(rx) = &self.events else { return false };
        let mut changed = false;
        while rx.try_recv().is_ok() {
            changed = true;
        }
        if changed {
            self.rescan();
        }
        changed
    }

    pub fn rescan(&mut self) {
        let cur = self.current().map(|e| e.path.clone());
        let old_index = self.index;
        self.entries = scan_dir(&self.dir, self.show_hidden);
        self.index = match cur.and_then(|c| self.entries.iter().position(|e| e.path == c)) {
            Some(i) => i,
            None => old_index.min(self.entries.len().saturating_sub(1)),
        };
    }
}

fn start_watcher(
    dir: &Path,
) -> (
    Option<notify::RecommendedWatcher>,
    Option<crossbeam_channel::Receiver<()>>,
) {
    use notify::{EventKind, RecursiveMode, Watcher};
    let (tx, rx) = crossbeam_channel::unbounded();
    let res = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            // Content modifications don't change the list; creates/removes/renames do.
            let relevant = matches!(
                ev.kind,
                EventKind::Create(_)
                    | EventKind::Remove(_)
                    | EventKind::Modify(notify::event::ModifyKind::Name(_))
            );
            if relevant {
                let _ = tx.send(());
            }
        }
    });
    match res {
        Ok(mut w) => match w.watch(dir, RecursiveMode::NonRecursive) {
            Ok(()) => (Some(w), Some(rx)),
            Err(e) => {
                log::warn!("watch {}: {e}", dir.display());
                (None, None)
            }
        },
        Err(e) => {
            log::warn!("watcher: {e}");
            (None, None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v: Vec<PathBuf> = ["IMG_10.jpg", "img_2.jpg", "IMG_1.jpg", "a.mp4", "B.png"]
            .iter()
            .map(PathBuf::from)
            .collect();
        v.sort_by(|a, b| natural_cmp(a, b));
        let names: Vec<_> = v.iter().map(|p| p.to_str().unwrap()).collect();
        assert_eq!(
            names,
            ["a.mp4", "B.png", "IMG_1.jpg", "img_2.jpg", "IMG_10.jpg"]
        );
    }

    #[test]
    fn scan_wrap_and_remove() {
        let dir = std::env::temp_dir().join(format!("rummv-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for n in [
            "b10.jpg",
            "b2.png",
            "c.mkv",
            "notes.txt",
            ".hidden.jpg",
            "song.mp3",
        ] {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        let mut pl = Playlist::open(&dir.join("b10.jpg"), false, false).unwrap();
        let names = |pl: &Playlist| -> Vec<String> {
            pl.entries
                .iter()
                .map(|e| e.path.file_name().unwrap().to_string_lossy().into())
                .collect()
        };
        assert_eq!(names(&pl), ["b2.png", "b10.jpg", "c.mkv"]);
        assert_eq!(pl.index, 1);
        pl.step(1);
        assert_eq!(pl.index, 2);
        pl.step(1);
        assert_eq!(pl.index, 0, "wraps forward");
        pl.step(-1);
        assert_eq!(pl.index, 2, "wraps backward");
        pl.remove_current();
        assert_eq!(pl.index, 0, "removing last wraps to first");
        assert_eq!(pl.len(), 2);

        let pl = Playlist::open(&dir.join(".hidden.jpg"), false, false).unwrap();
        assert_eq!(
            pl.current().unwrap().path.file_name().unwrap(),
            ".hidden.jpg"
        );

        let pl = Playlist::open(&dir, true, false).unwrap();
        assert_eq!(pl.index, 0);
        assert_eq!(pl.len(), 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
