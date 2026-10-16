use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CursorIcon, FontId, Id, Key, Modifiers as M, PointerButton, Rect,
    Sense, Stroke, UiBuilder,
};

use crate::{
    fsx::{self, Op},
    pane::*,
    theme::*,
};

const TITLE_H: f32 = 32.0; // tabs and caption buttons share this height
const STATUS_H: f32 = 22.0;
const SIDEBAR_W: f32 = 190.0;
const CAPTION_W: f32 = 46.0;

struct Clip {
    paths: Vec<PathBuf>,
    cut: bool,
}

pub struct App {
    settings: Settings,
    panes: [Pane; 2],
    active: usize,
    ratio: f32,
    clip: Option<Clip>,
    quick: Vec<(String, PathBuf)>,
    drives: Vec<PathBuf>,
    show_settings: bool,
    fx_dirty: bool,
    typing: bool,
    next_uid: u64,
    op_tx: Sender<Result<String, String>>,
    op_rx: Receiver<Result<String, String>>,
    busy: usize,
    status: String,
    status_until: f64,
    confirm: Option<(Vec<PathBuf>, bool)>,
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
        fonts
            .font_data
            .insert("segoe".to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "segoe".to_owned());
    }
    ctx.set_fonts(fonts);
}

fn quick_links() -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = Vec::new();
    let mut add = |n: &str, p: Option<PathBuf>| {
        if let Some(p) = p {
            v.push((n.to_string(), p));
        }
    };
    add("Desktop", dirs::desktop_dir());
    add("Downloads", dirs::download_dir());
    add("Documents", dirs::document_dir());
    add("Pictures", dirs::picture_dir());
    add("Music", dirs::audio_dir());
    add("Videos", dirs::video_dir());
    v
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

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let settings: Settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, "settings"))
            .unwrap_or_default();
        let ratio: f32 = cc.storage.and_then(|s| eframe::get_value(s, "ratio")).unwrap_or(0.5);
        install_fonts(&cc.egui_ctx);
        apply_visuals(&cc.egui_ctx, &settings);

        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("C:\\"));
        let right = dirs::download_dir().unwrap_or_else(|| home.clone());
        let (op_tx, op_rx) = mpsc::channel();
        let fx_dirty = settings.acrylic;
        App {
            panes: [
                Pane { tabs: vec![Tab::new(1, home, &cc.egui_ctx)], active: 0 },
                Pane { tabs: vec![Tab::new(2, right, &cc.egui_ctx)], active: 0 },
            ],
            settings,
            active: 0,
            ratio: ratio.clamp(0.25, 0.75),
            clip: None,
            quick: quick_links(),
            drives: fsx::list_drives(),
            show_settings: false,
            fx_dirty,
            typing: false,
            next_uid: 3,
            op_tx,
            op_rx,
            busy: 0,
            status: String::new(),
            status_until: 0.0,
            confirm: None,
        }
    }

    fn set_status(&mut self, ctx: &egui::Context, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_until = ctx.input(|i| i.time) + 6.0;
        ctx.request_repaint_after(Duration::from_secs_f32(6.2));
    }

    fn open_tab(&mut self, pane: usize, path: PathBuf, ctx: &egui::Context) {
        let uid = self.next_uid;
        self.next_uid += 1;
        let p = &mut self.panes[pane];
        p.tabs.push(Tab::new(uid, path, ctx));
        p.active = p.tabs.len() - 1;
    }

    fn select_tab(&mut self, pane: usize, idx: usize, ctx: &egui::Context) {
        let p = &mut self.panes[pane];
        p.active = idx.min(p.tabs.len() - 1);
        if p.tab().stale {
            p.tab_mut().refresh(ctx);
        }
    }

    fn spawn_op(&mut self, ctx: &egui::Context, op: Op) {
        let tx = self.op_tx.clone();
        let c = ctx.clone();
        self.busy += 1;
        thread::spawn(move || {
            let _ = tx.send(fsx::run_op(op));
            c.request_repaint();
        });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let sh = self.settings.show_hidden;
        for p in self.panes.iter_mut() {
            for t in p.tabs.iter_mut() {
                t.poll(sh);
            }
        }
        while let Ok(r) = self.op_rx.try_recv() {
            self.busy = self.busy.saturating_sub(1);
            match r {
                Ok(m) | Err(m) => self.set_status(ctx, m),
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
    }

    // ------------------------------------------------------------ shortcuts

    fn shortcuts(&mut self, ctx: &egui::Context, typing: bool, acts: &mut Vec<Act>) {
        // modal dialogs first
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
            // always-on shortcuts (most specific first: extra Shift/Alt are ignored when matching)
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

        let a = self.active;
        let row_h = row_height(&self.settings);
        if k.settings {
            self.show_settings = !self.show_settings;
        }
        if k.switch {
            self.active = 1 - self.active;
        }
        if k.new_tab {
            let p = self.panes[a].tab().path.clone();
            acts.push(Act::NewTab { pane: a, path: p });
        }
        if k.close_tab {
            let idx = self.panes[a].active;
            acts.push(Act::CloseTab { pane: a, idx });
        }
        if let Some(fwd) = k.next_tab {
            let n = self.panes[a].tabs.len();
            let cur = self.panes[a].active;
            let next = if fwd { (cur + 1) % n } else { (cur + n - 1) % n };
            self.select_tab(a, next, ctx);
        }
        let tab = self.panes[a].tab_mut();
        if k.addr {
            tab.focus_addr = true;
        }
        if k.search {
            tab.focus_search = true;
        }
        if k.refresh {
            tab.refresh(ctx);
        }
        if k.new_folder {
            acts.push(Act::NewFolder(a));
        }
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
        let paths = self.panes[a].tab().selected_paths();
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

    fn apply(&mut self, ctx: &egui::Context, act: Act) {
        match act {
            Act::Activate(i) => self.active = i,
            Act::GoTo(p) => self.panes[self.active].tab_mut().navigate(p, ctx),
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
                let p = &mut self.panes[pane];
                if p.tabs.len() > 1 && idx < p.tabs.len() {
                    p.tabs.remove(idx);
                    if idx < p.active || p.active >= p.tabs.len() {
                        p.active = p.active.saturating_sub(1);
                    }
                    let a = p.active;
                    self.select_tab(pane, a, ctx);
                }
            }
            Act::SelectTab { pane, idx } => self.select_tab(pane, idx, ctx),
            Act::Copy(paths) => {
                let n = paths.len();
                self.clip = Some(Clip { paths, cut: false });
                self.set_status(ctx, format!("Copied {n} item{}", if n == 1 { "" } else { "s" }));
            }
            Act::Cut(paths) => {
                let n = paths.len();
                self.clip = Some(Clip { paths, cut: true });
                self.set_status(ctx, format!("Cut {n} item{}", if n == 1 { "" } else { "s" }));
            }
            Act::Paste(pane) => {
                let clip = self.clip.as_ref().map(|c| (c.paths.clone(), c.cut));
                if let Some((items, cut)) = clip {
                    let dest = self.panes[pane].tab().path.clone();
                    if cut {
                        self.clip = None;
                        self.spawn_op(ctx, Op::Move { items, dest });
                    } else {
                        self.spawn_op(ctx, Op::Copy { items, dest });
                    }
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
            Act::Rename { pane, from, to } => match std::fs::rename(&from, &to) {
                Ok(()) => {
                    let tab = self.panes[pane].tab_mut();
                    tab.select_after_load = Some(to);
                    tab.refresh(ctx);
                    if tab.search_mode {
                        tab.exit_search(ctx);
                    }
                }
                Err(e) => self.set_status(ctx, format!("Rename failed: {e}")),
            },
            Act::NewFolder(pane) => {
                let tab = self.panes[pane].tab_mut();
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
            Act::Transfer { paths, to_pane, mv } => {
                if paths.is_empty() {
                    return;
                }
                let dest = self.panes[to_pane].tab().path.clone();
                let op = if mv { Op::Move { items: paths, dest } } else { Op::Copy { items: paths, dest } };
                self.spawn_op(ctx, op);
            }
            Act::CopyText(s) => ctx.copy_text(s),
        }
    }

    fn status_line(&self, ctx: &egui::Context) -> String {
        if ctx.input(|i| i.time) < self.status_until && !self.status.is_empty() {
            return self.status.clone();
        }
        self.panes[self.active].tab().selection_info()
    }
}

// ====================================================================== eframe glue

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0] // fully transparent: every surface is painted by the app
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "settings", &self.settings);
        eframe::set_value(storage, "ratio", &self.ratio);
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

        let pal = palette(&self.settings);
        let full = ui.max_rect();
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        ui.painter().rect_filled(full, R0, pal.chrome);

        // ---- layout
        let side_w = if self.settings.show_sidebar { SIDEBAR_W } else { 0.0 };
        let title = Rect::from_min_size(full.min, vec2(full.width(), TITLE_H));
        let status_rect = Rect::from_min_max(pos2(full.left(), full.bottom() - STATUS_H), full.max);
        let body = Rect::from_min_max(pos2(full.left(), title.bottom()), pos2(full.right(), status_rect.top()));
        let side_rect = Rect::from_min_size(body.min, vec2(side_w, body.height()));
        let panes_rect = Rect::from_min_max(pos2(body.left() + side_w, body.top()), body.max);
        let split_x = panes_rect.left() + (panes_rect.width() * self.ratio).round();
        let rects = [
            Rect::from_min_max(panes_rect.min, pos2(split_x - 2.0, panes_rect.bottom())),
            Rect::from_min_max(pos2(split_x + 3.0, panes_rect.top()), panes_rect.max),
        ];

        // ---- title bar: drag region first (widgets drawn later sit on top of it)
        let drag = ui.interact(title, Id::unique("titlebar_drag"), Sense::click_and_drag());
        if drag.double_clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
        } else if drag.drag_started_by(PointerButton::Primary) {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        // settings gear + app name (left zone)
        let left_zone = if self.settings.show_sidebar { SIDEBAR_W } else { CAPTION_W };
        let gear = Rect::from_min_size(title.min, vec2(CAPTION_W, TITLE_H));
        let gresp = ui.interact(gear, Id::unique("gear"), Sense::click());
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

        // tab strips (same height as caption buttons)
        let strips = [
            Rect::from_min_max(
                pos2(title.left() + left_zone, title.top()),
                pos2(split_x, title.bottom()),
            ),
            Rect::from_min_max(
                pos2(split_x, title.top()),
                pos2(title.right() - 3.0 * CAPTION_W, title.bottom()),
            ),
        ];
        for i in 0..2 {
            draw_tab_strip(ui, strips[i], i, &self.panes[i], self.active == i, &pal, &mut acts);
        }

        // caption buttons
        let kinds = [
            Glyph::Minimize,
            if maximized { Glyph::Restore } else { Glyph::Maximize },
            Glyph::Close,
        ];
        for (k, g) in kinds.iter().enumerate() {
            let r = Rect::from_min_size(
                pos2(title.right() - CAPTION_W * (3 - k) as f32, title.top()),
                vec2(CAPTION_W, TITLE_H),
            );
            let resp = ui.interact(r, Id::unique(("caption", k)), Sense::click());
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

        // ---- sidebar
        if self.settings.show_sidebar {
            let cur = self.panes[self.active].tab().path.clone();
            draw_sidebar(ui, side_rect, &self.quick, &self.drives, &cur, &pal, &mut acts);
        }

        // ---- panes
        let cut_paths: Vec<PathBuf> = match &self.clip {
            Some(c) if c.cut => c.paths.clone(),
            _ => Vec::new(),
        };
        let can_paste = self.clip.is_some();
        for i in 0..2 {
            let pc = PaneCtx {
                pal: &pal,
                settings: &self.settings,
                clip_cut: &cut_paths,
                can_paste,
                active: self.active == i,
                idx: i,
            };
            draw_pane(ui, rects[i], &mut self.panes[i], &pc, &mut acts, &mut typing_now);
        }

        // ---- divider
        let div = Rect::from_min_max(
            pos2(split_x - 3.0, panes_rect.top()),
            pos2(split_x + 3.0, panes_rect.bottom()),
        );
        let dresp = ui.interact(div, Id::unique("divider"), Sense::drag());
        if dresp.hovered() || dresp.dragged() {
            ctx.set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
        if dresp.dragged() {
            let nx = split_x + dresp.drag_delta().x - panes_rect.left();
            self.ratio = (nx / panes_rect.width()).clamp(0.25, 0.75);
        }
        let dcol = if dresp.dragged() { pal.accent } else { pal.border };
        ui.painter()
            .line_segment([pos2(split_x + 0.5, panes_rect.top()), pos2(split_x + 0.5, panes_rect.bottom())], Stroke::new(1.0, dcol));

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
        // delete confirmation
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

        // settings
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
                        ui.radio_value(&mut s.dark, false, "Light");
                        ui.radio_value(&mut s.dark, true, "Dark");
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
                    ui.add_space(6.0);
                    if ui.button("Reset to defaults").clicked() {
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
                    || s.dark != self.settings.dark;
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

fn draw_sidebar(
    ui: &mut egui::Ui,
    rect: Rect,
    quick: &[(String, PathBuf)],
    drives: &[PathBuf],
    current: &Path,
    pal: &Palette,
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
            let mut item = |ui: &mut egui::Ui, label: &str, path: &Path, drive: bool| {
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width() - 1.0, 26.0), Sense::click());
                if current == path {
                    ui.painter().rect_filled(r, R0, pal.sel);
                } else if resp.hovered() {
                    ui.painter().rect_filled(r, R0, pal.hover);
                }
                let icon = Rect::from_min_size(pos2(r.left() + 14.0, r.center().y - 8.0), vec2(16.0, 16.0));
                if drive {
                    paint_drive(ui.painter(), icon);
                } else {
                    paint_folder(ui.painter(), icon);
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
            };
            let header = |ui: &mut egui::Ui, text: &str| {
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
                ui.painter().text(
                    pos2(r.left() + 14.0, r.center().y + 2.0),
                    Align2::LEFT_CENTER,
                    text,
                    FontId::proportional(12.0),
                    pal.dim,
                );
            };
            header(ui, "Quick access");
            for (name, path) in quick {
                item(ui, name, path, false);
            }
            header(ui, "This PC");
            for d in drives {
                let letter = d.to_string_lossy().chars().next().unwrap_or('?');
                item(ui, &format!("Local Disk ({letter}:)"), d, true);
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
