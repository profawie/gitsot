//! Terminal widget: paints the emulator screen and turns keyboard and mouse
//! input into bytes for the shell.
//!
//! Gitsot's prompt uses a few private-use characters for its chips and icons.
//! They are drawn here as shapes, so the terminal needs no special font.

use alacritty_terminal::{
    index::Side,
    selection::SelectionType,
    term::{cell::Flags, TermMode},
    vte::ansi::{CursorShape, Rgb},
};
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Stroke, Vec2};

use crate::{
    term::{self, Key, KeyModifiers, Palette, Size, Terminal},
    theme,
};

const PADDING: f32 = 8.0;
/// Most lines scrolled in one frame.
const MAX_LINES_PER_FRAME: i32 = 50;

/// What the user asked for through the widget itself.
#[derive(Default)]
pub struct Outcome {
    /// Enter was pressed after the shell ended: start it again.
    pub restart: bool,
}

/// Background of the terminal for the current look.
pub fn background(p: &theme::Palette, dark: bool) -> Color32 {
    if dark { p.panel } else { p.surface }
}

/// Show the terminal in the remaining space. `interactive` is false while a
/// dialog is open, so typing goes to the dialog instead.
pub fn show(ui: &mut egui::Ui, terminal: &mut Terminal, font_size: f32, interactive: bool) -> Outcome {
    let mut outcome = Outcome::default();
    let font = FontId::monospace(font_size);
    let (cell_width, cell_height) = ui.fonts_mut(|fonts| (fonts.glyph_width(&font, 'M'), fonts.row_height(&font)));
    let cell = Vec2::new(cell_width.max(1.0), cell_height.max(1.0));

    let outer = ui.available_rect_before_wrap();
    let id = ui.scope_id().with("terminal");
    let response = ui.interact(outer, id, egui::Sense::click_and_drag());
    ui.advance_cursor_after_rect(outer);
    let area = outer.shrink(PADDING);

    terminal.resize(Size {
        cols: (area.width() / cell.x).floor().clamp(2.0, 1000.0) as u16,
        rows: (area.height() / cell.y).floor().clamp(1.0, 1000.0) as u16,
    });

    if interactive {
        if response.clicked() || response.drag_started() {
            response.request_focus();
        }
        // Take focus when nothing else has it, so typing works right away.
        if ui.memory(|memory| memory.focused().is_none()) {
            response.request_focus();
        }
    }
    let focused = interactive && response.has_focus();
    if focused {
        // Keep Tab, arrows and Escape for the terminal instead of moving focus.
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                id,
                egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
            );
        });
        if terminal.is_running() {
            handle_keyboard(ui, terminal);
        } else if ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            outcome.restart = true;
        }
    }
    if interactive {
        handle_mouse(ui, &response, terminal, area, cell);
    }

    paint(ui, terminal, &font, area, outer, cell, focused);
    outcome
}

fn handle_keyboard(ui: &mut egui::Ui, terminal: &mut Terminal) {
    let events = ui.input(|input| input.events.clone());
    let mode = terminal.mode();
    let mac = cfg!(target_os = "macos");
    let mut output: Vec<u8> = Vec::new();

    for event in events {
        match event {
            egui::Event::Text(text) => {
                // On Windows and Linux, Alt+key sends Escape then the key.
                let alt = !mac && ui.input(|input| input.modifiers.alt && !input.modifiers.ctrl);
                if alt {
                    output.push(0x1b);
                }
                output.extend_from_slice(text.as_bytes());
            }
            egui::Event::Paste(text) => output.extend(term::encode_paste(&text, mode)),
            // Cmd+C on macOS; Ctrl+C elsewhere, where it copies only when
            // something is selected and otherwise interrupts as usual.
            egui::Event::Copy => {
                if let Some(text) = terminal.selected_text() {
                    ui.ctx().copy_text(text);
                    terminal.clear_selection();
                } else if !mac {
                    output.push(0x03);
                }
            }
            egui::Event::Cut if !mac => output.push(0x18),
            egui::Event::Key { key, pressed: true, modifiers, .. } => {
                // Ctrl+Shift+C / Ctrl+Shift+V: explicit copy and paste.
                if !mac && modifiers.ctrl && modifiers.shift && key == egui::Key::C {
                    if let Some(text) = terminal.selected_text() {
                        ui.ctx().copy_text(text);
                    }
                    continue;
                }
                if !mac && modifiers.ctrl && modifiers.shift && key == egui::Key::V {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                    continue;
                }
                // Shift+PageUp / Shift+PageDown scroll through history.
                if modifiers.shift && matches!(key, egui::Key::PageUp | egui::Key::PageDown) {
                    let page = terminal.size().rows as i32 - 1;
                    terminal.scroll(if key == egui::Key::PageUp { page } else { -page });
                    continue;
                }
                let mods = KeyModifiers { shift: modifiers.shift, alt: modifiers.alt, ctrl: modifiers.ctrl };
                if let Some(bytes) = map_key(key, modifiers.ctrl).and_then(|key| term::encode_key(key, mods, mode)) {
                    output.extend(bytes);
                }
            }
            _ => {}
        }
    }

    if !output.is_empty() {
        terminal.clear_selection();
        terminal.scroll_to_bottom();
        terminal.send_input(output);
    }
}

/// Map an egui key to a terminal key. Plain characters are not mapped here:
/// they arrive as text events.
fn map_key(key: egui::Key, ctrl: bool) -> Option<Key> {
    use egui::Key as K;
    Some(match key {
        K::Enter => Key::Enter,
        K::Backspace => Key::Backspace,
        K::Tab => Key::Tab,
        K::Escape => Key::Escape,
        K::ArrowUp => Key::Up,
        K::ArrowDown => Key::Down,
        K::ArrowRight => Key::Right,
        K::ArrowLeft => Key::Left,
        K::Home => Key::Home,
        K::End => Key::End,
        K::PageUp => Key::PageUp,
        K::PageDown => Key::PageDown,
        K::Insert => Key::Insert,
        K::Delete => Key::Delete,
        K::F1 => Key::F(1),
        K::F2 => Key::F(2),
        K::F3 => Key::F(3),
        K::F4 => Key::F(4),
        K::F5 => Key::F(5),
        K::F6 => Key::F(6),
        K::F7 => Key::F(7),
        K::F8 => Key::F(8),
        K::F9 => Key::F(9),
        K::F10 => Key::F(10),
        K::F11 => Key::F(11),
        K::F12 => Key::F(12),
        K::Space if ctrl => Key::Ctrl(' '),
        K::OpenBracket if ctrl => Key::Ctrl('['),
        K::CloseBracket if ctrl => Key::Ctrl(']'),
        K::Backslash if ctrl => Key::Ctrl('\\'),
        K::Minus if ctrl => Key::Ctrl('_'),
        other if ctrl => {
            let name = other.name();
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphabetic() => Key::Ctrl(c),
                _ => return None,
            }
        }
        _ => return None,
    })
}

fn handle_mouse(ui: &mut egui::Ui, response: &egui::Response, terminal: &mut Terminal, area: Rect, cell: Vec2) {
    let size = terminal.size();
    let locate = |pos: Pos2| {
        let x = ((pos.x - area.left()) / cell.x).max(0.0);
        let y = ((pos.y - area.top()) / cell.y).max(0.0);
        let col = (x as usize).min(size.cols as usize - 1);
        let row = (y as usize).min(size.rows as usize - 1);
        let side = if x.fract() < 0.5 { Side::Left } else { Side::Right };
        (col, row, side)
    };

    if let Some(pos) = response.interact_pointer_pos() {
        let (col, row, side) = locate(pos);
        if response.double_clicked() {
            terminal.start_selection(SelectionType::Semantic, col, row, side);
        } else if response.triple_clicked() {
            terminal.start_selection(SelectionType::Lines, col, row, side);
        } else if response.drag_started() {
            terminal.start_selection(SelectionType::Simple, col, row, side);
        } else if response.dragged() {
            terminal.update_selection(col, row, side);
        } else if response.clicked() {
            terminal.clear_selection();
        }
    }

    if response.hovered() {
        let delta = ui.input(|input| input.smooth_scroll_delta.y);
        if delta != 0.0 {
            let total = terminal.scroll_remainder + delta / cell.y;
            let lines = (total.trunc() as i32).clamp(-MAX_LINES_PER_FRAME, MAX_LINES_PER_FRAME);
            terminal.scroll_remainder = total.fract();
            if lines == 0 {
                return;
            }
            let mode = terminal.mode();
            if mode.contains(TermMode::ALT_SCREEN) {
                // Full-screen programs have no scrollback; send arrow keys.
                let key = if lines > 0 { Key::Up } else { Key::Down };
                if let Some(bytes) = term::encode_key(key, KeyModifiers::default(), mode) {
                    terminal.send_input(bytes.repeat(lines.unsigned_abs() as usize));
                }
            } else {
                terminal.scroll(lines);
            }
        }
    }
}

fn color32(rgb: Rgb) -> Color32 {
    Color32::from_rgb(rgb.r, rgb.g, rgb.b)
}

fn rgb(color: Color32) -> Rgb {
    let [r, g, b, _] = color.to_array();
    Rgb { r, g, b }
}

/// Mix `over` onto `base`; `amount` 0 keeps `base`, 1 gives `over`.
fn mix(base: Color32, over: Color32, amount: f32) -> Color32 {
    let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount).round() as u8;
    let ([r1, g1, b1, _], [r2, g2, b2, _]) = (base.to_array(), over.to_array());
    Color32::from_rgb(channel(r1, r2), channel(g1, g2), channel(b1, b2))
}

fn paint(ui: &egui::Ui, terminal: &Terminal, font: &FontId, area: Rect, outer: Rect, cell: Vec2, focused: bool) {
    let p = theme::pal(ui);
    let dark = ui.visuals().dark_mode;
    let bg_color = background(&p, dark);
    let painter = ui.painter_at(outer);
    painter.rect_filled(outer, 0.0, bg_color);

    let palette = Palette { foreground: rgb(p.text), background: rgb(bg_color), cursor: rgb(p.accent_text), dark };
    let selection_bg = mix(bg_color, p.accent, 0.45);
    let term = terminal.term.lock();
    let content = term.renderable_content();
    let offset = content.display_offset as i32;
    let cell_pos = |col: usize, row: i32| Pos2::new(area.left() + col as f32 * cell.x, area.top() + row as f32 * cell.y);

    // A run of same-coloured ASCII cells is painted as one piece of text, and
    // a run of same-coloured backgrounds as one rectangle (no seams).
    struct Run {
        col: usize,
        row: i32,
        color: Color32,
        text: String,
    }
    struct Fill {
        col: usize,
        row: i32,
        cells: usize,
        color: Color32,
    }
    let mut runs: Vec<Run> = Vec::new();
    let mut fills: Vec<Fill> = Vec::new();
    let mut shapes: Vec<(char, Pos2, Color32)> = Vec::new();
    let mut underlines: Vec<(Pos2, Color32)> = Vec::new();
    let cursor_point = content.cursor.point;
    let mut cursor_char = ' ';

    for indexed in content.display_iter {
        let row = indexed.point.line.0 + offset;
        let col = indexed.point.column.0;
        let cell_data = indexed.cell;
        let flags = cell_data.flags;
        if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }

        let mut fg = color32(palette.resolve(cell_data.fg, content.colors));
        let mut bg = color32(palette.resolve(cell_data.bg, content.colors));
        if flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if flags.contains(Flags::DIM) {
            fg = mix(bg, fg, 0.6);
        }
        if content.selection.is_some_and(|range| range.contains(indexed.point)) {
            bg = selection_bg;
        }

        let width = if flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
        if bg != bg_color {
            match fills.last_mut() {
                Some(fill) if fill.row == row && fill.color == bg && fill.col + fill.cells == col => fill.cells += width,
                _ => fills.push(Fill { col, row, cells: width, color: bg }),
            }
        }
        if indexed.point == cursor_point {
            cursor_char = cell_data.c;
        }

        let c = cell_data.c;
        if c == ' ' || c == '\0' || flags.contains(Flags::HIDDEN) {
            continue;
        }
        if flags.intersects(Flags::ALL_UNDERLINES) {
            underlines.push((cell_pos(col, row), fg));
        }
        if is_drawn_glyph(c) {
            shapes.push((c, cell_pos(col, row), fg));
            continue;
        }
        match runs.last_mut() {
            Some(run)
                if c.is_ascii()
                    && run.row == row
                    && run.color == fg
                    && run.col + run.text.len() == col
                    && run.text.is_ascii() =>
            {
                run.text.push(c);
            }
            _ => runs.push(Run { col, row, color: fg, text: c.to_string() }),
        }
    }

    for fill in &fills {
        let size = Vec2::new(cell.x * fill.cells as f32, cell.y);
        painter.rect_filled(Rect::from_min_size(cell_pos(fill.col, fill.row), size), 0.0, fill.color);
    }
    for (c, pos, color) in shapes {
        paint_glyph(&painter, c, pos, cell, color);
    }
    for run in &runs {
        painter.text(cell_pos(run.col, run.row), egui::Align2::LEFT_TOP, &run.text, font.clone(), run.color);
    }
    for (pos, color) in underlines {
        let y = pos.y + cell.y - 1.5;
        painter.hline(pos.x..=pos.x + cell.x, y, Stroke::new(1.0, color));
    }

    // Cursor, only when the view is not scrolled back into history.
    let cursor_row = cursor_point.line.0 + offset;
    let visible = content.mode.contains(TermMode::SHOW_CURSOR)
        && content.cursor.shape != CursorShape::Hidden
        && offset == 0
        && terminal.is_running();
    if visible {
        let rect = Rect::from_min_size(cell_pos(cursor_point.column.0, cursor_row), cell);
        match content.cursor.shape {
            _ if !focused => {
                painter.rect_stroke(rect, 0.0, Stroke::new(1.0, p.accent_text), egui::StrokeKind::Inside);
            }
            CursorShape::Beam => {
                painter.rect_filled(Rect::from_min_size(rect.min, Vec2::new(2.0, cell.y)), 0.0, p.accent_text);
            }
            CursorShape::Underline => {
                painter.rect_filled(Rect::from_min_max(Pos2::new(rect.left(), rect.bottom() - 2.0), rect.max), 0.0, p.accent_text);
            }
            _ => {
                painter.rect_filled(rect, 0.0, p.accent_text);
                if cursor_char != ' ' && cursor_char != '\0' && !is_drawn_glyph(cursor_char) {
                    painter.text(rect.min, egui::Align2::LEFT_TOP, cursor_char, font.clone(), bg_color);
                }
            }
        }
    }

    // Small marker while looking at history.
    if offset > 0 {
        note(&painter, outer.right_top() + Vec2::new(-10.0, 8.0), egui::Align2::RIGHT_TOP, &format!("{offset} lines up"), &p, bg_color);
    }
    if let Some(code) = terminal.exited {
        let text = match code {
            Some(code) if code != 0 => format!("The shell ended (exit code {code}). Press Enter to start it again."),
            _ => "The shell ended. Press Enter to start it again.".to_string(),
        };
        note(&painter, outer.center_bottom() + Vec2::new(0.0, -14.0), egui::Align2::CENTER_BOTTOM, &text, &p, bg_color);
    }
}

/// A short message on a small plate, on top of the terminal.
fn note(painter: &egui::Painter, pos: Pos2, anchor: egui::Align2, text: &str, p: &theme::Palette, bg: Color32) {
    let galley = painter.layout_no_wrap(text.to_string(), FontId::proportional(12.5), p.text);
    let rect = anchor.anchor_size(pos, galley.size());
    painter.rect(rect.expand2(Vec2::new(8.0, 4.0)), 6.0, bg, Stroke::new(1.0, p.border), egui::StrokeKind::Outside);
    painter.galley(rect.min, galley, p.text);
}

// Characters drawn as shapes. The first four are the usual "powerline"
// separators; the rest are Gitsot's own prompt icons (see assets/prompt.*).
const CAP_LEFT: char = '\u{E0B6}';
const CAP_RIGHT: char = '\u{E0B4}';
const ARROW_RIGHT: char = '\u{E0B0}';
const ARROW_LEFT: char = '\u{E0B2}';
const ICON_FIRST: char = '\u{EE00}';
const ICON_LAST: char = '\u{EE0A}';

fn is_drawn_glyph(c: char) -> bool {
    matches!(c, CAP_LEFT | CAP_RIGHT | ARROW_RIGHT | ARROW_LEFT | ICON_FIRST..=ICON_LAST)
}

/// Draw one of the characters `is_drawn_glyph` accepts. `pos` is the top left
/// of its cell. Icons are followed by a space in the prompt and are centred
/// over both cells, so they can be a little larger than one character.
fn paint_glyph(painter: &egui::Painter, c: char, pos: Pos2, cell: Vec2, color: Color32) {
    let rect = Rect::from_min_size(pos, cell);
    match c {
        CAP_LEFT | CAP_RIGHT => {
            // Half an ellipse filling the cell, flat side towards the chip.
            // It reaches half a pixel into the chip so no gap shows.
            let left = c == CAP_LEFT;
            let cx = if left { rect.right() + 0.5 } else { rect.left() - 0.5 };
            let (rx, ry) = (cell.x.min(cell.y / 2.0) + 0.5, cell.y / 2.0);
            let steps = 14;
            let points: Vec<Pos2> = (0..=steps)
                .map(|i| {
                    let angle = std::f32::consts::PI * (i as f32 / steps as f32 - 0.5);
                    let dx = angle.cos() * rx * if left { -1.0 } else { 1.0 };
                    Pos2::new(cx + dx, rect.center().y + angle.sin() * ry)
                })
                .collect();
            painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        }
        ARROW_RIGHT => {
            let points = vec![rect.left_top(), rect.right_center(), rect.left_bottom()];
            painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        }
        ARROW_LEFT => {
            let points = vec![rect.right_top(), rect.left_center(), rect.right_bottom()];
            painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        }
        _ => paint_icon(painter, c as u32 - ICON_FIRST as u32, rect, color),
    }
}

fn paint_icon(painter: &egui::Painter, icon: u32, cell: Rect, color: Color32) {
    // A square over this cell and the next one.
    let side = (cell.width() * 2.0).min(cell.height()) * 0.74;
    let center = Pos2::new(cell.right(), cell.center().y);
    let at = |x: f32, y: f32| center + Vec2::new(x, y) * side;
    let stroke = Stroke::new((side * 0.11).max(1.3), color);
    let line = |points: Vec<Pos2>| painter.add(egui::Shape::line(points, stroke));
    let closed = |points: Vec<Pos2>| painter.add(egui::Shape::closed_line(points, stroke));
    let curve = |points: [Pos2; 4]| {
        painter.add(egui::epaint::CubicBezierShape::from_points_stroke(points, false, Color32::TRANSPARENT, stroke))
    };
    match icon {
        // Folder
        0 => {
            closed(vec![at(-0.5, -0.38), at(-0.12, -0.38), at(0.02, -0.22), at(0.5, -0.22), at(0.5, 0.4), at(-0.5, 0.4)]);
        }
        // Branch
        1 => {
            line(vec![at(-0.24, -0.3), at(-0.24, 0.3)]);
            curve([at(0.28, -0.2), at(0.28, 0.12), at(-0.24, 0.0), at(-0.24, 0.28)]);
            for (x, y) in [(-0.24, -0.36), (-0.24, 0.36), (0.28, -0.26)] {
                painter.circle_filled(at(x, y), side * 0.13, color);
            }
        }
        // Person
        2 => {
            painter.circle_stroke(at(0.0, -0.22), side * 0.2, stroke);
            curve([at(-0.42, 0.46), at(-0.42, 0.02), at(0.42, 0.02), at(0.42, 0.46)]);
        }
        // Pencil
        3 => {
            closed(vec![at(0.2, -0.48), at(0.48, -0.2), at(-0.18, 0.46), at(-0.46, 0.18)]);
            painter.add(egui::Shape::convex_polygon(vec![at(-0.46, 0.18), at(-0.18, 0.46), at(-0.52, 0.52)], color, stroke));
        }
        // Clock
        4 => {
            painter.circle_stroke(at(0.0, 0.0), side * 0.45, stroke);
            line(vec![at(0.0, -0.26), at(0.0, 0.0), at(0.2, 0.12)]);
        }
        // Check mark
        5 => {
            line(vec![at(-0.42, 0.02), at(-0.14, 0.3), at(0.42, -0.3)]);
        }
        // Cross
        6 => {
            line(vec![at(-0.32, -0.32), at(0.32, 0.32)]);
            line(vec![at(0.32, -0.32), at(-0.32, 0.32)]);
        }
        // Arrow up / down
        7 | 8 => {
            let flip = if icon == 7 { 1.0 } else { -1.0 };
            line(vec![at(0.0, 0.42 * flip), at(0.0, -0.4 * flip)]);
            line(vec![at(-0.3, -0.1 * flip), at(0.0, -0.42 * flip), at(0.3, -0.1 * flip)]);
        }
        // Warning triangle
        9 => {
            closed(vec![at(0.0, -0.46), at(0.5, 0.4), at(-0.5, 0.4)]);
            line(vec![at(0.0, -0.14), at(0.0, 0.12)]);
            painter.circle_filled(at(0.0, 0.26), stroke.width * 0.6, color);
        }
        // Prompt chevron
        _ => {
            let bold = Stroke::new(stroke.width * 1.35, color);
            painter.add(egui::Shape::line(vec![at(-0.3, -0.36), at(0.08, 0.0), at(-0.3, 0.36)], bold));
        }
    }
}
