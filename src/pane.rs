//! Tabs, panes and their drawing code. Drawing never performs cross-cutting file
//! operations itself; it pushes `Act`s which `App` executes after the frame.

use std::{
    collections::HashSet,
    path::PathBuf,
    sync::mpsc::{Receiver, TryRecvError},
};

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CursorIcon, FontId, Id, Key, Layout, Painter, PointerButton,
    Rect, Sense, Shape, Stroke, Ui, UiBuilder,
};

use crate::{
    fsx::{self, Entry, SearchHandle, SearchMsg},
    icons::{Icons, Spec},
    sizes::SizeIndex,
    theme::*,
};

pub const TOOLBAR_H: f32 = 36.0;
pub const CMD_H: f32 = 28.0;
pub const HEADER_H: f32 = 24.0;

#[derive(Clone, Copy, PartialEq)]
pub enum Zone {
    Merge,
    Left,
    Right,
    Top,
    Bottom,
}

/// Drag-and-drop payloads (in-app).
pub struct TabDrag {
    pub pane: usize,
    pub idx: usize,
}
pub struct FileDrag {
    pub paths: Vec<PathBuf>,
}

pub enum Act {
    Activate(usize),
    GoTo(PathBuf),
    GoToNewTab(PathBuf),
    OpenFile(PathBuf),
    Reveal(PathBuf),
    NewTab { pane: usize, path: PathBuf },
    CloseTab { pane: usize, idx: usize },
    SelectTab { pane: usize, idx: usize },
    Copy(Vec<PathBuf>),
    Cut(Vec<PathBuf>),
    Paste(usize),
    Delete { paths: Vec<PathBuf>, permanent: bool },
    Rename { pane: usize, from: PathBuf, to: PathBuf },
    NewFolder(usize),
    /// Copy/move to the next pane in layout order.
    Transfer { pane: usize, paths: Vec<PathBuf>, mv: bool },
    CopyText(String),
    /// Files dropped onto a folder / pane (copy or move decided from modifiers and drive).
    DropTo { paths: Vec<PathBuf>, dest: PathBuf },
    TabDrop { from: usize, idx: usize, to: usize, zone: Zone, at: Option<usize> },
    SetCols([f32; 4]),
    /// Manual refresh of a folder: its cached sizes are re-measured when next shown.
    RefreshSizes(PathBuf),
    PinQuick(PathBuf),
    UnpinQuick(usize),
    MoveQuick(usize, isize),
    OpenRecycleBin,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Modified,
    Kind,
    Size,
}

pub struct Renaming {
    pub path: PathBuf,
    pub buf: String,
    pub started: bool,
}

/// State of an in-progress selection rectangle (content coordinates).
pub struct Rubber {
    pub start: egui::Pos2,
    pub base: HashSet<PathBuf>,
}

pub struct Tab {
    pub uid: u64,
    pub path: PathBuf,
    back: Vec<PathBuf>,
    fwd: Vec<PathBuf>,
    pub entries: Vec<Entry>,
    pub visible: Vec<usize>,
    pub selected: HashSet<PathBuf>,
    pub anchor: usize,
    pub cursor: usize,
    pub sort: SortKey,
    pub asc: bool,
    loading: Option<Receiver<Result<Vec<Entry>, String>>>,
    pub error: Option<String>,
    pub addr: String,
    pub search_text: String,
    pub search: Option<SearchHandle>,
    pub search_mode: bool,
    pub search_running: bool,
    pub search_truncated: bool,
    pub search_root: PathBuf,
    pub renaming: Option<Renaming>,
    scroll_y: f32,
    scroll_x: f32,
    view_h: f32,
    pub rubber: Option<Rubber>,
    scroll_to_cursor: bool,
    scroll_target: Option<f32>,
    pub select_after_load: Option<PathBuf>,
    pub rename_after_load: bool,
    pub stale: bool,
    pub focus_addr: bool,
    pub focus_search: bool,
}

impl Tab {
    pub fn new(uid: u64, path: PathBuf, ctx: &egui::Context) -> Self {
        let mut t = Tab {
            uid,
            addr: path.display().to_string(),
            search_root: path.clone(),
            path,
            back: vec![],
            fwd: vec![],
            entries: vec![],
            visible: vec![],
            selected: HashSet::new(),
            anchor: 0,
            cursor: 0,
            sort: SortKey::Name,
            asc: true,
            loading: None,
            error: None,
            search_text: String::new(),
            search: None,
            search_mode: false,
            search_running: false,
            search_truncated: false,
            renaming: None,
            scroll_y: 0.0,
            scroll_x: 0.0,
            view_h: 400.0,
            rubber: None,
            scroll_to_cursor: false,
            scroll_target: None,
            select_after_load: None,
            rename_after_load: false,
            stale: false,
            focus_addr: false,
            focus_search: false,
        };
        t.start_load(ctx);
        t
    }

    pub fn title(&self) -> String {
        if self.search_mode {
            return format!("Search: {}", self.search_text);
        }
        match self.path.file_name() {
            Some(n) => n.to_string_lossy().into_owned(),
            None => self.path.display().to_string(),
        }
    }

    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }
    pub fn can_fwd(&self) -> bool {
        !self.fwd.is_empty()
    }

    fn start_load(&mut self, ctx: &egui::Context) {
        self.error = None;
        self.stale = false;
        self.loading = Some(fsx::spawn_load(self.path.clone(), ctx.clone()));
    }

    fn enter(&mut self, ctx: &egui::Context) {
        self.addr = self.path.display().to_string();
        self.selected.clear();
        self.cursor = 0;
        self.anchor = 0;
        self.search = None;
        self.search_running = false;
        self.search_mode = false;
        self.search_text.clear();
        self.renaming = None;
        self.scroll_target = Some(0.0);
        self.start_load(ctx);
    }

    pub fn navigate(&mut self, path: PathBuf, ctx: &egui::Context) {
        if path == self.path && !self.search_mode {
            self.refresh(ctx);
            return;
        }
        let old = std::mem::replace(&mut self.path, path);
        self.back.push(old);
        self.fwd.clear();
        self.enter(ctx);
    }

    pub fn go_back(&mut self, ctx: &egui::Context) {
        if let Some(p) = self.back.pop() {
            let cur = std::mem::replace(&mut self.path, p);
            self.fwd.push(cur);
            self.enter(ctx);
        }
    }

    pub fn go_forward(&mut self, ctx: &egui::Context) {
        if let Some(p) = self.fwd.pop() {
            let cur = std::mem::replace(&mut self.path, p);
            self.back.push(cur);
            self.enter(ctx);
        }
    }

    pub fn go_up(&mut self, ctx: &egui::Context) {
        if let Some(parent) = self.path.parent().map(|p| p.to_path_buf()) {
            let from = self.path.clone();
            self.navigate(parent, ctx);
            self.select_after_load = Some(from);
        }
    }

    pub fn refresh(&mut self, ctx: &egui::Context) {
        if self.search_mode {
            return;
        }
        self.start_load(ctx);
    }

    // ---------------------------------------------------------------- search

    pub fn start_search(&mut self, ctx: &egui::Context) {
        let q = self.search_text.trim().to_string();
        if q.is_empty() {
            self.exit_search(ctx);
            return;
        }
        self.search = None; // cancels any running walk
        self.loading = None;
        self.entries.clear();
        self.visible.clear();
        self.selected.clear();
        self.cursor = 0;
        self.anchor = 0;
        self.search_mode = true;
        self.search_running = true;
        self.search_truncated = false;
        self.search_root = self.path.clone();
        self.scroll_target = Some(0.0);
        self.search = Some(fsx::spawn_search(self.path.clone(), q, ctx.clone()));
    }

    pub fn exit_search(&mut self, ctx: &egui::Context) {
        self.search = None;
        self.search_running = false;
        self.search_text.clear();
        if self.search_mode {
            self.search_mode = false;
            self.selected.clear();
            self.start_load(ctx);
        }
    }

    // ---------------------------------------------------------------- data

    pub fn poll(&mut self, show_hidden: bool) -> bool {
        let mut changed = false;

        let loaded = self.loading.as_ref().map(|rx| rx.try_recv());
        match loaded {
            Some(Ok(res)) => {
                self.loading = None;
                match res {
                    Ok(v) => {
                        self.entries = v;
                        self.error = None;
                    }
                    Err(e) => {
                        self.entries.clear();
                        self.error = Some(e);
                    }
                }
                self.rebuild(show_hidden, true);
                self.after_load();
                changed = true;
            }
            Some(Err(TryRecvError::Disconnected)) => self.loading = None,
            _ => {}
        }

        let mut msgs = Vec::new();
        if let Some(h) = &self.search {
            while let Ok(m) = h.rx.try_recv() {
                msgs.push(m);
            }
        }
        if !msgs.is_empty() {
            let mut need_sort = false;
            for m in msgs {
                match m {
                    SearchMsg::Batch(b) => self.entries.extend(b),
                    SearchMsg::Done { truncated } => {
                        self.search_running = false;
                        self.search_truncated = truncated;
                        need_sort = true;
                    }
                }
            }
            self.rebuild(show_hidden, need_sort);
            changed = true;
        }
        changed
    }

    pub fn rebuild(&mut self, show_hidden: bool, sort: bool) {
        if sort {
            let (key, asc, in_search) = (self.sort, self.asc, self.search_mode);
            self.entries.sort_by(|a, b| {
                if !in_search {
                    let o = b.is_dir.cmp(&a.is_dir); // folders first, regardless of direction
                    if o != std::cmp::Ordering::Equal {
                        return o;
                    }
                }
                let o = match key {
                    SortKey::Name => fsx::natural_cmp(&a.name_lc, &b.name_lc),
                    SortKey::Modified => a.modified.cmp(&b.modified),
                    SortKey::Kind => a
                        .kind
                        .cmp(&b.kind)
                        .then_with(|| fsx::natural_cmp(&a.name_lc, &b.name_lc)),
                    SortKey::Size => a
                        .size
                        .cmp(&b.size)
                        .then_with(|| fsx::natural_cmp(&a.name_lc, &b.name_lc)),
                };
                if asc {
                    o
                } else {
                    o.reverse()
                }
            });
        }
        self.visible = (0..self.entries.len())
            .filter(|&i| show_hidden || !self.entries[i].hidden)
            .collect();
        if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len().saturating_sub(1);
        }
    }

    fn after_load(&mut self) {
        let existing: HashSet<&PathBuf> = self.entries.iter().map(|e| &e.path).collect();
        self.selected.retain(|p| existing.contains(p));
        if let Some(p) = self.select_after_load.take() {
            let pos = self.visible.iter().position(|&ix| self.entries[ix].path == p);
            if let Some(i) = pos {
                self.selected.clear();
                self.selected.insert(p);
                self.cursor = i;
                self.anchor = i;
                self.scroll_to_cursor = true;
                if self.rename_after_load {
                    self.start_rename();
                }
            }
        }
        self.rename_after_load = false;
    }

    // ---------------------------------------------------------------- selection

    pub fn selected_paths(&self) -> Vec<PathBuf> {
        self.visible
            .iter()
            .map(|&i| &self.entries[i])
            .filter(|e| self.selected.contains(&e.path))
            .map(|e| e.path.clone())
            .collect()
    }

    pub fn select_only(&mut self, i: usize) {
        self.selected.clear();
        if let Some(&ix) = self.visible.get(i) {
            self.selected.insert(self.entries[ix].path.clone());
        }
        self.cursor = i;
        self.anchor = i;
    }

    pub fn toggle(&mut self, i: usize) {
        if let Some(&ix) = self.visible.get(i) {
            let p = self.entries[ix].path.clone();
            if !self.selected.remove(&p) {
                self.selected.insert(p);
            }
        }
        self.cursor = i;
        self.anchor = i;
    }

    pub fn select_range(&mut self, a: usize, b: usize, keep: bool) {
        if !keep {
            self.selected.clear();
        }
        if self.visible.is_empty() {
            return;
        }
        let (lo, hi) = (a.min(b), a.max(b).min(self.visible.len() - 1));
        for i in lo..=hi {
            let ix = self.visible[i];
            self.selected.insert(self.entries[ix].path.clone());
        }
        self.cursor = b;
    }

    pub fn select_all(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        self.select_range(0, self.visible.len() - 1, false);
        self.anchor = 0;
    }

    fn set_cursor(&mut self, new: usize, extend: bool) {
        if extend {
            let a = self.anchor;
            self.select_range(a, new, false);
        } else {
            self.select_only(new);
        }
        self.cursor = new;
        self.scroll_to_cursor = true;
    }

    pub fn move_cursor(&mut self, delta: isize, extend: bool) {
        let n = self.visible.len();
        if n == 0 {
            return;
        }
        let new = if self.selected.is_empty() {
            if delta >= 0 {
                0
            } else {
                n - 1
            }
        } else {
            (self.cursor as isize + delta).clamp(0, n as isize - 1) as usize
        };
        self.set_cursor(new, extend);
    }

    pub fn page(&mut self, sign: isize, extend: bool, row_h: f32) {
        let rows = ((self.view_h / row_h) as isize - 1).max(1);
        self.move_cursor(sign * rows, extend);
    }

    pub fn jump(&mut self, to_end: bool, extend: bool) {
        if self.visible.is_empty() {
            return;
        }
        let n = if to_end { self.visible.len() - 1 } else { 0 };
        self.set_cursor(n, extend);
    }

    pub fn start_rename(&mut self) {
        if self.selected.len() != 1 {
            return;
        }
        if let Some(p) = self.selected.iter().next().cloned() {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            self.renaming = Some(Renaming { path: p, buf: name, started: false });
        }
    }

    pub fn activate(&mut self, ctx: &egui::Context) -> Vec<Act> {
        let paths = self.selected_paths();
        let mut acts = vec![];
        if paths.len() == 1 && paths[0].is_dir() {
            self.navigate(paths[0].clone(), ctx);
        } else {
            for p in paths {
                if !p.is_dir() {
                    acts.push(Act::OpenFile(p));
                }
            }
        }
        acts
    }

    pub fn selection_info(&self) -> String {
        if self.search_mode {
            let more = if self.search_truncated { "+ (limit reached)" } else { "" };
            let state = if self.search_running { "Searching..." } else { "Search complete:" };
            return format!("{state} {}{more} result{}", self.visible.len(), plural(self.visible.len()));
        }
        let n = self.visible.len();
        let mut s = format!("{n} item{}", plural(n));
        let k = self.selected.len();
        if k > 0 {
            s += &format!("   |   {k} selected");
            if k <= 2000 {
                let total: u64 = self
                    .visible
                    .iter()
                    .map(|&i| &self.entries[i])
                    .filter(|e| !e.is_dir && self.selected.contains(&e.path))
                    .map(|e| e.size)
                    .sum();
                if total > 0 {
                    s += &format!("   {}", fsx::fmt_size(total));
                }
            }
        }
        s
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

pub struct Pane {
    pub id: usize,
    pub tabs: Vec<Tab>,
    pub active: usize,
}
impl Pane {
    pub fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }
    pub fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }
}

// ====================================================================== drawing

/// Height of the tab strip; equals the caption-button height in the title bar.
pub const STRIP_H: f32 = 32.0;

pub struct PaneCtx<'a> {
    pub pal: &'a Palette,
    pub settings: &'a Settings,
    pub clip_cut: &'a [PathBuf],
    pub can_paste: bool,
    pub active: bool,
    pub idx: usize, // pane id
    pub icons: &'a Icons,
    pub sizes: &'a SizeIndex,
    pub strip_inset: f32, // width kept free at the strip's right end (caption buttons)
    pub can_split: bool,  // fewer than MAX_PANES panes exist
    pub has_next: bool,   // more than one pane exists
}

fn tip(r: egui::Response, name: &str, key: &str) -> egui::Response {
    if key.is_empty() {
        r.on_hover_text(name.to_string())
    } else {
        r.on_hover_text(format!("{name} ({key})"))
    }
}

fn paint_small_x(p: &Painter, c: egui::Pos2, col: Color32) {
    let st = Stroke::new(1.0, col);
    let c = pos2(c.x.floor() + 0.5, c.y.floor() + 0.5);
    p.line_segment([c + vec2(-3.0, -3.0), c + vec2(3.0, 3.0)], st);
    p.line_segment([c + vec2(-3.0, 3.0), c + vec2(3.0, -3.0)], st);
}

pub fn drop_zone(body: Rect, p: egui::Pos2, can_split: bool) -> Zone {
    if !can_split || p.y < body.top() {
        return Zone::Merge;
    }
    let fx = (p.x - body.left()) / body.width().max(1.0);
    let fy = (p.y - body.top()) / body.height().max(1.0);
    if fx < 0.25 {
        Zone::Left
    } else if fx > 0.75 {
        Zone::Right
    } else if fy < 0.25 {
        Zone::Top
    } else if fy > 0.75 {
        Zone::Bottom
    } else {
        Zone::Merge
    }
}

pub fn zone_rect(b: Rect, z: Zone) -> Rect {
    match z {
        Zone::Merge => b,
        Zone::Left => Rect::from_min_max(b.min, pos2(b.center().x, b.bottom())),
        Zone::Right => Rect::from_min_max(pos2(b.center().x, b.top()), b.max),
        Zone::Top => Rect::from_min_max(b.min, pos2(b.right(), b.center().y)),
        Zone::Bottom => Rect::from_min_max(pos2(b.left(), b.center().y), b.max),
    }
}

/// Tabs of one pane. Dragging a tab onto another strip moves it; dragging it onto the
/// edge of any pane splits that pane (see `draw_pane`).
fn draw_tabs(ui: &mut Ui, rect: Rect, pane: &Pane, pc: &PaneCtx, acts: &mut Vec<Act>) {
    let ctx = ui.ctx().clone();
    let pal = pc.pal;
    let pane_idx = pc.idx;
    let n = pane.tabs.len();
    let plus_w = 32.0;
    let avail = (rect.width() - plus_w).max(40.0);
    let tw = (avail / n as f32).clamp(40.0, 200.0);
    let closable = n > 1 || pc.has_next;

    // drop target: reorder / move a dragged tab into this pane
    let strip_resp = ui.interact(rect, Id::unique(("strip", pane_idx)), Sense::hover());
    if let Some(pl) = strip_resp.dnd_release_payload::<TabDrag>() {
        let x = ctx.pointer_interact_pos().map_or(rect.left(), |p| p.x);
        let at = (((x - rect.left()) / tw).round() as isize).clamp(0, n as isize) as usize;
        acts.push(Act::TabDrop { from: pl.pane, idx: pl.idx, to: pane_idx, zone: Zone::Merge, at: Some(at) });
    } else if strip_resp.dnd_hover_payload::<TabDrag>().is_some() {
        if let Some(p) = ctx.pointer_interact_pos() {
            let at = ((p.x - rect.left()) / tw).round().clamp(0.0, n as f32);
            let x = rect.left() + at * tw;
            ui.painter().rect_filled(
                Rect::from_min_size(pos2(x - 1.0, rect.top() + 2.0), vec2(2.0, rect.height() - 4.0)),
                R0,
                pal.accent,
            );
        }
    }
    let dragging: Option<(usize, usize)> = egui::DragAndDrop::payload::<TabDrag>(&ctx).map(|t| (t.pane, t.idx));

    for (idx, tab) in pane.tabs.iter().enumerate() {
        let tr = Rect::from_min_size(pos2(rect.left() + idx as f32 * tw, rect.top()), vec2(tw, rect.height()));
        if tr.right() > rect.right() + 1.0 {
            break;
        }
        let resp = ui.interact(tr, Id::unique(("tab", pane_idx, tab.uid)), Sense::click_and_drag());
        if resp.drag_started_by(PointerButton::Primary) {
            resp.dnd_set_drag_payload(TabDrag { pane: pane_idx, idx });
        }
        let active = idx == pane.active;
        let being_dragged = dragging == Some((pane_idx, idx));
        let p = ui.painter();
        if active {
            p.rect_filled(tr, R0, pal.content);
            let line = if pc.active { pal.accent } else { pal.border };
            p.rect_filled(Rect::from_min_size(tr.left_top(), vec2(tr.width(), 2.0)), R0, line);
        } else if resp.hovered() {
            p.rect_filled(tr, R0, pal.hover);
        }
        let ir = Rect::from_min_size(pos2(tr.left() + 8.0, tr.center().y - 8.0), vec2(16.0, 16.0));
        if !pc.icons.paint(p, ir, Spec::Dir) {
            paint_folder(p, ir);
        }
        let col = if being_dragged {
            pal.dim.gamma_multiply(0.5)
        } else if active {
            pal.text
        } else {
            pal.dim
        };
        let clip = Rect::from_min_max(pos2(tr.left() + 28.0, tr.top()), pos2(tr.right() - 24.0, tr.bottom()));
        p.with_clip_rect(clip).text(
            pos2(tr.left() + 28.0, tr.center().y),
            Align2::LEFT_CENTER,
            tab.title(),
            FontId::proportional(13.0),
            col,
        );

        let mut close_clicked = false;
        if closable && (active || resp.hovered()) {
            let cr = Rect::from_center_size(pos2(tr.right() - 14.0, tr.center().y), vec2(18.0, 18.0));
            let cresp = tip(
                ui.interact(cr, Id::unique(("tabclose", pane_idx, tab.uid)), Sense::click()),
                "Close tab",
                "Ctrl+W",
            );
            if cresp.hovered() {
                ui.painter().rect_filled(cr, R0, pal.caption_hover);
            }
            let mut g = ui.painter().clone();
            g.set_clip_rect(cr.expand(2.0));
            paint_small_x(&g, cr.center(), pal.text);
            close_clicked = cresp.clicked();
        }
        if close_clicked || (resp.middle_clicked() && closable) {
            acts.push(Act::CloseTab { pane: pane_idx, idx });
        } else if resp.clicked() {
            acts.push(Act::Activate(pane_idx));
            acts.push(Act::SelectTab { pane: pane_idx, idx });
        }
    }

    let px = (rect.left() + n as f32 * tw).min(rect.right() - plus_w);
    let pr = Rect::from_min_size(pos2(px, rect.top()), vec2(plus_w, rect.height()));
    let resp = tip(ui.interact(pr, Id::unique(("newtab", pane_idx)), Sense::click()), "New tab", "Ctrl+T");
    if resp.hovered() {
        ui.painter().rect_filled(pr, R0, pal.caption_hover);
    }
    paint_glyph(ui.painter(), pr.center(), Glyph::Plus, pal.text);
    if resp.clicked() {
        acts.push(Act::NewTab { pane: pane_idx, path: pane.tab().path.clone() });
    }
}

fn glyph_button(ui: &mut Ui, rect: Rect, id: Id, g: Glyph, enabled: bool, pal: &Palette) -> egui::Response {
    let resp = ui.interact(rect, id, if enabled { Sense::click() } else { Sense::hover() });
    if enabled && resp.hovered() {
        ui.painter().rect_filled(rect, R0, pal.caption_hover);
    }
    paint_glyph(ui.painter(), rect.center(), g, if enabled { pal.text } else { pal.dim });
    resp
}

/// Effective column widths for a pane `avail` pixels wide, plus the table width.
fn fit_cols(stored: [f32; 4], avail: f32) -> ([f32; 4], f32) {
    let mut e = stored;
    for i in 0..4 {
        e[i] = e[i].max(MIN_COLS[i]);
    }
    let min_sum: f32 = MIN_COLS.iter().sum();
    let target = avail.max(min_sum);
    let sum: f32 = e.iter().sum();
    if sum > target {
        // take the overflow from every column in proportion to its slack above the minimum
        let slack: f32 = (0..4).map(|i| e[i] - MIN_COLS[i]).sum();
        let need = sum - target;
        for i in 0..4 {
            e[i] -= need * (e[i] - MIN_COLS[i]) / slack.max(1e-6);
        }
    }
    let sum: f32 = e.iter().sum();
    (e, sum.max(avail))
}

/// Moves the boundary on the right edge of column `i` by `dx`. The boundary follows the
/// mouse: growing uses blank space first and then squeezes the columns to its right (down
/// to their minimums); shrinking just frees space. Nothing ever drops below its minimum.
fn resize_boundary(mut e: [f32; 4], i: usize, dx: f32, avail: f32) -> [f32; 4] {
    let target = avail.max(MIN_COLS.iter().sum());
    let room = (target - e.iter().sum::<f32>()).max(0.0);
    let mut d = dx.max(MIN_COLS[i] - e[i]);
    if d > room {
        let absorb: f32 = ((i + 1)..4).map(|j| e[j] - MIN_COLS[j]).sum();
        d = d.min(room + absorb);
    }
    let mut take = (d - room).max(0.0);
    e[i] += d;
    for j in (i + 1)..4 {
        if take <= 0.0 {
            break;
        }
        let t = take.min(e[j] - MIN_COLS[j]);
        e[j] -= t;
        take -= t;
    }
    e
}

pub fn row_height(s: &Settings) -> f32 {
    if s.compact {
        20.0
    } else {
        24.0
    }
}

// ---------------------------------------------------------------- command bar

#[derive(Clone, Copy, PartialEq)]
enum Cmd {
    NewFolder,
    Cut,
    Copy,
    Paste,
    Rename,
    Delete,
    CopyTo,
    MoveTo,
}

struct CmdItem {
    cmd: Cmd,
    label: &'static str,
    key: &'static str,
    en: bool,
    w: f32,
    group: usize,
}

/// Grouped command bar: New | Clipboard | Organize | Transfer. Groups are divided by
/// separators, every button has a fixed padding and a tooltip with its shortcut, and
/// anything that does not fit moves into an overflow menu instead of being clipped.
fn cmd_bar(ui: &mut Ui, cb: Rect, tab: &mut Tab, pc: &PaneCtx, acts: &mut Vec<Act>) {
    let pal = pc.pal;
    let idx = pc.idx;
    ui.painter().rect_filled(cb, R0, pal.chrome);
    ui.painter()
        .line_segment([cb.left_bottom(), cb.right_bottom()], Stroke::new(1.0, pal.border));

    let has_sel = !tab.selected.is_empty();
    let single = tab.selected.len() == 1;
    let spec: [(Cmd, &'static str, &'static str, bool, usize); 8] = [
        (Cmd::NewFolder, "New folder", "Ctrl+Shift+N", true, 0),
        (Cmd::Cut, "Cut", "Ctrl+X", has_sel, 1),
        (Cmd::Copy, "Copy", "Ctrl+C", has_sel, 1),
        (Cmd::Paste, "Paste", "Ctrl+V", pc.can_paste, 1),
        (Cmd::Rename, "Rename", "F2", single, 2),
        (Cmd::Delete, "Delete", "Del", has_sel, 2),
        (Cmd::CopyTo, "Copy to next pane", "", has_sel && pc.has_next, 3),
        (Cmd::MoveTo, "Move to next pane", "", has_sel && pc.has_next, 3),
    ];
    let font = FontId::proportional(13.0);
    let pad = 10.0;
    let sep_gap = 19.0; // 9px + 1px line + 9px
    let ov_w = 30.0;
    let items: Vec<CmdItem> = spec
        .iter()
        .map(|&(cmd, label, key, en, group)| CmdItem {
            cmd,
            label,
            key,
            en,
            w: ui.painter().layout_no_wrap(label.to_string(), font.clone(), pal.text).size().x + 2.0 * pad,
            group,
        })
        .collect();

    let x0 = cb.left() + 4.0;
    let avail = cb.right() - 4.0 - x0;
    let need: f32 = items.iter().map(|i| i.w).sum::<f32>() + 3.0 * sep_gap;
    let budget = if need <= avail { avail } else { avail - ov_w };

    let mut fired: Option<Cmd> = None;
    let mut cx = x0;
    let mut placed = 0usize;
    let mut last_group = items[0].group;
    for it in &items {
        let gap = if it.group != last_group { sep_gap } else { 0.0 };
        if cx + gap + it.w > x0 + budget {
            break;
        }
        if gap > 0.0 {
            let sx = (cx + 9.0).round() + 0.5;
            ui.painter().line_segment(
                [pos2(sx, cb.top() + 6.0), pos2(sx, cb.bottom() - 6.0)],
                Stroke::new(1.0, pal.border),
            );
            cx += gap;
        }
        last_group = it.group;
        let r = Rect::from_min_size(pos2(cx, cb.top() + 2.0), vec2(it.w, cb.height() - 4.0));
        let resp = ui.interact(
            r,
            Id::unique(("cmd", idx, it.label)),
            if it.en { Sense::click() } else { Sense::hover() },
        );
        if it.en && resp.hovered() {
            ui.painter().rect_filled(r, R0, pal.caption_hover);
        }
        ui.painter().text(
            r.center(),
            Align2::CENTER_CENTER,
            it.label,
            font.clone(),
            if it.en { pal.text } else { pal.dim },
        );
        let resp = tip(resp, it.label, it.key);
        if it.en && resp.clicked() {
            fired = Some(it.cmd);
        }
        cx += it.w;
        placed += 1;
    }
    if placed < items.len() {
        let ovr = Rect::from_min_size(pos2(cx + 4.0, cb.top() + 2.0), vec2(ov_w, cb.height() - 4.0));
        let mut cui = ui.new_child(UiBuilder::new().max_rect(ovr).layout(Layout::left_to_right(Align::Center)));
        cui.menu_button("...", |ui| {
            for it in &items[placed..] {
                if ui.add_enabled(it.en, egui::Button::new(it.label)).clicked() {
                    fired = Some(it.cmd);
                    ui.close();
                }
            }
        });
    }

    match fired {
        Some(Cmd::NewFolder) => acts.push(Act::NewFolder(idx)),
        Some(Cmd::Cut) => acts.push(Act::Cut(tab.selected_paths())),
        Some(Cmd::Copy) => acts.push(Act::Copy(tab.selected_paths())),
        Some(Cmd::Paste) => acts.push(Act::Paste(idx)),
        Some(Cmd::Rename) => tab.start_rename(),
        Some(Cmd::Delete) => acts.push(Act::Delete { paths: tab.selected_paths(), permanent: false }),
        Some(Cmd::CopyTo) => acts.push(Act::Transfer { pane: idx, paths: tab.selected_paths(), mv: false }),
        Some(Cmd::MoveTo) => acts.push(Act::Transfer { pane: idx, paths: tab.selected_paths(), mv: true }),
        None => {}
    }
}

// ---------------------------------------------------------------- pane

/// Draws one pane inside `rect`: tab strip, address/search toolbar, command bar,
/// resizable column header and the virtualised file list. The first `STRIP_H` pixels
/// are the tab strip, so a pane on the top row shares the title bar's height exactly.
pub fn draw_pane(ui: &mut Ui, rect: Rect, pane: &mut Pane, pc: &PaneCtx, acts: &mut Vec<Act>, typing: &mut bool) {
    let ctx = ui.ctx().clone();
    let pal = pc.pal;
    let idx = pc.idx;
    let settings = pc.settings;

    ui.painter().rect_filled(rect, R0, pal.content);
    let strip = Rect::from_min_size(rect.min, vec2(rect.width(), STRIP_H));
    ui.painter().rect_filled(strip, R0, pal.chrome);

    // clicking anywhere in the pane makes it the active one
    let pressed_inside = ctx.input(|i| {
        i.pointer.any_pressed() && i.pointer.interact_pos().map_or(false, |p| rect.contains(p))
    });
    if pressed_inside {
        acts.push(Act::Activate(idx));
    }

    draw_tabs(
        ui,
        Rect::from_min_max(strip.min, pos2((strip.right() - pc.strip_inset).max(strip.left() + 60.0), strip.bottom())),
        &*pane,
        pc,
        acts,
    );

    let tab = pane.tab_mut();
    let uid = tab.uid;

    // ---------------------------------------------------------- toolbar
    let tb = Rect::from_min_size(pos2(rect.left(), strip.bottom()), vec2(rect.width(), TOOLBAR_H));
    ui.painter().rect_filled(tb, R0, pal.chrome);
    let by = tb.center().y;
    let mut x = tb.left() + 4.0;
    let btn = |x: f32| Rect::from_center_size(pos2(x + 14.0, by), vec2(28.0, 28.0));

    let r = glyph_button(ui, btn(x), Id::unique(("back", uid)), Glyph::Back, tab.can_back(), pal);
    if tip(r, "Back", "Alt+Left").clicked() {
        tab.go_back(&ctx);
    }
    x += 30.0;
    let r = glyph_button(ui, btn(x), Id::unique(("fwd", uid)), Glyph::Forward, tab.can_fwd(), pal);
    if tip(r, "Forward", "Alt+Right").clicked() {
        tab.go_forward(&ctx);
    }
    x += 30.0;
    let can_up = tab.path.parent().is_some();
    let r = glyph_button(ui, btn(x), Id::unique(("up", uid)), Glyph::Up, can_up, pal);
    if tip(r, "Up one level", "Alt+Up").clicked() {
        tab.go_up(&ctx);
    }
    x += 30.0;
    let r = glyph_button(ui, btn(x), Id::unique(("refresh", uid)), Glyph::Refresh, !tab.search_mode, pal);
    if tip(r, "Refresh", "F5").clicked() {
        tab.refresh(&ctx);
        acts.push(Act::RefreshSizes(tab.path.clone()));
    }
    x += 32.0;

    let search_w = (rect.width() * 0.3).clamp(110.0, 260.0);
    let search_rect =
        Rect::from_min_max(pos2((tb.right() - search_w - 8.0).max(x + 60.0), by - 13.0), pos2(tb.right() - 8.0, by + 13.0));
    let addr_rect = Rect::from_min_max(pos2(x, by - 13.0), pos2((search_rect.left() - 8.0).max(x + 40.0), by + 13.0));

    let addr_resp = ui.put(
        addr_rect,
        egui::TextEdit::singleline(&mut tab.addr)
            .id(Id::unique(("addr", uid)))
            .desired_width(addr_rect.width()),
    );
    let addr_resp = tip(addr_resp, "Address", "Ctrl+L");
    if tab.focus_addr {
        tab.focus_addr = false;
        addr_resp.request_focus();
    }
    if addr_resp.has_focus() {
        *typing = true;
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            tab.addr = tab.path.display().to_string();
            addr_resp.surrender_focus();
        }
    }
    if addr_resp.lost_focus() && ctx.input(|i| i.key_pressed(Key::Enter)) {
        let p = PathBuf::from(tab.addr.trim().trim_matches('"'));
        if p.is_dir() {
            tab.navigate(p, &ctx);
        } else {
            tab.error = Some(format!("Cannot find '{}'", tab.addr.trim()));
            tab.addr = tab.path.display().to_string();
        }
    }

    let hint = format!(
        "Search {}",
        tab.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| tab.path.display().to_string())
    );
    let search_resp = ui.put(
        search_rect,
        egui::TextEdit::singleline(&mut tab.search_text)
            .id(Id::unique(("search", uid)))
            .hint_text(hint)
            .desired_width(search_rect.width()),
    );
    let search_resp = tip(search_resp, "Search this folder and all subfolders", "Ctrl+F");
    if tab.focus_search {
        tab.focus_search = false;
        search_resp.request_focus();
    }
    if search_resp.has_focus() {
        *typing = true;
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            tab.exit_search(&ctx);
            search_resp.surrender_focus();
        }
    }
    if search_resp.lost_focus() && ctx.input(|i| i.key_pressed(Key::Enter)) {
        tab.start_search(&ctx);
    }

    // ---------------------------------------------------------- command bar
    let mut y = tb.bottom();
    if rect.height() >= 300.0 {
        cmd_bar(ui, Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), CMD_H)), tab, pc, acts);
        y += CMD_H;
    }

    // ---------------------------------------------------------- rigid, resizable table
    let area = Rect::from_min_max(pos2(rect.left(), y), rect.max);
    // Widths are exactly what the user set, shrunk proportionally (never below the minimums)
    // when the pane is narrow. Spare room stays blank on the right, like Explorer; the table
    // only scrolls sideways when even the minimum widths do not fit.
    let (cols, table_w) = fit_cols(settings.cols, area.width());

    let hdr = Rect::from_min_size(area.min, vec2(area.width(), HEADER_H));
    ui.painter().rect_filled(hdr, R0, pal.content);
    ui.painter()
        .line_segment([hdr.left_bottom(), hdr.right_bottom()], Stroke::new(1.0, pal.border));
    let hx0 = hdr.left() - tab.scroll_x;
    let col_x = [hx0, hx0 + cols[0], hx0 + cols[0] + cols[1], hx0 + cols[0] + cols[1] + cols[2]];
    let labels = ["Name", "Date modified", "Type", "Size"];
    let keys = [SortKey::Name, SortKey::Modified, SortKey::Kind, SortKey::Size];
    let hp = ui.painter().with_clip_rect(hdr);
    let mut new_cols = cols;
    let mut cols_changed = false;
    for i in 0..4 {
        let cr = Rect::from_min_size(pos2(col_x[i], hdr.top()), vec2(cols[i], HEADER_H));
        let vis = cr.intersect(hdr);
        if vis.width() <= 0.0 {
            continue;
        }
        let r = tip(
            ui.interact(vis, Id::unique(("hdr", uid, labels[i])), Sense::click()),
            &format!("Sort by {}", labels[i].to_lowercase()),
            "",
        );
        if r.hovered() {
            hp.rect_filled(vis, R0, pal.hover);
        }
        let font = FontId::proportional(12.0);
        if i == 3 {
            hp.text(pos2(cr.right() - 10.0, cr.center().y + 1.0), Align2::RIGHT_CENTER, labels[i], font, pal.dim);
        } else {
            hp.text(pos2(cr.left() + 8.0, cr.center().y + 1.0), Align2::LEFT_CENTER, labels[i], font, pal.dim);
        }
        if tab.sort == keys[i] {
            let (ax, ay) = (cr.center().x, cr.top() + 4.0);
            let pts = if tab.asc {
                vec![pos2(ax - 3.0, ay + 3.0), pos2(ax + 3.0, ay + 3.0), pos2(ax, ay)]
            } else {
                vec![pos2(ax - 3.0, ay), pos2(ax + 3.0, ay), pos2(ax, ay + 3.0)]
            };
            hp.add(Shape::convex_polygon(pts, pal.dim, Stroke::NONE));
        }
        hp.line_segment(
            [pos2(cr.right() - 0.5, cr.top() + 4.0), pos2(cr.right() - 0.5, cr.bottom() - 4.0)],
            Stroke::new(1.0, pal.border),
        );
        if r.clicked() {
            if tab.sort == keys[i] {
                tab.asc = !tab.asc;
            } else {
                tab.sort = keys[i];
                tab.asc = matches!(keys[i], SortKey::Name | SortKey::Kind);
            }
            tab.rebuild(settings.show_hidden, true);
        }
        // resize handle on the column's right edge
        let hr = Rect::from_min_max(pos2(cr.right() - 3.0, hdr.top()), pos2(cr.right() + 3.0, hdr.bottom())).intersect(hdr);
        if hr.width() > 0.0 {
            let h = ui.interact(hr, Id::unique(("colresize", uid, i)), Sense::drag());
            if h.hovered() || h.dragged() {
                ctx.set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
            if h.dragged() {
                new_cols = resize_boundary(new_cols, i, h.drag_delta().x, area.width());
                cols_changed = true;
            }
        }
    }
    if cols_changed {
        acts.push(Act::SetCols(new_cols));
    }

    // ---------------------------------------------------------- list
    let list_rect = Rect::from_min_max(pos2(area.left(), hdr.bottom()), area.max);
    let row_h = row_height(settings);
    let n = tab.visible.len();

    if tab.scroll_to_cursor {
        tab.scroll_to_cursor = false;
        let top = tab.cursor as f32 * row_h;
        let bot = top + row_h;
        if top < tab.scroll_y {
            tab.scroll_target = Some(top);
        } else if bot > tab.scroll_y + tab.view_h {
            tab.scroll_target = Some(bot - tab.view_h);
        }
    }

    // empty-area interaction: click clears the selection, dragging draws a selection rectangle
    let bg = ui.interact(list_rect, Id::unique(("listbg", uid)), Sense::click_and_drag());
    let ptr = ctx.pointer_interact_pos();
    let to_content = |p: egui::Pos2, sx: f32, sy: f32| pos2(p.x - list_rect.left() + sx, p.y - list_rect.top() + sy);
    if bg.drag_started_by(PointerButton::Primary) {
        if let Some(p) = ptr {
            let additive = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
            let base = if additive { tab.selected.clone() } else { HashSet::new() };
            if !additive {
                tab.selected.clear();
            }
            tab.rubber = Some(Rubber { start: to_content(p, tab.scroll_x, tab.scroll_y), base });
        }
    }
    if bg.dragged() {
        if let (Some(rb), Some(p)) = (tab.rubber.take(), ptr) {
            let cur = to_content(p, tab.scroll_x, tab.scroll_y);
            let (y0, y1) = (rb.start.y.min(cur.y), rb.start.y.max(cur.y));
            let mut sel = rb.base.clone();
            if n > 0 && y1 >= 0.0 {
                let lo = (y0 / row_h).floor().max(0.0) as usize;
                let hi = ((y1 / row_h).floor() as usize).min(n - 1);
                if lo <= hi {
                    for i in lo..=hi {
                        sel.insert(tab.entries[tab.visible[i]].path.clone());
                    }
                }
            }
            tab.selected = sel;
            // paint the rectangle (content coords -> screen coords)
            let a = pos2(rb.start.x + list_rect.left() - tab.scroll_x, rb.start.y + list_rect.top() - tab.scroll_y);
            let sr = Rect::from_two_pos(a, p).intersect(list_rect);
            let lp = ui.painter().with_clip_rect(list_rect);
            lp.rect_filled(sr, R0, pal.sel);
            outline(&lp, sr, Stroke::new(1.0, pal.sel_border));
            // auto-scroll while dragging near the top/bottom edge
            if p.y < list_rect.top() + 16.0 {
                tab.scroll_target = Some((tab.scroll_y - 14.0).max(0.0));
                ctx.request_repaint();
            } else if p.y > list_rect.bottom() - 16.0 {
                tab.scroll_target = Some(tab.scroll_y + 14.0);
                ctx.request_repaint();
            }
            tab.rubber = Some(rb);
        }
    }
    if bg.drag_stopped() {
        tab.rubber = None;
    }
    let target = tab.scroll_target.take();

    let mut click: Option<(usize, egui::Modifiers)> = None;
    let mut dbl: Option<usize> = None;
    let mut mid: Option<PathBuf> = None;
    let mut ctx_sel: Option<usize> = None;
    let mut rename_req: Option<usize> = None;
    let mut rename_res: Option<Option<String>> = None;

    let mut child = ui.new_child(UiBuilder::new().max_rect(list_rect));
    child.spacing_mut().item_spacing = vec2(0.0, 0.0);
    let mut sa = egui::ScrollArea::both()
        .id_salt(("list", uid))
        .auto_shrink([false, false])
        .scroll_source(egui::containers::scroll_area::ScrollSource {
            drag: egui::containers::scroll_area::DragScroll::Never,
            ..Default::default()
        });
    if let Some(yo) = target {
        sa = sa.vertical_scroll_offset(yo.max(0.0));
    }

    let Tab { entries, visible, selected, renaming, search_mode, search_root, .. } = &mut *tab;
    let search_mode = *search_mode;
    let sel_fill = if pc.active { pal.sel } else { pal.sel_inactive };

    let out = sa.show_rows(&mut child, row_h, n, |ui, range| {
        for i in range {
            let e = &entries[visible[i]];
            let (rr, resp) = ui.allocate_exact_size(vec2(table_w, row_h), Sense::click_and_drag());
            let x0 = rr.left();
            let cx = [x0, x0 + cols[0], x0 + cols[0] + cols[1], x0 + cols[0] + cols[1] + cols[2]];
            let is_sel = selected.contains(&e.path);

            // folders accept dropped files
            let mut drop_hover = false;
            if e.is_dir {
                if let Some(pl) = resp.dnd_release_payload::<FileDrag>() {
                    if !pl.paths.contains(&e.path) {
                        acts.push(Act::DropTo { paths: pl.paths.clone(), dest: e.path.clone() });
                    }
                } else if let Some(pl) = resp.dnd_hover_payload::<FileDrag>() {
                    drop_hover = !pl.paths.contains(&e.path);
                }
            }

            {
                let p = ui.painter();
                if drop_hover {
                    p.rect_filled(rr, R0, pal.sel);
                    outline(p, rr, Stroke::new(1.0, pal.accent));
                } else if is_sel {
                    p.rect_filled(rr, R0, sel_fill);
                    if pc.active {
                        outline(p, rr, Stroke::new(1.0, pal.sel_border));
                    }
                } else if resp.hovered() {
                    p.rect_filled(rr, R0, pal.hover);
                }
            }

            let is_cut = !pc.clip_cut.is_empty() && pc.clip_cut.iter().any(|c| c == &e.path);
            let mut tcol = pal.text;
            let mut dcol = pal.dim;
            if is_cut {
                tcol = tcol.gamma_multiply(0.5);
                dcol = dcol.gamma_multiply(0.5);
            }
            let cy = rr.center().y;
            let font = FontId::proportional(13.0);

            let icon = Rect::from_min_size(pos2(cx[0] + 6.0, cy - 8.0), vec2(16.0, 16.0));
            if !pc.icons.paint(ui.painter(), icon, Spec::for_entry(e)) {
                if e.is_dir {
                    if e.path.parent().is_none() {
                        paint_drive(ui.painter(), icon);
                    } else {
                        paint_folder(ui.painter(), icon);
                    }
                } else {
                    paint_file(ui.painter(), icon);
                }
            }

            // name (or inline rename editor)
            let name_clip = Rect::from_min_max(pos2(cx[0] + 26.0, rr.top()), pos2(cx[1] - 4.0, rr.bottom()));
            let renaming_here = renaming.as_ref().map_or(false, |r| r.path == e.path);
            if renaming_here {
                *typing = true;
                if let Some(r) = renaming.as_mut() {
                    let te = Rect::from_min_max(pos2(cx[0] + 24.0, rr.top() + 1.0), pos2(cx[1] - 4.0, rr.bottom() - 1.0));
                    let id = Id::unique(("rename", uid));
                    let mut cui = ui.new_child(UiBuilder::new().max_rect(te));
                    let o = egui::TextEdit::singleline(&mut r.buf)
                        .id(id)
                        .desired_width(te.width())
                        .show(&mut cui);
                    let (response, mut state) = (o.response, o.state);
                    if !r.started {
                        r.started = true;
                        response.request_focus();
                        let stem = if e.is_dir {
                            r.buf.chars().count()
                        } else {
                            match r.buf.rfind('.') {
                                Some(p) if p > 0 => r.buf[..p].chars().count(),
                                _ => r.buf.chars().count(),
                            }
                        };
                        state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(stem),
                        )));
                        state.store(ui.ctx(), id);
                    } else if ui.input(|i| i.key_pressed(Key::Escape)) {
                        rename_res = Some(None);
                    } else if response.lost_focus() {
                        rename_res = Some(Some(r.buf.clone()));
                    }
                }
            } else {
                let p = ui.painter().with_clip_rect(name_clip);
                let nr = p.text(pos2(cx[0] + 28.0, cy), Align2::LEFT_CENTER, &e.name, font.clone(), tcol);
                if search_mode {
                    if let Some(rel) = e.path.parent().and_then(|pp| pp.strip_prefix(&*search_root).ok()) {
                        let s = rel.display().to_string();
                        if !s.is_empty() {
                            p.text(pos2(nr.right() + 10.0, cy), Align2::LEFT_CENTER, s, FontId::proportional(12.0), dcol);
                        }
                    }
                }
            }

            // other columns
            {
                let p = ui.painter();
                let clip = |a: f32, b: f32| Rect::from_min_max(pos2(a, rr.top()), pos2(b, rr.bottom()));
                p.with_clip_rect(clip(cx[1], cx[2]))
                    .text(pos2(cx[1] + 8.0, cy), Align2::LEFT_CENTER, fsx::fmt_date(e.modified), font.clone(), dcol);
                p.with_clip_rect(clip(cx[2], cx[3]))
                    .text(pos2(cx[2] + 8.0, cy), Align2::LEFT_CENTER, &e.kind, font.clone(), dcol);
                let size_txt = if !e.is_dir {
                    Some(fsx::fmt_size(e.size))
                } else if e.path.parent().is_some() {
                    Some(match pc.sizes.get(&e.path) {
                        Some(b) => fsx::fmt_size(b),
                        None => "...".to_string(),
                    })
                } else {
                    None
                };
                if let Some(t) = size_txt {
                    p.with_clip_rect(clip(cx[3], cx[3] + cols[3]))
                        .text(pos2(cx[3] + cols[3] - 10.0, cy), Align2::RIGHT_CENTER, t, font.clone(), dcol);
                }
            }

            // interaction
            if resp.drag_started_by(PointerButton::Primary) {
                let paths: Vec<PathBuf> = if is_sel {
                    visible
                        .iter()
                        .map(|&ix| &entries[ix])
                        .filter(|x| selected.contains(&x.path))
                        .map(|x| x.path.clone())
                        .collect()
                } else {
                    ctx_sel = Some(i);
                    vec![e.path.clone()]
                };
                resp.dnd_set_drag_payload(FileDrag { paths });
            }
            if resp.clicked() {
                click = Some((i, ui.input(|inp| inp.modifiers)));
            }
            if resp.double_clicked() {
                dbl = Some(i);
            }
            if resp.middle_clicked() && e.is_dir {
                mid = Some(e.path.clone());
            }
            if resp.secondary_clicked() && !is_sel {
                ctx_sel = Some(i);
            }
            resp.context_menu(|ui| {
                let targets: Vec<PathBuf> = if is_sel {
                    visible
                        .iter()
                        .map(|&ix| &entries[ix])
                        .filter(|x| selected.contains(&x.path))
                        .map(|x| x.path.clone())
                        .collect()
                } else {
                    vec![e.path.clone()]
                };
                row_menu(ui, e, i, targets, idx, pc, acts, &mut rename_req);
            });
        }
    });
    tab.scroll_y = out.state.clamped_offset().y;
    tab.scroll_x = out.state.clamped_offset().x;
    tab.view_h = out.inner_rect.height();

    // ---------------------------------------------------------- apply recorded interaction
    if let Some(i) = ctx_sel {
        tab.select_only(i);
    }
    if let Some((i, m)) = click {
        if m.shift {
            let a = tab.anchor;
            tab.select_range(a, i, m.ctrl || m.command);
        } else if m.ctrl || m.command {
            tab.toggle(i);
        } else {
            tab.select_only(i);
        }
    } else if bg.clicked() {
        tab.selected.clear();
    }
    if let Some(i) = dbl {
        if let Some(&ix) = tab.visible.get(i) {
            let (path, is_dir) = (tab.entries[ix].path.clone(), tab.entries[ix].is_dir);
            if is_dir {
                tab.navigate(path, &ctx);
            } else {
                acts.push(Act::OpenFile(path));
            }
        }
    }
    if let Some(p) = mid {
        acts.push(Act::GoToNewTab(p));
    }
    if let Some(i) = rename_req {
        tab.select_only(i);
        tab.start_rename();
    }
    if let Some(res) = rename_res {
        if let Some(r) = tab.renaming.take() {
            if let Some(buf) = res {
                let nn = buf.trim();
                let old = r.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if !nn.is_empty() && nn != old {
                    acts.push(Act::Rename { pane: idx, from: r.path.clone(), to: r.path.with_file_name(nn) });
                }
            }
        }
    }

    // empty-area context menu
    bg.context_menu(|ui| {
        if ui.button("New folder").clicked() {
            acts.push(Act::NewFolder(idx));
            ui.close();
        }
        if ui.add_enabled(pc.can_paste, egui::Button::new("Paste")).clicked() {
            acts.push(Act::Paste(idx));
            ui.close();
        }
        if ui.button("Refresh").clicked() {
            tab.refresh(&ctx);
            acts.push(Act::RefreshSizes(tab.path.clone()));
            ui.close();
        }
    });

    if tab.visible.is_empty() {
        let msg = if let Some(e) = &tab.error {
            e.clone()
        } else if tab.search_mode {
            if tab.search_running { "Searching...".into() } else { "No matches.".into() }
        } else if tab.loading.is_some() {
            "Loading...".into()
        } else {
            "This folder is empty.".into()
        };
        ui.painter().text(
            pos2(list_rect.center().x, list_rect.top() + 36.0),
            Align2::CENTER_CENTER,
            msg,
            FontId::proportional(13.0),
            pal.dim,
        );
    }

    // ---------------------------------------------------------- drop targets for the whole pane
    let pane_drop = ui.interact(rect, Id::unique(("panedrop", idx)), Sense::hover());
    if let Some(pl) = pane_drop.dnd_release_payload::<FileDrag>() {
        acts.push(Act::DropTo { paths: pl.paths.clone(), dest: tab.path.clone() });
    } else if pane_drop.dnd_hover_payload::<FileDrag>().is_some() {
        outline(ui.painter(), list_rect, Stroke::new(2.0, pal.accent));
    }
}

#[allow(clippy::too_many_arguments)]
fn row_menu(
    ui: &mut Ui,
    e: &Entry,
    row: usize,
    targets: Vec<PathBuf>,
    pane: usize,
    pc: &PaneCtx,
    acts: &mut Vec<Act>,
    rename_req: &mut Option<usize>,
) {
    if ui.button("Open").clicked() {
        if e.is_dir {
            acts.push(Act::GoTo(e.path.clone()));
        } else {
            acts.push(Act::OpenFile(e.path.clone()));
        }
        ui.close();
    }
    if e.is_dir && ui.button("Open in new tab").clicked() {
        acts.push(Act::GoToNewTab(e.path.clone()));
        ui.close();
    }
    if e.is_dir && ui.button("Pin to Quick access").clicked() {
        acts.push(Act::PinQuick(e.path.clone()));
        ui.close();
    }
    ui.separator();
    if ui.button("Cut").clicked() {
        acts.push(Act::Cut(targets.clone()));
        ui.close();
    }
    if ui.button("Copy").clicked() {
        acts.push(Act::Copy(targets.clone()));
        ui.close();
    }
    if ui.add_enabled(pc.can_paste, egui::Button::new("Paste")).clicked() {
        acts.push(Act::Paste(pane));
        ui.close();
    }
    if ui.button("Copy path").clicked() {
        acts.push(Act::CopyText(e.path.display().to_string()));
        ui.close();
    }
    ui.separator();
    if ui.add_enabled(pc.has_next, egui::Button::new("Copy to next pane")).clicked() {
        acts.push(Act::Transfer { pane, paths: targets.clone(), mv: false });
        ui.close();
    }
    if ui.add_enabled(pc.has_next, egui::Button::new("Move to next pane")).clicked() {
        acts.push(Act::Transfer { pane, paths: targets.clone(), mv: true });
        ui.close();
    }
    ui.separator();
    if targets.len() == 1 && ui.button("Rename").clicked() {
        *rename_req = Some(row);
        ui.close();
    }
    if ui.button("Delete").clicked() {
        acts.push(Act::Delete { paths: targets.clone(), permanent: false });
        ui.close();
    }
    if ui.button("Delete permanently").clicked() {
        acts.push(Act::Delete { paths: targets.clone(), permanent: true });
        ui.close();
    }
    ui.separator();
    if ui.button("Show in Explorer").clicked() {
        acts.push(Act::Reveal(e.path.clone()));
        ui.close();
    }
}
