//! Colours, visuals and a few small custom widgets shared by the whole UI.

use eframe::egui::{
    self, Color32, CornerRadius, FontId, Pos2, Rect, Response, RichText, Sense, Stroke, Ui, Vec2,
    Visuals,
};

pub const BLUE: Color32 = Color32::from_rgb(47, 129, 247);
pub const BLUE_HOVER: Color32 = Color32::from_rgb(76, 148, 250);
pub const BLUE_ACTIVE: Color32 = Color32::from_rgb(31, 107, 214);
pub const RED: Color32 = Color32::from_rgb(229, 83, 75);
pub const ORANGE: Color32 = Color32::from_rgb(219, 109, 40);

/// Colours for branch lanes in the history tree.
pub const LANES: [Color32; 8] = [
    Color32::from_rgb(47, 129, 247),  // blue
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

/// Background of the left navigation sidebar.
pub fn nav_bg(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(27, 29, 33)
    } else {
        Color32::from_rgb(236, 238, 242)
    }
}

/// Background of cards.
pub fn card_bg(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(42, 45, 51)
    } else {
        Color32::WHITE
    }
}

pub fn card_border(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(58, 62, 70)
    } else {
        Color32::from_rgb(214, 218, 224)
    }
}

pub fn apply(ctx: &egui::Context, light: bool) {
    ctx.set_theme(if light { egui::Theme::Light } else { egui::Theme::Dark });
    ctx.set_visuals_of(egui::Theme::Dark, dark_visuals());
    ctx.set_visuals_of(egui::Theme::Light, light_visuals());
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(12.0, 5.0);
        style.spacing.interact_size.y = 28.0;
        style.spacing.menu_margin = egui::Margin::same(8);
    });
}

fn dark_visuals() -> Visuals {
    let mut v = Visuals::dark();
    let bg = Color32::from_rgb(34, 37, 43);
    v.panel_fill = bg;
    v.window_fill = Color32::from_rgb(40, 43, 50);
    v.extreme_bg_color = Color32::from_rgb(26, 28, 32);
    v.faint_bg_color = Color32::from_rgb(40, 43, 50);
    v.code_bg_color = Color32::from_rgb(26, 28, 32);
    v.window_stroke = Stroke::new(1.0, card_border(true));
    v.selection.bg_fill = BLUE;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.hyperlink_color = BLUE_HOVER;
    common(&mut v, true);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(48, 52, 60);
    v.widgets.inactive.bg_fill = Color32::from_rgb(48, 52, 60);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(60, 65, 74);
    v.widgets.hovered.bg_fill = Color32::from_rgb(60, 65, 74);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(70, 76, 86);
    v
}

fn light_visuals() -> Visuals {
    let mut v = Visuals::light();
    v.panel_fill = Color32::from_rgb(246, 247, 249);
    v.window_fill = Color32::WHITE;
    v.extreme_bg_color = Color32::WHITE;
    v.faint_bg_color = Color32::from_rgb(240, 242, 245);
    v.window_stroke = Stroke::new(1.0, card_border(false));
    v.selection.bg_fill = BLUE;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.hyperlink_color = BLUE_ACTIVE;
    common(&mut v, false);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(228, 231, 236);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(216, 220, 227);
    v
}

fn common(v: &mut Visuals, _dark: bool) {
    let r = CornerRadius::same(6);
    v.widgets.noninteractive.corner_radius = r;
    v.widgets.inactive.corner_radius = r;
    v.widgets.hovered.corner_radius = r;
    v.widgets.active.corner_radius = r;
    v.widgets.open.corner_radius = r;
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.widgets.hovered.bg_stroke = Stroke::NONE;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
}

/// Run `add` with button colours switched to the blue accent.
pub fn blue_scope<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.scope(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        for (state, fill) in [
            (&mut w.inactive, BLUE),
            (&mut w.hovered, BLUE_HOVER),
            (&mut w.active, BLUE_ACTIVE),
        ] {
            state.weak_bg_fill = fill;
            state.bg_fill = fill;
            state.fg_stroke = Stroke::new(1.0, Color32::WHITE);
            state.bg_stroke = Stroke::NONE;
        }
        add(ui)
    })
    .inner
}

/// The default action button: solid blue.
pub fn primary(ui: &mut Ui, text: impl Into<String>) -> Response {
    let text = text.into();
    blue_scope(ui, |ui| ui.button(RichText::new(text).color(Color32::WHITE)))
}

pub fn primary_enabled(ui: &mut Ui, enabled: bool, text: impl Into<String>) -> Response {
    let text = text.into();
    blue_scope(ui, |ui| {
        ui.add_enabled(enabled, egui::Button::new(RichText::new(text).color(Color32::WHITE)))
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

/// A sidebar navigation item: blue when selected, subtle hover otherwise.
pub fn nav_item(ui: &mut Ui, selected: bool, text: &str, badge: Option<usize>) -> Response {
    let size = Vec2::new(ui.available_width(), 32.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let visuals = ui.visuals();
    let fill = if selected {
        BLUE
    } else if resp.hovered() {
        visuals.widgets.hovered.weak_bg_fill
    } else {
        Color32::TRANSPARENT
    };
    let text_color = if selected { Color32::WHITE } else { visuals.text_color() };
    let painter = ui.painter();
    painter.rect_filled(rect, 6.0, fill);
    painter.text(
        rect.left_center() + Vec2::new(12.0, 0.0),
        egui::Align2::LEFT_CENTER,
        text,
        FontId::proportional(14.5),
        text_color,
    );
    if let Some(n) = badge.filter(|n| *n > 0) {
        let label = n.to_string();
        let w = 10.0 + 7.0 * label.len() as f32;
        let badge_rect = Rect::from_center_size(rect.right_center() - Vec2::new(10.0 + w / 2.0, 0.0), Vec2::new(w, 18.0));
        let (bg, fg) = if selected {
            (Color32::from_white_alpha(60), Color32::WHITE)
        } else {
            (BLUE, Color32::WHITE)
        };
        painter.rect_filled(badge_rect, 9.0, bg);
        painter.text(badge_rect.center(), egui::Align2::CENTER_CENTER, label, FontId::proportional(11.5), fg);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
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

/// A rounded "card" frame.
pub fn card_frame(ui: &Ui, highlighted: bool) -> egui::Frame {
    let dark = ui.visuals().dark_mode;
    egui::Frame::new()
        .fill(card_bg(dark))
        .stroke(Stroke::new(if highlighted { 1.5 } else { 1.0 }, if highlighted { BLUE } else { card_border(dark) }))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::same(14))
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
