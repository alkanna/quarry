//! The macOS menu bar, built from the command registry (`quarry_core::cmd::CMDS`). A pick calls its command, except
//! where a view, an editor or a text field reads the key itself (`Scope::View` and `Scope::Native`): macOS hands a menu's
//! key equivalents to the menu instead of the window, so those picks replay the key as an egui event for them to get it.
//! That is also why Cut, Copy and Paste go through here.

use eframe::egui::{self, Key, Modifiers};
use muda::{AboutMetadata, Icon, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu, accelerator::Accelerator};
use quarry_core::cmd::{Cmd, CmdSpec, Menu as M, Scope, menu_items};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy)]
pub enum Act {
    Key(Modifiers, Key),
    Cut,
    Copy,
    Paste,
    Cmd(Cmd),
}

fn act(s: &CmdSpec) -> Act {
    match s.cmd {
        Cmd::Cut => Act::Cut,
        Cmd::Copy => Act::Copy,
        Cmd::Paste => Act::Paste,
        // Quit is the menu's own; it closes the window through eframe, so the app shuts down normally.
        Cmd::Quit => Act::Cmd(Cmd::Quit),
        c if matches!(s.scope, Scope::View | Scope::Native) => match s.keys.first().map(crate::app::shortcut) {
            Some(k) => Act::Key(k.modifiers, k.logical_key),
            None => Act::Cmd(c),
        },
        c => Act::Cmd(c),
    }
}

/// muda's spelling of a shortcut, for the label macOS draws next to the item.
fn accel(s: &CmdSpec) -> Option<Accelerator> {
    s.keys.first()?.text().parse().ok()
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
        let mut add = |sub: &Submenu, m: M, groups: std::ops::RangeInclusive<u8>| {
            for it in menu_items(m, groups) {
                match it {
                    Some(s) => {
                        let item = MenuItem::new(s.menu.map_or("", |p| p.title), true, accel(s));
                        acts.insert(item.id().clone(), act(s));
                        let _ = sub.append(&item);
                    }
                    None => {
                        let _ = sub.append(&PredefinedMenuItem::separator());
                    }
                }
            }
        };
        let all = 0..=u8::MAX;

        let app = Submenu::new("Quarry", true);
        let _ = app.append(&PredefinedMenuItem::about(Some("About Quarry"), Some(about())));
        add(&app, M::App, 0..=1);
        let _ = app.append_items(&[
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::services(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(Some("Hide Quarry")),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::show_all(None),
            &PredefinedMenuItem::separator(),
        ]);
        add(&app, M::App, 2..=2);

        let file = Submenu::new("File", true);
        add(&file, M::File, all.clone());

        let edit = Submenu::new("Edit", true);
        add(&edit, M::Edit, all.clone());

        // View items name the view of both engines in that position, since menu labels are fixed when the bar is built.
        let view = Submenu::new("View", true);
        add(&view, M::View, all.clone());
        let _ = view.append(&PredefinedMenuItem::separator());
        let _ = view.append(&PredefinedMenuItem::fullscreen(None));

        let query = Submenu::new("Query", true);
        add(&query, M::Query, all.clone());

        let window = Submenu::new("Window", true);
        let _ = window.append_items(&[&PredefinedMenuItem::minimize(None), &PredefinedMenuItem::maximize(Some("Zoom")), &PredefinedMenuItem::separator()]);
        add(&window, M::Window, all.clone());
        let _ = window.append(&PredefinedMenuItem::separator());
        let _ = window.append(&PredefinedMenuItem::bring_all_to_front(None));

        let help = Submenu::new("Help", true);
        add(&help, M::Help, all);

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

    /// Turns pending menu picks into input for this frame; returns the commands the app runs itself.
    pub fn feed(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) -> Vec<Cmd> {
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
                Act::Cmd(c) => rest.push(c),
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
