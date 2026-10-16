//! Persistent folder size index.
//!
//! * Sizes are measured lazily, only for folders that are actually shown, by a small pool
//!   of worker threads (each walk is parallel through jwalk, and per-file stat calls run
//!   inside jwalk's own workers).
//! * Results are stored on disk (%LOCALAPPDATA%\Alexandria\folder-sizes.json) and shown
//!   instantly on the next start. A cached folder is only re-measured when it is marked
//!   stale (a file operation or a manual refresh touched it), when its own modified time
//!   changed, or when the measurement is older than `TTL_SECS`. While a re-measure runs,
//!   the old value stays on screen.
//! * The newest request is served first, and navigating away only drops work that has not
//!   started yet - running walks finish and are cached.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use eframe::egui;
use serde::{Deserialize, Serialize};

const TTL_SECS: u64 = 24 * 60 * 60;
const MAX_ENTRIES: usize = 100_000;
const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Serialize, Deserialize)]
struct Rec {
    size: u64,
    scanned: u64, // unix seconds of the measurement
    mtime: u64,   // folder's own modified time at measurement
    #[serde(default)]
    stale: bool,
    #[serde(skip)]
    verified: bool, // checked during this session
}

#[derive(Serialize, Deserialize)]
struct Saved {
    version: u32,
    items: Vec<(String, Rec)>,
}

#[derive(Default)]
struct State {
    done: HashMap<PathBuf, Rec>,
    pending: HashSet<PathBuf>, // queued or running
    queue: Vec<PathBuf>,       // LIFO: newest (currently visible) first
    dirty: bool,
    touch_gen: u64,
}

struct Shared {
    st: Mutex<State>,
    cv: Condvar,
}

pub struct SizeIndex {
    sh: Arc<Shared>,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn dir_mtime(p: &Path) -> u64 {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

fn cache_file() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("Alexandria").join("folder-sizes.json"))
}

fn walk(root: &Path) -> u64 {
    let walker = jwalk::WalkDirGeneric::<((), u64)>::new(root)
        .skip_hidden(false)
        .follow_links(false)
        .process_read_dir(|_depth, _path, _state, children| {
            // runs on jwalk's worker threads: stat every file in parallel
            for child in children.iter_mut().flatten() {
                if child.file_type().is_file() {
                    child.client_state = child.metadata().map(|m| m.len()).unwrap_or(0);
                }
            }
        });
    walker.into_iter().flatten().map(|de| de.client_state).sum()
}

fn worker(sh: Arc<Shared>, ctx: egui::Context) {
    loop {
        let (path, old, gen) = {
            let Ok(mut s) = sh.st.lock() else { return };
            while s.queue.is_empty() {
                s = match sh.cv.wait(s) {
                    Ok(s) => s,
                    Err(_) => return,
                };
            }
            let p = s.queue.pop().unwrap();
            let old = s.done.get(&p).cloned();
            (p, old, s.touch_gen)
        };
        let cur = dir_mtime(&path);
        let now = now_secs();
        let need = match &old {
            None => true,
            Some(r) => r.stale || r.mtime != cur || now.saturating_sub(r.scanned) > TTL_SECS,
        };
        let size = if need { Some(walk(&path)) } else { None };
        if let Ok(mut s) = sh.st.lock() {
            s.pending.remove(&path);
            let touched_meanwhile = s.touch_gen != gen; // something changed while we were scanning
            match size {
                Some(size) => {
                    s.done.insert(
                        path,
                        Rec { size, scanned: now, mtime: cur, stale: touched_meanwhile, verified: !touched_meanwhile },
                    );
                    s.dirty = true;
                }
                None => {
                    if let Some(r) = s.done.get_mut(&path) {
                        r.verified = true; // cache entry confirmed up to date
                    }
                }
            }
        }
        ctx.request_repaint();
    }
}

impl SizeIndex {
    pub fn new(ctx: &egui::Context) -> Self {
        let mut st = State::default();
        if let Some(saved) = cache_file()
            .and_then(|f| fs::read_to_string(f).ok())
            .and_then(|s| serde_json::from_str::<Saved>(&s).ok())
        {
            if saved.version == FORMAT_VERSION {
                st.done = saved.items.into_iter().map(|(k, r)| (PathBuf::from(k), r)).collect();
            }
        }
        let sh = Arc::new(Shared { st: Mutex::new(st), cv: Condvar::new() });
        let workers = thread::available_parallelism().map_or(2, |n| n.get() / 2).clamp(2, 4);
        for _ in 0..workers {
            let (sh2, c) = (sh.clone(), ctx.clone());
            thread::spawn(move || worker(sh2, c));
        }
        SizeIndex { sh }
    }

    fn enqueue(&self, s: &mut State, path: &Path) {
        if s.pending.insert(path.to_path_buf()) {
            s.queue.push(path.to_path_buf());
            self.sh.cv.notify_one();
        }
    }

    /// Cached size (possibly from a previous session) or `None` if never measured.
    /// Unverified or missing entries are queued for (re)measurement in the background.
    pub fn get(&self, path: &Path) -> Option<u64> {
        let mut s = self.sh.st.lock().ok()?;
        let hit = s.done.get(path).map(|r| (r.size, r.verified));
        match hit {
            Some((size, verified)) => {
                if !verified {
                    self.enqueue(&mut s, path);
                }
                Some(size)
            }
            None => {
                self.enqueue(&mut s, path);
                None
            }
        }
    }

    /// A file operation changed `path`: it and every cached ancestor must be re-measured.
    pub fn touch(&self, path: &Path) {
        if let Ok(mut s) = self.sh.st.lock() {
            s.touch_gen += 1;
            for a in path.ancestors() {
                if let Some(r) = s.done.get_mut(a) {
                    r.stale = true;
                    r.verified = false;
                }
            }
            s.dirty = true;
        }
    }

    /// Manual refresh: everything cached at or below `path` is re-measured when next shown.
    pub fn mark_subtree_stale(&self, path: &Path) {
        if let Ok(mut s) = self.sh.st.lock() {
            s.touch_gen += 1;
            for (k, r) in s.done.iter_mut() {
                if k.starts_with(path) {
                    r.stale = true;
                    r.verified = false;
                }
            }
            for a in path.ancestors() {
                if let Some(r) = s.done.get_mut(a) {
                    r.stale = true;
                    r.verified = false;
                }
            }
            s.dirty = true;
        }
    }

    /// Navigation: drop queued work that has not started. Running walks finish and are cached.
    pub fn cancel_pending(&self) {
        if let Ok(mut s) = self.sh.st.lock() {
            let queued: Vec<PathBuf> = s.queue.drain(..).collect();
            for p in queued {
                s.pending.remove(&p);
            }
        }
    }

    /// Writes the index to disk if it changed (called from eframe's periodic save and on exit).
    pub fn save(&self) {
        let items: Vec<(String, Rec)> = {
            let Ok(mut s) = self.sh.st.lock() else { return };
            if !s.dirty {
                return;
            }
            s.dirty = false;
            if s.done.len() > MAX_ENTRIES {
                let mut by_age: Vec<(u64, PathBuf)> = s.done.iter().map(|(k, r)| (r.scanned, k.clone())).collect();
                by_age.sort_unstable();
                let excess = s.done.len() - MAX_ENTRIES * 9 / 10;
                for (_, k) in by_age.into_iter().take(excess) {
                    s.done.remove(&k);
                }
            }
            s.done
                .iter()
                .map(|(k, r)| (k.to_string_lossy().into_owned(), r.clone()))
                .collect()
        };
        let Some(file) = cache_file() else { return };
        if let Some(dir) = file.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string(&Saved { version: FORMAT_VERSION, items }) {
            let tmp = file.with_extension("json.tmp");
            if fs::write(&tmp, json).is_ok() {
                let _ = fs::rename(&tmp, &file); // atomic replace
            }
        }
    }
}
