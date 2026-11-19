//! Colours, visuals and a few small custom widgets shared by the whole UI.

use eframe::egui::{
    self, Color32, CornerRadius, FontId, Pos2, Rect, Response, RichText, Sense, Stroke, Ui, Vec2,
    Visuals,
};

/// The look is shared with termker: same colours, text sizes, spacing and sidebar.
#[derive(Clone, Copy)]
pub struct Palette {
    /// Page background.
    pub bg: Color32,
    /// Sidebar and dialog background.
    pub panel: Color32,
    /// Card and input field background.
    pub surface: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    /// Fill of primary buttons and of the selected item.
    pub accent: Color32,
    /// Accent for text, outlines and highlights, readable on the backgrounds.
    pub accent_text: Color32,
    pub danger: Color32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

pub const DARK: Palette = Palette {
    bg: rgb(0x24272B),
    panel: rgb(0x1C1F22),
    surface: rgb(0x2E3237),
    border: rgb(0x40454D),
    text: rgb(0xE4E6E9),
    text_dim: rgb(0xA4ABB3),
    accent: rgb(0x0B6BA8),
    accent_text: rgb(0x58A6FF),
    danger: rgb(0xF87171),
};

pub const LIGHT: Palette = Palette {
    bg: rgb(0xF6F7F8),
    panel: rgb(0xECEEF1),
    surface: rgb(0xFFFFFF),
    border: rgb(0xD2D7DD),
    text: rgb(0x1F2328),
    text_dim: rgb(0x57606A),
    accent: rgb(0x0969DA),
    accent_text: rgb(0x0550AE),
    danger: rgb(0xB91C1C),
};

pub fn palette(dark: bool) -> Palette {
    if dark { DARK } else { LIGHT }
}

pub fn pal(ui: &Ui) -> Palette {
    palette(ui.visuals().dark_mode)
}

/// Accent fill, used where the look doesn't matter (logo, icon).
pub const BLUE: Color32 = DARK.accent;
pub const RED: Color32 = rgb(0xE5534B);
pub const ORANGE: Color32 = rgb(0xDB6D28);

/// Colours for branch lanes in the history tree.
pub const LANES: [Color32; 8] = [
    Color32::from_rgb(88, 166, 255),  // blue
    Color32::from_rgb(240, 136, 62),  // orange
    Color32::from_rgb(140, 200, 100), // green
    Color32::from_rgb(196, 140, 196), // mauve
    Color32::from_rgb(32, 178, 190),  // teal
    Color32::from_rgb(235, 87, 87),   // red
    Color32::from_rgb(245, 200, 80),  // yellow
    Color32::from_rgb(150, 130, 240), // violet
];

pub fn lane_color(i: usize) -> Color32 {
    LANES[i % LANES.len()]
}

pub fn apply(ctx: &egui::Context, light: bool) {
    ctx.set_theme(if light { egui::Theme::Light } else { egui::Theme::Dark });
    ctx.set_visuals_of(egui::Theme::Dark, visuals(&DARK, true));
    ctx.set_visuals_of(egui::Theme::Light, visuals(&LIGHT, false));
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
        style.spacing.interact_size.y = 28.0;
        style.spacing.menu_margin = egui::Margin::same(8);
        for (text_style, font) in style.text_styles.iter_mut() {
            font.size = match text_style {
                egui::TextStyle::Small => 12.0,
                egui::TextStyle::Heading => 22.0,
                _ => 14.0,
            };
        }
    });
}

/// Same widget look as termker.
fn visuals(p: &Palette, dark: bool) -> Visuals {
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    let radius = CornerRadius::same(6);
    v.override_text_color = Some(p.text);
    v.weak_text_color = Some(p.text_dim);
    v.panel_fill = p.bg;
    v.window_fill = p.panel;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.extreme_bg_color = p.surface;
    v.faint_bg_color = p.surface;
    v.code_bg_color = p.surface;
    v.hyperlink_color = p.accent_text;
    v.selection.bg_fill = p.accent.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, p.accent_text);
    v.warn_fg_color = p.danger;
    v.error_fg_color = p.danger;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = radius;

    w.inactive.bg_fill = p.surface;
    w.inactive.weak_bg_fill = p.surface;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = radius;

    w.hovered.bg_fill = p.accent.gamma_multiply(0.18);
    w.hovered.weak_bg_fill = p.accent.gamma_multiply(0.18);
    w.hovered.bg_stroke = Stroke::new(1.0, p.accent_text);
    w.hovered.fg_stroke = Stroke::new(1.5, p.text);
    w.hovered.corner_radius = radius;

    w.active.bg_fill = p.accent.gamma_multiply(0.3);
    w.active.weak_bg_fill = p.accent.gamma_multiply(0.3);
    w.active.bg_stroke = Stroke::new(1.0, p.accent_text);
    w.active.fg_stroke = Stroke::new(2.0, p.text);
    w.active.corner_radius = radius;

    w.open = w.hovered;
    v
}

/// Page title with a short description under it (termker's page header).
pub fn page_header(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.label(RichText::new(title).size(24.0).strong());
    if !subtitle.is_empty() {
        ui.label(RichText::new(subtitle).weak());
    }
    ui.add_space(12.0);
}

/// Section title with a small description, as on termker's Settings page.
pub fn section(ui: &mut Ui, title: &str, description: &str) {
    ui.label(RichText::new(title).strong());
    if !description.is_empty() {
        ui.add(egui::Label::new(RichText::new(description).small().weak()).wrap());
    }
    ui.add_space(4.0);
}

/// Run `add` with button colours switched to the accent fill.
pub fn blue_scope<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.scope(|ui| {
        let p = pal(ui);
        let w = &mut ui.visuals_mut().widgets;
        for (state, fill, stroke) in [
            (&mut w.inactive, p.accent, Stroke::NONE),
            (&mut w.hovered, p.accent, Stroke::new(1.0, p.accent_text)),
            (&mut w.active, p.accent.linear_multiply(0.85), Stroke::new(1.0, p.accent_text)),
        ] {
            state.weak_bg_fill = fill;
            state.bg_fill = fill;
            state.fg_stroke = Stroke::new(1.0, Color32::WHITE);
            state.bg_stroke = stroke;
        }
        add(ui)
    })
    .inner
}

/// The default action button: solid blue.
pub fn primary(ui: &mut Ui, text: impl Into<String>) -> Response {
    let text = text.into();
    blue_scope(ui, |ui| ui.button(RichText::new(text).color(Color32::WHITE).strong()))
}

pub fn primary_enabled(ui: &mut Ui, enabled: bool, text: impl Into<String>) -> Response {
    let text = text.into();
    blue_scope(ui, |ui| {
        ui.add_enabled(enabled, egui::Button::new(RichText::new(text).color(Color32::WHITE).strong()))
    })
}

/// A full-width blue button of a given height.
pub fn primary_wide(ui: &mut Ui, enabled: bool, text: impl Into<String>, height: f32) -> Response {
    let text = text.into();
    let w = ui.available_width();
    blue_scope(ui, |ui| {
        ui.add_enabled(
            enabled,
            egui::Button::new(RichText::new(text).color(Color32::WHITE).strong()).min_size(Vec2::new(w, height)),
        )
    })
}

/// A sidebar navigation item, like termker's: full width, 34 px, accent fill
/// with white bold text when selected, a light hover otherwise.
pub fn nav_item(ui: &mut Ui, selected: bool, text: &str, badge: Option<usize>) -> Response {
    let p = pal(ui);
    let label = if selected {
        RichText::new(text).color(Color32::WHITE).strong()
    } else {
        RichText::new(text)
    };
    let mut button = egui::Button::new(label).min_size(Vec2::new(ui.available_width(), 34.0)).frame_when_inactive(false);
    if selected {
        button = button.fill(p.accent).frame_when_inactive(true);
    }
    let resp = ui.add(button);
    if let Some(n) = badge.filter(|n| *n > 0) {
        let label = n.to_string();
        let w = 10.0 + 7.0 * label.len() as f32;
        let rect = resp.rect;
        let badge_rect = Rect::from_center_size(rect.right_center() - Vec2::new(10.0 + w / 2.0, 0.0), Vec2::new(w, 18.0));
        let (bg, fg) = if selected {
            (Color32::from_white_alpha(60), Color32::WHITE)
        } else {
            (p.accent, Color32::WHITE)
        };
        let painter = ui.painter();
        painter.rect_filled(badge_rect, 9.0, bg);
        painter.text(badge_rect.center(), egui::Align2::CENTER_CENTER, label, FontId::proportional(11.5), fg);
    }
    resp
}

/// Gitsot's logo: a small branch diagram (two lanes joining) in a rounded square.
pub fn paint_logo(painter: &egui::Painter, rect: Rect) {
    painter.rect_filled(rect, rect.width() * 0.22, BLUE);
    let s = rect.width();
    let p = |x: f32, y: f32| Pos2::new(rect.left() + x * s, rect.top() + y * s);
    let stroke = Stroke::new((s * 0.085).max(1.5), Color32::WHITE);
    let r = s * 0.1;
    // Main line
    painter.line_segment([p(0.34, 0.22), p(0.34, 0.78)], stroke);
    // Branch curve
    let curve = egui::epaint::CubicBezierShape::from_points_stroke(
        [p(0.68, 0.34), p(0.68, 0.56), p(0.34, 0.50), p(0.34, 0.70)],
        false,
        Color32::TRANSPARENT,
        stroke,
    );
    painter.add(curve);
    for c in [p(0.34, 0.24), p(0.68, 0.30), p(0.34, 0.76)] {
        painter.circle_filled(c, r, Color32::WHITE);
    }
}

pub fn logo(ui: &mut Ui, size: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_logo(ui.painter(), rect);
    resp
}

/// Card frame: `surface` background with a thin border (termker's card).
pub fn card_frame(ui: &Ui, highlighted: bool) -> egui::Frame {
    let p = pal(ui);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(if highlighted { 1.5 } else { 1.0 }, if highlighted { p.accent_text } else { p.border }))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::same(14))
}

#[derive(Clone, Copy, PartialEq)]
pub enum Icon {
    Rename,
    Delete,
}

/// A small square button with a drawn icon (pencil or bin), as in termker.
/// The name is shown when the pointer rests on it.
pub fn icon_button(ui: &mut Ui, icon: Icon, name: &str) -> Response {
    let p = pal(ui);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(26.0, 26.0), Sense::click());
    let hovered = response.hovered();
    let color = match (icon, hovered) {
        (Icon::Delete, true) => p.danger,
        (_, true) => p.text,
        (_, false) => p.text_dim,
    };
    let painter = ui.painter();
    if hovered {
        painter.rect_filled(rect, 6.0, p.border);
    }
    let stroke = Stroke::new(1.4, color);
    let c = rect.center();
    let at = |x: f32, y: f32| c + Vec2::new(x, y);
    match icon {
        Icon::Rename => {
            painter.add(egui::Shape::closed_line(vec![at(2.5, -6.5), at(6.5, -2.5), at(-2.5, 6.5), at(-6.5, 2.5)], stroke));
            painter.add(egui::Shape::convex_polygon(vec![at(-6.5, 2.5), at(-2.5, 6.5), at(-7.0, 7.0)], color, stroke));
            painter.line_segment([at(0.0, -4.0), at(4.0, 0.0)], stroke);
        }
        Icon::Delete => {
            painter.line_segment([at(-6.5, -4.0), at(6.5, -4.0)], stroke);
            painter.line_segment([at(-2.0, -6.5), at(2.0, -6.5)], stroke);
            painter.line_segment([at(-2.0, -6.5), at(-2.0, -4.0)], stroke);
            painter.line_segment([at(2.0, -6.5), at(2.0, -4.0)], stroke);
            painter.add(egui::Shape::line(vec![at(-5.0, -4.0), at(-4.0, 7.0), at(4.0, 7.0), at(5.0, -4.0)], stroke));
            painter.line_segment([at(-1.5, -1.0), at(-1.5, 4.0)], stroke);
            painter.line_segment([at(1.5, -1.0), at(1.5, 4.0)], stroke);
        }
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(name)
}

/// The open/close arrow of a group header.
pub fn fold_arrow(ui: &mut Ui, collapsed: bool) -> Response {
    let p = pal(ui);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(14.0, 20.0), Sense::click());
    let c = rect.center();
    let points = if collapsed {
        vec![c + Vec2::new(-3.0, -5.0), c + Vec2::new(4.0, 0.0), c + Vec2::new(-3.0, 5.0)]
    } else {
        vec![c + Vec2::new(-5.0, -3.0), c + Vec2::new(5.0, -3.0), c + Vec2::new(0.0, 4.0)]
    };
    ui.painter().add(egui::Shape::convex_polygon(points, p.text_dim, Stroke::NONE));
    resp
}

/// Pixels for the window icon (the logo rendered at `size`).
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];
    let inside_rounded = |x: f32, y: f32| {
        let r = s * 0.22;
        let cx = x.clamp(r, s - r);
        let cy = y.clamp(r, s - r);
        (x - cx).powi(2) + (y - cy).powi(2) <= r * r
    };
    let dist_seg = |x: f32, y: f32, a: (f32, f32), b: (f32, f32)| {
        let (ax, ay) = (a.0 * s, a.1 * s);
        let (bx, by) = (b.0 * s, b.1 * s);
        let (dx, dy) = (bx - ax, by - ay);
        let t = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        ((x - ax - t * dx).powi(2) + (y - ay - t * dy).powi(2)).sqrt()
    };
    for yi in 0..size {
        for xi in 0..size {
            let (x, y) = (xi as f32 + 0.5, yi as f32 + 0.5);
            let i = ((yi * size + xi) * 4) as usize;
            if !inside_rounded(x, y) {
                continue;
            }
            let w = s * 0.045;
            let on_line = dist_seg(x, y, (0.34, 0.22), (0.34, 0.78)) < w
                || dist_seg(x, y, (0.68, 0.30), (0.62, 0.50)) < w
                || dist_seg(x, y, (0.62, 0.50), (0.40, 0.56)) < w
                || dist_seg(x, y, (0.40, 0.56), (0.34, 0.68)) < w;
            let dot = |cx: f32, cy: f32| ((x - cx * s).powi(2) + (y - cy * s).powi(2)).sqrt() < s * 0.1;
            let white = on_line || dot(0.34, 0.24) || dot(0.68, 0.30) || dot(0.34, 0.76);
            let c = if white { Color32::WHITE } else { BLUE };
            px[i..i + 4].copy_from_slice(&[c.r(), c.g(), c.b(), 255]);
        }
    }
    px
}
