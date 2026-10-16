//! Tabs, panes and their drawing code. Drawing never performs cross-cutting file
//! operations itself; it pushes `Act`s which `App` executes after the frame.

use std::{
    collections::HashSet,
    path::PathBuf,
    sync::mpsc::{Receiver, TryRecvError},
};

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, FontId, Id, Key, Layout, Painter, Rect, Sense, Shape,
    Stroke, Ui, UiBuilder,
};

use crate::{
    fsx::{self, Entry, SearchHandle, SearchMsg},
    theme::*,
};

pub const TOOLBAR_H: f32 = 36.0;
pub const CMD_H: f32 = 28.0;
pub const HEADER_H: f32 = 24.0;

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
    Transfer { paths: Vec<PathBuf>, to_pane: usize, mv: bool },
    CopyText(String),
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
    view_h: f32,
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
            view_h: 400.0,
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

// ====================================================================== tab strip

/// Draws a pane's tabs inside the title bar. Tab height == title bar height, so tabs
/// line up exactly with the caption buttons.
pub fn draw_tab_strip(
    ui: &mut Ui,
    rect: Rect,
    pane_idx: usize,
    pane: &Pane,
    pane_active: bool,
    pal: &Palette,
    acts: &mut Vec<Act>,
) {
    let n = pane.tabs.len();
    let plus_w = 32.0;
    let avail = (rect.width() - plus_w).max(40.0);
    let tw = (avail / n as f32).clamp(40.0, 200.0);
    let multiple = n > 1;

    for (idx, tab) in pane.tabs.iter().enumerate() {
        let tr = Rect::from_min_size(pos2(rect.left() + idx as f32 * tw, rect.top()), vec2(tw, rect.height()));
        let resp = ui.interact(tr, Id::unique(("tab", pane_idx, tab.uid)), Sense::click());
        let active = idx == pane.active;
        let p = ui.painter();
        if active {
            p.rect_filled(tr, R0, pal.content);
            let line = if pane_active { pal.accent } else { pal.border };
            p.rect_filled(Rect::from_min_size(tr.left_top(), vec2(tr.width(), 2.0)), R0, line);
        } else if resp.hovered() {
            p.rect_filled(tr, R0, pal.hover);
        }
        paint_folder(p, Rect::from_min_size(pos2(tr.left() + 8.0, tr.center().y - 8.0), vec2(16.0, 16.0)));
        let col = if active { pal.text } else { pal.dim };
        let clip = Rect::from_min_max(pos2(tr.left() + 28.0, tr.top()), pos2(tr.right() - 24.0, tr.bottom()));
        p.with_clip_rect(clip).text(
            pos2(tr.left() + 28.0, tr.center().y),
            Align2::LEFT_CENTER,
            tab.title(),
            FontId::proportional(13.0),
            col,
        );

        let mut close_clicked = false;
        if multiple && (active || resp.hovered()) {
            let cr = Rect::from_center_size(pos2(tr.right() - 14.0, tr.center().y), vec2(18.0, 18.0));
            let cresp = ui.interact(cr, Id::unique(("tabclose", pane_idx, tab.uid)), Sense::click());
            if cresp.hovered() {
                ui.painter().rect_filled(cr, R0, pal.caption_hover);
            }
            let mut g = ui.painter().clone();
            g.set_clip_rect(cr.expand(2.0));
            paint_small_x(&g, cr.center(), pal.text);
            close_clicked = cresp.clicked();
        }
        if close_clicked || resp.middle_clicked() && multiple {
            acts.push(Act::CloseTab { pane: pane_idx, idx });
        } else if resp.clicked() {
            acts.push(Act::Activate(pane_idx));
            acts.push(Act::SelectTab { pane: pane_idx, idx });
        }
    }

    let pr = Rect::from_min_size(pos2(rect.left() + n as f32 * tw, rect.top()), vec2(plus_w, rect.height()));
    let resp = ui.interact(pr, Id::unique(("newtab", pane_idx)), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(pr, R0, pal.caption_hover);
    }
    paint_glyph(ui.painter(), pr.center(), Glyph::Plus, pal.text);
    if resp.clicked() {
        acts.push(Act::NewTab { pane: pane_idx, path: pane.tab().path.clone() });
    }
}

fn paint_small_x(p: &Painter, c: egui::Pos2, col: Color32) {
    let st = Stroke::new(1.0, col);
    let c = pos2(c.x.floor() + 0.5, c.y.floor() + 0.5);
    p.line_segment([c + vec2(-3.0, -3.0), c + vec2(3.0, 3.0)], st);
    p.line_segment([c + vec2(-3.0, 3.0), c + vec2(3.0, -3.0)], st);
}

// ====================================================================== pane body

pub struct PaneCtx<'a> {
    pub pal: &'a Palette,
    pub settings: &'a Settings,
    pub clip_cut: &'a [PathBuf],
    pub can_paste: bool,
    pub active: bool,
    pub idx: usize,
}

fn glyph_button(ui: &mut Ui, rect: Rect, id: Id, g: Glyph, enabled: bool, pal: &Palette) -> egui::Response {
    let resp = ui.interact(rect, id, if enabled { Sense::click() } else { Sense::hover() });
    if enabled && resp.hovered() {
        ui.painter().rect_filled(rect, R0, pal.caption_hover);
    }
    paint_glyph(ui.painter(), rect.center(), g, if enabled { pal.text } else { pal.dim });
    resp
}

fn flat(ui: &mut Ui, label: &str, enabled: bool) -> bool {
    ui.add_enabled(enabled, egui::Button::new(label).frame(false)).clicked()
}

pub fn row_height(s: &Settings) -> f32 {
    if s.compact {
        20.0
    } else {
        24.0
    }
}

pub fn draw_pane(ui: &mut Ui, rect: Rect, pane: &mut Pane, pc: &PaneCtx, acts: &mut Vec<Act>, typing: &mut bool) {
    let ctx = ui.ctx().clone();
    let pal = pc.pal;
    let idx = pc.idx;
    let settings = pc.settings;
    let tab = pane.tab_mut();
    let uid = tab.uid;

    ui.painter().rect_filled(rect, R0, pal.content);

    // clicking anywhere in the pane makes it the active one
    let pressed_inside = ctx.input(|i| {
        i.pointer.any_pressed() && i.pointer.interact_pos().map_or(false, |p| rect.contains(p))
    });
    if pressed_inside {
        acts.push(Act::Activate(idx));
    }

    // ---------------------------------------------------------- toolbar
    let tb = Rect::from_min_size(rect.min, vec2(rect.width(), TOOLBAR_H));
    ui.painter().rect_filled(tb, R0, pal.chrome);
    let by = tb.center().y;
    let mut x = tb.left() + 4.0;
    let btn = |x: f32| Rect::from_center_size(pos2(x + 14.0, by), vec2(28.0, 28.0));

    if glyph_button(ui, btn(x), Id::unique(("back", uid)), Glyph::Back, tab.can_back(), pal).clicked() {
        tab.go_back(&ctx);
    }
    x += 30.0;
    if glyph_button(ui, btn(x), Id::unique(("fwd", uid)), Glyph::Forward, tab.can_fwd(), pal).clicked() {
        tab.go_forward(&ctx);
    }
    x += 30.0;
    let can_up = tab.path.parent().is_some();
    if glyph_button(ui, btn(x), Id::unique(("up", uid)), Glyph::Up, can_up, pal).clicked() {
        tab.go_up(&ctx);
    }
    x += 30.0;
    if glyph_button(ui, btn(x), Id::unique(("refresh", uid)), Glyph::Refresh, !tab.search_mode, pal).clicked() {
        tab.refresh(&ctx);
    }
    x += 32.0;

    let search_w = (rect.width() * 0.3).clamp(130.0, 260.0);
    let search_rect =
        Rect::from_min_max(pos2(tb.right() - search_w - 8.0, by - 13.0), pos2(tb.right() - 8.0, by + 13.0));
    let addr_rect = Rect::from_min_max(pos2(x, by - 13.0), pos2(search_rect.left() - 8.0, by + 13.0));

    // address bar
    let addr_resp = ui.put(
        addr_rect,
        egui::TextEdit::singleline(&mut tab.addr)
            .id(Id::unique(("addr", uid)))
            .desired_width(addr_rect.width()),
    );
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

    // search box (recursive, jwalk)
    let hint = format!(
        "Search {}",
        tab.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| tab.path.display().to_string())
    );
    let search_resp = ui.put(
        search_rect,
        egui::TextEdit::singleline(&mut tab.search_text)
            .id(Id::unique(("search", uid)))
            .hint_text(hint)
            .desired_width(search_rect.width()),
    );
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
    let cb = Rect::from_min_size(pos2(rect.left(), tb.bottom()), vec2(rect.width(), CMD_H));
    ui.painter().rect_filled(cb, R0, pal.chrome);
    ui.painter().line_segment([cb.left_bottom(), cb.right_bottom()], Stroke::new(1.0, pal.border));
    {
        let mut cui = ui.new_child(
            UiBuilder::new()
                .max_rect(cb.shrink2(vec2(6.0, 0.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        cui.spacing_mut().item_spacing.x = 2.0;
        let has_sel = !tab.selected.is_empty();
        if flat(&mut cui, "New folder", true) {
            acts.push(Act::NewFolder(idx));
        }
        cui.separator();
        if flat(&mut cui, "Cut", has_sel) {
            acts.push(Act::Cut(tab.selected_paths()));
        }
        if flat(&mut cui, "Copy", has_sel) {
            acts.push(Act::Copy(tab.selected_paths()));
        }
        if flat(&mut cui, "Paste", pc.can_paste) {
            acts.push(Act::Paste(idx));
        }
        cui.separator();
        if flat(&mut cui, "Rename", tab.selected.len() == 1) {
            tab.start_rename();
        }
        if flat(&mut cui, "Delete", has_sel) {
            acts.push(Act::Delete { paths: tab.selected_paths(), permanent: false });
        }
        if rect.width() > 720.0 {
            cui.separator();
            if flat(&mut cui, "Copy to other pane", has_sel) {
                acts.push(Act::Transfer { paths: tab.selected_paths(), to_pane: 1 - idx, mv: false });
            }
            if flat(&mut cui, "Move to other pane", has_sel) {
                acts.push(Act::Transfer { paths: tab.selected_paths(), to_pane: 1 - idx, mv: true });
            }
        }
    }

    // ---------------------------------------------------------- column header
    let w = rect.width();
    let size_w = 84.0;
    let date_w = 132.0;
    let kind_w = if w > 520.0 { 112.0 } else { 0.0 };
    let name_w = (w - size_w - date_w - kind_w).max(100.0);
    let name_x = rect.left();
    let date_x = name_x + name_w;
    let kind_x = date_x + date_w;
    let size_x = kind_x + kind_w;

    let hdr = Rect::from_min_size(pos2(rect.left(), cb.bottom()), vec2(w, HEADER_H));
    ui.painter().rect_filled(hdr, R0, pal.content);
    ui.painter().line_segment([hdr.left_bottom(), hdr.right_bottom()], Stroke::new(1.0, pal.border));
    let cols = [
        (SortKey::Name, "Name", name_x, name_w),
        (SortKey::Modified, "Date modified", date_x, date_w),
        (SortKey::Kind, "Type", kind_x, kind_w),
        (SortKey::Size, "Size", size_x, size_w),
    ];
    for (key, label, cx, cw) in cols {
        if cw <= 0.0 {
            continue;
        }
        let cr = Rect::from_min_size(pos2(cx, hdr.top()), vec2(cw, hdr.height()));
        let r = ui.interact(cr, Id::unique(("hdr", uid, label)), Sense::click());
        if r.hovered() {
            ui.painter().rect_filled(cr, R0, pal.hover);
        }
        let p = ui.painter().with_clip_rect(cr);
        let font = FontId::proportional(12.0);
        if key == SortKey::Size {
            p.text(pos2(cr.right() - 8.0, cr.center().y + 1.0), Align2::RIGHT_CENTER, label, font, pal.dim);
        } else {
            p.text(pos2(cr.left() + 8.0, cr.center().y + 1.0), Align2::LEFT_CENTER, label, font, pal.dim);
        }
        if tab.sort == key {
            let (ax, ay) = (cr.center().x, cr.top() + 4.0);
            let pts = if tab.asc {
                vec![pos2(ax - 3.0, ay + 3.0), pos2(ax + 3.0, ay + 3.0), pos2(ax, ay)]
            } else {
                vec![pos2(ax - 3.0, ay), pos2(ax + 3.0, ay), pos2(ax, ay + 3.0)]
            };
            p.add(Shape::convex_polygon(pts, pal.dim, Stroke::NONE));
        }
        ui.painter().line_segment(
            [pos2(cr.right() - 0.5, cr.top() + 4.0), pos2(cr.right() - 0.5, cr.bottom() - 4.0)],
            Stroke::new(1.0, pal.border),
        );
        if r.clicked() {
            if tab.sort == key {
                tab.asc = !tab.asc;
            } else {
                tab.sort = key;
                tab.asc = matches!(key, SortKey::Name | SortKey::Kind);
            }
            tab.rebuild(settings.show_hidden, true);
        }
    }

    // ---------------------------------------------------------- list
    let list_rect = Rect::from_min_max(pos2(rect.left(), hdr.bottom()), rect.max);
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
    let target = tab.scroll_target.take();

    let bg = ui.interact(list_rect, Id::unique(("listbg", uid)), Sense::click());

    let mut click: Option<(usize, egui::Modifiers)> = None;
    let mut dbl: Option<usize> = None;
    let mut mid: Option<PathBuf> = None;
    let mut ctx_sel: Option<usize> = None;
    let mut rename_req: Option<usize> = None;
    let mut rename_res: Option<Option<String>> = None;

    let mut child = ui.new_child(UiBuilder::new().max_rect(list_rect));
    child.spacing_mut().item_spacing.y = 0.0;
    let mut sa = egui::ScrollArea::vertical()
        .id_salt(("list", uid))
        .auto_shrink([false, false])
        .scroll_source(egui::containers::scroll_area::ScrollSource {
            drag: egui::containers::scroll_area::DragScroll::Never,
            ..Default::default()
        });

    if let Some(y) = target {
        sa = sa.vertical_scroll_offset(y.max(0.0));
    }

    let Tab { entries, visible, selected, renaming, search_mode, search_root, .. } = &mut *tab;
    let search_mode = *search_mode;
    let sel_fill = if pc.active { pal.sel } else { pal.sel_inactive };

    let out = sa.show_rows(&mut child, row_h, n, |ui, range| {
        for i in range {
            let e = &entries[visible[i]];
            let (rr, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
            let is_sel = selected.contains(&e.path);
            {
                let p = ui.painter();
                if is_sel {
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

            // icon
            let icon = Rect::from_min_size(pos2(name_x + 6.0, cy - 8.0), vec2(16.0, 16.0));
            if e.is_dir {
                if e.path.parent().is_none() {
                    paint_drive(ui.painter(), icon);
                } else {
                    paint_folder(ui.painter(), icon);
                }
            } else {
                paint_file(ui.painter(), icon);
            }

            // name (or inline rename editor)
            let name_clip = Rect::from_min_max(pos2(name_x + 26.0, rr.top()), pos2(name_x + name_w - 4.0, rr.bottom()));
            let renaming_here = renaming.as_ref().map_or(false, |r| r.path == e.path);
            if renaming_here {
                *typing = true;
                if let Some(r) = renaming.as_mut() {
                    let te = Rect::from_min_max(
                        pos2(name_x + 24.0, rr.top() + 1.0),
                        pos2(name_x + name_w - 4.0, rr.bottom() - 1.0),
                    );
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
                let nr = p.text(pos2(name_x + 28.0, cy), Align2::LEFT_CENTER, &e.name, font.clone(), tcol);
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
                let pd = p.with_clip_rect(Rect::from_min_max(pos2(date_x, rr.top()), pos2(date_x + date_w, rr.bottom())));
                pd.text(pos2(date_x + 8.0, cy), Align2::LEFT_CENTER, fsx::fmt_date(e.modified), font.clone(), dcol);
                if kind_w > 0.0 {
                    let pk = p.with_clip_rect(Rect::from_min_max(pos2(kind_x, rr.top()), pos2(kind_x + kind_w, rr.bottom())));
                    pk.text(pos2(kind_x + 8.0, cy), Align2::LEFT_CENTER, &e.kind, font.clone(), dcol);
                }
                if !e.is_dir {
                    p.text(pos2(size_x + size_w - 8.0, cy), Align2::RIGHT_CENTER, fsx::fmt_size(e.size), font.clone(), dcol);
                }
            }

            // interaction
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
                row_menu(ui, e, i, targets, idx, pc.can_paste, acts, &mut rename_req);
            });
        }
    });
    tab.scroll_y = out.state.clamped_offset().y;
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
            ui.close();
        }
    });

    // placeholder text
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
}

#[allow(clippy::too_many_arguments)]
fn row_menu(
    ui: &mut Ui,
    e: &Entry,
    row: usize,
    targets: Vec<PathBuf>,
    pane: usize,
    can_paste: bool,
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
    ui.separator();
    if ui.button("Cut").clicked() {
        acts.push(Act::Cut(targets.clone()));
        ui.close();
    }
    if ui.button("Copy").clicked() {
        acts.push(Act::Copy(targets.clone()));
        ui.close();
    }
    if ui.add_enabled(can_paste, egui::Button::new("Paste")).clicked() {
        acts.push(Act::Paste(pane));
        ui.close();
    }
    if ui.button("Copy path").clicked() {
        acts.push(Act::CopyText(e.path.display().to_string()));
        ui.close();
    }
    ui.separator();
    if ui.button("Copy to other pane").clicked() {
        acts.push(Act::Transfer { paths: targets.clone(), to_pane: 1 - pane, mv: false });
        ui.close();
    }
    if ui.button("Move to other pane").clicked() {
        acts.push(Act::Transfer { paths: targets.clone(), to_pane: 1 - pane, mv: true });
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
