//! The macOS menu bar. An item with a shortcut replays that shortcut as an egui key event, so
//! App::keys stays the one place that acts on it; the menu makes every shortcut discoverable and
//! clickable. macOS hands a menu's key equivalents to the menu instead of the window, which is
//! why even Cut, Copy and Paste go through here.

use eframe::egui::{self, Key, Modifiers};
use muda::{AboutMetadata, Icon, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu, accelerator::Accelerator};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy)]
pub enum Act {
    Key(Modifiers, Key),
    Cut,
    Copy,
    Paste,
    Quit,
    CheckUpdates,
    Export,
    Insert,
    Url(&'static str),
}

const CMD: Modifiers = Modifiers::COMMAND;
const CMD_SHIFT: Modifiers = Modifiers { shift: true, ..Modifiers::COMMAND };
const CMD_ALT: Modifiers = Modifiers { alt: true, ..Modifiers::COMMAND };

/// One custom menu entry; None is a separator.
type Item = Option<(&'static str, Act)>;

fn key(m: Modifiers, k: Key) -> Act {
    Act::Key(m, k)
}

/// muda's spelling of a shortcut, for the label macOS draws next to the item.
fn accel(a: &Act) -> Option<Accelerator> {
    let (m, k) = match *a {
        Act::Key(m, k) => (m, k),
        Act::Cut => (CMD, Key::X),
        Act::Copy => (CMD, Key::C),
        Act::Paste => (CMD, Key::V),
        Act::Quit => (CMD, Key::Q),
        _ => return None,
    };
    let name = match k {
        Key::Comma => ",".to_string(),
        Key::Slash => "/".into(),
        Key::OpenBracket => "[".into(),
        Key::CloseBracket => "]".into(),
        Key::Equals => "=".into(),
        Key::Minus => "-".into(),
        Key::Num0 => "0".into(),
        other => other.name().to_string(),
    };
    let mut s = String::from("Cmd+");
    if m.shift {
        s += "Shift+";
    }
    if m.alt {
        s += "Alt+";
    }
    (s + &name).parse().ok()
}

pub struct MenuBar {
    /// Kept alive for as long as the app runs; dropping it would empty the menu bar.
    _menu: Menu,
    pending: Arc<Mutex<Vec<Act>>>,
    /// Modifiers to put back the frame after a replayed shortcut set its own.
    restore: Option<Modifiers>,
}

impl MenuBar {
    pub fn new(ctx: &egui::Context) -> Self {
        let menu = Menu::new();
        let mut acts: HashMap<MenuId, Act> = HashMap::new();
        let mut add = |sub: &Submenu, items: &[Item]| {
            for it in items {
                match it {
                    Some((label, a)) => {
                        let item = MenuItem::new(*label, true, accel(a));
                        acts.insert(item.id().clone(), *a);
                        let _ = sub.append(&item);
                    }
                    None => {
                        let _ = sub.append(&PredefinedMenuItem::separator());
                    }
                }
            }
        };

        let app = Submenu::new("Quarry", true);
        let _ = app.append(&PredefinedMenuItem::about(Some("About Quarry"), Some(about())));
        add(&app, &[Some(("Check for Updates…", Act::CheckUpdates)), None, Some(("Settings…", key(CMD, Key::Comma))), None]);
        let _ = app.append_items(&[
            &PredefinedMenuItem::services(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(Some("Hide Quarry")),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::show_all(None),
            &PredefinedMenuItem::separator(),
        ]);
        // Our own Quit closes the window through eframe, so the app shuts down normally.
        add(&app, &[Some(("Quit Quarry", Act::Quit))]);

        let file = Submenu::new("File", true);
        add(&file, &[
            Some(("New Connection…", key(CMD, Key::N))),
            Some(("New Tab…", key(CMD, Key::T))),
            Some(("Open Collection…", key(CMD, Key::K))),
            Some(("Reopen Closed Tab", key(CMD_SHIFT, Key::T))),
            None,
            Some(("Save to Library", key(CMD, Key::S))),
            None,
            Some(("Insert Document…", Act::Insert)),
            Some(("Export…", Act::Export)),
            None,
            Some(("Close Tab", key(CMD, Key::W))),
        ]);

        let edit = Submenu::new("Edit", true);
        add(&edit, &[
            Some(("Undo", key(CMD, Key::Z))),
            Some(("Redo", key(CMD_SHIFT, Key::Z))),
            None,
            Some(("Cut", Act::Cut)),
            Some(("Copy", Act::Copy)),
            Some(("Paste", Act::Paste)),
            Some(("Select All", key(CMD, Key::A))),
            None,
            Some(("Find in Query", key(CMD, Key::F))),
            Some(("Find Collection", key(CMD_SHIFT, Key::F))),
        ]);

        let view = Submenu::new("View", true);
        add(&view, &[
            Some(("Documents", key(CMD_ALT, Key::Num1))),
            Some(("Aggregate", key(CMD_ALT, Key::Num2))),
            Some(("Indexes", key(CMD_ALT, Key::Num3))),
            Some(("Schema", key(CMD_ALT, Key::Num4))),
            Some(("Import", key(CMD_ALT, Key::Num5))),
            Some(("Compare", key(CMD_ALT, Key::Num6))),
            Some(("Shell", key(CMD_ALT, Key::Num7))),
            None,
            Some(("Query Log", key(CMD, Key::L))),
            None,
            Some(("Bigger Interface", key(CMD, Key::Equals))),
            Some(("Smaller Interface", key(CMD, Key::Minus))),
            Some(("Actual Size", key(CMD, Key::Num0))),
            None,
        ]);
        let _ = view.append(&PredefinedMenuItem::fullscreen(None));

        let query = Submenu::new("Query", true);
        add(&query, &[
            Some(("Run Pipeline or Script", key(CMD, Key::Enter))),
            Some(("Explain", key(CMD, Key::E))),
            None,
            Some(("Reload Tab", key(CMD, Key::R))),
            Some(("Refresh Connection", key(CMD_SHIFT, Key::R))),
        ]);

        let window = Submenu::new("Window", true);
        let _ = window.append_items(&[&PredefinedMenuItem::minimize(None), &PredefinedMenuItem::maximize(Some("Zoom")), &PredefinedMenuItem::separator()]);
        add(&window, &[
            Some(("Show Next Tab", key(CMD_SHIFT, Key::CloseBracket))),
            Some(("Show Previous Tab", key(CMD_SHIFT, Key::OpenBracket))),
            Some(("Back", key(CMD, Key::OpenBracket))),
            Some(("Forward", key(CMD, Key::CloseBracket))),
            None,
        ]);
        let _ = window.append(&PredefinedMenuItem::bring_all_to_front(None));

        let help = Submenu::new("Help", true);
        add(&help, &[
            Some(("Keyboard Shortcuts", key(CMD, Key::Slash))),
            None,
            Some(("Quarry on GitHub", Act::Url("https://github.com/alkanna/quarry"))),
            Some(("Release Notes", Act::Url("https://github.com/alkanna/quarry/releases"))),
            Some(("Report an Issue", Act::Url("https://github.com/alkanna/quarry/issues"))),
        ]);

        let _ = menu.append_items(&[&app, &file, &edit, &view, &query, &window, &help]);
        menu.init_for_nsapp();
        window.set_as_windows_menu_for_nsapp();
        help.set_as_help_menu_for_nsapp();

        let pending = Arc::new(Mutex::new(Vec::new()));
        let (p, ctx) = (pending.clone(), ctx.clone());
        // Menu clicks arrive outside egui's input, so they also wake the otherwise idle app.
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            if let Some(a) = acts.get(&e.id) {
                p.lock().unwrap().push(*a);
                ctx.request_repaint();
            }
        }));
        Self { _menu: menu, pending, restore: None }
    }

    /// Turns pending menu picks into input for this frame; returns the ones the app handles itself.
    pub fn feed(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) -> Vec<Act> {
        if let Some(m) = self.restore.take() {
            raw.events.push(egui::Event::ModifiersChanged(m));
        }
        let mut rest = Vec::new();
        for a in self.pending.lock().unwrap().drain(..) {
            match a {
                // Clicked with the mouse, the modifiers are not held; set them for one frame so
                // checks like `i.modifiers.command && key_pressed(Enter)` still match.
                Act::Key(m, key) => {
                    let held = ctx.input(|i| i.modifiers);
                    raw.events.push(egui::Event::ModifiersChanged(m));
                    raw.events.push(egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: m });
                    self.restore = Some(held);
                    ctx.request_repaint();
                }
                Act::Cut => raw.events.push(egui::Event::Cut),
                Act::Copy => raw.events.push(egui::Event::Copy),
                Act::Paste => {
                    if let Ok(text) = arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
                        raw.events.push(egui::Event::Paste(text));
                    }
                }
                other => rest.push(other),
            }
        }
        rest
    }
}

fn about() -> AboutMetadata {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png")).ok().and_then(|i| Icon::from_rgba(i.rgba, i.width, i.height).ok());
    AboutMetadata {
        name: Some("Quarry".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        short_version: Some(env!("CARGO_PKG_VERSION").into()),
        copyright: Some("Copyright © 2026 Khiman Louer. MIT License.".into()),
        credits: Some(format!("{}\n{}\nFonts by Inter, JetBrains Mono, Geist, Cascadia Mono, Fira Code and OpenDyslexic, under the SIL Open Font License.", env!("CARGO_PKG_DESCRIPTION"), env!("CARGO_PKG_REPOSITORY"))),
        icon,
        ..Default::default()
    }
}
