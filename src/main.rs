// Release builds on Windows are GUI apps; without this a console window opens next to them.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod db;
mod ssh;
mod update;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    // `--screenshot out.png`: draw ~120 frames of an offline demo, save the picture and exit.
    // The demo never touches the saved connections or the keychain.
    let shot = args.iter().position(|a| a == "--screenshot").and_then(|i| args.get(i + 1)).map(std::path::PathBuf::from);
    let mut viewport = eframe::egui::ViewportBuilder::default();
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png")) {
        viewport = viewport.with_icon(icon);
    }
    let opts = eframe::NativeOptions {
        viewport: viewport
            .with_title("Quarry")
            .with_inner_size([1400.0, 860.0])
            .with_min_inner_size([900.0, 560.0])
            // macOS: draw our own header under a transparent title bar, keeping the window buttons.
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false),
        ..Default::default()
    };
    eframe::run_native("Quarry", opts, Box::new(move |cc| Ok(Box::new(app::App::new(cc, shot)))))
}
