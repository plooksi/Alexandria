//! Windows 10 design language: flat, square corners, Segoe UI, accent colour,
//! caption-button glyphs and small file/folder icons drawn with the painter
//! (no icon fonts needed).

use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke};
use serde::{Deserialize, Serialize};

pub const R0: CornerRadius = CornerRadius::ZERO;

pub const ACCENTS: [[u8; 3]; 8] = [
    [0, 120, 215],
    [0, 153, 188],
    [16, 137, 62],
    [255, 140, 0],
    [232, 17, 35],
    [227, 0, 140],
    [136, 23, 152],
    [104, 118, 138],
];

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub dark: bool,
    pub accent: [u8; 3],
    pub acrylic: bool,
    pub acrylic_alpha: u8,
    pub show_hidden: bool,
    pub show_sidebar: bool,
    pub confirm_delete: bool,
    pub compact: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            dark: false,
            accent: ACCENTS[0],
            acrylic: false,
            acrylic_alpha: 170,
            show_hidden: false,
            show_sidebar: true,
            confirm_delete: true,
            compact: false,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub chrome: Color32,
    pub content: Color32,
    pub side: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub border: Color32,
    pub hover: Color32,
    pub sel: Color32,
    pub sel_inactive: Color32,
    pub sel_border: Color32,
    pub accent: Color32,
    pub caption_hover: Color32,
    pub caption_press: Color32,
}

pub fn rgb(h: u32) -> Color32 {
    Color32::from_rgb((h >> 16) as u8, (h >> 8) as u8, h as u8)
}
fn with_alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

pub fn palette(s: &Settings) -> Palette {
    let accent = Color32::from_rgb(s.accent[0], s.accent[1], s.accent[2]);
    let (mut chrome, mut content, mut side, text, dim, border, hover, ch, cp, sel_inactive) = if s.dark {
        (
            rgb(0x202020),
            rgb(0x191919),
            rgb(0x1C1C1C),
            rgb(0xF2F2F2),
            rgb(0x9A9A9A),
            rgb(0x3C3C3C),
            Color32::from_white_alpha(14),
            Color32::from_white_alpha(26),
            Color32::from_white_alpha(40),
            Color32::from_white_alpha(28),
        )
    } else {
        (
            rgb(0xF3F3F3),
            rgb(0xFFFFFF),
            rgb(0xF7F7F7),
            rgb(0x111111),
            rgb(0x666666),
            rgb(0xD8D8D8),
            Color32::from_black_alpha(10),
            Color32::from_black_alpha(18),
            Color32::from_black_alpha(32),
            Color32::from_black_alpha(18),
        )
    };
    if s.acrylic {
        let a = s.acrylic_alpha;
        chrome = with_alpha(chrome, a);
        side = with_alpha(side, a);
        content = with_alpha(content, ((a as u16 + 255) / 2) as u8);
    }
    let (sa, ba) = if s.dark { (80, 150) } else { (45, 130) };
    Palette {
        chrome,
        content,
        side,
        text,
        dim,
        border,
        hover,
        sel: with_alpha(accent, sa),
        sel_inactive,
        sel_border: with_alpha(accent, ba),
        accent,
        caption_hover: ch,
        caption_press: cp,
    }
}

pub fn apply_visuals(ctx: &egui::Context, s: &Settings) {
    let p = palette(s);
    let theme = if s.dark { egui::Theme::Dark } else { egui::Theme::Light };
    ctx.set_theme(theme);

    let mut v = if s.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = Color32::TRANSPARENT; // the app paints every surface itself
    v.window_fill = if s.dark { rgb(0x2B2B2B) } else { Color32::WHITE };
    v.window_stroke = Stroke::new(1.0, p.border);
    v.extreme_bg_color = if s.dark { rgb(0x2B2B2B) } else { Color32::WHITE };
    v.override_text_color = Some(p.text);
    v.selection.bg_fill = with_alpha(p.accent, 90);
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.hyperlink_color = p.accent;
    v.window_corner_radius = R0;
    v.menu_corner_radius = R0;
    v.window_shadow = egui::Shadow::NONE;
    v.popup_shadow = egui::Shadow::NONE;

    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = R0;
    }
    let (btn, btn_h, btn_a, btn_border) = if s.dark {
        (rgb(0x333333), rgb(0x404040), rgb(0x4D4D4D), rgb(0x555555))
    } else {
        (rgb(0xE1E1E1), rgb(0xE5F1FB), rgb(0xCCE4F7), rgb(0xADADAD))
    };
    v.widgets.inactive.weak_bg_fill = btn;
    v.widgets.inactive.bg_fill = btn;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, btn_border);
    v.widgets.hovered.weak_bg_fill = btn_h;
    v.widgets.hovered.bg_fill = btn_h;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.accent);
    v.widgets.active.weak_bg_fill = btn_a;
    v.widgets.active.bg_fill = btn_a;
    v.widgets.active.bg_stroke = Stroke::new(1.0, p.accent);
    ctx.set_visuals_of(theme, v);

    ctx.all_styles_mut(|st| {
        use egui::{FontId, TextStyle as T};
        st.spacing.item_spacing = vec2(6.0, 4.0);
        st.spacing.button_padding = vec2(8.0, 3.0);
        st.spacing.interact_size.y = 24.0;
        st.text_styles.insert(T::Body, FontId::proportional(13.0));
        st.text_styles.insert(T::Button, FontId::proportional(13.0));
        st.text_styles.insert(T::Small, FontId::proportional(11.0));
        st.text_styles.insert(T::Heading, FontId::proportional(18.0));
        st.text_styles.insert(T::Monospace, FontId::monospace(12.0));
    });
}

// ------------------------------------------------------------------ glyphs

#[derive(Clone, Copy, PartialEq)]
pub enum Glyph {
    Back,
    Forward,
    Up,
    Refresh,
    Close,
    Plus,
    Gear,
    Minimize,
    Maximize,
    Restore,
}

pub fn outline(p: &Painter, r: Rect, stroke: Stroke) {
    let (l, t, rt, b) = (r.left() + 0.5, r.top() + 0.5, r.right() - 0.5, r.bottom() - 0.5);
    p.add(Shape::closed_line(
        vec![pos2(l, t), pos2(rt, t), pos2(rt, b), pos2(l, b)],
        stroke,
    ));
}

pub fn paint_glyph(p: &Painter, c: Pos2, g: Glyph, col: Color32) {
    let st = Stroke::new(1.0, col);
    let c = pos2(c.x.floor() + 0.5, c.y.floor() + 0.5);
    let line = |a: Vec2x, b: Vec2x| {
        p.line_segment([c + vec2(a.0, a.1), c + vec2(b.0, b.1)], st);
    };
    match g {
        Glyph::Minimize => line((-5.0, 0.0), (5.0, 0.0)),
        Glyph::Maximize => {
            p.add(Shape::closed_line(
                vec![c + vec2(-5.0, -5.0), c + vec2(5.0, -5.0), c + vec2(5.0, 5.0), c + vec2(-5.0, 5.0)],
                st,
            ));
        }
        Glyph::Restore => {
            p.add(Shape::line(
                vec![
                    c + vec2(-2.0, -3.0),
                    c + vec2(-2.0, -5.0),
                    c + vec2(5.0, -5.0),
                    c + vec2(5.0, 2.0),
                    c + vec2(3.0, 2.0),
                ],
                st,
            ));
            p.add(Shape::closed_line(
                vec![c + vec2(-5.0, -2.0), c + vec2(3.0, -2.0), c + vec2(3.0, 5.0), c + vec2(-5.0, 5.0)],
                st,
            ));
        }
        Glyph::Close => {
            line((-5.0, -5.0), (5.0, 5.0));
            line((-5.0, 5.0), (5.0, -5.0));
        }
        Glyph::Plus => {
            line((-5.0, 0.0), (5.0, 0.0));
            line((0.0, -5.0), (0.0, 5.0));
        }
        Glyph::Back => {
            line((-6.0, 0.0), (6.0, 0.0));
            line((-6.0, 0.0), (-1.0, -5.0));
            line((-6.0, 0.0), (-1.0, 5.0));
        }
        Glyph::Forward => {
            line((6.0, 0.0), (-6.0, 0.0));
            line((6.0, 0.0), (1.0, -5.0));
            line((6.0, 0.0), (1.0, 5.0));
        }
        Glyph::Up => {
            line((0.0, 6.0), (0.0, -6.0));
            line((0.0, -6.0), (-5.0, -1.0));
            line((0.0, -6.0), (5.0, -1.0));
        }
        Glyph::Refresh => {
            let r = 5.5;
            let pts: Vec<Pos2> = (0..=18)
                .map(|k| {
                    let a = (40.0 + k as f32 * 15.0).to_radians();
                    c + vec2(a.cos(), a.sin()) * r
                })
                .collect();
            let e = *pts.last().unwrap();
            p.add(Shape::line(pts, st));
            let a = 310.0f32.to_radians();
            let t = vec2(-a.sin(), a.cos());
            let rot = |ang: f32| {
                let (s, co) = ang.sin_cos();
                vec2(t.x * co - t.y * s, t.x * s + t.y * co) * 4.0
            };
            p.line_segment([e, e + rot(2.6)], st);
            p.line_segment([e, e + rot(-2.6)], st);
        }
        Glyph::Gear => {
            p.circle_stroke(c, 3.0, st);
            p.circle_stroke(c, 5.0, st);
            for k in 0..8 {
                let a = (k as f32 * 45.0).to_radians();
                let d = vec2(a.cos(), a.sin());
                p.line_segment([c + d * 5.0, c + d * 7.0], Stroke::new(2.0, col));
            }
        }
    }
}
type Vec2x = (f32, f32);

// ------------------------------------------------------------------ icons (16x16)

pub fn paint_folder(p: &Painter, r: Rect) {
    let (x, y) = (r.left(), r.top());
    let back = rgb(0xE8B04A);
    let front = rgb(0xFFD767);
    p.rect_filled(Rect::from_min_size(pos2(x + 1.0, y + 2.0), vec2(6.0, 3.0)), R0, back);
    p.rect_filled(Rect::from_min_size(pos2(x + 1.0, y + 4.0), vec2(14.0, 2.0)), R0, back);
    p.rect_filled(Rect::from_min_size(pos2(x + 1.0, y + 6.0), vec2(14.0, 8.0)), R0, front);
}

pub fn paint_file(p: &Painter, r: Rect) {
    let (x, y) = (r.left(), r.top());
    let page = Rect::from_min_size(pos2(x + 3.0, y + 1.0), vec2(10.0, 14.0));
    p.rect_filled(page, R0, rgb(0xFAFAFA));
    outline(p, page, Stroke::new(1.0, rgb(0x9A9A9A)));
    p.add(Shape::convex_polygon(
        vec![pos2(x + 9.0, y + 1.0), pos2(x + 13.0, y + 5.0), pos2(x + 9.0, y + 5.0)],
        rgb(0xD0D0D0),
        Stroke::NONE,
    ));
}

pub fn paint_drive(p: &Painter, r: Rect) {
    let (x, y) = (r.left(), r.top());
    p.rect_filled(Rect::from_min_size(pos2(x + 1.0, y + 5.0), vec2(14.0, 7.0)), R0, rgb(0xB5B5B5));
    p.rect_filled(Rect::from_min_size(pos2(x + 1.0, y + 10.0), vec2(14.0, 2.0)), R0, rgb(0x7A7A7A));
    p.rect_filled(Rect::from_min_size(pos2(x + 12.0, y + 7.0), vec2(2.0, 2.0)), R0, rgb(0x2EB82E));
}
