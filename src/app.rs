use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::{Component, Path, PathBuf},
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    thread,
    time::Duration,
};

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CursorIcon, FontId, Id, Key, LayerId, Modifiers as M, Order,
    PointerButton, Rect, Sense, Stroke, UiBuilder,
};
use serde::{Deserialize, Serialize};

use crate::{
    clip,
    fsx::{self, Op},
    icons::{Icons, Spec},
    layout::{Node, MAX_PANES},
    osdrag,
    pane::*,
    sizes::SizeIndex,
    state::WinState,
    theme::*,
};

const TITLE_H: f32 = STRIP_H; // tabs and caption buttons share this height
const STATUS_H: f32 = 22.0;
const SIDEBAR_W: f32 = 190.0;
const CAPTION_W: f32 = 46.0;

/// Cached view of the system clipboard's file list (re-read when its sequence number changes).
struct ClipCache {
    seq: u32,
    files: Vec<PathBuf>,
    cut: bool,
}

#[derive(Serialize, Deserialize)]
struct PaneSave {
    id: usize,
    tabs: Vec<PathBuf>,
    active: usize,
}
#[derive(Serialize, Deserialize)]
struct SessionSave {
    layout: Node,
    panes: Vec<PaneSave>,
    active: usize,
    next_pane: usize,
}

pub struct App {
    settings: Settings,
    panes: Vec<Pane>,
    layout: Node,
    active: usize, // pane id
    next_pane: usize,
    next_uid: u64,
    clipc: ClipCache,
    icons: Icons,
    sizes: SizeIndex,
    drives: Vec<PathBuf>,
    show_settings: bool,
    fx_dirty: bool,
    typing: bool,
    os_drag_started: bool,
    nav_sig: u64,
    op_tx: Sender<(Result<String, String>, Vec<PathBuf>)>,
    op_rx: Receiver<(Result<String, String>, Vec<PathBuf>)>,
    busy: usize,
    status: String,
    status_until: f64,
    confirm: Option<(Vec<PathBuf>, bool)>,
    win: WinState,
    win_seen: bool,
}

/// Segoe UI first, then system fallbacks so CJK, Korean, Indic, symbols etc. render
/// in file names. Fonts that are not installed are skipped.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let dir = std::env::var("WINDIR")
        .map(|w| PathBuf::from(w).join("Fonts"))
        .unwrap_or_else(|_| PathBuf::from(r"C:\Windows\Fonts"));
    let wanted = [
        ("segoe", "segoeui.ttf"),
        ("seguisym", "seguisym.ttf"),
        ("yahei", "msyh.ttc"),
        ("yugothic", "YuGothR.ttc"),
        ("malgun", "malgun.ttf"),
        ("nirmala", "Nirmala.ttf"),
    ];
    let mut added: Vec<String> = Vec::new();
    for (name, file) in wanted {
        if let Ok(bytes) = std::fs::read(dir.join(file)) {
            fonts
                .font_data
                .insert(name.to_string(), Arc::new(egui::FontData::from_owned(bytes)));
            added.push(name.to_string());
        }
    }
    let prop = fonts.families.entry(egui::FontFamily::Proportional).or_default();
    for (i, n) in added.iter().enumerate() {
        prop.insert(i, n.clone());
    }
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .extend(added.iter().skip(1).cloned());
    ctx.set_fonts(fonts);
}

#[cfg(windows)]
fn apply_effects(frame: &eframe::Frame, s: &Settings) {
    if s.acrylic {
        let (r, g, b) = if s.dark { (32, 32, 32) } else { (243, 243, 243) };
        let _ = window_vibrancy::apply_acrylic(frame, Some((r, g, b, s.acrylic_alpha)));
    } else {
        let _ = window_vibrancy::clear_acrylic(frame);
    }
}
#[cfg(not(windows))]
fn apply_effects(_frame: &eframe::Frame, _s: &Settings) {}

/// Folders whose cached sizes an operation invalidates (together with their ancestors).
fn affected_paths(op: &Op) -> Vec<PathBuf> {
    match op {
        Op::Copy { dest, .. } => vec![dest.clone()],
        Op::Move { items, dest } => {
            let mut v: Vec<PathBuf> = items.iter().filter_map(|p| p.parent().map(|x| x.to_path_buf())).collect();
            v.push(dest.clone());
            v
        }
        Op::Delete { items, .. } => items.iter().filter_map(|p| p.parent().map(|x| x.to_path_buf())).collect(),
    }
}

fn same_volume(a: &Path, b: &Path) -> bool {
    match (a.components().next(), b.components().next()) {
        (Some(Component::Prefix(x)), Some(Component::Prefix(y))) => x.as_os_str().eq_ignore_ascii_case(y.as_os_str()),
        _ => true,
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut settings: Settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, "settings"))
            .unwrap_or_default();
        for i in 0..4 {
            settings.cols[i] = settings.cols[i].clamp(MIN_COLS[i], 1000.0); // repairs widths saved by older builds
        }
        install_fonts(&cc.egui_ctx);
        apply_visuals(&cc.egui_ctx, &settings);

        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("C:\\"));
        let mut uid = 1u64;
        let session: Option<SessionSave> = cc.storage.and_then(|s| eframe::get_value(s, "session"));
        let restored = session.and_then(|s| {
            let mut ids = Vec::new();
            s.layout.leaves(&mut ids);
            let mut have: Vec<usize> = s.panes.iter().map(|p| p.id).collect();
            have.sort_unstable();
            let mut want = ids.clone();
            want.sort_unstable();
            if want != have || want.is_empty() || want.len() > MAX_PANES {
                return None;
            }
            let panes: Vec<Pane> = s
                .panes
                .iter()
                .map(|ps| {
                    let mut tabs: Vec<Tab> = ps
                        .tabs
                        .iter()
                        .map(|p| if p.is_dir() { p.clone() } else { home.clone() })
                        .map(|p| {
                            uid += 1;
                            Tab::new(uid, p, &cc.egui_ctx)
                        })
                        .collect();
                    if tabs.is_empty() {
                        uid += 1;
                        tabs.push(Tab::new(uid, home.clone(), &cc.egui_ctx));
                    }
                    let active = ps.active.min(tabs.len() - 1);
                    Pane { id: ps.id, tabs, active }
                })
                .collect();
            let active = if want.contains(&s.active) { s.active } else { want[0] };
            Some((panes, s.layout, active, s.next_pane.max(want.iter().max().copied().unwrap_or(0) + 1)))
        });
        // Default: ONE pane; more are created by dragging tabs to the pane edges.
        let (panes, layout, active, next_pane) = restored.unwrap_or_else(|| {
            uid += 1;
            (
                vec![Pane { id: 0, tabs: vec![Tab::new(uid, home.clone(), &cc.egui_ctx)], active: 0 }],
                Node::Leaf(0),
                0,
                1,
            )
        });

        let (op_tx, op_rx) = mpsc::channel();
        let fx_dirty = settings.acrylic;
        App {
            panes,
            layout,
            active,
            next_pane,
            next_uid: uid + 1,
            settings,
            clipc: ClipCache { seq: u32::MAX, files: Vec::new(), cut: false },
            icons: Icons::new(&cc.egui_ctx),
            sizes: SizeIndex::new(&cc.egui_ctx),
            drives: fsx::list_drives(),
            show_settings: false,
            fx_dirty,
            typing: false,
            os_drag_started: false,
            nav_sig: 0,
            op_tx,
            op_rx,
            busy: 0,
            status: String::new(),
            status_until: 0.0,
            confirm: None,
            win: WinState { pos: [100.0, 100.0], size: [1280.0, 800.0], maximized: false },
            win_seen: false,
        }
    }

    // ------------------------------------------------------------ helpers

    fn pane_idx(&self, id: usize) -> Option<usize> {
        self.panes.iter().position(|p| p.id == id)
    }

    fn active_idx(&mut self) -> usize {
        match self.pane_idx(self.active) {
            Some(i) => i,
            None => {
                self.active = self.panes[0].id;
                0
            }
        }
    }

    /// The pane after `id` in layout order (None when only one pane exists).
    fn next_pane_of(&self, id: usize) -> Option<usize> {
        let mut l = Vec::new();
        self.layout.leaves(&mut l);
        if l.len() < 2 {
            return None;
        }
        let p = l.iter().position(|&x| x == id)?;
        Some(l[(p + 1) % l.len()])
    }

    fn set_status(&mut self, ctx: &egui::Context, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_until = ctx.input(|i| i.time) + 6.0;
        ctx.request_repaint_after(Duration::from_secs_f32(6.2));
    }

    fn open_tab(&mut self, pane: usize, path: PathBuf, ctx: &egui::Context) {
        let uid = self.next_uid;
        self.next_uid += 1;
        if let Some(pi) = self.pane_idx(pane) {
            let p = &mut self.panes[pi];
            p.tabs.push(Tab::new(uid, path, ctx));
            p.active = p.tabs.len() - 1;
        }
    }

    fn select_tab(&mut self, pane: usize, idx: usize, ctx: &egui::Context) {
        if let Some(pi) = self.pane_idx(pane) {
            let p = &mut self.panes[pi];
            p.active = idx.min(p.tabs.len() - 1);
            if p.tab().stale {
                p.tab_mut().refresh(ctx);
            }
        }
    }

    fn refresh_clip(&mut self) {
        let seq = clip::sequence();
        if seq != self.clipc.seq {
            self.clipc.seq = seq;
            let (files, cut) = clip::get_files().unwrap_or_default();
            self.clipc.files = files;
            self.clipc.cut = cut;
        }
    }

    fn put_clip(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>, cut: bool) {
        let n = paths.len();
        if clip::set_files(&paths, cut) {
            self.refresh_clip();
            let verb = if cut { "Cut" } else { "Copied" };
            self.set_status(ctx, format!("{verb} {n} item{}", if n == 1 { "" } else { "s" }));
        } else {
            self.set_status(ctx, "Could not access the system clipboard");
        }
    }

    fn spawn_op(&mut self, ctx: &egui::Context, op: Op) {
        let tx = self.op_tx.clone();
        let c = ctx.clone();
        self.busy += 1;
        let affected = affected_paths(&op);
        thread::spawn(move || {
            let _ = tx.send((fsx::run_op(op), affected));
            c.request_repaint();
        });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        self.icons.poll(ctx);
        self.refresh_clip();
        let sh = self.settings.show_hidden;
        for p in self.panes.iter_mut() {
            for t in p.tabs.iter_mut() {
                t.poll(sh);
            }
        }
        while let Ok((r, affected)) = self.op_rx.try_recv() {
            self.busy = self.busy.saturating_sub(1);
            match r {
                Ok(m) | Err(m) => self.set_status(ctx, m),
            }
            // only the touched folders and their ancestors need re-measuring
            for p in &affected {
                self.sizes.touch(p);
            }
            for p in self.panes.iter_mut() {
                let act = p.active;
                for (i, t) in p.tabs.iter_mut().enumerate() {
                    if i == act {
                        t.refresh(ctx);
                    } else {
                        t.stale = true;
                    }
                }
            }
        }
        // navigation anywhere cancels queued folder-size work for folders no longer shown
        let mut h = DefaultHasher::new();
        for p in &self.panes {
            p.tab().path.hash(&mut h);
        }
        let sig = h.finish();
        if sig != self.nav_sig {
            self.nav_sig = sig;
            self.sizes.cancel_pending();
        }
    }

    // ------------------------------------------------------------ shortcuts

    fn shortcuts(&mut self, ctx: &egui::Context, typing: bool, acts: &mut Vec<Act>) {
        if self.confirm.is_some() {
            let (enter, esc) = ctx.input_mut(|i| {
                (i.consume_key(M::NONE, Key::Enter), i.consume_key(M::NONE, Key::Escape))
            });
            if enter {
                if let Some((items, permanent)) = self.confirm.take() {
                    self.spawn_op(ctx, Op::Delete { items, permanent });
                }
            } else if esc {
                self.confirm = None;
            }
            return;
        }
        if self.show_settings && ctx.input_mut(|i| i.consume_key(M::NONE, Key::Escape)) {
            self.show_settings = false;
            return;
        }

        #[derive(Default)]
        struct K {
            copy: bool,
            cut: bool,
            paste: bool,
            all: bool,
            delete: Option<bool>,
            rename: bool,
            open: bool,
            up: bool,
            back: bool,
            fwd: bool,
            cursor: Option<(isize, bool)>,
            page: Option<(isize, bool)>,
            home: Option<bool>,
            end: Option<bool>,
            new_tab: bool,
            close_tab: bool,
            next_tab: Option<bool>,
            addr: bool,
            search: bool,
            refresh: bool,
            new_folder: bool,
            switch: bool,
            esc: bool,
            settings: bool,
        }
        fn take_event(i: &mut egui::InputState, f: impl Fn(&egui::Event) -> bool) -> bool {
            let before = i.events.len();
            i.events.retain(|e| !f(e));
            i.events.len() != before
        }

        let mut k = K::default();
        ctx.input_mut(|i| {
            let c = M::CTRL;
            k.settings = i.consume_key(c, Key::Comma);
            if i.consume_key(c | M::SHIFT, Key::Tab) {
                k.next_tab = Some(false);
            } else if i.consume_key(c, Key::Tab) {
                k.next_tab = Some(true);
            }
            if i.consume_key(c, Key::PageDown) {
                k.next_tab = Some(true);
            }
            if i.consume_key(c, Key::PageUp) {
                k.next_tab = Some(false);
            }
            k.new_tab = i.consume_key(c, Key::T);
            k.close_tab = i.consume_key(c, Key::W);
            k.addr = i.consume_key(c, Key::L);
            k.search = i.consume_key(c, Key::F) | i.consume_key(c, Key::E) | i.consume_key(M::NONE, Key::F3);
            k.new_folder = i.consume_key(c | M::SHIFT, Key::N);
            k.refresh = i.consume_key(M::NONE, Key::F5) | i.consume_key(c, Key::R);
            if typing {
                return;
            }
            k.copy = i.consume_key(c, Key::C) | take_event(i, |e| matches!(e, egui::Event::Copy));
            k.cut = i.consume_key(c, Key::X) | take_event(i, |e| matches!(e, egui::Event::Cut));
            k.paste = i.consume_key(c, Key::V) | take_event(i, |e| matches!(e, egui::Event::Paste(_)));
            k.all = i.consume_key(c, Key::A);
            if i.consume_key(M::SHIFT, Key::Delete) {
                k.delete = Some(true);
            } else if i.consume_key(M::NONE, Key::Delete) {
                k.delete = Some(false);
            }
            k.rename = i.consume_key(M::NONE, Key::F2);
            k.open = i.consume_key(M::NONE, Key::Enter);
            k.back = i.consume_key(M::ALT, Key::ArrowLeft);
            k.fwd = i.consume_key(M::ALT, Key::ArrowRight);
            k.up = i.consume_key(M::ALT, Key::ArrowUp) | i.consume_key(M::NONE, Key::Backspace);
            for (key, d) in [(Key::ArrowDown, 1isize), (Key::ArrowUp, -1isize)] {
                if i.consume_key(M::SHIFT, key) {
                    k.cursor = Some((d, true));
                } else if i.consume_key(M::NONE, key) {
                    k.cursor = Some((d, false));
                }
            }
            for (key, d) in [(Key::PageDown, 1isize), (Key::PageUp, -1isize)] {
                if i.consume_key(M::SHIFT, key) {
                    k.page = Some((d, true));
                } else if i.consume_key(M::NONE, key) {
                    k.page = Some((d, false));
                }
            }
            if i.consume_key(M::SHIFT, Key::Home) {
                k.home = Some(true);
            } else if i.consume_key(M::NONE, Key::Home) {
                k.home = Some(false);
            }
            if i.consume_key(M::SHIFT, Key::End) {
                k.end = Some(true);
            } else if i.consume_key(M::NONE, Key::End) {
                k.end = Some(false);
            }
            k.switch = i.consume_key(M::NONE, Key::Tab);
            k.esc = i.consume_key(M::NONE, Key::Escape);
        });

        let a = self.active; // pane id
        let ai = self.active_idx();
        let row_h = row_height(&self.settings);
        if k.settings {
            self.show_settings = !self.show_settings;
        }
        if k.switch {
            if let Some(n) = self.next_pane_of(a) {
                self.active = n;
                return;
            }
        }
        if k.new_tab {
            let p = self.panes[ai].tab().path.clone();
            acts.push(Act::NewTab { pane: a, path: p });
        }
        if k.close_tab {
            let idx = self.panes[ai].active;
            acts.push(Act::CloseTab { pane: a, idx });
        }
        if let Some(fwd) = k.next_tab {
            let n = self.panes[ai].tabs.len();
            let cur = self.panes[ai].active;
            let next = if fwd { (cur + 1) % n } else { (cur + n - 1) % n };
            self.select_tab(a, next, ctx);
        }
        let tab = self.panes[ai].tab_mut();
        if k.addr {
            tab.focus_addr = true;
        }
        if k.search {
            tab.focus_search = true;
        }
        if k.refresh {
            tab.refresh(ctx);
            self.sizes.mark_subtree_stale(&tab.path);
        }
        if k.new_folder {
            acts.push(Act::NewFolder(a));
        }
        let tab = self.panes[ai].tab_mut();
        if k.all {
            tab.select_all();
        }
        if k.rename {
            tab.start_rename();
        }
        if k.back {
            tab.go_back(ctx);
        }
        if k.fwd {
            tab.go_forward(ctx);
        }
        if k.up {
            tab.go_up(ctx);
        }
        if k.open {
            acts.extend(tab.activate(ctx));
        }
        if let Some((d, ext)) = k.cursor {
            tab.move_cursor(d, ext);
        }
        if let Some((d, ext)) = k.page {
            tab.page(d, ext, row_h);
        }
        if let Some(ext) = k.home {
            tab.jump(false, ext);
        }
        if let Some(ext) = k.end {
            tab.jump(true, ext);
        }
        if k.esc {
            if tab.search_mode {
                tab.exit_search(ctx);
            } else {
                tab.selected.clear();
            }
        }
        let paths = self.panes[ai].tab().selected_paths();
        if k.copy && !paths.is_empty() {
            acts.push(Act::Copy(paths.clone()));
        }
        if k.cut && !paths.is_empty() {
            acts.push(Act::Cut(paths.clone()));
        }
        if k.paste {
            acts.push(Act::Paste(a));
        }
        if let Some(permanent) = k.delete {
            if !paths.is_empty() {
                acts.push(Act::Delete { paths, permanent });
            }
        }
    }

    // ------------------------------------------------------------ actions

    fn tab_drop(&mut self, ctx: &egui::Context, from: usize, idx: usize, to: usize, zone: Zone, at: Option<usize>) {
        let (Some(fi), Some(_)) = (self.pane_idx(from), self.pane_idx(to)) else { return };
        if idx >= self.panes[fi].tabs.len() {
            return;
        }
        let splitting = zone != Zone::Merge;
        if splitting && self.panes.len() >= MAX_PANES {
            return;
        }
        if from == to {
            if !splitting {
                // reorder inside the same pane
                let Some(mut at) = at else { return };
                let p = &mut self.panes[fi];
                let tab = p.tabs.remove(idx);
                if at > idx {
                    at -= 1;
                }
                let at = at.min(p.tabs.len());
                p.tabs.insert(at, tab);
                p.active = at;
                return;
            }
        }
        // Splitting a pane's ONLY tab off itself opens the same folder in the new pane
        // (the original tab stays); otherwise the tab is moved.
        let duplicate = from == to && self.panes[fi].tabs.len() < 2;
        let tab = if duplicate {
            let path = self.panes[fi].tabs[idx].path.clone();
            let uid = self.next_uid;
            self.next_uid += 1;
            Tab::new(uid, path, ctx)
        } else {
            let tab = self.panes[fi].tabs.remove(idx);
            let p = &mut self.panes[fi];
            if !p.tabs.is_empty() {
                if idx < p.active {
                    p.active -= 1;
                }
                p.active = p.active.min(p.tabs.len() - 1);
            }
            if self.panes[fi].tabs.is_empty() {
                self.layout.remove_leaf(from);
                self.panes.remove(fi);
            }
            tab
        };
        match zone {
            Zone::Merge => {
                if let Some(ti) = self.pane_idx(to) {
                    let p = &mut self.panes[ti];
                    let at = at.unwrap_or(p.tabs.len()).min(p.tabs.len());
                    p.tabs.insert(at, tab);
                    p.active = at;
                    self.active = to;
                }
            }
            _ => {
                let new_id = self.next_pane;
                self.next_pane += 1;
                self.panes.push(Pane { id: new_id, tabs: vec![tab], active: 0 });
                let (horiz, new_first) = match zone {
                    Zone::Left => (true, true),
                    Zone::Right => (true, false),
                    Zone::Top => (false, true),
                    _ => (false, false),
                };
                if !self.layout.split_leaf(to, new_id, horiz, new_first) {
                    // target vanished (should not happen): fall back to a plain merge
                    let t = self.panes.pop().map(|p| p.tabs).unwrap_or_default();
                    if let Some(p) = self.panes.first_mut() {
                        p.tabs.extend(t);
                    }
                    return;
                }
                self.active = new_id;
            }
        }
        let a = self.active;
        let ai = self.active_idx();
        let ti = self.panes[ai].active;
        self.select_tab(a, ti, ctx);
    }

    fn apply(&mut self, ctx: &egui::Context, act: Act) {
        match act {
            Act::Activate(i) => {
                if self.pane_idx(i).is_some() {
                    self.active = i;
                }
            }
            Act::GoTo(p) => {
                let ai = self.active_idx();
                self.panes[ai].tab_mut().navigate(p, ctx);
            }
            Act::GoToNewTab(p) => self.open_tab(self.active, p, ctx),
            Act::OpenFile(p) => {
                if let Err(e) = opener::open(&p) {
                    self.set_status(ctx, format!("Cannot open {}: {e}", p.display()));
                }
            }
            Act::Reveal(p) => {
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    let _ = std::process::Command::new("explorer.exe")
                        .raw_arg(format!("/select,\"{}\"", p.display()))
                        .spawn();
                }
                #[cfg(not(windows))]
                let _ = p;
            }
            Act::NewTab { pane, path } => self.open_tab(pane, path, ctx),
            Act::CloseTab { pane, idx } => {
                let Some(pi) = self.pane_idx(pane) else { return };
                let n = self.panes[pi].tabs.len();
                if idx >= n {
                    return;
                }
                if n > 1 {
                    let p = &mut self.panes[pi];
                    p.tabs.remove(idx);
                    if idx < p.active || p.active >= p.tabs.len() {
                        p.active = p.active.saturating_sub(1);
                    }
                    let a = p.active;
                    self.select_tab(pane, a, ctx);
                } else if self.panes.len() > 1 {
                    // closing the last tab of a pane closes the pane
                    self.layout.remove_leaf(pane);
                    self.panes.remove(pi);
                    if self.active == pane {
                        self.active = self.panes[0].id;
                    }
                }
            }
            Act::SelectTab { pane, idx } => self.select_tab(pane, idx, ctx),
            Act::Copy(paths) => self.put_clip(ctx, paths, false),
            Act::Cut(paths) => self.put_clip(ctx, paths, true),
            Act::Paste(pane) => {
                let (items, cut) = (self.clipc.files.clone(), self.clipc.cut);
                let Some(pi) = self.pane_idx(pane) else { return };
                if items.is_empty() {
                    return;
                }
                let dest = self.panes[pi].tab().path.clone();
                if cut {
                    clip::clear(); // Explorer semantics: a cut can only be pasted once
                    self.clipc.files.clear();
                    self.clipc.cut = false;
                    self.spawn_op(ctx, Op::Move { items, dest });
                } else {
                    self.spawn_op(ctx, Op::Copy { items, dest });
                }
            }
            Act::Delete { paths, permanent } => {
                if paths.is_empty() {
                    return;
                }
                if permanent || self.settings.confirm_delete {
                    self.confirm = Some((paths, permanent));
                } else {
                    self.spawn_op(ctx, Op::Delete { items: paths, permanent: false });
                }
            }
            Act::Rename { pane, from, to } => {
                let Some(pi) = self.pane_idx(pane) else { return };
                match std::fs::rename(&from, &to) {
                    Ok(()) => {
                        if let Some(par) = from.parent() {
                            self.sizes.touch(par);
                        }
                        let tab = self.panes[pi].tab_mut();
                        tab.select_after_load = Some(to);
                        tab.refresh(ctx);
                        if tab.search_mode {
                            tab.exit_search(ctx);
                        }
                    }
                    Err(e) => self.set_status(ctx, format!("Rename failed: {e}")),
                }
            }
            Act::NewFolder(pane) => {
                let Some(pi) = self.pane_idx(pane) else { return };
                let tab = self.panes[pi].tab_mut();
                if tab.search_mode {
                    tab.exit_search(ctx);
                }
                let p = fsx::unique_name(&tab.path, "New folder");
                match std::fs::create_dir(&p) {
                    Ok(()) => {
                        tab.select_after_load = Some(p);
                        tab.rename_after_load = true;
                        tab.refresh(ctx);
                    }
                    Err(e) => self.set_status(ctx, format!("Cannot create folder: {e}")),
                }
            }
            Act::Transfer { pane, paths, mv } => {
                if paths.is_empty() {
                    return;
                }
                let Some(to) = self.next_pane_of(pane) else { return };
                let Some(ti) = self.pane_idx(to) else { return };
                let dest = self.panes[ti].tab().path.clone();
                let op = if mv { Op::Move { items: paths, dest } } else { Op::Copy { items: paths, dest } };
                self.spawn_op(ctx, op);
            }
            Act::CopyText(s) => ctx.copy_text(s),
            Act::DropTo { paths, dest } => {
                if paths.is_empty() {
                    return;
                }
                // Explorer rules: Shift = move, Ctrl = copy, otherwise move on the same drive, copy across drives
                let m = ctx.input(|i| i.modifiers);
                let mv = if m.shift {
                    true
                } else if m.ctrl || m.command {
                    false
                } else {
                    paths.first().map_or(false, |p| same_volume(p, &dest))
                };
                let op = if mv { Op::Move { items: paths, dest } } else { Op::Copy { items: paths, dest } };
                self.spawn_op(ctx, op);
            }
            Act::TabDrop { from, idx, to, zone, at } => self.tab_drop(ctx, from, idx, to, zone, at),
            Act::SetCols(c) => {
                let mut c = c;
                for i in 0..4 {
                    c[i] = c[i].max(MIN_COLS[i]);
                }
                self.settings.cols = c;
            }
            Act::RefreshSizes(p) => self.sizes.mark_subtree_stale(&p),
            Act::PinQuick(p) => {
                if p.is_dir() && !self.settings.quick.iter().any(|(_, q)| q == &p) {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| p.display().to_string());
                    self.settings.quick.push((name, p));
                }
            }
            Act::UnpinQuick(i) => {
                if i < self.settings.quick.len() {
                    self.settings.quick.remove(i);
                }
            }
            Act::MoveQuick(i, d) => {
                let j = i as isize + d;
                if i < self.settings.quick.len() && j >= 0 && (j as usize) < self.settings.quick.len() {
                    self.settings.quick.swap(i, j as usize);
                }
            }
            Act::OpenRecycleBin => {
                let _ = std::process::Command::new("explorer.exe").arg("shell:RecycleBinFolder").spawn();
            }
        }
    }

    fn status_line(&self, ctx: &egui::Context) -> String {
        if ctx.input(|i| i.time) < self.status_until && !self.status.is_empty() {
            return self.status.clone();
        }
        match self.pane_idx(self.active) {
            Some(i) => self.panes[i].tab().selection_info(),
            None => String::new(),
        }
    }
}

// ====================================================================== eframe glue

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0] // fully transparent: every surface is painted by the app
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "settings", &self.settings);
        self.sizes.save();
        let session = SessionSave {
            layout: self.layout.clone(),
            panes: self
                .panes
                .iter()
                .map(|p| PaneSave {
                    id: p.id,
                    tabs: p.tabs.iter().map(|t| t.path.clone()).collect(),
                    active: p.active,
                })
                .collect(),
            active: self.active,
            next_pane: self.next_pane,
        };
        eframe::set_value(storage, "session", &session);
        if self.win_seen {
            self.win.save();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.fx_dirty {
            self.fx_dirty = false;
            apply_effects(&*frame, &self.settings);
        }
        let typing_prev = self.typing;
        let mut typing_now = false;

        self.poll(&ctx);
        let mut acts: Vec<Act> = Vec::new();
        self.shortcuts(&ctx, typing_prev, &mut acts);
        for a in acts.drain(..) {
            self.apply(&ctx, a);
        }

        // follow the effective theme (System mode tracks the OS, including live changes)
        let dark_now = ctx.theme() == egui::Theme::Dark;
        if dark_now != self.settings.dark {
            self.settings.dark = dark_now;
            self.fx_dirty = true; // acrylic tint depends on light/dark
        }

        let pal = palette(&self.settings);
        let full = ui.max_rect();
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));

        // remember window geometry (only while it is in its normal, visible state)
        let (inner, outer, minimized) = ctx.input(|i| {
            let v = i.viewport();
            (v.inner_rect, v.outer_rect, v.minimized.unwrap_or(false))
        });
        self.win.maximized = maximized;
        if !maximized && !minimized {
            if let (Some(inn), Some(out)) = (inner, outer) {
                if out.min.x > -10000.0 {
                    self.win.pos = [out.min.x, out.min.y];
                    self.win.size = [inn.width(), inn.height()];
                    self.win_seen = true;
                }
            }
        }

        ui.painter().rect_filled(full, R0, pal.chrome);

        // ---- layout
        let left_zone = if self.settings.show_sidebar { SIDEBAR_W } else { CAPTION_W };
        let title = Rect::from_min_size(full.min, vec2(full.width(), TITLE_H));
        let status_rect = Rect::from_min_max(pos2(full.left(), full.bottom() - STATUS_H), full.max);
        let side_rect = Rect::from_min_max(pos2(full.left(), title.bottom()), pos2(full.left() + left_zone, status_rect.top()));
        let tree_rect = Rect::from_min_max(pos2(full.left() + left_zone, full.top()), pos2(full.right(), status_rect.top()));
        let mut rects: Vec<(usize, Rect)> = Vec::new();
        let mut divs = Vec::new();
        self.layout.layout(tree_rect, &mut Vec::new(), &mut rects, &mut divs);

        // ---- title bar: drag region first (widgets drawn later sit on top of it)
        let drag = ui.interact(title, Id::unique("titlebar_drag"), Sense::click_and_drag());
        if drag.double_clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
        } else if drag.drag_started_by(PointerButton::Primary) {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        // settings gear + app name (left zone)
        let gear = Rect::from_min_size(title.min, vec2(CAPTION_W, TITLE_H));
        let gresp = ui
            .interact(gear, Id::unique("gear"), Sense::click())
            .on_hover_text("Settings (Ctrl+,)");
        if gresp.hovered() || self.show_settings {
            ui.painter().rect_filled(gear, R0, pal.caption_hover);
        }
        paint_glyph(ui.painter(), gear.center(), Glyph::Gear, pal.text);
        if gresp.clicked() {
            self.show_settings = !self.show_settings;
        }
        if left_zone > CAPTION_W + 20.0 {
            ui.painter().text(
                pos2(title.left() + CAPTION_W + 2.0, title.center().y),
                Align2::LEFT_CENTER,
                &format!("Alexandria v{}", env!("CARGO_PKG_VERSION")),
                FontId::proportional(13.0),
                pal.dim,
            );
        }

        // ---- sidebar (or a thin rail when hidden)
        if self.settings.show_sidebar {
            let cur = {
                let ai = self.active_idx();
                self.panes[ai].tab().path.clone()
            };
            draw_sidebar(ui, side_rect, &self.settings.quick, &self.drives, &cur, &pal, &self.icons, &mut acts);
        } else {
            ui.painter().rect_filled(side_rect, R0, pal.side);
            ui.painter()
                .line_segment([side_rect.right_top(), side_rect.right_bottom()], Stroke::new(1.0, pal.border));
        }

        // ---- panes
        let n_panes = self.panes.len();
        {
            let cut_paths: &[PathBuf] = if self.clipc.cut { &self.clipc.files } else { &[] };
            let can_paste = !self.clipc.files.is_empty();
            for (id, r) in &rects {
                let Some(pi) = self.panes.iter().position(|p| p.id == *id) else { continue };
                let top_right = r.top() <= title.bottom() + 0.5 && r.right() >= full.right() - 0.5;
                let pc = PaneCtx {
                    pal: &pal,
                    settings: &self.settings,
                    clip_cut: cut_paths,
                    can_paste,
                    active: self.active == *id,
                    idx: *id,
                    icons: &self.icons,
                    sizes: &self.sizes,
                    strip_inset: if top_right { 3.0 * CAPTION_W } else { 0.0 },
                    can_split: n_panes < MAX_PANES,
                    has_next: n_panes > 1,
                };
                draw_pane(ui, *r, &mut self.panes[pi], &pc, &mut acts, &mut typing_now);
            }
        }

        // ---- dividers between panes
        for (k, d) in divs.iter().enumerate() {
            let resp = ui.interact(d.rect, Id::unique(("div", k)), Sense::drag());
            if resp.hovered() || resp.dragged() {
                ctx.set_cursor_icon(if d.horiz { CursorIcon::ResizeHorizontal } else { CursorIcon::ResizeVertical });
            }
            if resp.dragged() {
                if let Some(p) = ctx.pointer_interact_pos() {
                    let ratio = if d.horiz {
                        (p.x - d.parent.left()) / d.parent.width()
                    } else {
                        (p.y - d.parent.top()) / d.parent.height()
                    };
                    if let Node::Split { ratio: r, .. } = self.layout.node_mut(&d.path) {
                        *r = ratio.clamp(0.15, 0.85);
                    }
                }
            }
            let col = if resp.dragged() { pal.accent } else { pal.border };
            let c = d.rect.center();
            let (a, b) = if d.horiz {
                (pos2(c.x + 0.5, d.rect.top()), pos2(c.x + 0.5, d.rect.bottom()))
            } else {
                (pos2(d.rect.left(), c.y + 0.5), pos2(d.rect.right(), c.y + 0.5))
            };
            ui.painter().line_segment([a, b], Stroke::new(1.0, col));
        }

        // ---- caption buttons (on top of the top-right pane's strip)
        let kinds = [
            (Glyph::Minimize, "Minimize"),
            (if maximized { Glyph::Restore } else { Glyph::Maximize }, if maximized { "Restore" } else { "Maximize" }),
            (Glyph::Close, "Close (Alt+F4)"),
        ];
        for (k, (g, name)) in kinds.iter().enumerate() {
            let r = Rect::from_min_size(
                pos2(title.right() - CAPTION_W * (3 - k) as f32, title.top()),
                vec2(CAPTION_W, TITLE_H),
            );
            let resp = ui
                .interact(r, Id::unique(("caption", k)), Sense::click())
                .on_hover_text(*name);
            let close = k == 2;
            let (bg, fg) = if resp.is_pointer_button_down_on() {
                if close {
                    (Some(rgb(0xF1707A)), Color32::WHITE)
                } else {
                    (Some(pal.caption_press), pal.text)
                }
            } else if resp.hovered() {
                if close {
                    (Some(rgb(0xE81123)), Color32::WHITE)
                } else {
                    (Some(pal.caption_hover), pal.text)
                }
            } else {
                (None, pal.text)
            };
            if let Some(bg) = bg {
                ui.painter().rect_filled(r, R0, bg);
            }
            paint_glyph(ui.painter(), r.center(), *g, fg);
            if resp.clicked() {
                match k {
                    0 => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                    1 => ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized)),
                    _ => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                }
            }
        }

        // ---- status bar
        ui.painter().rect_filled(status_rect, R0, pal.chrome);
        ui.painter()
            .line_segment([status_rect.left_top(), status_rect.right_top()], Stroke::new(1.0, pal.border));
        ui.painter().text(
            pos2(status_rect.left() + 10.0, status_rect.center().y),
            Align2::LEFT_CENTER,
            self.status_line(&ctx),
            FontId::proportional(12.0),
            pal.dim,
        );
        if self.busy > 0 {
            ui.painter().text(
                pos2(status_rect.right() - 10.0, status_rect.center().y),
                Align2::RIGHT_CENTER,
                "Working...",
                FontId::proportional(12.0),
                pal.accent,
            );
        }

        // ---- files dragged in from other applications (winit gives no cursor position
        //      during OS drags, so they go to the active pane)
        let hovering_os = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if hovering_os {
            if let Some((_, r)) = rects.iter().find(|(id, _)| *id == self.active) {
                outline(ui.painter(), r.shrink(1.0), Stroke::new(3.0, pal.accent));
                ui.painter().text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    "Drop to copy here (Shift: move)",
                    FontId::proportional(15.0),
                    pal.accent,
                );
            }
        }
        let dropped: Vec<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            if let Some(pi) = self.pane_idx(self.active) {
                let dest = self.panes[pi].tab().path.clone();
                // external drops default to copy unless Shift is held
                let shift = ctx.input(|i| i.modifiers.shift);
                let op = if shift { Op::Move { items: dropped, dest } } else { Op::Copy { items: dropped, dest } };
                self.spawn_op(&ctx, op);
            }
        }

        // ---- tab drag: pane drop zones (split on edges, merge in the middle)
        if let Some(td) = egui::DragAndDrop::payload::<TabDrag>(&ctx) {
            if let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) {
                if let Some((id, r)) = rects.iter().find(|(_, r)| r.contains(pos)) {
                    let body = Rect::from_min_max(pos2(r.left(), r.top() + STRIP_H), r.max);
                    let zone = drop_zone(body, pos, n_panes < MAX_PANES);
                    if pos.y >= body.top() {
                        let zr = zone_rect(body, zone);
                        let fill = Color32::from_rgba_unmultiplied(pal.accent.r(), pal.accent.g(), pal.accent.b(), 60);
                        ui.painter().rect_filled(zr, R0, fill);
                        outline(ui.painter(), zr, Stroke::new(2.0, pal.accent));
                    }
                    if ctx.input(|i| i.pointer.any_released()) {
                        acts.push(Act::TabDrop { from: td.pane, idx: td.idx, to: *id, zone, at: None });
                        egui::DragAndDrop::clear_payload(&ctx);
                    }
                }
            }
        }

        // ---- in-app drag ghost + hand-off to the OS when the cursor leaves the window
        let file_drag = egui::DragAndDrop::payload::<FileDrag>(&ctx);
        let tab_drag = egui::DragAndDrop::payload::<TabDrag>(&ctx);
        if let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) {
            let label = if let Some(f) = &file_drag {
                let n = f.paths.len();
                Some(format!("{n} item{}   (Ctrl: copy, Shift: move)", if n == 1 { "" } else { "s" }))
            } else if tab_drag.is_some() {
                Some("Move tab: drop on a tab strip to merge, on a pane edge to split".to_string())
            } else {
                None
            };
            if let Some(label) = label {
                let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::unique("dnd_ghost")));
                let galley = painter.layout_no_wrap(label, FontId::proportional(12.0), pal.text);
                let r = Rect::from_min_size(pos + vec2(14.0, 14.0), galley.size() + vec2(12.0, 6.0));
                painter.rect_filled(r, R0, if self.settings.dark { rgb(0x2B2B2B) } else { Color32::WHITE });
                outline(&painter, r, Stroke::new(1.0, pal.accent));
                painter.galley(r.min + vec2(6.0, 3.0), galley, pal.text);
            }
            if let Some(f) = &file_drag {
                if !full.contains(pos) && !self.os_drag_started {
                    self.os_drag_started = true;
                    let started = osdrag::start(&*frame, f.paths.clone());
                    egui::DragAndDrop::clear_payload(&ctx);
                    if started {
                        // the files may have moved elsewhere: reload what is on screen
                        for p in self.panes.iter_mut() {
                            p.tab_mut().refresh(&ctx);
                        }
                        for p in &f.paths {
                            if let Some(par) = p.parent() {
                                self.sizes.touch(par);
                            }
                        }
                    }
                }
            }
        }
        if file_drag.is_none() {
            self.os_drag_started = false;
        }

        // ---- window border (not when maximized)
        if !maximized {
            outline(ui.painter(), full, Stroke::new(1.0, pal.border));
        }

        self.typing = typing_now;
        for a in acts.drain(..) {
            self.apply(&ctx, a);
        }

        self.dialogs(&ctx);
        edge_resize(&ctx, full, maximized);
    }
}

impl App {
    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some((paths, permanent)) = self.confirm.clone() {
            let mut decision: Option<bool> = None;
            let title = if permanent { "Delete permanently" } else { "Delete" };
            egui::Window::new(title)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    let what = if paths.len() == 1 {
                        format!("'{}'", paths[0].file_name().unwrap_or_default().to_string_lossy())
                    } else {
                        format!("these {} items", paths.len())
                    };
                    if permanent {
                        ui.label(format!("Permanently delete {what}? This cannot be undone."));
                    } else {
                        ui.label(format!("Move {what} to the Recycle Bin?"));
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Delete").clicked() {
                            decision = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
                            decision = Some(false);
                        }
                    });
                });
            match decision {
                Some(true) => {
                    self.confirm = None;
                    self.spawn_op(ctx, Op::Delete { items: paths, permanent });
                }
                Some(false) => self.confirm = None,
                None => {}
            }
        }

        if self.show_settings {
            let mut open = true;
            let mut s = self.settings.clone();
            egui::Window::new("Settings")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .default_width(360.0)
                .show(ctx, |ui| {
                    ui.heading("Appearance");
                    ui.horizontal(|ui| {
                        ui.label("Theme");
                        ui.radio_value(&mut s.theme_mode, ThemeMode::System, "System");
                        ui.radio_value(&mut s.theme_mode, ThemeMode::Light, "Light");
                        ui.radio_value(&mut s.theme_mode, ThemeMode::Dark, "Dark");
                    });
                    ui.horizontal(|ui| {
                        ui.label("Accent");
                        for c in ACCENTS {
                            let (r, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                            ui.painter().rect_filled(r, R0, Color32::from_rgb(c[0], c[1], c[2]));
                            if s.accent == c {
                                outline(ui.painter(), r.expand(2.0), Stroke::new(1.5, ui.visuals().text_color()));
                            }
                            if resp.clicked() {
                                s.accent = c;
                            }
                        }
                        ui.color_edit_button_srgb(&mut s.accent);
                    });
                    ui.add_space(6.0);
                    ui.heading("Effects");
                    ui.checkbox(&mut s.acrylic, "Acrylic (blurred translucent background)");
                    ui.add_enabled(
                        s.acrylic,
                        egui::Slider::new(&mut s.acrylic_alpha, 30..=240).text("Tint opacity"),
                    );
                    ui.label(
                        egui::RichText::new("Acrylic needs Windows 10 1803+. Dragging/resizing may feel laggy with it on.")
                            .small()
                            .weak(),
                    );
                    ui.add_space(6.0);
                    ui.heading("Explorer");
                    ui.checkbox(&mut s.show_hidden, "Show hidden and system files");
                    ui.checkbox(&mut s.show_sidebar, "Show navigation pane");
                    ui.checkbox(&mut s.compact, "Compact rows");
                    ui.checkbox(&mut s.confirm_delete, "Confirm before moving to Recycle Bin");
                    ui.horizontal(|ui| {
                        if ui.button("Reset columns").clicked() {
                            s.cols = DEFAULT_COLS;
                        }
                        if ui.button("Reset Quick access").clicked() {
                            s.quick = default_quick();
                        }
                    });
                    ui.add_space(6.0);
                    if ui.button("Reset all to defaults").clicked() {
                        s = Settings::default();
                    }
                });
            if !open {
                self.show_settings = false;
            }
            if s != self.settings {
                let rebuild = s.show_hidden != self.settings.show_hidden;
                let fx = s.acrylic != self.settings.acrylic
                    || s.acrylic_alpha != self.settings.acrylic_alpha
                    || s.theme_mode != self.settings.theme_mode;
                self.settings = s;
                apply_visuals(ctx, &self.settings);
                if fx {
                    self.fx_dirty = true;
                }
                if rebuild {
                    let sh = self.settings.show_hidden;
                    for p in self.panes.iter_mut() {
                        for t in p.tabs.iter_mut() {
                            t.rebuild(sh, false);
                        }
                    }
                }
                ctx.request_repaint();
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_sidebar(
    ui: &mut egui::Ui,
    rect: Rect,
    quick: &[(String, PathBuf)],
    drives: &[PathBuf],
    current: &Path,
    pal: &Palette,
    icons: &Icons,
    acts: &mut Vec<Act>,
) {
    ui.painter().rect_filled(rect, R0, pal.side);
    ui.painter()
        .line_segment([rect.right_top(), rect.right_bottom()], Stroke::new(1.0, pal.border));
    let mut cui = ui.new_child(UiBuilder::new().max_rect(rect.shrink2(vec2(0.0, 6.0))));
    egui::ScrollArea::vertical()
        .id_salt("sidebar")
        .auto_shrink([false, false])
        .show(&mut cui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let qlen = quick.len();
            let item = |ui: &mut egui::Ui, acts: &mut Vec<Act>, label: &str, path: &Path, drive: bool, qidx: Option<usize>| {
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width() - 1.0, 26.0), Sense::click_and_drag());
                // dropping files onto an entry copies/moves them there
                let mut hover_drop = false;
                if let Some(pl) = resp.dnd_release_payload::<FileDrag>() {
                    acts.push(Act::DropTo { paths: pl.paths.clone(), dest: path.to_path_buf() });
                } else if resp.dnd_hover_payload::<FileDrag>().is_some() {
                    hover_drop = true;
                }
                if hover_drop {
                    ui.painter().rect_filled(r, R0, pal.sel);
                    outline(ui.painter(), r, Stroke::new(1.0, pal.accent));
                } else if current == path {
                    ui.painter().rect_filled(r, R0, pal.sel);
                } else if resp.hovered() {
                    ui.painter().rect_filled(r, R0, pal.hover);
                }
                let icon = Rect::from_min_size(pos2(r.left() + 14.0, r.center().y - 8.0), vec2(16.0, 16.0));
                if !icons.paint(ui.painter(), icon, Spec::Path(path)) {
                    if drive {
                        paint_drive(ui.painter(), icon);
                    } else {
                        paint_folder(ui.painter(), icon);
                    }
                }
                ui.painter().text(
                    pos2(r.left() + 38.0, r.center().y),
                    Align2::LEFT_CENTER,
                    label,
                    FontId::proportional(13.0),
                    pal.text,
                );
                if resp.clicked() {
                    acts.push(Act::GoTo(path.to_path_buf()));
                }
                if resp.middle_clicked() {
                    acts.push(Act::GoToNewTab(path.to_path_buf()));
                }
                resp.context_menu(|ui| {
                    if ui.button("Open in new tab").clicked() {
                        acts.push(Act::GoToNewTab(path.to_path_buf()));
                        ui.close();
                    }
                    if let Some(qi) = qidx {
                        ui.separator();
                        if ui.add_enabled(qi > 0, egui::Button::new("Move up")).clicked() {
                            acts.push(Act::MoveQuick(qi, -1));
                            ui.close();
                        }
                        if ui.add_enabled(qi + 1 < qlen, egui::Button::new("Move down")).clicked() {
                            acts.push(Act::MoveQuick(qi, 1));
                            ui.close();
                        }
                        if ui.button("Remove from Quick access").clicked() {
                            acts.push(Act::UnpinQuick(qi));
                            ui.close();
                        }
                    } else if !drive {
                        ui.separator();
                        if ui.button("Pin to Quick access").clicked() {
                            acts.push(Act::PinQuick(path.to_path_buf()));
                            ui.close();
                        }
                    }
                });
            };

            // "Quick access" header: dropping folders here pins them
            let (hr, hresp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
            if let Some(pl) = hresp.dnd_release_payload::<FileDrag>() {
                for p in pl.paths.iter().filter(|p| p.is_dir()) {
                    acts.push(Act::PinQuick(p.clone()));
                }
            } else if hresp.dnd_hover_payload::<FileDrag>().is_some() {
                ui.painter().rect_filled(hr, R0, pal.sel);
            }
            ui.painter().text(
                pos2(hr.left() + 14.0, hr.center().y + 2.0),
                Align2::LEFT_CENTER,
                "Quick access  (drop folders to pin)",
                FontId::proportional(12.0),
                pal.dim,
            );
            for (i, (name, path)) in quick.iter().enumerate() {
                item(ui, acts, name, path, false, Some(i));
            }

            // Recycle Bin: opens the system Recycle Bin window
            {
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width() - 1.0, 26.0), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(r, R0, pal.hover);
                }
                let icon = Rect::from_min_size(pos2(r.left() + 14.0, r.center().y - 8.0), vec2(16.0, 16.0));
                if !icons.paint(ui.painter(), icon, Spec::Recycle) {
                    paint_folder(ui.painter(), icon);
                }
                ui.painter().text(
                    pos2(r.left() + 38.0, r.center().y),
                    Align2::LEFT_CENTER,
                    "Recycle Bin",
                    FontId::proportional(13.0),
                    pal.text,
                );
                if resp.on_hover_text("Open the Recycle Bin").clicked() {
                    acts.push(Act::OpenRecycleBin);
                }
            }

            let (tr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
            ui.painter().text(
                pos2(tr.left() + 14.0, tr.center().y + 2.0),
                Align2::LEFT_CENTER,
                "This PC",
                FontId::proportional(12.0),
                pal.dim,
            );
            for d in drives {
                let letter = d.to_string_lossy().chars().next().unwrap_or('?');
                item(ui, acts, &format!("Local Disk ({letter}:)"), d, true, None);
            }
        });
}

/// Resize handles for the undecorated window.
fn edge_resize(ctx: &egui::Context, full: Rect, maximized: bool) {
    use egui::ResizeDirection as D;
    if maximized {
        return;
    }
    let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else { return };
    const B: f32 = 5.0;
    let l = pos.x - full.left() < B;
    let r = full.right() - pos.x < B;
    let t = pos.y - full.top() < B;
    let b = full.bottom() - pos.y < B;
    let dir = match (l, r, t, b) {
        (true, _, true, _) => D::NorthWest,
        (_, true, true, _) => D::NorthEast,
        (true, _, _, true) => D::SouthWest,
        (_, true, _, true) => D::SouthEast,
        (true, ..) => D::West,
        (_, true, ..) => D::East,
        (_, _, true, _) => D::North,
        (_, _, _, true) => D::South,
        _ => return,
    };
    ctx.set_cursor_icon(match dir {
        D::West | D::East => CursorIcon::ResizeHorizontal,
        D::North | D::South => CursorIcon::ResizeVertical,
        D::NorthWest | D::SouthEast => CursorIcon::ResizeNwSe,
        D::NorthEast | D::SouthWest => CursorIcon::ResizeNeSw,
    });
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
    }
}
