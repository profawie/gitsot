// Hide the console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod cli;
mod git;
mod graph;
mod theme;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Gitsot")
            .with_app_id("gitsot")
            .with_inner_size([1200.0, 760.0])
            .with_min_inner_size([860.0, 520.0])
            .with_icon(eframe::egui::IconData { rgba: theme::icon_rgba(64), width: 64, height: 64 }),
        ..Default::default()
    };
    eframe::run_native(
        "Gitsot",
        options,
        Box::new(|cc| Ok(Box::new(app::GitsotApp::new(cc)))),
    )
}
