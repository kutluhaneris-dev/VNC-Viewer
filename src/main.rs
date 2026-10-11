// No console window behind the app on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod keys;
mod rfb;
mod store;
mod tight;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("VNC Viewer")
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };
    // `vnc-viewer host[:port]` connects straight away.
    let host = std::env::args().nth(1);
    eframe::run_native(
        "VNC Viewer",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc, host)))),
    )
}
