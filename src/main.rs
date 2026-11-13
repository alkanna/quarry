// Release builds on Windows are GUI apps; without this a console window opens next to them.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// Views and SQL builders take the state they draw as plain arguments; argument structs and type aliases for each
// would be the extra layers the codebase keeps out.
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

mod agents;
mod app;
mod cloudsql;
mod db;
#[cfg(target_os = "macos")]
mod menu;
mod ssh;
mod ui;
mod update;
mod vault;

/// QUARRY_NO_FOCUS runs benchmark and screenshot windows in the background: never the active app,
/// so they cannot take focus from whatever the user is doing.
fn takes_focus() -> bool {
    std::env::var("QUARRY_NO_FOCUS").is_err()
}

/// winit picks Wayland whenever WAYLAND_DISPLAY is set and fails outright if libwayland-client is
/// not installed, instead of trying X11. Dropping the variable makes it use XWayland.
#[cfg(target_os = "linux")]
fn fall_back_to_x11_without_wayland_lib() {
    if std::env::var_os("WAYLAND_DISPLAY").is_none_or(|v| v.is_empty()) {
        return;
    }
    // SAFETY: plain dlopen of a library name, closed again right away.
    let lib = unsafe { libc::dlopen(c"libwayland-client.so.0".as_ptr(), libc::RTLD_LAZY) };
    if lib.is_null() {
        // SAFETY: called first thing in main, before any thread exists, so nothing can race the environment.
        unsafe { std::env::remove_var("WAYLAND_DISPLAY") };
    } else {
        unsafe { libc::dlclose(lib) };
    }
}

fn main() -> eframe::Result {
    // `quarry mcp` is the MCP server an agent starts: stdio only, no window, nothing else of the app runs.
    if std::env::args().nth(1).as_deref() == Some("mcp") {
        std::process::exit(agents::bridge::run());
    }
    #[cfg(target_os = "linux")]
    fall_back_to_x11_without_wayland_lib();
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
            // QUARRY_TEST_TALL=1 gives screenshots room for long dialogs such as the whole Advanced tab.
            .with_inner_size([1400.0, if std::env::var("QUARRY_TEST_TALL").is_ok() { 1180.0 } else { 860.0 }])
            .with_min_inner_size([900.0, 560.0])
            .with_active(takes_focus())
            // macOS: draw our own header under a transparent title bar, keeping the window buttons.
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false),
        #[cfg(target_os = "macos")]
        event_loop_builder: (!takes_focus()).then(|| -> eframe::EventLoopBuilderHook {
            Box::new(|b| {
                use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
                b.with_activation_policy(ActivationPolicy::Accessory).with_activate_ignoring_other_apps(false);
            })
        }),
        ..Default::default()
    };
    let res = eframe::run_native("Quarry", opts, Box::new(move |cc| Ok(Box::new(app::App::new(cc, shot)))));
    // The raw winit error for a missing display library is unreadable, so say what to install.
    #[cfg(target_os = "linux")]
    if let Err(eframe::Error::WinitEventLoop(_) | eframe::Error::AppCreation(_)) = &res {
        if std::path::Path::new("/etc/NIXOS").exists() || std::env::var_os("NIX_STORE").is_some() {
            eprintln!("Quarry could not open a window. On NixOS the libraries are installed but not on the library path. Add wayland, libxkbcommon, libGL, vulkan-loader and gtk3 (file dialogs) to programs.nix-ld.libraries, or run with LD_LIBRARY_PATH pointing at them.");
        } else {
            eprintln!("Quarry could not open a window. Install libwayland-client0 (Wayland) or libx11-6, libxcursor1 and libxkbcommon-x11-0 (X11).");
        }
    }
    res
}
