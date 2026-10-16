//! Filesystem helpers: directory loading, jwalk recursive search, file operations.
//! Everything slow runs on worker threads and reports back through channels.

use std::{
    cmp::Ordering,
    fs, io,
    iter::Peekable,
    path::{Path, PathBuf},
    str::Chars,
    sync::{
        atomic::{AtomicBool, Ordering as AO},
        mpsc::{self, Receiver},
        Arc,
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use eframe::egui;

#[derive(Clone)]
pub struct Entry {
    pub name: String,
    pub name_lc: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub hidden: bool,
    pub kind: String,
}

#[cfg(windows)]
fn is_hidden(md: &fs::Metadata, _name: &str) -> bool {
    use std::os::windows::fs::MetadataExt;
    md.file_attributes() & 0x6 != 0 // HIDDEN | SYSTEM
}
#[cfg(not(windows))]
fn is_hidden(_md: &fs::Metadata, name: &str) -> bool {
    name.starts_with('.')
}

impl Entry {
    pub fn new(path: PathBuf, name: String, md: Option<&fs::Metadata>) -> Self {
        let mut is_dir = md.map_or(false, |m| m.is_dir());
        if let Some(m) = md {
            if m.file_type().is_symlink() {
                is_dir = fs::metadata(&path).map_or(false, |t| t.is_dir());
            }
        }
        let hidden = md.map_or(false, |m| is_hidden(m, &name));
        let kind = if is_dir {
            "File folder".to_string()
        } else {
            match path.extension() {
                Some(e) => format!("{} File", e.to_string_lossy().to_uppercase()),
                None => "File".to_string(),
            }
        };
        let size = if is_dir { 0 } else { md.map_or(0, |m| m.len()) };
        let modified = md.and_then(|m| m.modified().ok());
        Entry {
            name_lc: name.to_lowercase(),
            name,
            path,
            is_dir,
            size,
            modified,
            hidden,
            kind,
        }
    }
}

// ---------------------------------------------------------------- loading

pub fn spawn_load(path: PathBuf, ctx: egui::Context) -> Receiver<Result<Vec<Entry>, String>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let res = (|| -> io::Result<Vec<Entry>> {
            let mut v = Vec::new();
            for de in fs::read_dir(&path)? {
                let Ok(de) = de else { continue };
                let name = de.file_name().to_string_lossy().into_owned();
                let md = de.metadata().ok();
                v.push(Entry::new(de.path(), name, md.as_ref()));
            }
            Ok(v)
        })();
        let _ = tx.send(res.map_err(|e| format!("{}: {}", path.display(), e)));
        ctx.request_repaint();
    });
    rx
}

// ---------------------------------------------------------------- search

pub enum SearchMsg {
    Batch(Vec<Entry>),
    Done { truncated: bool },
}

/// Dropping the handle cancels the walk.
pub struct SearchHandle {
    pub rx: Receiver<SearchMsg>,
    cancel: Arc<AtomicBool>,
}
impl Drop for SearchHandle {
    fn drop(&mut self) {
        self.cancel.store(true, AO::Relaxed);
    }
}

pub const MAX_RESULTS: usize = 100_000;

/// Recursive, case-insensitive name search using jwalk's parallel walker.
/// Plain text = substring match; `*` and `?` switch to wildcard matching.
pub fn spawn_search(root: PathBuf, query: String, ctx: egui::Context) -> SearchHandle {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    thread::spawn(move || {
        let q_str = query.to_lowercase();
        let q: Vec<char> = q_str.chars().collect();
        let wild = q.iter().any(|c| *c == '*' || *c == '?');
        let mut batch: Vec<Entry> = Vec::with_capacity(512);
        let mut total = 0usize;
        let mut last = Instant::now();
        let mut truncated = false;

        let walker = jwalk::WalkDir::new(&root)
            .skip_hidden(false)
            .follow_links(false)
            .sort(false);

        for res in walker {
            if flag.load(AO::Relaxed) {
                return;
            }
            if !batch.is_empty() && last.elapsed() >= Duration::from_millis(60) {
                if tx.send(SearchMsg::Batch(std::mem::take(&mut batch))).is_err() {
                    return;
                }
                ctx.request_repaint();
                last = Instant::now();
            }
            let Ok(de) = res else { continue };
            if de.depth() == 0 {
                continue;
            }
            let name = de.file_name().to_string_lossy().into_owned();
            let lc = name.to_lowercase();
            let hit = if wild {
                let t: Vec<char> = lc.chars().collect();
                wildcard(&q, &t)
            } else {
                lc.contains(&q_str)
            };
            if !hit {
                continue;
            }
            let md = de.metadata().ok();
            batch.push(Entry::new(de.path(), name, md.as_ref()));
            total += 1;
            if batch.len() >= 512 {
                if tx.send(SearchMsg::Batch(std::mem::take(&mut batch))).is_err() {
                    return;
                }
                ctx.request_repaint();
                last = Instant::now();
            }
            if total >= MAX_RESULTS {
                truncated = true;
                break;
            }
        }
        if !flag.load(AO::Relaxed) {
            if !batch.is_empty() {
                let _ = tx.send(SearchMsg::Batch(batch));
            }
            let _ = tx.send(SearchMsg::Done { truncated });
            ctx.request_repaint();
        }
    });
    SearchHandle { rx, cancel }
}

fn wildcard(p: &[char], t: &[char]) -> bool {
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

// ---------------------------------------------------------------- sorting

fn take_digits(it: &mut Peekable<Chars>) -> String {
    let mut s = String::new();
    while let Some(&c) = it.peek() {
        if c.is_ascii_digit() {
            s.push(c);
            it.next();
        } else {
            break;
        }
    }
    s.trim_start_matches('0').to_string()
}

/// Explorer-style natural ordering ("file2" < "file10"). Inputs are pre-lowercased.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                if x.is_ascii_digit() && y.is_ascii_digit() {
                    let na = take_digits(&mut ai);
                    let nb = take_digits(&mut bi);
                    let o = na.len().cmp(&nb.len()).then_with(|| na.cmp(&nb));
                    if o != Ordering::Equal {
                        return o;
                    }
                } else {
                    let o = x.cmp(&y);
                    if o != Ordering::Equal {
                        return o;
                    }
                    ai.next();
                    bi.next();
                }
            }
        }
    }
}

// ---------------------------------------------------------------- operations

pub enum Op {
    Copy { items: Vec<PathBuf>, dest: PathBuf },
    Move { items: Vec<PathBuf>, dest: PathBuf },
    Delete { items: Vec<PathBuf>, permanent: bool },
}

pub fn unique_dest(dir: &Path, src: &Path) -> PathBuf {
    let name = src.file_name().unwrap_or_default();
    let cand = dir.join(name);
    if !cand.exists() {
        return cand;
    }
    let (stem, ext) = if src.is_dir() {
        (name.to_string_lossy().into_owned(), String::new())
    } else {
        (
            src.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
            src.extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default(),
        )
    };
    let mut n = 1;
    loop {
        let label = if n == 1 { " - Copy".to_string() } else { format!(" - Copy ({n})") };
        let c = dir.join(format!("{stem}{label}{ext}"));
        if !c.exists() {
            return c;
        }
        n += 1;
    }
}

pub fn unique_name(dir: &Path, base: &str) -> PathBuf {
    let first = dir.join(base);
    if !first.exists() {
        return first;
    }
    let mut n = 2;
    loop {
        let c = dir.join(format!("{base} ({n})"));
        if !c.exists() {
            return c;
        }
        n += 1;
    }
}

fn copy_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    let md = fs::symlink_metadata(src)?;
    if md.is_dir() {
        fs::create_dir_all(dst)?;
        for e in fs::read_dir(src)? {
            let e = e?;
            copy_recursive(&e.path(), &dst.join(e.file_name()))?;
        }
    } else {
        fs::copy(src, dst)?;
    }
    Ok(())
}

fn summarize(verb: &str, ok: usize, errs: Vec<String>) -> Result<String, String> {
    if errs.is_empty() {
        Ok(format!("{verb} {ok} item{}", if ok == 1 { "" } else { "s" }))
    } else {
        Err(format!("{verb}: {} failed - {}", errs.len(), errs[0]))
    }
}

pub fn run_op(op: Op) -> Result<String, String> {
    match op {
        Op::Copy { items, dest } => {
            let (mut ok, mut errs) = (0usize, Vec::new());
            for src in &items {
                if src.is_dir() && dest.starts_with(src) {
                    errs.push(format!("cannot copy '{}' into itself", src.display()));
                    continue;
                }
                let dst = unique_dest(&dest, src);
                match copy_recursive(src, &dst) {
                    Ok(()) => ok += 1,
                    Err(e) => errs.push(format!("{}: {e}", src.display())),
                }
            }
            summarize("Copied", ok, errs)
        }
        Op::Move { items, dest } => {
            let (mut ok, mut errs) = (0usize, Vec::new());
            for src in &items {
                if src.parent() == Some(dest.as_path()) {
                    continue;
                }
                if src.is_dir() && dest.starts_with(src) {
                    errs.push(format!("cannot move '{}' into itself", src.display()));
                    continue;
                }
                let mut dst = dest.join(src.file_name().unwrap_or_default());
                if dst.exists() {
                    dst = unique_dest(&dest, src);
                }
                let r = fs::rename(src, &dst).or_else(|_| {
                    copy_recursive(src, &dst)?;
                    if src.is_dir() {
                        fs::remove_dir_all(src)
                    } else {
                        fs::remove_file(src)
                    }
                });
                match r {
                    Ok(()) => ok += 1,
                    Err(e) => errs.push(format!("{}: {e}", src.display())),
                }
            }
            summarize("Moved", ok, errs)
        }
        Op::Delete { items, permanent } => {
            if permanent {
                let (mut ok, mut errs) = (0usize, Vec::new());
                for p in &items {
                    let r = if p.is_dir() { fs::remove_dir_all(p) } else { fs::remove_file(p) };
                    match r {
                        Ok(()) => ok += 1,
                        Err(e) => errs.push(format!("{}: {e}", p.display())),
                    }
                }
                summarize("Deleted", ok, errs)
            } else {
                trash::delete_all(items.iter())
                    .map(|_| {
                        format!(
                            "Moved {} item{} to the Recycle Bin",
                            items.len(),
                            if items.len() == 1 { "" } else { "s" }
                        )
                    })
                    .map_err(|e| e.to_string())
            }
        }
    }
}

// ---------------------------------------------------------------- misc

#[cfg(windows)]
pub fn list_drives() -> Vec<PathBuf> {
    // GetLogicalDrives is an instant bitmask; avoids probing (possibly asleep) drives.
    let mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
    (0..26u32)
        .filter(|i| mask & (1u32 << i) != 0)
        .map(|i| PathBuf::from(format!("{}:\\", (b'A' + i as u8) as char)))
        .collect()
}
#[cfg(not(windows))]
pub fn list_drives() -> Vec<PathBuf> {
    vec![PathBuf::from("/")]
}

pub fn fmt_size(b: u64) -> String {
    const U: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    let mut v = b as f64 / 1024.0;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} KB", v.ceil() as u64) // Explorer rounds file sizes up to whole KB
    } else {
        format!("{:.1} {}", v, U[i])
    }
}

pub fn fmt_date(t: Option<SystemTime>) -> String {
    match t {
        Some(t) => {
            let dt: chrono::DateTime<chrono::Local> = t.into();
            dt.format("%Y-%m-%d %H:%M").to_string()
        }
        None => String::new(),
    }
}
