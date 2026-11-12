//! The Advanced tab of the connection form on PostgreSQL: the search_path list, the session settings (rows from an
//! allow-list, added through a picker) and the connection timeouts. What is allowed and how values are written is in
//! `db::sql::pg_advanced`; this file only draws it and edits the form's copy.

use super::conn_form::{note_row, secs_field};
use super::*;
use db::sql::pg_advanced as adv;

/// Width of the setting name column and of the value box.
const NAME_W: f32 = 230.0;
const VALUE_W: f32 = 230.0;

/// A small painted arrow that moves a list row; the glyphs of a fallback font sit off center.
fn arrow(ui: &mut egui::Ui, up: bool, enabled: bool, label: &str) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(CLOSE_W, CLOSE_W), if enabled { Sense::click() } else { Sense::hover() });
    let state = if enabled { state_of(&r) } else { 0 };
    ui.painter().circle_filled(rect.center(), CLOSE_W / 2.0 - 1.0, hover_fill_on(ui, state));
    let color = if !enabled { tint(FAINT(), 90) } else if state > 0 { BRIGHT() } else { FAINT() };
    let (c, d) = (rect.center(), 3.5);
    let s = if up { -1.0 } else { 1.0 };
    ui.painter().add(egui::Shape::line(vec![c + egui::vec2(-d, 1.5 * -s), c + egui::vec2(0.0, 1.5 * s), c + egui::vec2(d, 1.5 * -s)], Stroke::new(1.3, color)));
    focus_ring(ui, &r, rect, CLOSE_W / 2.0);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    if enabled { r.on_hover_cursor(egui::CursorIcon::PointingHand) } else { r }
}

fn remove_x(ui: &mut egui::Ui, label: &str) -> bool {
    let r = close_x(ui);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    r.clicked()
}

fn small(ui: &mut egui::Ui, text: &str, color: Color32) {
    ui.add(egui::Label::new(RichText::new(text).size(TXT_SMALL).color(color)).wrap());
}

fn error_line(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(NAME_W + 8.0);
        let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), Sense::hover());
        ui.painter().circle_filled(r.center(), 5.0, ALERT());
        let (c, d) = (r.center(), 1.8);
        for (a, b) in [(egui::vec2(-d, -d), egui::vec2(d, d)), (egui::vec2(-d, d), egui::vec2(d, -d))] {
            ui.painter().line_segment([c + a, c + b], Stroke::new(1.1, ON_PRIMARY()));
        }
        small(ui, text, ALERT());
    });
}

/// What the form says about one row: its own shape check, or what the server answered on the last Test for this value.
fn row_error(f: &ConnForm, check: &RowCheck) -> Option<String> {
    check.error.clone().or_else(|| {
        let v = check.value.trim();
        f.pg_refused.iter().find(|(n, tested, _)| *n == check.name && tested == v).map(|(_, _, m)| m.clone())
    })
}

/// The check of row `i`, made again only when its name or value is not the one the last check saw.
fn check_row(f: &mut ConnForm, i: usize) {
    let (name, value) = &f.pg_rows[i];
    if f.pg_checks.get(i).is_some_and(|c| c.name == *name && c.value == *value) {
        return;
    }
    let v = value.trim();
    let error = if v.is_empty() { None } else { adv::check_value(name, v).err() };
    let check = RowCheck { name: name.clone(), value: value.clone(), error, def: adv::def(name) };
    f.pg_checks.resize_with(f.pg_rows.len().max(f.pg_checks.len()), || RowCheck { name: String::new(), value: String::new(), error: None, def: None });
    f.pg_checks[i] = check;
}

/// The allow-list and the kept list lowercased once, in the order of `adv::ALLOWED` and `adv::KEPT`, for the picker's filter.
fn lowered() -> &'static (Vec<[String; 3]>, Vec<[String; 2]>) {
    static LOWER: std::sync::OnceLock<(Vec<[String; 3]>, Vec<[String; 2]>)> = std::sync::OnceLock::new();
    LOWER.get_or_init(|| (adv::ALLOWED.iter().map(|d| [d.name.to_ascii_lowercase(), d.desc.to_ascii_lowercase(), d.group.label().to_ascii_lowercase()]).collect(), adv::KEPT.iter().map(|(n, why)| [n.to_ascii_lowercase(), why.to_ascii_lowercase()]).collect()))
}

fn search_path(ui: &mut egui::Ui, f: &mut ConnForm) {
    ui.horizontal(|ui| {
        caption(ui, "SEARCH PATH");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| small(ui, "First match wins", FAINT()));
    });
    ui.add_space(2.0);
    let mut act: Option<(usize, i32)> = None;
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.horizontal(|ui| {
            ui.label(RichText::new("1").size(TXT_SMALL).color(FAINT()));
            ui.label(RichText::new("Schema picked in the sidebar").color(MUTED()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| small(ui, "always first", FAINT()));
        });
        let n = f.pg_path.len();
        for i in 0..n {
            let name = f.pg_path[i].clone();
            ui.horizontal(|ui| {
                grip(ui);
                ui.label(RichText::new((i + 2).to_string()).size(TXT_SMALL).color(FAINT()));
                ui.label(RichText::new(&name).monospace().color(BRIGHT()));
                // A schema the last Test did not find in the default database is only a warning: another database may have it.
                if f.pg_tested && !f.pg_schemas.is_empty() && name != "$user" && !f.pg_schemas.contains(&name) {
                    ui.label(RichText::new("Not found in the default database on the last Test").size(TXT_SMALL).color(WARNING()));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if remove_x(ui, &format!("Remove {name}")) {
                        act = Some((i, 0));
                    }
                    if arrow(ui, false, i + 1 < n, &format!("Move {name} down")).clicked() {
                        act = Some((i, 1));
                    }
                    if arrow(ui, true, i > 0, &format!("Move {name} up")).clicked() {
                        act = Some((i, -1));
                    }
                });
            });
        }
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let known: Vec<String> = f.pg_schemas.iter().filter(|s| !f.pg_path.contains(s)).cloned().collect();
            if !known.is_empty() {
                egui::ComboBox::from_id_salt("pg-add-schema").width(150.0).selected_text("Add schema").show_ui(ui, |ui| {
                    menu_look(ui);
                    for s in known {
                        if ui.selectable_label(false, &s).clicked() {
                            f.pg_path.push(s);
                        }
                    }
                });
            }
            let r = ui.add(egui::TextEdit::singleline(&mut f.pg_schema_text).hint_text(if f.pg_schemas.is_empty() { "Schema name, Test lists the schemas" } else { "or type a name" }).font(egui::TextStyle::Monospace).desired_width(190.0).margin(FIELD_MARGIN));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Schema name"));
            let typed = f.pg_schema_text.trim().to_string();
            let go = ui.add_enabled(!typed.is_empty(), egui::Button::new("Add")).clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !typed.is_empty());
            if go {
                if !f.pg_path.contains(&typed) {
                    f.pg_path.push(typed);
                }
                f.pg_schema_text.clear();
            }
        });
        // On its own line: beside the Add button it overlapped it in a narrow form and took its clicks.
        if ui.add(egui::Button::new(RichText::new("Reset to \"$user\", public").size(TXT_SMALL).color(PRIMARY())).frame(false)).clicked() {
            f.pg_path = vec!["$user".into(), "public".into()];
        }
    });
    match act {
        Some((i, 0)) => {
            f.pg_path.remove(i);
        }
        Some((i, d)) => f.pg_path.swap(i, (i as i32 + d) as usize),
        None => {}
    }
    if let Some((_, _, m)) = f.pg_refused.iter().find(|(n, _, _)| n == "search_path") {
        error_line_free(ui, m);
    }
    ui.add_space(2.0);
    small(ui, "Unqualified names resolve in this order. Use the arrows to reorder. Test lists the schemas of the default database. With no rows the server's own path applies.", FAINT());
    ui.add_space(10.0);
}

fn error_line_free(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).size(TXT_SMALL).color(ALERT())).wrap());
}

fn time_zone_row(ui: &mut egui::Ui, f: &mut ConnForm) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(NAME_W, BUTTON_H), egui::Layout::left_to_right(egui::Align::Center), |ui| ui.label(RichText::new("TimeZone").monospace().color(BRIGHT())));
        segmented(ui, &mut f.zone_kind, &[(ZoneKind::Server, "Default"), (ZoneKind::Utc, "UTC"), (ZoneKind::System, "System"), (ZoneKind::Custom, "Custom")]);
        match f.zone_kind {
            ZoneKind::Custom => {
                let r = ui.add(egui::TextEdit::singleline(&mut f.zone_text).hint_text("Europe/Paris or +02:00").font(egui::TextStyle::Monospace).desired_width(160.0).margin(FIELD_MARGIN));
                r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "TimeZone value"));
            }
            ZoneKind::System => match db::sql::advanced::system_zone_name() {
                Some(z) => small(ui, &format!("Sends {z}"), FAINT()),
                None => small(ui, "This computer's zone could not be read", ALERT()),
            },
            ZoneKind::Server => match db::sql::advanced::system_zone_name() {
                Some(z) => small(ui, &format!("Shows timestamptz in {z}, this computer's zone"), FAINT()),
                None => small(ui, "Shows timestamptz in UTC", FAINT()),
            },
            _ => {}
        }
    });
    if let Some((_, _, m)) = f.pg_refused.iter().find(|(n, _, _)| n == "TimeZone") {
        error_line(ui, m);
    }
}

fn hint_of(name: &str, def: Option<&adv::Def>, default_s: u64) -> String {
    match name {
        "statement_timeout" => format!("empty follows Settings ({default_s} s); a value here wins"),
        "lock_timeout" => "reads and queries only; edits and imports keep 10s, structure and trigger changes 3s".into(),
        "application_name" => "shown in pg_stat_activity".into(),
        _ => def.map(|d| d.desc.to_string()).unwrap_or_default(),
    }
}

fn session_rows(ui: &mut egui::Ui, f: &mut ConnForm, default_s: u64) {
    ui.horizontal(|ui| {
        caption(ui, "SESSION SETTINGS");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| small(ui, "Sent as SET when a session opens", FAINT()));
    });
    ui.add_space(2.0);
    let mut remove = None;
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 6.0;
        time_zone_row(ui, f);
        for i in 0..f.pg_rows.len() {
            let name = f.pg_rows[i].0.clone();
            let fixed = PG_FIXED.contains(&name.as_str());
            check_row(f, i);
            let def = f.pg_checks[i].def;
            let hint = if name == "statement_timeout" { format!("{default_s}s") } else { def.map_or("", |d| d.hint).to_string() };
            let err = row_error(f, &f.pg_checks[i]);
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(egui::vec2(NAME_W, BUTTON_H), egui::Layout::left_to_right(egui::Align::Center), |ui| ui.label(RichText::new(&name).monospace().color(BRIGHT())));
                let r = ui.add(egui::TextEdit::singleline(&mut f.pg_rows[i].1).hint_text(RichText::new(hint).color(FAINT())).font(egui::TextStyle::Monospace).desired_width(VALUE_W).margin(FIELD_MARGIN));
                r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, format!("{name} value")));
                if err.is_some() {
                    ui.painter().rect_stroke(r.rect, R_SM, Stroke::new(1.0, ALERT()), egui::StrokeKind::Inside);
                }
                if !fixed && remove_x(ui, &format!("Remove {name}")) {
                    remove = Some(i);
                }
                if err.is_none() {
                    ui.add(egui::Label::new(RichText::new(hint_of(&name, def, default_s)).size(TXT_SMALL).color(FAINT())).truncate());
                }
            });
            if let Some(e) = err {
                error_line(ui, &e);
            }
        }
    });
    if let Some(i) = remove {
        f.pg_rows.remove(i);
    }
    ui.add_space(4.0);
    small(ui, "statement_timeout here wins over the timeout in Settings. Structure changes, imports, edits and other writes always run with no statement timeout, whatever is set here. Edits and imports wait 10 s for a lock and structure or trigger changes 3 s, whatever lock_timeout is set here.", FAINT());
    ui.add_space(6.0);
    picker(ui, f);
    ui.add_space(10.0);
}

/// One line of the picker: the name in mono, what it does, and its group at the right edge.
fn pick_row(ui: &mut egui::Ui, d: &adv::Def) -> egui::Response {
    side_row(ui, 26.0, 0.0, false, d.name, |p, rect| {
        let name = p.layout_job(egui::text::LayoutJob::single_section(d.name.to_string(), mono_fmt(TXT_SMALL, Color32::PLACEHOLDER, 400.0)));
        galley_at(p, rect.left(), rect.center().y, name, BRIGHT());
        let desc = p.layout_job(egui::text::LayoutJob::single_section(d.desc.to_string(), ui_fmt(TXT_SMALL, Color32::PLACEHOLDER, 400.0, 0.0)));
        galley_at(p, rect.left() + 250.0, rect.center().y, desc, MUTED());
        let grp = p.layout_job(egui::text::LayoutJob::single_section(d.group.label().to_string(), ui_fmt(TXT_SMALL, Color32::PLACEHOLDER, 400.0, 0.0)));
        galley_at(p, rect.right() - grp.size().x, rect.center().y, grp, FAINT());
    })
}

fn picker(ui: &mut egui::Ui, f: &mut ConnForm) {
    let Some(mut filter) = f.pg_picker.take() else {
        if add_chip(ui, "setting").clicked() {
            f.pg_picker = Some(String::new());
        }
        return;
    };
    let mut close = false;
    let mut chosen: Option<&'static str> = None;
    egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, PRIMARY())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut filter).hint_text("Add setting: type a name or what it does").font(egui::TextStyle::Monospace).desired_width(ui.available_width() - 70.0).margin(FIELD_MARGIN));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Setting filter"));
            if !r.has_focus() && filter.is_empty() && !r.lost_focus() {
                r.request_focus();
            }
            if ui.button("Close").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
        });
        let q = filter.trim().to_ascii_lowercase();
        let hit = |s: &str| q.is_empty() || s.contains(&q);
        let present = |n: &str| f.pg_rows.iter().any(|(r, _)| r == n);
        let (allowed_low, kept_low) = lowered();
        let matches: Vec<&adv::Def> = adv::ALLOWED.iter().zip(allowed_low).filter(|(d, low)| !present(d.name) && low.iter().any(|s| hit(s))).map(|(d, _)| d).collect();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            caption(ui, "ALLOWED");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| small(ui, &format!("{} matching", matches.len()), FAINT()));
        });
        egui::ScrollArea::vertical().max_height(190.0).id_salt("pg-picker").show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            for d in &matches {
                if pick_row(ui, d).clicked() {
                    chosen = Some(d.name);
                }
            }
            if matches.is_empty() {
                small(ui, "Nothing allowed matches. A setting that is not listed is never sent.", FAINT());
            }
        });
        let kept: Vec<&(&str, &str)> = adv::KEPT.iter().zip(kept_low).filter(|(_, low)| low.iter().any(|s| hit(s))).map(|(k, _)| k).collect();
        if !kept.is_empty() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                caption(ui, "KEPT BY QUARRY");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| small(ui, "Not offered", FAINT()));
            });
            for (n, why) in kept {
                ui.horizontal(|ui| {
                    ui.add_enabled(false, egui::Label::new(RichText::new(*n).monospace().size(TXT_SMALL)));
                    small(ui, why, FAINT());
                });
            }
            small(ui, "The read-only lock and the way Quarry reads and writes values depend on these, so they are never sent.", FAINT());
        }
    });
    if let Some(name) = chosen {
        f.pg_rows.push((name.to_string(), String::new()));
        close = true;
    }
    if !close {
        f.pg_picker = Some(filter);
    }
}

pub(super) fn pg_advanced_tab(ui: &mut egui::Ui, f: &mut ConnForm, default_s: u64) {
    f.pg_ensure_rows();
    note_row(ui, PRIMARY(), true, "Applies when a session opens; open tabs pick it up on Reconnect. Behind PgBouncer in transaction mode Quarry sends these as SET LOCAL inside every transaction.");
    ui.add_space(8.0);
    search_path(ui, f);
    session_rows(ui, f, default_s);
    settings_section(ui, "CONNECTION", |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Connect").color(MUTED()));
            secs_field(ui, &mut f.t_connect, &default_s.to_string());
            ui.add_space(6.0);
            ui.label(RichText::new("Idle").color(MUTED()));
            secs_field(ui, &mut f.t_idle, "300");
        });
        ui.add_space(4.0);
        small(ui, "Empty uses the timeout in Settings. Idle closes pooled connections unused that long.", FAINT());
    });
}
