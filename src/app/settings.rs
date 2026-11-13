//! The settings dialog: categories on the left, the chosen one's settings on the right.

use super::*;

#[derive(Clone, Copy, PartialEq, Default)]
pub(super) enum SettingsCat {
    #[default]
    Appearance,
    Reading,
    Documents,
    Editing,
    Tools,
    StatusBar,
    Updates,
}

const CATS: [(SettingsCat, &str); 7] = [
    (SettingsCat::Appearance, "Appearance"),
    (SettingsCat::Reading, "Reading"),
    (SettingsCat::Documents, "Documents"),
    (SettingsCat::Editing, "Editing"),
    (SettingsCat::Tools, "Queries and tools"),
    (SettingsCat::StatusBar, "Status bar"),
    (SettingsCat::Updates, "Updates"),
];

impl SettingsCat {
    /// The category whose name starts with this word, for the QUARRY_TEST_SETTINGS switch.
    pub(super) fn named(word: &str) -> Self {
        let w = word.to_lowercase();
        CATS.iter().find(|(_, n)| n.to_lowercase().split(' ').any(|p| p == w)).map_or_else(Self::default, |c| c.0)
    }
}

const SETTINGS_W: f32 = 780.0;
const SETTINGS_SIDE_W: f32 = 188.0;

impl App {
    /// Opens Settings on the category that holds the Bun and database tool paths.
    pub(super) fn open_tool_settings(&mut self) {
        self.settings_cat = SettingsCat::Tools;
        self.modal = Some(Modal::Settings);
    }

    /// Cmd+=, Cmd+- and Cmd+0 step through SCALES; they work over dialogs so Settings can stay open.
    pub(super) fn zoom_keys(&mut self, ctx: &egui::Context) {
        let cmd = egui::Modifiers::COMMAND;
        let (up, down, reset) = ctx.input_mut(|i| (i.consume_key(cmd, Key::Equals) || i.consume_key(cmd, Key::Plus), i.consume_key(cmd, Key::Minus), i.consume_key(cmd, Key::Num0)));
        let at = SCALES.iter().position(|s| s.0 == self.prefs.scale()).unwrap_or(0);
        let to = match (up, down, reset) {
            (_, _, true) => 0,
            (true, _, _) => (at + 1).min(SCALES.len() - 1),
            (_, true, _) => at.saturating_sub(1),
            _ => return,
        };
        if SCALES[to].0 != self.prefs.scale() {
            self.prefs.ui_scale = SCALES[to].0;
            style(ctx, self.prefs.look());
            self.save_prefs();
        }
    }

    pub(super) fn settings(&mut self, ctx: &egui::Context) {
        let mut keep = true;
        let look = self.prefs.look();
        // Settings that save when changed. Tuples compare up to 12 items, so small ones are grouped.
        let saved = |p: &UiPrefs| (p.open_inline, p.auto_save, p.always_writable, p.confirm_save, p.default_cols, p.timeout_ms, p.shell_runtime.clone(), p.mongodump.clone(), p.mongorestore.clone(), p.auto_update, (p.show_server, p.show_fps, p.show_mem, p.show_cpu, p.object_preview), (p.keep_toasts, p.appearance, p.gcloud.clone(), p.cloud_sql_proxy.clone(), p.value_panel, p.lock_scroll_axis));
        let before = saved(&self.prefs);
        let r = egui::Modal::new(egui::Id::unique(("quarry-layer", "modal"))).frame(dialog_frame()).show(ctx, |ui| {
            let screen = ui.ctx().content_rect();
            let w = SETTINGS_W.min(screen.width() - 40.0);
            let body_h = (screen.height() - 220.0).clamp(240.0, 480.0);
            ui.set_width(w);
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::Frame::new().inner_margin(egui::Margin { left: 20, right: 20, top: 14, bottom: 12 }).show(ui, |ui| {
                ui.set_width(w - 40.0);
                dialog_title(ui, "Settings");
            });
            ui.painter().hline(egui::Rangef::new(ui.min_rect().left(), ui.min_rect().left() + w), ui.cursor().top(), Stroke::new(1.0, LINE()));
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                // Wide fonts such as OpenDyslexic need a wider list than Inter does.
                let widest = CATS.iter().map(|(_, l)| ui.painter().layout_job(egui::text::LayoutJob::single_section(l.to_string(), ui_fmt(TXT_BODY, TEXT(), W_MEDIUM, 0.0))).size().x).fold(0.0, f32::max);
                let side_w = SETTINGS_SIDE_W.max(widest + 60.0);
                let (side, _) = ui.allocate_exact_size(egui::vec2(side_w, body_h), Sense::hover());
                ui.painter().rect_filled(side, 0, PANEL());
                ui.painter().vline(side.right(), side.y_range(), Stroke::new(1.0, LINE()));
                let mut rows = ui.new_child(egui::UiBuilder::new().max_rect(side.shrink2(egui::vec2(10.0, 12.0))).layout(egui::Layout::top_down(egui::Align::Min)));
                rows.spacing_mut().item_spacing.y = 2.0;
                for (cat, label) in CATS {
                    let on = self.settings_cat == cat;
                    let resp = side_row(&mut rows, ROW_H, 0.0, on, label, |p, r| {
                        let c = r.left_center() + egui::vec2(7.0, 0.0);
                        cat_icon(p, cat, c, if on { PRIMARY() } else { MUTED() });
                        let g = p.layout_job(egui::text::LayoutJob::single_section(label.to_string(), ui_fmt(TXT_BODY, if on { BRIGHT() } else { TEXT() }, if on { W_MEDIUM } else { 400.0 }, 0.0)));
                        let dy = optical_dy(&g);
                        p.galley(egui::pos2(r.left() + 24.0, r.center().y - g.size().y / 2.0 + dy), g, TEXT());
                    });
                    if resp.clicked() {
                        self.settings_cat = cat;
                    }
                }
                ui.allocate_ui_with_layout(egui::vec2(w - side_w, body_h), egui::Layout::top_down(egui::Align::Min), |ui| {
                    egui::ScrollArea::vertical().id_salt("settings-scroll").max_height(body_h).min_scrolled_height(body_h).auto_shrink([false, false]).show(ui, |ui| {
                        egui::Frame::new().inner_margin(egui::Margin { left: 24, right: 24, top: 18, bottom: 12 }).show(ui, |ui| {
                            ui.set_width(w - side_w - 48.0);
                            ui.spacing_mut().item_spacing.y = 6.0;
                            let title = CATS.iter().find(|c| c.0 == self.settings_cat).map_or("", |c| c.1);
                            ui.label(semibold(RichText::new(title).size(TXT_SUBHEAD).color(BRIGHT())));
                            ui.add_space(6.0);
                            self.settings_pane(ui);
                        });
                    });
                });
            });
            ui.painter().hline(egui::Rangef::new(ui.min_rect().left(), ui.min_rect().left() + w), ui.cursor().top(), Stroke::new(1.0, LINE()));
            egui::Frame::new().inner_margin(egui::Margin::symmetric(20, 12)).show(ui, |ui| {
                ui.set_width(w - 40.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Changes are saved right away.").size(TXT_SMALL).color(FAINT()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Done").on_hover_text("Esc").clicked() {
                            keep = false;
                        }
                    });
                });
            });
        });
        // The pref unlocks every connection each frame, so turning it off has to lock them back.
        if before.2 && !self.prefs.always_writable {
            self.conns.iter_mut().for_each(|c| c.writable = false);
        }
        if self.prefs.look() != look {
            style(ctx, self.prefs.look());
            // Column widths were measured in the old font and number display.
            self.width_cache.clear();
        }
        if before != saved(&self.prefs) || self.prefs.look() != look {
            self.save_prefs();
        }
        if !keep || r.should_close() {
            self.modal = None;
        }
    }

    fn settings_pane(&mut self, ui: &mut egui::Ui) {
        match self.settings_cat {
            SettingsCat::Appearance => {
                settings_section(ui, "THEME", |ui| {
                    setting_row(ui, "Mode", "System follows the light or dark setting of your computer.", |ui| {
                        let was = self.prefs.appearance;
                        segmented(ui, &mut self.prefs.appearance, &[(Appearance::System, "System"), (Appearance::Light, "Light"), (Appearance::Dark, "Dark")]);
                        if self.prefs.appearance != was {
                            self.prefs.appearance.apply(ui.ctx());
                        }
                    });
                    let look = self.prefs.look();
                    for light in [false, true] {
                        ui.separator();
                        let note = if self.prefs.appearance == Appearance::System {
                            if light { "used when the system is light" } else { "used when the system is dark" }
                        } else if is_light() == light {
                            "in use"
                        } else {
                            ""
                        };
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            ui.label(medium(RichText::new(if light { "Light theme" } else { "Dark theme" }).size(TXT_TITLE).color(BRIGHT())));
                            ui.label(RichText::new(note).size(TXT_SMALL).color(FAINT()));
                        });
                        if let Some(i) = theme_cards(ui, light, &look) {
                            *(if light { &mut self.prefs.light_theme } else { &mut self.prefs.dark_theme }) = themes(light)[i].name.into();
                        }
                    }
                    ui.separator();
                    setting_row(ui, "Background", "Soft lowers the contrast between text and background, for less glare.", |ui| {
                        segmented(ui, &mut self.prefs.soft, &[(false, "Standard"), (true, "Soft")]);
                    });
                    ui.separator();
                    setting_row(ui, "Accent", "Each theme has its own. Override it for every theme.", |ui| {
                        if let Some(i) = accent_swatches(ui, &look) {
                            self.prefs.accent = accent_name(i).into();
                        }
                    });
                });
                settings_section(ui, "TEXT", |ui| {
                    setting_row(ui, "Interface size", "Makes text, buttons and rows bigger together. Also Cmd+= and Cmd+- anywhere.", |ui| {
                        segmented(ui, &mut self.prefs.ui_scale, &SCALES);
                    });
                    ui.separator();
                    let look = self.prefs.look();
                    setting_row(ui, "Interface font", "OpenDyslexic has heavier letter bottoms that some people with dyslexia find easier to read.", |ui| {
                        if let Some(i) = font_combo(ui, "ui-font", &UI_FONTS, look.ui_font) {
                            self.prefs.ui_font = UI_FONTS[i].0.into();
                        }
                    });
                    ui.separator();
                    setting_row(ui, "Data font", "For values, queries and the Shell.", |ui| {
                        if let Some(i) = font_combo(ui, "data-font", &DATA_FONTS, look.data_font) {
                            self.prefs.data_font = DATA_FONTS[i].0.into();
                        }
                    });
                });
            }
            SettingsCat::Reading => {
                settings_section(ui, "TABLES", |ui| {
                    setting_row(ui, "Reading line", "Highlights the whole row under the pointer, so your eye keeps its place across wide tables.", |ui| {
                        toggle(ui, &mut self.prefs.reading_line);
                    });
                    ui.separator();
                    setting_row(ui, "Group digits", "Shows 1 234 567 instead of 1234567. Only the display changes: copy, edit and export keep the raw value.", |ui| {
                        toggle(ui, &mut self.prefs.group_digits);
                    });
                    ui.separator();
                    setting_row(ui, "Scroll straight", "A swipe that starts down or across stays on that line. Diagonal swipes move freely.", |ui| {
                        toggle(ui, &mut self.prefs.lock_scroll_axis);
                    });
                });
                settings_section(ui, "MESSAGES", |ui| {
                    setting_row(ui, "Keep messages until closed", "Warnings and confirmations stay on screen until you close them, instead of fading after a few seconds. An Undo offer still ends on its timer.", |ui| {
                        toggle(ui, &mut self.prefs.keep_toasts);
                    });
                });
            }
            SettingsCat::Documents => {
                settings_section(ui, "", |ui| {
                    setting_row(ui, "Open documents", "Where a clicked row shows its document.", |ui| {
                        segmented(ui, &mut self.prefs.open_inline, &[(false, "Side panel"), (true, "Under the row")]);
                    });
                    ui.separator();
                    setting_row(ui, "Columns shown", "How many fields the table shows before you pick your own.", |ui| {
                        stepper(ui, &mut self.prefs.default_cols, 3, 30);
                    });
                    ui.separator();
                    setting_row(ui, "Big objects in columns", "Objects and arrays too long for a cell show their size, or their first characters as in Mingo. Small ones always show in full.", |ui| {
                        segmented(ui, &mut self.prefs.object_preview, &[(false, "Size"), (true, "First characters")]);
                    });
                });
            }
            SettingsCat::Editing => {
                settings_section(ui, "", |ui| {
                    setting_row(ui, "Auto-save edits", "Write each edit right away and offer Undo for 8 seconds.", |ui| {
                        toggle(ui, &mut self.prefs.auto_save);
                    });
                    ui.separator();
                    setting_row(ui, "Always allow writes", "Skip read-only mode on every connection and save edits without showing the changes first.", |ui| {
                        toggle(ui, &mut self.prefs.always_writable);
                    });
                    ui.separator();
                    setting_row(ui, "Long values", "Where Shift+Enter or the button at the end of an inline editor opens a value. A value with line breaks opens there directly.", |ui| {
                        segmented(ui, &mut self.prefs.value_panel, &[(false, "Dialog"), (true, "Side panel")]);
                    });
                    ui.separator();
                    let auto = self.prefs.auto_save || self.prefs.always_writable;
                    setting_row(ui, "Show changes before saving", "List every changed field before writing unsaved edits.", |ui| {
                        ui.add_enabled_ui(!auto, |ui| toggle(ui, &mut self.prefs.confirm_save));
                    });
                });
                ui.label(RichText::new(if self.prefs.always_writable { "A save never overwrites someone else's change." } else { "Writes always need the connection unlocked, and a save never overwrites someone else's change." }).size(TXT_SMALL).color(FAINT()));
            }
            SettingsCat::Tools => {
                settings_section(ui, "QUERIES", |ui| {
                    setting_row(ui, "Query timeout", "How long a query may run before it is stopped.", |ui| {
                        egui::ComboBox::from_id_salt("timeout").width(90.0).selected_text(format!("{} s", self.prefs.timeout_ms / 1000)).show_ui(ui, |ui| {
                            menu_look(ui);
                            // Same presets as the original's preferences.
                            for s in [5, 10, 30, 60, 300, 3600] {
                                ui.selectable_value(&mut self.prefs.timeout_ms, s * 1000, format!("{s} s"));
                            }
                        });
                    });
                });
                settings_section(ui, "SHELL", |ui| {
                    let desc = match find_bun("").map(|p| p.display().to_string()) {
                        Some(p) => format!("Found automatically at {p}. Set a path only to use another Bun."),
                        None => "Bun was not found. Install it from bun.sh or enter its path.".to_string(),
                    };
                    setting_row(ui, "Bun runtime", &desc, |ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.prefs.shell_runtime).hint_text("automatic").desired_width(190.0));
                    });
                });
                let desc = self.gcloud_found_text();
                settings_section(ui, "CLOUD SQL", |ui| {
                    setting_row(ui, "gcloud", &desc, |ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.prefs.gcloud).hint_text("automatic").desired_width(190.0));
                    });
                    ui.separator();
                    setting_row(ui, "Cloud SQL proxy", "Found automatically, or downloaded by Quarry. Set a path only to use another build.", |ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.prefs.cloud_sql_proxy).hint_text("automatic").desired_width(190.0));
                    });
                });
                settings_section(ui, "DATABASE TOOLS", |ui| {
                    for (i, (name, path)) in [("mongodump", &mut self.prefs.mongodump), ("mongorestore", &mut self.prefs.mongorestore)].into_iter().enumerate() {
                        if i > 0 {
                            ui.separator();
                        }
                        let desc = match find_exe(name, "") {
                            Some(p) => format!("Found automatically at {}.", p.display()),
                            None => "Not found. Install the MongoDB Database Tools or enter its path.".to_string(),
                        };
                        setting_row(ui, name, &desc, |ui| {
                            ui.add(egui::TextEdit::singleline(path).hint_text("automatic").desired_width(190.0));
                        });
                    }
                });
                if find_exe("mongodump", "").is_none() || find_exe("mongorestore", "").is_none() {
                    self.install_tools_button(ui);
                }
            }
            SettingsCat::StatusBar => {
                settings_section(ui, "", |ui| {
                    setting_row(ui, "Server", "The connection's host, MongoDB version and replica set, with a dot for whether it is connected.", |ui| {
                        toggle(ui, &mut self.prefs.show_server);
                    });
                    ui.separator();
                    setting_row(ui, "Frame rate", "Frames per second and the time each frame takes. Quarry follows your display's refresh rate (vsync), up to 120 Hz on ProMotion screens.", |ui| {
                        toggle(ui, &mut self.prefs.show_fps);
                    });
                    ui.separator();
                    setting_row(ui, "Memory", "How much memory Quarry uses. Same number as Activity Monitor's Memory column on macOS, resident memory elsewhere.", |ui| {
                        toggle(ui, &mut self.prefs.show_mem);
                    });
                    ui.separator();
                    setting_row(ui, "CPU", "Quarry's CPU use over the last second, in percent of one core like Activity Monitor.", |ui| {
                        toggle(ui, &mut self.prefs.show_cpu);
                    });
                });
            }
            SettingsCat::Updates => {
                settings_section(ui, "", |ui| {
                    let desc = if self.update_note.is_empty() { format!("Version {}. New releases of {} are installed in the background, then Quarry restarts when idle with nothing unsaved.", env!("CARGO_PKG_VERSION"), crate::update::REPO) } else { format!("Version {}. {}", env!("CARGO_PKG_VERSION"), self.update_note) };
                    setting_row(ui, "Update automatically", &desc, |ui| {
                        toggle(ui, &mut self.prefs.auto_update);
                    });
                });
                if !self.demo && !cfg!(debug_assertions) && self.updated.is_none() && ui.button("Check now").clicked() {
                    self.check_update();
                }
            }
        }
    }
}

/// The 14 px line icon of a Settings category, painted as shapes so it sits centered.
fn cat_icon(p: &egui::Painter, cat: SettingsCat, c: egui::Pos2, color: Color32) {
    let s = Stroke::new(1.3, color);
    let pt = |x: f32, y: f32| c + egui::vec2(x, y);
    let path = |pts: Vec<egui::Pos2>| p.add(egui::Shape::closed_line(pts, s));
    let line = |a: egui::Pos2, b: egui::Pos2| p.line_segment([a, b], s);
    match cat {
        SettingsCat::Appearance => {
            p.circle_stroke(c, 6.0, s);
            let half: Vec<_> = (0..=12).map(|i| std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / 12.0).map(|a| pt(6.0 * a.cos(), -6.0 * a.sin())).collect();
            p.add(egui::Shape::convex_polygon(half, color, Stroke::NONE));
        }
        SettingsCat::Reading => {
            path(vec![pt(0.0, -4.0), pt(-3.5, -5.5), pt(-7.0, -5.0), pt(-7.0, 5.0), pt(-3.5, 4.5), pt(0.0, 6.0), pt(3.5, 4.5), pt(7.0, 5.0), pt(7.0, -5.0), pt(3.5, -5.5)]);
            line(pt(0.0, -4.0), pt(0.0, 6.0));
        }
        SettingsCat::Documents => {
            path(vec![pt(-5.0, -6.0), pt(2.0, -6.0), pt(5.0, -3.0), pt(5.0, 6.0), pt(-5.0, 6.0)]);
            line(pt(-2.5, -1.0), pt(2.5, -1.0));
            line(pt(-2.5, 2.0), pt(2.5, 2.0));
        }
        SettingsCat::Editing => {
            path(vec![pt(-6.0, 6.0), pt(-5.0, 2.0), pt(2.0, -5.0), pt(5.0, -2.0), pt(-2.0, 5.0)]);
            line(pt(0.5, -3.5), pt(3.5, -0.5));
        }
        SettingsCat::Tools => {
            p.rect_stroke(egui::Rect::from_center_size(c, egui::vec2(14.0, 12.0)), 2.5, s, egui::StrokeKind::Inside);
            p.add(egui::Shape::line(vec![pt(-4.0, -2.0), pt(-1.5, 0.0), pt(-4.0, 2.0)], s));
            line(pt(0.0, 2.5), pt(4.0, 2.5));
        }
        SettingsCat::StatusBar => {
            p.rect_stroke(egui::Rect::from_center_size(c, egui::vec2(14.0, 12.0)), 2.5, s, egui::StrokeKind::Inside);
            line(pt(-6.0, 2.5), pt(6.0, 2.5));
        }
        SettingsCat::Updates => {
            let arc: Vec<_> = (0..=20).map(|i| -0.9 + 5.4 * i as f32 / 20.0).map(|a| pt(5.5 * a.cos(), 5.5 * a.sin())).collect();
            p.add(egui::Shape::line(arc, s));
            p.add(egui::Shape::line(vec![pt(4.0, -7.5), pt(4.0, -4.0), pt(0.5, -4.0)], s));
        }
    }
}

/// A dropdown of font names; returns the index picked this frame.
fn font_combo(ui: &mut egui::Ui, id: &str, list: &[(&str, &[u8])], cur: usize) -> Option<usize> {
    let mut picked = None;
    egui::ComboBox::from_id_salt(id).width(150.0).selected_text(list[cur].0).show_ui(ui, |ui| {
        menu_look(ui);
        for (i, f) in list.iter().enumerate() {
            if ui.selectable_label(i == cur, f.0).clicked() {
                picked = Some(i);
            }
        }
    });
    picked.filter(|&i| i != cur)
}

/// One card per theme of the kind, each a miniature window in that theme's own colors. Returns the index clicked.
fn theme_cards(ui: &mut egui::Ui, light: bool, look: &Look) -> Option<usize> {
    let sel = if light { look.light_theme } else { look.dark_theme };
    let gap = PICK_GAP + 4.0;
    // Five to a row when they fit; on a narrow pane they keep their minimum width and wrap.
    let w = ((ui.available_width() - 4.0 * gap) / 5.0).clamp(THEME_CARD_MIN_W, THEME_CARD_MAX_W);
    let mut picked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
        for (i, t) in themes(light).iter().enumerate() {
            let (rect, r) = ui.allocate_exact_size(egui::vec2(w, THEME_THUMB_H + THEME_LABEL_H), Sense::click());
            let on = i == sel;
            r.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, true, on, t.name));
            if r.clicked() && !on {
                picked = Some(i);
            }
            let p = ui.painter();
            // Inset so the selection ring fits inside the card.
            let th = egui::Rect::from_min_size(rect.min + egui::vec2(3.0, 3.0), egui::vec2(w - 6.0, THEME_THUMB_H - 6.0));
            let ring = th.expand(3.0);
            if let Some(s) = pick_ring(on, &r, PRIMARY()) {
                p.rect_stroke(ring, R_MD + 3, s, egui::StrokeKind::Inside);
            }
            focus_ring(ui, &r, ring, (R_MD + 3) as f32);
            // With Soft and the accent override, so a card shows what picking it gives.
            paint_thumb(p, th, palette_of(light, i, look.soft, look.accent));
            if on {
                let c = egui::pos2(th.right() - 2.0, th.top() + 2.0);
                p.circle_filled(c, THEME_BADGE_R, PRIMARY());
                paint_check(p, c, ON_PRIMARY());
            }
            let g = p.layout_job(egui::text::LayoutJob::single_section(t.name.to_string(), ui_fmt(TXT_SMALL, if on { BRIGHT() } else { TEXT() }, if on { W_MEDIUM } else { 400.0 }, 0.0)));
            p.galley(egui::pos2(th.left(), rect.bottom() - g.size().y - 2.0), g, TEXT());
        }
    });
    picked
}

/// The theme's own accent, captioned "Theme", then each of ACCENTS as this theme draws it. Returns the index clicked.
fn accent_swatches(ui: &mut egui::Ui, look: &Look) -> Option<usize> {
    let light = is_light();
    let theme = if light { look.light_theme } else { look.dark_theme };
    let mut picked = None;
    // A left-to-right box inside the right-to-left control slot, so Tab moves through the swatches in reading order.
    let size = egui::vec2(ACCENT_CELL * (ACCENTS.len() + 1) as f32, ACCENT_CELL + TXT_CAPTION + 4.0);
    ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Min), |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for i in 0..=ACCENTS.len() {
            let (rect, r) = ui.allocate_exact_size(egui::vec2(ACCENT_CELL, size.y), Sense::click());
            let on = i == look.accent;
            let name = accent_name(i);
            r.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, true, on, name));
            let r = r.on_hover_text(if i == 0 { "This theme's own accent" } else { name });
            if r.clicked() && !on {
                picked = Some(i);
            }
            // As this theme draws it, which can be a shade lighter or darker than the one listed.
            let color = palette_of(light, theme, look.soft, i).primary;
            let c = egui::pos2(rect.center().x, rect.top() + ACCENT_CELL / 2.0);
            let p = ui.painter();
            p.circle_filled(c, ACCENT_DOT_R, color);
            if let Some(s) = pick_ring(on, &r, color) {
                p.circle_stroke(c, ACCENT_DOT_R + 3.0, s);
            }
            focus_ring(ui, &r, egui::Rect::from_center_size(c, egui::Vec2::splat(2.0 * ACCENT_DOT_R + 6.0)), ACCENT_DOT_R + 3.0);
            if i == 0 {
                // Left-aligned, so a wide caption runs on under the next swatch, which has none.
                p.text(egui::pos2(rect.left() + 2.0, rect.bottom()), egui::Align2::LEFT_BOTTOM, name, FontId::proportional(TXT_CAPTION), FAINT());
            }
        }
    });
    picked
}

/// Title bar, sidebar with a selected row, three typed values per row and an accent button. The miniature is
/// laid out at 78 px wide and its x positions scale with the card, so nothing spills over at any width.
fn paint_thumb(p: &egui::Painter, r: egui::Rect, t: &Palette) {
    let k = r.width() / 78.0;
    let at = |x: f32, y: f32, w: f32, h: f32| egui::Rect::from_min_size(r.min + egui::vec2(x * k, y), egui::vec2(w * k, h));
    let bar = |x: f32, y: f32, w: f32, h: f32, c: Color32| p.rect_filled(at(x, y, w, h), R_SM, c);
    let title_h = 9.0;
    p.rect_filled(r, R_MD, t.bg);
    p.rect_filled(at(0.0, 0.0, 78.0, title_h), egui::CornerRadius { nw: R_MD, ne: R_MD, sw: 0, se: 0 }, t.chrome);
    p.rect_filled(at(0.0, title_h, 24.0, r.height() - title_h), egui::CornerRadius { sw: R_MD, ..Default::default() }, t.panel);
    bar(4.0, 15.0, 16.0, 4.0, t.sel);
    bar(4.0, 23.0, 12.0, 3.0, t.faint);
    bar(4.0, 30.0, 14.0, 3.0, t.faint);
    for (j, c) in [t.c_string, t.c_number, t.c_date].into_iter().enumerate() {
        let y = 15.0 + j as f32 * 10.0;
        if j == 1 {
            p.rect_filled(at(24.0, y - 3.0, 54.0, 10.0), 0, t.sel);
        }
        bar(29.0, y, 14.0, 4.0, t.c_oid);
        bar(46.0, y, 16.0, 4.0, c);
        bar(65.0, y, 8.0, 4.0, tint(t.text, 150));
    }
    bar(56.0, r.height() - 12.0, 18.0, 7.0, t.primary);
    p.rect_stroke(r, R_MD, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
}
