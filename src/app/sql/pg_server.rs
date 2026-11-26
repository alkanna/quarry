//! The Server window of a PostgreSQL connection (design A on the "Server window" page of docs/sql/postgres.figcraft): Overview,
//! Settings, Activity and Locks. It follows the MySQL window (`sql_server.rs`): it asks only while it is open and Quarry is
//! focused, keeps errors in the window, and offers Cancel and Terminate only on an unlocked connection, after a second click.

use crate::app::sql::server::{arm_time, confirmed, error_row, header, kv_rows, paint_cell, tile, up_text, PAD};
use crate::app::*;
use db::sql::pg::server::{chains, metrics, Role, Session, Setting};
use quarry_core::action::{Action, PgServerA, PgText, ServerA};
use std::collections::HashMap;
pub(crate) use quarry_core::state::sql::pg_server::*;
pub(crate) use quarry_core::state::sql::pg_server::confirm_ok;

const ROW2_H: f32 = 40.0;
const HEAD_H: f32 = 50.0;

/// Whether the row may get a Cancel (`terminate` false) or a Terminate: a session the role may signal and could see, of a kind
/// that takes the signal, never one of Quarry's own; a cancel only while a statement runs.
fn can_signal(own: &HashMap<u64, String>, s: &Session, terminate: bool) -> bool {
    s.signal && !s.ours && !own.contains_key(&s.pid) && !s.hidden && !s.backend_start.is_empty() && s.signalable() && (terminate || s.active())
}

/// The Activity rows the filter and "Hide idle" leave.
fn shown<'a>(rows: &'a [Session], filter: &str, hide_idle: bool) -> Vec<&'a Session> {
    let f = filter.trim().to_lowercase();
    rows.iter().filter(|s| !(hide_idle && s.idle())).filter(|s| f.is_empty() || s.search.contains(&f)).collect()
}

/// Settings by name, all or only the changed ones.
fn shown_settings<'a>(rows: &'a [Setting], filter: &str, changed_only: bool) -> Vec<&'a Setting> {
    let f = filter.trim().to_lowercase();
    rows.iter().filter(|s| !changed_only || s.changed()).filter(|s| f.is_empty() || s.lower.contains(&f)).collect()
}

/// "12 min", "2 min 22 s", "40 s", "3 h 5 min".
fn span(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    match s {
        0..60 => format!("{s} s"),
        60..600 if !s.is_multiple_of(60) => format!("{} min {} s", s / 60, s % 60),
        60..3600 => format!("{} min", s / 60),
        _ => format!("{} h {} min", s / 3600, s / 60 % 60),
    }
}

/// The source of a setting as the window words it, naming the database or role it came from.
fn source_text(s: &Setting, user: &str, database: &str) -> String {
    match s.source.as_str() {
        "client" | "session" => "session".into(),
        "database" => format!("database {database}"),
        "user" => format!("user {user}"),
        "database user" => format!("database {database}, user {user}"),
        other => other.into(),
    }
}

/// Whether Quarry set this setting for the session that read the list.
fn set_by_quarry(s: &Setting, quarry: &[String]) -> bool {
    matches!(s.source.as_str(), "client" | "session") && quarry.contains(&s.lower)
}

/// A pg_settings number with its unit as SHOW would print it: "16384" in units of "8kB" is "128MB".
fn with_unit(v: &str, unit: &str) -> String {
    let Ok(n) = v.trim().parse::<i64>() else { return v.to_string() };
    let (k, base) = match unit {
        "B" => (1, "B"),
        "kB" => (1, "kB"),
        "8kB" => (8, "kB"),
        "MB" => (1, "MB"),
        "ms" | "s" | "min" => return format!("{n}{unit}"),
        _ => return format!("{v}{}{unit}", if unit.is_empty() { "" } else { " " }),
    };
    if n <= 0 {
        return format!("{n}{base}");
    }
    let mut x = n * k;
    let units = ["B", "kB", "MB", "GB", "TB"];
    let mut i = units.iter().position(|u| *u == base).unwrap_or(0);
    while x % 1024 == 0 && i + 1 < units.len() {
        x /= 1024;
        i += 1;
    }
    format!("{x}{}", units[i])
}

/// A painted one-line text from `x` to `x + w` at the vertical center `cy`, cut with an ellipsis.
fn text_at(ui: &egui::Ui, x: f32, w: f32, cy: f32, h: f32, s: &str, color: Color32, mono: bool, size: f32, right: bool) {
    let font = if mono { FontId::monospace(size) } else { FontId::proportional(size) };
    place(ui, egui::Rect::from_min_max(egui::pos2(x, cy - h / 2.0), egui::pos2(x + w, cy + h / 2.0)), s, font, color, right, PAD);
}

/// A note above the list whose text is laid out by `add` (several styled pieces), on the frame of the shared
/// `banner`; true when the button was clicked.
fn banner(ui: &mut egui::Ui, color: Color32, add: impl FnOnce(&mut egui::Ui), button: Option<&str>) -> bool {
    let mut clicked = false;
    crate::ui::dialog::banner_frame(color).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.set_min_height(26.0);
            add(ui);
            if let Some(b) = button {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| clicked = ui.button(b).clicked());
            }
        });
    });
    ui.add_space(14.0);
    clicked
}

/// The read-only note of the Activity and Locks tabs, true when Unlock was clicked.
fn locked_banner(ui: &mut egui::Ui) -> bool {
    crate::ui::dialog::banner(ui, Notice::Locked, "Read-only.", "Unlock writes to cancel queries or terminate sessions.", Some("Unlock…"))
}

/// What a click on Cancel or Terminate does on a locked connection: a popover that offers Unlock instead of acting. True when
/// Unlock was clicked.
fn ask_unlock(ui: &egui::Ui, r: &egui::Response, conn_name: &str, what: &str) -> bool {
    let mut unlock = false;
    let mut close = false;
    popover(r, 340.0, false, |ui| {
        menu_look(ui);
        popover_body(ui, 320.0, |ui| {
            ui.label(semibold(RichText::new(format!("{conn_name} is read-only")).size(TXT_BODY).color(BRIGHT())));
            ui.add_space(4.0);
            ui.label(RichText::new(format!("Unlock writes to {what}.")).size(TXT_SMALL).color(MUTED()));
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if primary(ui, "Unlock…", PRIMARY()).clicked() {
                    (unlock, close) = (true, true);
                }
                if ui.button("Not now").clicked() {
                    close = true;
                }
            });
        });
    });
    if close {
        ui.ctx().data_mut(|d| d.insert_temp(r.id.with("clause-popover"), false));
    }
    unlock
}

impl App {
    pub(in crate::app) fn pg_server_window(&mut self, ctx: &egui::Context) {
        let Some(s) = self.core.pg_server.as_ref() else { return };
        let conn = s.conn;
        let name = self.conn(conn).map(|c| c.meta.name.clone()).unwrap_or_default();
        let product = self.conn(conn).map(|c| format!("{} {}", c.server.product, c.server.version)).unwrap_or_default();
        let live = self.conn(conn).is_some_and(|c| c.sql.is_some()) || self.core.demo;
        let mut acts: Vec<PgServerA> = Vec::new();
        let screen = ctx.content_rect();
        let r = banded(egui::Id::unique(("quarry-layer", "pg-server-window"))).show(ctx, |ui| {
            ui.set_width(fit_width(ctx, (screen.width() * 0.7).clamp(760.0, 1100.0)));
            const TABS: [(PgTab, &str); 5] = [(PgTab::Overview, "Overview"), (PgTab::Settings, "Settings"), (PgTab::Activity, "Activity"), (PgTab::Locks, "Locks"), (PgTab::Counters, "Counters")];
            let labels = TABS.map(|t| t.1);
            let active = TABS.iter().position(|t| t.0 == s.tab).unwrap_or(0);
            let picked = dialog_head(ui, &Head::new("Server").sub(subtitle(&s, &name, product.trim())).glyph(glyphs::server).tabs(&labels, active));
            if let Some(i) = picked && TABS[i].0 != s.tab {
                acts.push(PgServerA::Tab(TABS[i].0));
            }
            let max = screen.height() - 400.0;
            dialog_pane(ui, |ui| {
                if !live {
                    ui.label(RichText::new("This connection is closed. Reconnect to see the server again.").color(MUTED()));
                } else {
                    match s.tab {
                        PgTab::Overview => overview_view(ui, s, &mut acts),
                        PgTab::Settings => settings_view(ui, s, max, &mut acts),
                        PgTab::Activity => activity_view(ui, s, max, &name, &mut acts),
                        PgTab::Locks => locks_view(ui, s, max, &name, &mut acts),
                        PgTab::Counters => counters_view(ui, s, max, &mut acts),
                    }
                }
            });
            if dialog_done(ui) {
                // Done wins over a second click in the same frame.
                acts.retain(|a| !matches!(a, PgServerA::Signal { .. }));
                return false;
            }
            true
        });
        let open = r.inner;
        for a in acts {
            self.exec(Action::Server(ServerA::Pg(a)));
        }
        if !open || (r.should_close() && self.modal.is_none()) {
            self.exec(Action::Server(ServerA::Pg(PgServerA::Close)));
        }
    }
}

fn subtitle(s: &PgServerWin, name: &str, product: &str) -> String {
    let rows = s.rows();
    let clients = || rows.iter().filter(|r| r.kind == "client backend" || r.hidden).count();
    match (s.tab, &s.act) {
        (PgTab::Activity, Some(Ok(a))) if !a.read_all => format!("{name} · signed in as {} · {} sessions", a.user, clients()),
        (PgTab::Activity, Some(Ok(_))) => format!("{name} · {} sessions, {} active, {} idle in transaction", clients(), rows.iter().filter(|r| r.active() && r.kind == "client backend").count(), rows.iter().filter(|r| r.idle_in_tx()).count()),
        (PgTab::Locks, Some(Ok(_))) => {
            let ch = chains(rows);
            let waiting: usize = ch.iter().map(|c| c.waiting.len()).sum();
            if ch.is_empty() { format!("{name} · no session waits for a lock") } else { format!("{name} · {waiting} {} waiting behind {} {}", if waiting == 1 { "session" } else { "sessions" }, ch.len(), if ch.len() == 1 { "blocker" } else { "blockers" }) }
        }
        _ => {
            let (version, up) = match s.overview() {
                Some(o) => (format!("PostgreSQL {}", o.short_version), o.uptime),
                None => (product.to_string(), None),
            };
            let up = up.map(|u| format!(" · up {}", up_text(u))).unwrap_or_default();
            format!("{name} · {version}{up}")
        }
    }
}

fn overview_view(ui: &mut egui::Ui, s: &PgServerWin, acts: &mut Vec<PgServerA>) {
    let o = match &s.ov {
        None => return spin(ui),
        Some(Err(e)) => {
            if error_row(ui, format!("Could not read the server status: {e}")) {
                acts.push(PgServerA::Refresh);
            }
            return;
        }
        Some(Ok((_, o))) => o,
    };
    let m = metrics(o, s.ov_prev.as_ref());
    let int = |v: Option<f64>| v.map(|v| group(&format!("{v:.0}"))).unwrap_or_else(|| "…".into());
    let gap = 12.0;
    let w = (ui.available_width() - gap * 4.0) / 5.0;
    let (strip, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 86.0), Sense::hover());
    let at = |i: usize| egui::Rect::from_min_size(egui::pos2(strip.left() + i as f32 * (w + gap), strip.top()), egui::vec2(w, strip.height()));
    let of = o.max_connections.map(|v| format!("of {}", group(&format!("{v:.0}")))).unwrap_or_default();
    tile(ui, at(0), "Connections", &int(o.connections), &of, &format!("{} active, {} waiting", int(o.active), int(o.waiting)));
    let commit = m.commit.map(|c| format!("{:.1}% commit", c * 100.0)).unwrap_or_default();
    tile(ui, at(1), "Transactions per second", &int(m.tps), "", &commit);
    let buffers = o.shared_buffers.map(|b| format!("shared_buffers {}", human_bytes(b as usize))).unwrap_or_default();
    tile(ui, at(2), "Cache hit ratio", &m.hit.map(|h| format!("{:.1}%", h * 100.0)).unwrap_or_else(|| "…".into()), "", &buffers);
    let (total, count) = match &s.sizes {
        Some(v) => (Some(v.iter().filter_map(|(_, b)| *b).sum::<u64>()), format!("{} {}", v.len(), if v.len() == 1 { "database" } else { "databases" })),
        None => (None, String::new()),
    };
    tile(ui, at(3), "Database size", &total.map(|t| human_bytes(t as usize)).unwrap_or_else(|| "…".into()), "", &count);
    let lag = |l: Option<f64>| l.map(|l| format!("{l:.1} s").replace(".0 s", " s")).unwrap_or_else(|| "none yet".into());
    let (big, sub) = match &o.role {
        Some(Role::Primary { standbys, visible }) if standbys.is_empty() => ("primary".to_string(), if *visible { "no replicas".to_string() } else { String::new() }),
        Some(Role::Primary { standbys, visible }) => {
            let n = standbys.len();
            let worst = standbys.iter().filter_map(|x| x.lag).fold(None, |a: Option<f64>, l| Some(a.map_or(l, |a| a.max(l))));
            ("primary".into(), if *visible { format!("{n} {}, lag {}", if n == 1 { "replica" } else { "replicas" }, lag(worst)) } else { format!("{n} {}", if n == 1 { "replica" } else { "replicas" }) })
        }
        Some(Role::Replica { lag: l, .. }) => ("replica".into(), format!("lag {}", lag(*l))),
        None => ("…".into(), String::new()),
    };
    tile(ui, at(4), "Replication", &big, "", &sub);
    ui.add_space(16.0);
    let dir = if o.data_dir.is_empty() { "Not readable by this role (pg_read_all_settings)".to_string() } else { o.data_dir.clone() };
    let server = [
        ("Version", o.version.clone(), BRIGHT()),
        ("Host", if o.host.is_empty() { "local socket".into() } else { o.host.clone() }, BRIGHT()),
        ("Port", o.port.clone(), C_NUMBER()),
        ("TLS", if o.tls.is_empty() { "Not encrypted".into() } else { o.tls.clone() }, BRIGHT()),
        ("Data directory", dir, if o.data_dir.is_empty() { MUTED() } else { BRIGHT() }),
        ("Encoding", [o.encoding.as_str(), o.collate.as_str()].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(", "), BRIGHT()),
        ("Time zone", o.time_zone.clone(), BRIGHT()),
        ("Started", o.started.clone(), BRIGHT()),
    ];
    let (read_all, user) = (o.read_all, o.user.clone());
    let role = o.role.clone();
    let sizes = s.sizes.clone();
    let sizes_err = s.sizes_err.clone();
    ui.columns(2, |cols| {
        let ui = &mut cols[0];
        caption(ui, "SERVER");
        ui.add_space(4.0);
        let w = ui.available_width();
        ui.spacing_mut().item_spacing.y = 0.0;
        kv_rows(ui, w, &server);
        let ui = &mut cols[1];
        caption(ui, "DATABASE SIZES");
        ui.add_space(4.0);
        let w = ui.available_width();
        ui.spacing_mut().item_spacing.y = 0.0;
        size_bars(ui, w, sizes.as_deref(), sizes_err.as_deref());
        ui.add_space(16.0);
        caption(ui, "REPLICATION");
        ui.add_space(4.0);
        replication(ui, w, role.as_ref());
    });
    if !read_all {
        ui.add_space(10.0);
        ui.label(RichText::new(format!("Active and waiting count only the sessions {user} may see: the role lacks pg_read_all_stats (pg_monitor grants it).")).size(TXT_SMALL).color(MUTED()));
    }
}

/// The size bars, or why they could not be read; a failure after a good read keeps the last values and says so under them.
pub(in crate::app) fn size_bars(ui: &mut egui::Ui, w: f32, sizes: Option<&[(String, Option<u64>)]>, err: Option<&str>) {
    let note = |ui: &mut egui::Ui, t: String| {
        ui.label(RichText::new(t).size(TXT_SMALL).color(MUTED()));
    };
    let Some(sizes) = sizes else {
        return match err {
            Some(e) => note(ui, format!("Could not read the sizes: {e}")),
            None => spin(ui),
        };
    };
    if let Some(e) = err {
        note(ui, format!("Last read sizes; the latest read failed: {e}"));
    }
    let max = sizes.iter().filter_map(|(_, b)| *b).max().unwrap_or(1).max(1) as f32;
    let (name_w, size_w) = (w * 0.3, 90.0);
    for (i, (name, bytes)) in sizes.iter().take(8).enumerate() {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::hover());
        ui.painter().rect_filled(rect, 0.0, if i % 2 == 1 { PANEL() } else { BG() });
        paint_cell(ui, rect, rect.left(), name_w, name, BRIGHT(), true, false);
        let bar = egui::Rect::from_min_max(egui::pos2(rect.left() + name_w, rect.center().y - 3.0), egui::pos2(rect.right() - size_w - PAD, rect.center().y + 3.0));
        ui.painter().rect_filled(bar, 3.0, LINE());
        match bytes {
            Some(b) => {
                let fill = egui::Rect::from_min_size(bar.min, egui::vec2((bar.width() * *b as f32 / max).max(4.0), bar.height()));
                ui.painter().rect_filled(fill, 3.0, C_NUMBER());
                paint_cell(ui, rect, rect.right() - size_w, size_w, &human_bytes(*b as usize), C_NUMBER(), true, true);
            }
            None => {
                paint_cell(ui, rect, rect.right() - size_w, size_w, "hidden", FAINT(), true, true);
                resp.on_hover_text("This role may not connect to it, and sizing it needs pg_read_all_stats.");
            }
        }
    }
    if sizes.len() > 8 {
        ui.label(RichText::new(format!("and {} more", sizes.len() - 8)).size(TXT_SMALL).color(MUTED()));
    }
}

fn replication(ui: &mut egui::Ui, w: f32, role: Option<&Role>) {
    match role {
        None => {}
        Some(Role::Primary { standbys, .. }) if standbys.is_empty() => {
            ui.label(RichText::new("No replica streams from this server").color(MUTED()));
        }
        Some(Role::Primary { standbys, visible }) => {
            let cw = [w * 0.42, w * 0.2, w * 0.16, w * 0.22];
            header(ui, &[("Replica", cw[0], false), ("State", cw[1], false), ("Sync", cw[2], false), ("Replay lag", cw[3], true)]);
            for (i, r) in standbys.iter().enumerate() {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::hover());
                ui.painter().rect_filled(rect, 0.0, if i % 2 == 1 { PANEL() } else { BG() });
                let who = [r.name.as_str(), r.addr.as_str()].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join("  ");
                paint_cell(ui, rect, rect.left(), cw[0], &who, BRIGHT(), true, false);
                paint_cell(ui, rect, rect.left() + cw[0], cw[1], &r.state, if r.state == "streaming" { SUCCESS() } else { WARNING() }, true, false);
                paint_cell(ui, rect, rect.left() + cw[0] + cw[1], cw[2], &r.sync, TEXT(), true, false);
                let lag = r.lag.map(|l| format!("{l:.1} s").replace(".0 s", " s")).unwrap_or_default();
                paint_cell(ui, rect, rect.left() + cw[0] + cw[1] + cw[2], cw[3], &lag, C_NUMBER(), true, true);
            }
            if !visible {
                ui.add_space(4.0);
                ui.label(RichText::new("Their state and lag are hidden: the role needs pg_read_all_stats (pg_monitor grants it).").size(TXT_SMALL).color(MUTED()));
            }
        }
        Some(Role::Replica { from, status, lag }) => {
            let lag = lag.map(|l| format!("{l:.1} s")).unwrap_or_else(|| "No transaction replayed yet".into());
            let hidden = "Hidden: needs pg_read_all_stats".to_string();
            kv_rows(ui, w, &[("Role", "replica".into(), BRIGHT()), ("Primary", if from.is_empty() { hidden.clone() } else { from.clone() }, if from.is_empty() { MUTED() } else { BRIGHT() }), ("Receiver", if status.is_empty() { hidden } else { status.clone() }, if status == "streaming" { SUCCESS() } else { MUTED() }), ("Replay lag", lag, C_NUMBER())]);
        }
    }
}

/// pg_stat_database per database: what each one did since its counters were reset.
fn counters_view(ui: &mut egui::Ui, s: &PgServerWin, max_h: f32, acts: &mut Vec<PgServerA>) {
    let v = match &s.counters {
        None => return spin(ui),
        Some(Err(e)) => {
            if error_row(ui, format!("Could not read the counters: {e}")) {
                acts.push(PgServerA::Refresh);
            }
            return;
        }
        Some(Ok(v)) => v,
    };
    let int = |n: Option<u64>| n.map(|n| group(&n.to_string())).unwrap_or_default();
    let w = ui.available_width();
    // Each database keeps its own reset date, so it is a column rather than one note for all.
    let cols: [(&str, f32, bool); 11] = [("Database", 0.13, false), ("Sessions", 0.07, true), ("Commits", 0.1, true), ("Rollbacks", 0.08, true), ("Cache hit", 0.08, true), ("Inserted", 0.09, true), ("Updated", 0.09, true), ("Deleted", 0.08, true), ("Deadlocks", 0.08, true), ("Temp written", 0.1, true), ("Since", 0.1, true)];
    let cols = cols.map(|(n, f, r)| (n, w * f, r));
    header(ui, &cols);
    let gap = std::mem::replace(&mut ui.spacing_mut().item_spacing.y, 0.0);
    egui::ScrollArea::vertical().id_salt("pg-counters").max_height(max_h.max(120.0)).auto_shrink([false, true]).show_rows(ui, ROW_H, v.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for i in range {
            let c = &v[i];
            let (rect, _) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::hover());
            ui.painter().rect_filled(rect, 0.0, if i % 2 == 1 { PANEL() } else { BG() });
            let cells = [c.db.clone(), int(c.sessions), int(c.commits), int(c.rollbacks), c.hit().map(|h| format!("{:.1}%", h * 100.0)).unwrap_or_default(), int(c.inserted), int(c.updated), int(c.deleted), int(c.deadlocks), c.temp_bytes.map(|b| human_bytes(b as usize)).unwrap_or_default(), if c.reset.is_empty() { "server start".into() } else { c.reset.clone() }];
            let mut x = rect.left();
            for (j, t) in cells.iter().enumerate() {
                let color = if j == 0 { BRIGHT() } else if j == 8 && c.deadlocks.is_some_and(|d| d > 0) { WARNING() } else if j == 10 { MUTED() } else { C_NUMBER() };
                paint_cell(ui, rect, x, cols[j].1, t, color, true, cols[j].2);
                x += cols[j].1;
            }
        }
    });
    ui.spacing_mut().item_spacing.y = gap;
    ui.add_space(8.0);
    ui.label(RichText::new("Totals since each database's statistics were last reset, or since the server started.").size(TXT_SMALL).color(MUTED()));
}

fn settings_view(ui: &mut egui::Ui, s: &PgServerWin, max_h: f32, acts: &mut Vec<PgServerA>) {
    let total = match &s.settings {
        Some(Ok(v)) => v.rows.len(),
        _ => 0,
    };
    let count = match &s.settings {
        Some(Ok(v)) => shown_settings(&v.rows, &s.set_filter, s.changed_only).len(),
        _ => 0,
    };
    ui.horizontal(|ui| {
        let mut text = s.set_filter.clone();
        if ui.add(egui::TextEdit::singleline(&mut text).hint_text("Filter settings").desired_width(260.0)).changed() {
            acts.push(PgServerA::Text(PgText::Settings, text));
        }
        let mut changed = s.changed_only;
        ui.checkbox(&mut changed, "Changed only");
        if changed != s.changed_only {
            acts.push(PgServerA::ChangedOnly(changed));
        }
        ui.add_space(8.0);
        let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
        ui.painter().circle_filled(dot.center(), 3.5, WARNING());
        ui.label(RichText::new("Needs a restart").color(MUTED()));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if total > 0 {
                ui.label(RichText::new(format!("{count} of {total} settings")).color(FAINT()));
            }
        });
    });
    ui.add_space(8.0);
    let v = match &s.settings {
        None => return spin(ui),
        Some(Err(e)) => {
            if error_row(ui, format!("Could not read the settings: {e}")) {
                acts.push(PgServerA::Refresh);
            }
            return;
        }
        Some(Ok(v)) => v,
    };
    let rows = shown_settings(&v.rows, &s.set_filter, s.changed_only);
    let w = ui.available_width();
    let (name_w, val_w, src_w) = (w * 0.34, w * 0.17, w * 0.18);
    let note_w = w - name_w - val_w - src_w;
    header(ui, &[("Name", name_w, false), ("Value", val_w, false), ("Source", src_w, false), ("Note", note_w, false)]);
    if rows.is_empty() {
        ui.add_space(8.0);
        ui.label(RichText::new("No setting matches.").color(MUTED()));
        return;
    }
    let mut sel = s.set_sel.clone();
    let detail_h = if sel.is_some() { 110.0 } else { 0.0 };
    // show_rows steps by the row height plus the parent's item spacing, which would leave a gap per row.
    let gap = std::mem::replace(&mut ui.spacing_mut().item_spacing.y, 0.0);
    egui::ScrollArea::vertical().id_salt("pg-settings").max_height((max_h - detail_h).max(120.0)).auto_shrink([false, true]).show_rows(ui, ROW_H, rows.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for i in range {
            let r = rows[i];
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::click());
            let selected = sel.as_deref() == Some(r.name.as_str());
            let fill = match (selected, state_of(&resp)) {
                (true, 0) => SEL(),
                (true, _) => SEL_HOVER(),
                (false, 0) => if i % 2 == 1 { PANEL() } else { BG() },
                (false, st) => hover_fill(st),
            };
            ui.painter().rect_filled(rect, 0.0, fill);
            let ours = set_by_quarry(r, &s.quarry);
            paint_cell(ui, rect, rect.left(), name_w, &r.name, BRIGHT(), true, false);
            paint_cell(ui, rect, rect.left() + name_w, val_w, &r.value, C_NUMBER(), true, false);
            paint_cell(ui, rect, rect.left() + name_w + val_w, src_w, &source_text(r, &v.user, &v.database), if ours { C_LINK() } else if r.changed() { TEXT() } else { MUTED() }, false, false);
            let nx = rect.left() + name_w + val_w + src_w;
            if ours {
                paint_cell(ui, rect, nx, note_w, "Set by Quarry for this session", C_LINK(), true, false);
            } else if r.pending {
                ui.painter().circle_filled(egui::pos2(nx + PAD + 3.5, rect.center().y), 3.5, WARNING());
                let note = match &r.after_restart {
                    Some(a) => format!("{} after a restart", with_unit(a, "")),
                    None => "Needs a restart".into(),
                };
                paint_cell(ui, rect, nx + 12.0, note_w - 12.0, &note, WARNING(), false, false);
            }
            if resp.clicked() {
                sel = if selected { None } else { Some(r.name.clone()) };
            }
            if r.value.chars().count() > 24 {
                resp.on_hover_text(&r.value);
            }
        }
    });
    ui.spacing_mut().item_spacing.y = gap;
    if let Some(r) = sel.as_deref().and_then(|n| v.rows.iter().find(|r| r.name == n)) {
        ui.add_space(10.0);
        setting_detail(ui, r);
    }
    if sel != s.set_sel {
        acts.push(PgServerA::Setting(sel));
    }
}

fn setting_detail(ui: &mut egui::Ui, r: &Setting) {
    egui::Frame::new().fill(BG()).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 4.0;
        let k = |t: &str| RichText::new(t).size(TXT_SMALL).color(MUTED());
        let v = |t: &str| RichText::new(t).size(TXT_SMALL).color(TEXT());
        ui.horizontal(|ui| {
            ui.label(RichText::new(&r.name).monospace().color(BRIGHT()));
            ui.label(RichText::new(&r.desc).color(TEXT()));
        });
        ui.horizontal(|ui| {
            ui.label(k("Context"));
            ui.label(v(&r.context));
            if !r.min.is_empty() || !r.max.is_empty() {
                ui.add_space(8.0);
                ui.label(k("Range"));
                ui.label(v(&format!("{} to {}", with_unit(&r.min, &r.unit), with_unit(&r.max, &r.unit))));
            }
            ui.add_space(8.0);
            ui.label(k("Default"));
            ui.label(v(&with_unit(&r.boot, &r.unit)));
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Set in").monospace().size(TXT_SMALL).color(MUTED()));
            let file = if r.file.is_empty() { if r.source == "configuration file" { "not readable by this role" } else { "" } } else { r.file.as_str() };
            ui.label(RichText::new(file).monospace().size(TXT_SMALL).color(TEXT()));
            ui.add_space(12.0);
            ui.label(RichText::new("pending_restart").monospace().size(TXT_SMALL).color(MUTED()));
            ui.label(RichText::new(if r.pending { "true" } else { "false" }).monospace().size(TXT_SMALL).color(if r.pending { WARNING() } else { TEXT() }));
        });
    });
}

/// The right end of the Activity toolbar: Cancel query and Terminate for the selected row, or the second step of one.
fn act_buttons(ui: &mut egui::Ui, s: &PgServerWin, confirm: Option<&PgConfirm>, sel: Option<&Session>, name: &str, acts: &mut Vec<PgServerA>) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if let Some(c) = confirm {
            // Drawn where Terminate was, so the guard keeps a double click from sending it.
            let r = primary(ui, if c.terminate { "Terminate" } else { "Cancel query" }, ALERT());
            if confirmed(ui, &r, c.at) {
                acts.push(PgServerA::Signal { pid: c.s.pid });
            }
            if ui.button("Keep").clicked() {
                acts.push(PgServerA::Disarm);
            }
            ui.label(RichText::new(if c.terminate { format!("End session {}?", c.s.pid) } else { format!("Cancel the query of {}?", c.s.pid) }).color(ALERT()));
            return;
        }
        let (can_t, can_c) = sel.map_or((false, false), |r| (can_signal(&s.own, r, true), can_signal(&s.own, r, false)));
        let term = ui.add_enabled_ui(can_t, |ui| danger(ui, "Terminate")).inner;
        let cancel = ui.add_enabled(can_c, egui::Button::new("Cancel query"));
        if let Some(r) = sel {
            for (resp, terminate) in [(&term, true), (&cancel, false)] {
                if s.writable {
                    if resp.clicked() {
                        acts.push(PgServerA::ArmSignal { pid: r.pid, terminate, at: arm_time(ui) });
                    }
                } else if ask_unlock(ui, resp, name, &if terminate { format!("terminate session {}", r.pid) } else { format!("cancel the query of {}", r.pid) }) {
                    acts.push(PgServerA::Unlock);
                }
            }
        }
    });
}

const PID_W: f32 = 64.0;
const ACT_COLS: [(&str, f32); 6] = [("Pid", PID_W), ("User · client", 150.0), ("Database · app", 150.0), ("State", 180.0), ("Wait event", 180.0), ("Xact age", 80.0)];

fn activity_view(ui: &mut egui::Ui, s: &PgServerWin, max_h: f32, name: &str, acts: &mut Vec<PgServerA>) {
    let (read_all, user, own_app) = match &s.act {
        Some(Ok(a)) => (a.read_all, a.user.clone(), a.own_app.clone()),
        _ => (true, String::new(), None),
    };
    if !s.writable && locked_banner(ui) {
        acts.push(PgServerA::Unlock);
    }
    let hidden = s.rows().iter().any(|r| r.hidden);
    if !read_all && hidden {
        banner(ui, PRIMARY(), |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label(RichText::new("Other roles’ sessions read ").color(TEXT()));
            ui.label(RichText::new("insufficient privilege").color(FAINT()));
            ui.label(RichText::new(format!(": {user} lacks pg_read_all_stats, so their state, client and query are hidden.")).color(TEXT()));
        }, None);
    }
    let rows: Vec<Session> = shown(s.rows(), &s.filter, s.hide_idle).into_iter().cloned().collect();
    let idle_hidden = if s.hide_idle { s.rows().iter().filter(|r| r.idle()).count() } else { 0 };
    // A pending action whose row the filter hid or that changed is dropped.
    let mut confirm = s.confirm.clone();
    if confirm.as_ref().is_some_and(|c| !confirm_ok(c, &rows)) {
        confirm = None;
        acts.push(PgServerA::Disarm);
    }
    let sel_row = s.sel.and_then(|p| rows.iter().find(|r| r.pid == p)).cloned();
    ui.horizontal(|ui| {
        let mut text = s.filter.clone();
        if ui.add(egui::TextEdit::singleline(&mut text).hint_text("Filter by user, database or query").desired_width(240.0)).changed() {
            acts.push(PgServerA::Text(PgText::Activity, text));
        }
        let (mut hide, mut auto) = (s.hide_idle, s.auto);
        ui.checkbox(&mut hide, "Hide idle");
        ui.checkbox(&mut auto, "Refresh every 2 s");
        if hide != s.hide_idle {
            acts.push(PgServerA::HideIdle(hide));
        }
        if auto != s.auto {
            acts.push(PgServerA::Auto(auto));
        }
        if !s.auto && quiet(ui, "Refresh").clicked() {
            acts.push(PgServerA::Refresh);
        }
        act_buttons(ui, s, confirm.as_ref(), sel_row.as_ref(), name, acts);
    });
    ui.add_space(8.0);
    match &s.act {
        None => return spin(ui),
        Some(Err(e)) => {
            if error_row(ui, format!("Could not list the sessions: {e}")) {
                acts.push(PgServerA::Refresh);
            }
            return;
        }
        Some(Ok(_)) => {}
    }
    let w = ui.available_width();
    let fixed: f32 = ACT_COLS.iter().map(|c| c.1).sum();
    let query_w = (w - fixed).max(140.0);
    let mut cols: Vec<(&str, f32, bool)> = ACT_COLS.iter().map(|(l, w)| (*l, *w, matches!(*l, "Pid" | "Xact age"))).collect();
    cols.push(("Query", query_w, false));
    header(ui, &cols);
    if rows.is_empty() {
        ui.add_space(8.0);
        ui.label(RichText::new(if s.filter.trim().is_empty() { "No session is running." } else { "No session matches." }).color(MUTED()));
    } else {
        let mut sel = s.sel;
        let mut arm: Option<(u64, bool)> = None;
        let mut fetch: Option<u64> = None;
        let own = s.own.clone();
        let writable = s.writable;
        let now = arm_time(ui);
        // show_rows steps by the row height plus the parent's item spacing, which would leave a gap per row.
        let gap = std::mem::replace(&mut ui.spacing_mut().item_spacing.y, 0.0);
        egui::ScrollArea::vertical().id_salt("pg-activity").max_height(max_h - 30.0).auto_shrink([false, true]).show_rows(ui, ROW2_H, rows.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for i in range {
                let r = &rows[i];
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW2_H), Sense::click());
                let resp = resp.on_hover_cursor(egui::CursorIcon::Default);
                let selected = sel == Some(r.pid);
                session_row(ui, rect, &resp, r, selected, i, &own, query_w);
                let r2 = resp.clone();
                r2.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Session {}", r.pid)));
                if resp.clicked() || resp.secondary_clicked() {
                    sel = Some(r.pid);
                }
                resp.context_menu(|ui| {
                    menu_look(ui);
                    if ui.add_enabled(writable && can_signal(&own, r, false), egui::Button::new("Cancel query").frame_when_inactive(false)).clicked() {
                        arm = Some((r.pid, false));
                        ui.close();
                    }
                    if ui.add_enabled_ui(writable && can_signal(&own, r, true), |ui| danger_item(ui, "Terminate session")).inner.clicked() {
                        arm = Some((r.pid, true));
                        ui.close();
                    }
                    if ui.add_enabled(!r.query.is_empty() && !r.hidden, egui::Button::new("Copy query").frame_when_inactive(false)).clicked() {
                        if r.cut {
                            fetch = Some(r.pid);
                        } else {
                            ui.ctx().copy_text(r.query.clone());
                        }
                        ui.close();
                    }
                });
            }
        });
        ui.spacing_mut().item_spacing.y = gap;
        if let Some(pid) = sel.filter(|p| s.sel != Some(*p)) {
            acts.push(PgServerA::Select(pid));
        }
        if let Some((pid, terminate)) = arm {
            acts.push(PgServerA::ArmSignal { pid, terminate, at: now });
        }
        if let Some(pid) = fetch {
            acts.push(PgServerA::FetchQuery { pid });
        }
    }
    ui.add_space(8.0);
    // Ownership is per server: every Quarry connection to it is known, or behind a pooler only the application name is.
    let ours = match &own_app {
        Some(app) => format!("Behind a pooler Quarry knows its own sessions on this server only by their name, {app}, and never offers them."),
        None => "Quarry’s own sessions on this server, from any of its connections, are never offered.".into(),
    };
    let note = if !read_all {
        format!("Cancel and Terminate work on {user}’s own sessions, or on others with pg_signal_backend. {ours}")
    } else {
        let idle = if idle_hidden > 0 { format!("{idle_hidden} idle {} hidden. ", if idle_hidden == 1 { "session" } else { "sessions" }) } else { String::new() };
        format!("{idle}Cancel stops the running statement (pg_cancel_backend); Terminate ends the session and rolls back its transaction. {ours}")
    };
    ui.label(RichText::new(note).size(TXT_SMALL).color(FAINT()));
}

#[allow(clippy::too_many_arguments)]
fn session_row(ui: &egui::Ui, rect: egui::Rect, resp: &egui::Response, r: &Session, selected: bool, i: usize, own: &HashMap<u64, String>, query_w: f32) {
    let warn = r.idle_in_tx();
    let fill = match (selected, state_of(resp)) {
        (true, 0) => SEL(),
        (true, _) => SEL_HOVER(),
        (false, 0) if warn => tint(WARNING(), 20),
        (false, 0) => if i % 2 == 1 { PANEL() } else { BG() },
        (false, st) => hover_fill(st),
    };
    let p = ui.painter();
    p.rect_filled(rect, 0.0, fill);
    if warn && !selected {
        p.rect_filled(egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())), 0.0, WARNING());
    }
    let (y1, y2, h) = (rect.top() + 12.0, rect.top() + 28.0, 16.0);
    let mut x = rect.left();
    let col = |k: usize| ACT_COLS[k].1;
    text_at(ui, x, col(0), y1, h, &r.pid.to_string(), C_NUMBER(), true, TXT_SMALL, true);
    x += col(0);
    text_at(ui, x, col(1), y1, h, &r.user, if r.kind == "client backend" || r.hidden { BRIGHT() } else { MUTED() }, true, TXT_SMALL, false);
    if !r.client.is_empty() {
        text_at(ui, x, col(1), y2, h, &r.client, FAINT(), true, TXT_CAPTION, false);
    }
    x += col(1);
    text_at(ui, x, col(2), y1, h, &r.db, BRIGHT(), true, TXT_SMALL, false);
    let mine = r.ours || own.contains_key(&r.pid);
    if mine {
        text_at(ui, x, col(2), y2, h, "quarry (you)", C_LINK(), true, TXT_CAPTION, false);
    } else if !r.app.is_empty() {
        text_at(ui, x, col(2), y2, h, &r.app, FAINT(), true, TXT_CAPTION, false);
    }
    x += col(2);
    let (state, state_c) = match r.state.as_str() {
        _ if r.hidden => (String::new(), MUTED()),
        "active" => ("active".into(), SUCCESS()),
        st if st.starts_with("idle in transaction") => (st.to_string(), WARNING()),
        "idle" => ("idle".into(), FAINT()),
        "" => (r.kind.clone(), FAINT()),
        st => (st.to_string(), MUTED()),
    };
    text_at(ui, x, col(3), y1, h, &state, state_c, false, TXT_SMALL, false);
    if warn && let Some(t) = r.state_secs {
        text_at(ui, x, col(3), y2, h, &format!("for {}", span(t)), WARNING(), false, TXT_SMALL, false);
    }
    x += col(3);
    if !r.wait_type.is_empty() {
        let lock = r.wait_type == "Lock";
        text_at(ui, x, col(4), y1, h, &format!("{}: {}", r.wait_type, r.wait), if lock { WARNING() } else { TEXT() }, false, TXT_SMALL, false);
        if !r.blockers.is_empty() {
            let by = r.blockers.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(", ");
            text_at(ui, x, col(4), y2, h, &format!("blocked by {by}"), C_LINK(), false, TXT_SMALL, false);
        }
    }
    x += col(4);
    if let Some(a) = r.xact_secs {
        text_at(ui, x, col(5), y1, h, &mss(a), if warn { WARNING() } else { C_NUMBER() }, true, TXT_SMALL, true);
    }
    x += col(5);
    text_at(ui, x, query_w, y1, h, &r.line, if r.hidden { FAINT() } else { TEXT() }, true, TXT_SMALL, false);
    if resp.hovered() && !r.hidden && !r.query.is_empty() && resp.hover_pos().is_some_and(|h| h.x >= x) {
        let long: String = r.query.chars().take(1200).collect();
        resp.clone().on_hover_text(long);
    } else if mine && resp.hover_pos().is_some_and(|h| h.x >= rect.left() + col(0) + col(1) && h.x < rect.left() + col(0) + col(1) + col(2)) {
        let what = own.get(&r.pid).map(String::as_str).unwrap_or("the Server window or another of its connections to this server");
        resp.clone().on_hover_text(format!("Quarry's own session ({what}). Cancel and Terminate are not offered for it."));
    }
}

const LOCK_COLS: [(&str, f32); 5] = [("Pid", 120.0), ("User", 120.0), ("Database", 110.0), ("Waits for", 200.0), ("Relation", 200.0)];
const WAIT_W: f32 = 70.0;

fn locks_view(ui: &mut egui::Ui, s: &PgServerWin, max_h: f32, name: &str, acts: &mut Vec<PgServerA>) {
    if !s.writable && locked_banner(ui) {
        acts.push(PgServerA::Unlock);
    }
    ui.horizontal(|ui| {
        let mut auto = s.auto;
        ui.checkbox(&mut auto, "Refresh every 2 s");
        if auto != s.auto {
            acts.push(PgServerA::Auto(auto));
        }
        if !s.auto && quiet(ui, "Refresh").clicked() {
            acts.push(PgServerA::Refresh);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new("From pg_locks and pg_blocking_pids()").color(FAINT()));
        });
    });
    ui.add_space(8.0);
    match &s.act {
        None => return spin(ui),
        Some(Err(e)) => {
            if error_row(ui, format!("Could not read the locks: {e}")) {
                acts.push(PgServerA::Refresh);
            }
            return;
        }
        Some(Ok(_)) => {}
    }
    let rows = s.rows().to_vec();
    let ch = chains(&rows);
    let mut confirm = s.confirm.clone();
    if confirm.as_ref().is_some_and(|c| !confirm_ok(c, &rows) || !ch.iter().any(|x| x.head == c.s.pid)) {
        confirm = None;
        acts.push(PgServerA::Disarm);
    }
    let w = ui.available_width();
    let fixed: f32 = LOCK_COLS.iter().map(|c| c.1).sum::<f32>() + WAIT_W;
    let query_w = (w - fixed).max(140.0);
    let mut cols: Vec<(&str, f32, bool)> = LOCK_COLS.iter().map(|(l, w)| (*l, *w, false)).collect();
    cols.push(("Waiting", WAIT_W, true));
    cols.push(("Query", query_w, false));
    header(ui, &cols);
    if ch.is_empty() {
        ui.add_space(8.0);
        ui.label(RichText::new("No session waits for a lock.").color(MUTED()));
        return;
    }
    let by_pid: HashMap<u64, &Session> = rows.iter().map(|r| (r.pid, r)).collect();
    let mut deeper: Option<(u64, u64, u64)> = None;
    egui::ScrollArea::vertical().id_salt("pg-locks").max_height(max_h - 30.0).auto_shrink([false, true]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for c in &ch {
            let gone = Session { pid: c.head, ..Default::default() };
            let head = by_pid.get(&c.head).copied().unwrap_or(&gone);
            blocker_row(ui, s, &mut confirm, head, w, query_w, name, acts);
            let mut parents: Vec<u64> = vec![c.head];
            for (pid, depth) in &c.waiting {
                parents.truncate(*depth);
                if *depth > 1 && deeper.is_none() {
                    deeper = Some((*pid, parents[depth - 1], c.head));
                }
                if let Some(r) = by_pid.get(pid) {
                    waiter_row(ui, r, *depth, w, query_w);
                }
                parents.push(*pid);
            }
        }
    });
    ui.add_space(8.0);
    let lead = deeper.map(|(p, q, h)| format!("{p} queues behind {q}, not behind {h} directly. ")).unwrap_or_default();
    ui.label(RichText::new(format!("{lead}Terminating a blocker rolls back its transaction and frees every session under it.")).size(TXT_SMALL).color(FAINT()));
}

#[allow(clippy::too_many_arguments)]
fn blocker_row(ui: &mut egui::Ui, s: &PgServerWin, confirm: &mut Option<PgConfirm>, r: &Session, w: f32, query_w: f32, name: &str, acts: &mut Vec<PgServerA>) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, HEAD_H), Sense::hover());
    let warn = r.idle_in_tx();
    let p = ui.painter();
    p.rect_filled(rect, 0.0, if warn { tint(WARNING(), 20) } else { PANEL() });
    if warn {
        p.rect_filled(egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())), 0.0, WARNING());
    }
    let (y1, y2, h) = (rect.top() + 15.0, rect.top() + 35.0, 18.0);
    let mut x = rect.left();
    text_at(ui, x, LOCK_COLS[0].1, y1, h, &r.pid.to_string(), C_NUMBER(), true, TXT_SMALL, false);
    x += LOCK_COLS[0].1;
    text_at(ui, x, LOCK_COLS[1].1, y1, h, &r.user, BRIGHT(), true, TXT_SMALL, false);
    let qx = x;
    x += LOCK_COLS[1].1;
    text_at(ui, x, LOCK_COLS[2].1, y1, h, &r.db, BRIGHT(), true, TXT_SMALL, false);
    x += LOCK_COLS[2].1;
    let g = ui.painter().layout_no_wrap("Blocker".into(), FontId::proportional(TXT_SMALL), ALERT());
    let bw = g.size().x;
    ui.painter().galley(egui::pos2(x + PAD, y1 - g.size().y / 2.0 + optical_dy(&g)), g, ALERT());
    let (what, color) = if r.hidden {
        (String::new(), MUTED())
    } else if warn {
        (format!("{} for {}", r.state, span(r.state_secs.unwrap_or(0.0))), WARNING())
    } else if r.active() {
        (format!("active for {}", span(r.query_secs.unwrap_or(0.0))), SUCCESS())
    } else {
        (r.state.clone(), MUTED())
    };
    let buttons_w = 260.0;
    text_at(ui, x + bw + 4.0, (rect.right() - buttons_w - x - bw).max(40.0), y1, h, &what, color, false, TXT_SMALL, false);
    text_at(ui, qx, (rect.right() - buttons_w - qx).max(60.0), y2, h, &r.line, if r.hidden { FAINT() } else { MUTED() }, true, TXT_SMALL, false);
    let _ = query_w;
    let area = egui::Rect::from_min_max(egui::pos2(rect.right() - buttons_w, rect.top() + 9.0), egui::pos2(rect.right() - 8.0, rect.bottom() - 9.0));
    let mut k = ui.new_child(egui::UiBuilder::new().max_rect(area).layout(egui::Layout::right_to_left(egui::Align::Center)));
    match confirm.clone().filter(|c| c.s.pid == r.pid) {
        Some(c) => {
            // Drawn where Terminate was, so the guard keeps a double click from sending it.
            let b = primary(&mut k, if c.terminate { "Terminate" } else { "Cancel query" }, ALERT());
            if confirmed(&k, &b, c.at) {
                acts.push(PgServerA::Signal { pid: r.pid });
                *confirm = None;
            }
            if k.button("Keep").clicked() {
                acts.push(PgServerA::Disarm);
                *confirm = None;
            }
            k.label(RichText::new(if c.terminate { format!("End session {}?", r.pid) } else { format!("Cancel the query of {}?", r.pid) }).size(TXT_SMALL).color(ALERT()));
        }
        None => {
            let term = k.add_enabled_ui(can_signal(&s.own, r, true), |ui| danger(ui, "Terminate")).inner;
            let cancel = k.add_enabled(can_signal(&s.own, r, false), egui::Button::new("Cancel query"));
            for (resp, terminate) in [(&term, true), (&cancel, false)] {
                if s.writable {
                    if resp.clicked() {
                        *confirm = Some(PgConfirm { s: r.clone(), terminate, at: arm_time(&k) });
                        acts.push(PgServerA::ArmSignal { pid: r.pid, terminate, at: arm_time(&k) });
                        acts.push(PgServerA::Select(r.pid));
                    }
                } else if ask_unlock(&k, resp, name, &if terminate { format!("terminate session {}", r.pid) } else { format!("cancel the query of {}", r.pid) }) {
                    acts.push(PgServerA::Unlock);
                }
            }
        }
    }
}

fn waiter_row(ui: &mut egui::Ui, r: &Session, depth: usize, w: f32, query_w: f32) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 0.0, BG());
    // The tree line: a stem from the row above and a tick into this one, one step in per level.
    let indent = 14.0 * depth as f32;
    let tx = rect.left() + PAD + indent - 8.0;
    let stroke = Stroke::new(1.0, FAINT());
    p.line_segment([egui::pos2(tx, rect.top()), egui::pos2(tx, rect.center().y)], stroke);
    p.line_segment([egui::pos2(tx, rect.center().y), egui::pos2(tx + 6.0, rect.center().y)], stroke);
    let cy = rect.center().y;
    let mut x = rect.left();
    text_at(ui, x + indent, LOCK_COLS[0].1 - indent, cy, ROW_H, &r.pid.to_string(), C_NUMBER(), true, TXT_SMALL, false);
    x += LOCK_COLS[0].1;
    text_at(ui, x, LOCK_COLS[1].1, cy, ROW_H, &r.user, BRIGHT(), true, TXT_SMALL, false);
    x += LOCK_COLS[1].1;
    text_at(ui, x, LOCK_COLS[2].1, cy, ROW_H, &r.db, BRIGHT(), true, TXT_SMALL, false);
    x += LOCK_COLS[2].1;
    let strong = matches!(r.lock_mode.as_str(), "AccessExclusiveLock" | "ExclusiveLock");
    text_at(ui, x, LOCK_COLS[3].1, cy, ROW_H, &r.lock_mode, if strong { WARNING() } else { TEXT() }, true, TXT_SMALL, false);
    x += LOCK_COLS[3].1;
    text_at(ui, x, LOCK_COLS[4].1, cy, ROW_H, &r.lock_on, TEXT(), true, TXT_SMALL, false);
    x += LOCK_COLS[4].1;
    text_at(ui, x, WAIT_W, cy, ROW_H, &r.query_secs.map(mss).unwrap_or_default(), C_NUMBER(), true, TXT_SMALL, true);
    x += WAIT_W;
    text_at(ui, x, query_w, cy, ROW_H, &r.line, if r.hidden { FAINT() } else { MUTED() }, true, TXT_SMALL, false);
    if resp.hovered() && !r.hidden && !r.query.is_empty() && resp.hover_pos().is_some_and(|h| h.x >= x) {
        let long: String = r.query.chars().take(1200).collect();
        resp.on_hover_text(long);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use db::sql::pg::server::Answer;
    use quarry_core::state::sql::server::POLL;
    use std::time::Instant;

    fn row(pid: u64, user: &str, state: &str, query: &str) -> Session {
        let mut s = Session::new(pid, user, "10.0.0.1", "shop", "app", state, query, "client backend");
        (s.backend_start, s.signal) = ("2026-11-12 10:00:00+00".into(), true);
        s
    }

    #[test]
    fn signals_are_offered_only_where_they_can_land() {
        let none = HashMap::new();
        let active = row(7, "app", "active", "SELECT pg_sleep(9)");
        let idle_tx = row(8, "app", "idle in transaction", "UPDATE t SET a = 1");
        assert!(can_signal(&none, &active, false) && can_signal(&none, &active, true));
        // An idle session has no statement to cancel but can be ended.
        assert!(!can_signal(&none, &idle_tx, false) && can_signal(&none, &idle_tx, true));
        // Quarry's own (the reading connection, a pooled one, or a Query run Stop ends) is never offered.
        let ours = Session { ours: true, ..active.clone() };
        assert!(!can_signal(&none, &ours, true) && !can_signal(&none, &ours, false));
        let own: HashMap<u64, String> = [(7, "Query tab".to_string())].into();
        assert!(!can_signal(&own, &active, true));
        // A row the role may not see or signal, and a server process, get neither.
        let hidden = Session::new(9, "other", "", "shop", "app", "", "<insufficient privilege>", "");
        assert!(hidden.hidden && !can_signal(&none, &Session { signal: true, ..hidden }, true));
        assert!(!can_signal(&none, &Session { signal: false, ..active.clone() }, true));
        assert!(!can_signal(&none, &Session { kind: "checkpointer".into(), ..active.clone() }, true));
    }

    #[test]
    fn a_pending_signal_dies_with_any_change_of_its_row() {
        let c = PgConfirm { s: row(9, "app", "active", "SELECT 1"), terminate: true, at: 0.0 };
        assert!(confirm_ok(&c, &[row(9, "app", "active", "SELECT 1")]));
        for other in [row(9, "app", "active", "SELECT 2"), row(9, "app", "idle", "SELECT 1"), row(9, "x", "active", "SELECT 1"), row(10, "app", "active", "SELECT 1"), Session { backend_start: "2026-11-12 11:00:00+00".into(), ..row(9, "app", "active", "SELECT 1") }] {
            assert!(!confirm_ok(&c, &[other]));
        }
    }

    #[test]
    fn an_overview_answer_never_moves_the_activity_poll() {
        let now = Instant::now();
        let mut s = PgServerWin::new(1, Vec::new());
        let due = now + POLL;
        (s.tab, s.act_next, s.ov_req, s.ov_wait) = (PgTab::Activity, Some(due), 5, true);
        // A failed Overview read that lands after the switch to Activity backs off the Overview only.
        s.take(5, Answer::Overview(Err("timed out".into())), now);
        assert_eq!(s.act_next, Some(due));
        assert!(s.ov_next.is_some_and(|t| t > due) && !s.ov_wait);
        // And the other way round.
        (s.act_req, s.act_wait) = (6, true);
        let ov = s.ov_next;
        s.take(6, Answer::Activity(Err("timed out".into())), now);
        assert_eq!(s.ov_next, ov);
    }

    #[test]
    fn a_failed_size_read_keeps_the_last_sizes() {
        let mut s = PgServerWin::new(1, Vec::new());
        s.sz_req = 3;
        s.take(3, Answer::Sizes(Ok(vec![("shop".into(), Some(10))])), Instant::now());
        s.sz_req = 4;
        s.take(4, Answer::Sizes(Err("canceling statement due to statement timeout".into())), Instant::now());
        assert_eq!(s.sizes, Some(vec![("shop".to_string(), Some(10))]));
        assert!(s.sizes_err.as_deref().is_some_and(|e| e.contains("timeout")));
    }

    #[test]
    fn hide_idle_keeps_open_transactions_and_hidden_rows() {
        let rows = vec![row(1, "app", "idle", ""), row(2, "app", "idle in transaction", "UPDATE t"), row(3, "app", "active", "SELECT 1"), Session::new(4, "other", "", "shop", "", "", "<insufficient privilege>", ""), Session::new(5, "", "", "", "", "", "", "checkpointer")];
        assert_eq!(shown(&rows, "", true).iter().map(|r| r.pid).collect::<Vec<_>>(), [2, 3, 4]);
        assert_eq!(shown(&rows, "", false).len(), 5);
        assert_eq!(shown(&rows, "UPDATE", false).iter().map(|r| r.pid).collect::<Vec<_>>(), [2]);
    }

    #[test]
    fn times_read_like_the_design() {
        assert_eq!(mss(724.4), "12:04");
        assert_eq!(mss(48.0), "0:48");
        assert_eq!(mss(3725.0), "1:02:05");
        assert_eq!(span(720.0), "12 min");
        assert_eq!(span(142.0), "2 min 22 s");
        assert_eq!(span(40.0), "40 s");
        assert_eq!(span(3900.0), "1 h 5 min");
    }

    #[test]
    fn units_and_sources_read_like_show() {
        assert_eq!(with_unit("16384", "8kB"), "128MB");
        assert_eq!(with_unit("4096", "kB"), "4MB");
        assert_eq!(with_unit("100", ""), "100");
        assert_eq!(with_unit("500", "ms"), "500ms");
        assert_eq!(with_unit("-1", "kB"), "-1kB");
        let s = |source: &str, name: &str| Setting { source: source.into(), lower: name.into(), ..Default::default() };
        assert_eq!(source_text(&s("database", "work_mem"), "jane", "analytics"), "database analytics");
        assert_eq!(source_text(&s("user", "search_path"), "reporting", "analytics"), "user reporting");
        assert_eq!(source_text(&s("client", "application_name"), "u", "d"), "session");
        let q = vec!["statement_timeout".to_string()];
        assert!(set_by_quarry(&s("session", "statement_timeout"), &q) && !set_by_quarry(&s("configuration file", "statement_timeout"), &q));
        let rows = vec![s("default", "a"), s("configuration file", "b_mem")];
        assert_eq!(shown_settings(&rows, "", true).len(), 1);
        assert_eq!(shown_settings(&rows, "MEM", false).len(), 1);
    }
}
