// Release builds on Windows are GUI apps; without this a console window opens next to them.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod db;
#[cfg(target_os = "macos")]
mod menu;
mod ssh;
mod ui;
mod update;

/// QUARRY_NO_FOCUS keeps benchmark and screenshot runs from stealing focus from the user.
fn takes_focus() -> bool {
    std::env::var("QUARRY_NO_FOCUS").is_err()
}

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
            .with_active(takes_focus())
            // macOS: draw our own header under a transparent title bar, keeping the window buttons.
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false),
        ..Default::default()
    };
    eframe::run_native("Quarry", opts, Box::new(move |cc| Ok(Box::new(app::App::new(cc, shot)))))
}
