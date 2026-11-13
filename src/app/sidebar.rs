//! The sidebar: connection picker, database tree and the jump palette.

use super::*;

/// How well one lowercase query token matches one lowercase name.
pub(super) fn seg_score(t: &str, s: &str) -> Option<u32> {
    if s == t {
        Some(8)
    } else if s.starts_with(t) {
        Some(6)
    } else if s.match_indices(t).any(|(p, _)| s[..p].ends_with(['-', '_', '.', ' '])) {
        Some(4)
    } else if s.contains(t) {
        Some(2)
    } else {
        fuzzy(t, s, false).then_some(1)
    }
}

/// Typed without separators, like "shopord", a token can run across names. It must start where a
/// name starts, or "ord" would match every table under a connection called "shop-prod".
fn spans_names(t: &str, segs: &[&str]) -> bool {
    let Some(first) = t.chars().next() else { return false };
    segs.iter().enumerate().any(|(k, s)| s.starts_with(first) && fuzzy(t, &segs[k..].join("."), false))
}

/// Scores an entry's lowercase names (connection, database, collection) against the query
/// tokens, or None when a token matches nowhere. Each token counts its best name, so
/// "vairon travels" finds thetreep-api-vairon.travels. A collection whose own name matches
/// the last token gets a bonus, as people usually type the collection last.
pub(super) fn jump_score(tokens: &[String], segs: &[&str]) -> Option<u32> {
    let mut total = 0;
    for (n, t) in tokens.iter().enumerate() {
        // A scattered-letter match counts on the item's own name only: "ord" fuzzily matches the
        // connection "shop-prod", which would pull in every table under it.
        let best = segs.iter().enumerate().filter_map(|(i, s)| seg_score(t, s).filter(|&sc| sc > 1 || i + 1 == segs.len()).map(|sc| (sc, i))).max();
        let (sc, i) = match best {
            Some(b) => b,
            None if spans_names(t, segs) => (1, 0),
            None => return None,
        };
        total += sc;
        if n + 1 == tokens.len() && segs.len() >= 3 && i + 1 == segs.len() {
            total += 3;
        }
    }
    Some(total)
}

/// The lowercase words of a palette query; dots and slashes split names like a space.
pub(super) fn tokens(q: &str) -> Vec<String> {
    q.to_lowercase().split(|c: char| c.is_whitespace() || c == '.' || c == '/').filter(|t| !t.is_empty()).map(String::from).collect()
}

/// Palette entries matching `q` as (label, hint, target), best match first. With an empty query
/// they keep tree order, active connection first.
pub(super) fn jumps(conns: &[Conn], active: Option<u64>, q: &str) -> Vec<(String, String, Jump)> {
    const MAX: usize = 50;
    let tokens = tokens(q);
    let mut out = Vec::new();
    let mut order: Vec<&Conn> = conns.iter().collect();
    order.sort_by_key(|c| Some(c.meta.id) != active);
    for c in order {
        let id = c.meta.id;
        let bonus = u32::from(Some(id) == active);
        let name = c.meta.name.to_lowercase();
        let mut push = |segs: &[&str], label: String, hint: &str, j: Jump| {
            if let Some(s) = jump_score(&tokens, segs) {
                out.push((s + bonus, label, hint.to_string(), j));
            }
        };
        push(&[&name], c.meta.name.clone(), &c.masked_uri, Jump::Conn(id));
        for (db, colls) in &c.dbs {
            let dbl = db.to_lowercase();
            push(&[&name, &dbl], db.clone(), &c.meta.name, Jump::Db(id, db.clone()));
            if c.meta.engine == db::sql::Engine::Postgres {
                // Only the schemas that were opened have their tables loaded, and those are what the palette knows.
                let mut loaded: Vec<(&String, &Vec<String>)> = c.tables.get(db).into_iter().flatten().collect();
                loaded.sort_by(|a, b| a.0.cmp(b.0));
                for (schema, tables) in loaded {
                    for coll in tables {
                        push(&[&name, &dbl, &schema.to_lowercase(), &coll.to_lowercase()], format!("{db}.{schema}.{coll}"), &c.meta.name, Jump::Coll(Sel { conn: id, db: db.clone(), schema: schema.clone(), coll: coll.clone() }));
                    }
                }
                continue;
            }
            for coll in colls.iter().flatten() {
                push(&[&name, &dbl, &coll.to_lowercase()], format!("{db}.{coll}"), &c.meta.name, Jump::Coll(Sel { conn: id, db: db.clone(), coll: coll.clone(), ..Default::default() }));
            }
        }
    }
    if !tokens.is_empty() {
        out.sort_by_key(|(s, label, ..)| (std::cmp::Reverse(*s), label.len()));
    }
    out.truncate(MAX);
    out.into_iter().map(|(_, label, hint, j)| (label, hint, j)).collect()
}

/// Offered in the connection form; any other text is a custom tag.
pub(super) const TAG_PRESETS: [&str; 5] = ["Production", "Staging", "Development", "Internal", "Local"];

/// Production is red so it is never mistaken for a scratch server. Only the first word counts,
/// so custom tags like "Prod EU" keep their environment's color.
pub(super) fn tag_color(tag: &str) -> Color32 {
    let t = tag.trim().to_ascii_lowercase();
    match t.split(|c: char| !c.is_ascii_alphanumeric()).next().unwrap_or("") {
        "production" | "prod" | "live" => ALERT(),
        "staging" | "preprod" | "uat" => WARNING(),
        "development" | "dev" | "local" | "test" => SUCCESS(),
        "internal" => PRIMARY(),
        _ => C_KEYWORD(),
    }
}

pub(super) fn status_color(s: &Status) -> Color32 {
    match s {
        Status::Ready => SUCCESS(),
        Status::Connecting => WARNING(),
        Status::Failed(_) => ALERT(),
        Status::Idle => FAINT(),
    }
}

/// One line of text cut with an ellipsis at `w`.
pub(super) fn truncated(p: &egui::Painter, text: &str, font: FontId, color: Color32, w: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(w.max(0.0));
    p.layout_job(job)
}

/// Paints a tag badge at most `max_w` wide whose right edge is at `right`, and returns its left edge.
pub(super) fn paint_tag(p: &egui::Painter, right: f32, cy: f32, tag: &str, max_w: f32) -> f32 {
    let g = truncated(p, &tag.to_uppercase(), FontId::proportional(TXT_CAPTION), Color32::PLACEHOLDER, (max_w - 14.0).min(140.0));
    let r = egui::Rect::from_min_max(egui::pos2(right - g.size().x - 14.0, cy - 9.0), egui::pos2(right, cy + 9.0));
    paint_chip(p, r, g, tag_color(tag), true, 0);
    r.left()
}

/// `name` cut to fit before its tag badge, both centered on `cy` within `x`.
pub(super) fn paint_name_tag(p: &egui::Painter, x: egui::Rangef, cy: f32, name: &str, font: FontId, color: Color32, tag: &str) {
    let name_right = if tag.is_empty() { x.max } else { paint_tag(p, x.max, cy, tag, x.span() * 0.4) - 8.0 };
    let g = truncated(p, name, font, color, name_right - x.min);
    p.galley(egui::pos2(x.min, cy - g.size().y / 2.0), g, color);
}

/// Typing "none" means no tag, the same as the None chip.
pub(super) fn clean_tag(tag: &str) -> &str {
    let t = tag.trim();
    if t.eq_ignore_ascii_case("none") { "" } else { t }
}

/// A saved connection in the picker: status, name and tag, then the masked address.
pub(super) fn conn_row(ui: &mut egui::Ui, c: &Conn, active: bool) -> egui::Response {
    side_row(ui, 48.0, 4.0, active, &c.meta.name, |p, r| {
        let (left, right, top) = (r.left(), r.right() - 2.0, r.top() + 17.0);
        p.circle_filled(egui::pos2(left + 3.5, top), 3.5, status_color(&c.status));
        paint_name_tag(p, egui::Rangef::new(left + 16.0, right), top, &c.meta.name, FontId::proportional(TXT_TITLE), if active { BRIGHT() } else { TEXT() }, &c.meta.tag);
        let uri = if c.masked_uri.is_empty() { "no address saved" } else { &c.masked_uri };
        let addr = truncated(p, uri, FontId::monospace(TXT_CAPTION), FAINT(), right - left - 16.0);
        p.galley(egui::pos2(left + 16.0, r.top() + 34.0 - addr.size().y / 2.0), addr, FAINT());
    })
}

impl App {
    pub(super) fn sidebar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("tree")
            .default_size(SIDEBAR_W)
            .size_range(200.0..=480.0)
            .resizable(true)
            .frame(egui::Frame::new().fill(PANEL()).inner_margin(egui::Margin { left: 12, right: 12, top: 12, bottom: 8 }))
            .show(ui, |ui| match self.prefs.side_mode() {
                // A panel that is not built yet shows the explorer, so a setting from a newer build never leaves the sidebar empty.
                SideMode::Connections if conns_panel::READY => self.conns_panel(ui),
                SideMode::Library if library::panel::READY => self.library_panel(ui),
                _ => self.explorer_panel(ui),
            });
    }

    /// The database explorer: connection picker, tree filter and the tree of the active connection.
    pub(super) fn explorer_panel(&mut self, ui: &mut egui::Ui) {
        ui.label(LayoutJob::single_section("DATABASE EXPLORER".into(), ui_fmt(10.5, MUTED(), W_SEMIBOLD, 0.6)));
        ui.add_space(6.0);
        self.conn_picker(ui);
        ui.add_space(8.0);
        let hint = if self.active.and_then(|id| self.conn(id)).is_some_and(|c| c.meta.engine.is_sql()) { "Filter tables" } else { "Filter collections" };
        let r = ui.add(
            egui::TextEdit::singleline(&mut self.tree_filter)
                .id(egui::Id::unique("tree-filter"))
                .hint_text(RichText::new(hint).color(FAINT()))
                .desired_width(f32::INFINITY)
                .margin(egui::Margin { left: 10, right: 28, top: 5, bottom: 5 }),
        );
        if !r.has_focus() && self.tree_filter.is_empty() {
            let g = ui.painter().layout_job(LayoutJob::single_section("/".into(), mono_fmt(10.0, MUTED(), 400.0)));
            let kr = egui::Rect::from_min_size(egui::pos2(r.rect.right() - 24.0, r.rect.center().y - 8.0), egui::vec2(18.0, 16.0));
            paint_kbd(ui.painter(), kr, g);
        }
        r.on_hover_text("Press / or Cmd+Shift+F");
        ui.add_space(14.0);
        let sql_conn = self.active.and_then(|id| self.conn(id)).filter(|c| c.meta.engine.is_sql() && c.status == Status::Ready).map(|c| (c.meta.id, c.writable, c.meta.engine == db::sql::Engine::Postgres));
        let mut new_db = None;
        ui.horizontal(|ui| {
            ui.label(LayoutJob::single_section("Databases".into(), ui_fmt(10.5, FAINT(), W_SEMIBOLD, 0.8)));
            if let Some((id, writable, pg)) = sql_conn {
                // PostgreSQL's create asks to unlock on a locked connection; MySQL's button stays disabled until it is unlocked.
                let on = writable || pg;
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if plus_button(ui, on).on_hover_text(if writable { "New database…" } else if pg { "New database… (asks to unlock writes first)" } else { "Read-only: unlock writes to create a database" }).clicked() && on {
                        new_db = Some(id);
                    }
                });
            }
        });
        if let Some(id) = new_db {
            self.side_act(id, SideAct::NewDb);
        }
        ui.add_space(2.0);
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.db_tree(ui));
    }

    pub(super) fn conn_picker(&mut self, ui: &mut egui::Ui) {
        let cur = self.active.and_then(|id| self.conn(id));
        let (name, tag, dot) = match cur {
            Some(c) => (c.meta.name.clone(), c.meta.tag.clone(), status_color(&c.status)),
            None => ("Add a connection".to_string(), String::new(), FAINT()),
        };
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), Sense::click());
        field_frame(ui, rect, &resp, false);
        let p = ui.painter();
        p.circle_filled(egui::pos2(rect.left() + 14.0, rect.center().y), 4.0, dot);
        chevron(p, egui::pos2(rect.right() - 14.0, rect.center().y), true, if resp.hovered() { MUTED() } else { FAINT() });
        paint_name_tag(p, egui::Rangef::new(rect.left() + 26.0, rect.right() - 28.0), rect.center().y, &name, FontId::proportional(TXT_TITLE), BRIGHT(), &tag);
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Connections"));
        if cur.is_none() && resp.clicked() {
            return self.new_conn_modal();
        }
        let mut pick = None;
        let mut new = false;
        let mut cloud = false;
        let mut remove = None;
        let mut edit = None;
        let mut refresh = None;
        let mut sql_users = None;
        egui::Popup::menu(&resp).width(rect.width().max(320.0)).show(|ui| {
            menu_look(ui);
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.spacing_mut().button_padding = egui::vec2(10.0, 6.0);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                caption(ui, "CONNECTIONS");
            });
            ui.add_space(2.0);
            // Only the list scrolls, so the actions below stay reachable however many connections there are.
            let room = (ui.ctx().content_rect().bottom() - ui.cursor().top() - 260.0).max(4.0 * 34.0);
            // The menu was not drawn last frame, so it just opened: bring the active connection into view once.
            let seen = egui::Id::unique("conn-menu-frame");
            let frame = ui.ctx().cumulative_frame_nr();
            let opened = ui.ctx().data(|d| d.get_temp::<u64>(seen)) != Some(frame.wrapping_sub(1));
            ui.ctx().data_mut(|d| d.insert_temp(seen, frame));
            egui::ScrollArea::vertical().id_salt("conn-menu").max_height(room).show(ui, |ui| {
                for c in &self.conns {
                    let on = Some(c.meta.id) == self.active;
                    let r = conn_row(ui, c, on);
                    if on && opened {
                        r.scroll_to_me(Some(egui::Align::Center));
                    }
                    if r.clicked() {
                        pick = Some(c.meta.id);
                    }
                }
            });
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(2.0);
            if ui.button("+  New connection…").clicked() {
                new = true;
            }
            if ui.button("Add from Cloud SQL…").on_hover_text("PostgreSQL instances your gcloud account can reach").clicked() {
                cloud = true;
            }
            if let Some(c) = cur {
                if ui.button(format!("Edit \"{}\"…", c.meta.name)).clicked() {
                    edit = Some(c.meta.id);
                }
                if ui.button("Refresh databases").on_hover_text("Reconnect and reload the list of databases and collections").clicked() {
                    refresh = Some(c.meta.id);
                }
                if c.meta.engine == db::sql::Engine::MySql && c.status == Status::Ready && ui.button("Users and privileges…").clicked() {
                    sql_users = Some(c.meta.id);
                }
                if c.meta.engine == db::sql::Engine::Postgres && c.status == Status::Ready && ui.button("Roles and privileges…").clicked() {
                    sql_users = Some(c.meta.id);
                }
                ui.add_space(2.0);
                ui.separator();
                ui.add_space(2.0);
                if danger(ui, format!("Remove \"{}\"…", c.meta.name)).clicked() {
                    remove = Some((c.meta.id, c.meta.name.clone()));
                }
            }
            ui.add_space(2.0);
        });
        if let Some(id) = edit {
            self.edit_conn(id);
        }
        if let Some(id) = sql_users {
            self.side_act(id, SideAct::SqlUsers);
        }
        if let Some(id) = refresh && !self.demo {
            self.connect(id);
        }
        if let Some(id) = pick {
            self.activate(id);
        }
        if new {
            self.new_conn_modal();
        }
        if cloud {
            self.open_cloud_browser(false);
        }
        if let Some((id, name)) = remove {
            self.modal = Some(Modal::Typed {
                title: format!("Remove the saved connection \"{name}\"? Its address is deleted from the Keychain. No data is touched."),
                expect: name,
                typed: String::new(),
                action: Danger::RemoveConnection(id),
            });
        }
    }

    /// Opens the connection form for a saved connection.
    pub(super) fn edit_conn(&mut self, id: u64) {
        if let Some(meta) = self.conn(id).map(|c| c.meta.clone()) {
            self.conn_test = None;
            self.test_info = None;
            // Opening the form with an empty address would overwrite the stored one on save,
            // unless there is none, which is when re-entering it is the way out.
            // Demo ids overlap real ones, so the demo must not show the user's real secrets.
            let uri = if self.ephemeral {
                self.conn(id).map(|c| c.masked_uri.clone()).unwrap_or_default()
            } else {
                match stored_uri(id) {
                    Ok(u) => u,
                    Err(keyring::Error::NoEntry) => String::new(),
                    Err(e) => return self.err(uri_err(e)),
                }
            };
            let stored = |what: &str| if self.ephemeral { String::new() } else { secret(id, what).unwrap_or_default() };
            let mut form = ConnForm {
                id: Some(id),
                name: meta.name.clone(),
                tag: meta.tag.clone(),
                engine: meta.engine,
                addr: if meta.engine.is_sql() { db::sql::Addr::parse(&uri).unwrap_or_default() } else { Default::default() },
                uri: if meta.engine.is_sql() { String::new() } else { uri },
                ssh_on: meta.ssh.is_some(),
                ssh: meta.ssh.clone().unwrap_or_default(),
                ssh_pwd: stored("ssh-password"),
                ssh_pass: stored("ssh-passphrase"),
                proxy_on: meta.proxy.is_some(),
                proxy: meta.proxy.clone().unwrap_or_default(),
                proxy_pwd: stored("proxy-password"),
                pg_key_pass: stored("pg-key-passphrase"),
                cloud_sql: meta.cloud_sql.clone(),
                show: false,
                testing: false,
                tab: Default::default(),
                ..Default::default()
            };
            form.set_advanced(&meta.advanced);
            self.modal = Some(Modal::Conn(Box::new(form)));
        }
    }

    pub(super) fn activate(&mut self, id: u64) {
        self.active = Some(id);
        if self.conn(id).is_some_and(|c| c.status == Status::Idle) && !self.demo {
            self.connect(id);
        }
    }

    /// Opens the explorer if it was collapsed from the rail or showing another panel, remembering that for the next launch.
    /// Callers reach for the tree: a jump, `/` or Cmd+Shift+F.
    pub(super) fn show_sidebar(&mut self) {
        if self.prefs.sidebar_hidden || self.prefs.side_mode() != SideMode::Explorer {
            self.prefs.sidebar_hidden = false;
            self.prefs.set_side_mode(SideMode::Explorer);
            self.save_prefs();
        }
    }

    pub(super) fn jump(&mut self, j: Jump, how: Open) {
        // The jump reveals its target in the tree, which must be on screen to show it.
        self.show_sidebar();
        let (id, db) = match &j {
            Jump::Conn(id) => (*id, None),
            Jump::Db(id, db) => (*id, Some(db.clone())),
            Jump::Coll(s) => (s.conn, Some(s.db.clone())),
        };
        self.activate(id);
        self.reveal = db.clone().map(|d| (id, d, if let Jump::Coll(s) = &j { Some(s.coll.clone()) } else { None }));
        // Reveal the target in the tree, which a leftover filter could hide.
        if let Some(db) = db {
            self.tree_filter.clear();
            self.open_dbs.insert((id, db));
        }
        if let Jump::Coll(sel) = j {
            self.open_collection(sel, how);
        }
    }

    pub(super) fn db_tree(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.active else {
            ui.add_space(8.0);
            ui.label(RichText::new("No connections yet.").color(MUTED()));
            return;
        };
        ui.spacing_mut().item_spacing.y = 2.0;
        let filter = self.tree_filter.to_lowercase();
        let cur_sel = self.t().map(|t| t.sel.clone());
        let mut toggle = None;
        let mut open = None;
        let mut side: Vec<SideAct> = Vec::new();
        let mut connect = false;
        let mut revealed = false;
        let mut lazy: Vec<String> = Vec::new();
        let mut folded: Vec<(u64, String, bool)> = Vec::new();
        let mut pick_schema: Option<(String, String)> = None;
        let mut retry: Option<String> = None;
        {
            let Some(c) = self.conn(id) else { return };
            match &c.status {
                Status::Ready => {}
                Status::Connecting => {
                    ui.horizontal(|ui| {
                        spin(ui);
                        ui.label(RichText::new("Connecting…").color(MUTED()));
                    });
                }
                st => {
                    ui.add_space(6.0);
                    // The main area explains a failure in full; here a short reason is enough.
                    if let Status::Failed(e) = st {
                        let why = match db::classify(e) {
                            db::ErrKind::SignIn => "Sign-in failed",
                            db::ErrKind::Unreachable => "Server not reachable",
                            db::ErrKind::Tls => "TLS failed",
                            db::ErrKind::Tunnel => "SSH tunnel failed",
                            db::ErrKind::CloudSql => "Cloud SQL proxy failed",
                            db::ErrKind::Lost => "Connection lost",
                            db::ErrKind::Privilege => "Not allowed to list databases",
                            _ => "Could not connect",
                        };
                        ui.label(medium(RichText::new(why).size(TXT_SMALL).color(ALERT()))).on_hover_text(e.as_str());
                    }
                    let label = if matches!(st, Status::Failed(_)) { "Retry" } else { "Connect" };
                    connect = primary(ui, label, PRIMARY()).clicked();
                }
            }
            let sql = c.meta.engine.is_sql();
            let pg = c.meta.engine == db::sql::Engine::Postgres;
            let engine = c.meta.engine;
            let writable = c.writable;
            for (db_name, colls) in &c.dbs {
                // A PostgreSQL database lists the tables of the schema picked in its switcher; `colls` holds its schemas.
                let picked = self.ns_of(id, db_name);
                let ns = ns_key(db_name, picked);
                let shown: Vec<&String> = if pg { c.tables_in(db_name, picked).into_iter().flatten().collect() } else { colls.iter().flatten().collect() };
                let mut matches: Vec<&String> = shown.into_iter().filter(|n| contains_ci(n, &filter)).collect();
                // Copies still running are not listed by the server yet, so they are drawn from `copying`.
                let pending: Vec<&String> = self.copying.iter().filter(|c| c.0 == id && c.1 == *db_name).map(|c| &c.2).collect();
                if !pending.is_empty() {
                    let extra: Vec<&String> = pending.iter().copied().filter(|n| !matches.contains(n) && contains_ci(n, &filter)).collect();
                    matches.extend(extra);
                    matches.sort();
                }
                if !filter.is_empty() && matches.is_empty() {
                    continue;
                }
                // Tables first, then views, each under its own caption.
                let db_views = self.views.of(id, &ns);
                let is_view_of = |n: &String| db_views.is_some_and(|v| v.contains(n.as_str()));
                let n_views = if sql { matches.iter().filter(|n| is_view_of(n)).count() } else { 0 };
                if sql {
                    matches.sort_by_key(|n| is_view_of(n));
                }
                let mut group: Option<bool> = None;
                let is_open = !filter.is_empty() || self.open_dbs.contains(&(id, db_name.clone()));
                if sql && is_open && colls.is_none() && c.status == Status::Ready && !self.sql_asked.contains(&(id, db_name.clone())) {
                    lazy.push(db_name.clone());
                }
                let dim = SYSTEM_DBS.contains(&db_name.as_str());
                let count = colls.as_ref().map(|v| v.len().to_string()).unwrap_or_else(|| "…".into());
                let r = side_row(ui, SIDE_ROW_H, 0.0, false, db_name, |p, r| {
                    chevron(p, egui::pos2(r.left() + 4.0, r.center().y), is_open, FAINT());
                    p.text(egui::pos2(r.left() + 18.0, r.center().y), egui::Align2::LEFT_CENTER, db_name, FontId::proportional(TXT_TITLE), if dim { FAINT() } else { TEXT() });
                    p.text(egui::pos2(r.right(), r.center().y), egui::Align2::RIGHT_CENTER, &count, FontId::monospace(TXT_CAPTION), FAINT());
                });
                if matches!(&self.reveal, Some((c, d, None)) if *c == id && d == db_name) {
                    r.scroll_to_me(Some(egui::Align::Center));
                    revealed = true;
                }
                if r.clicked() {
                    toggle = Some(db_name.clone());
                }
                r.context_menu(|ui| {
                    menu_look(ui);
                    ui.label(RichText::new(format!("Database: {db_name}")).size(TXT_SMALL).color(MUTED()));
                    if sql {
                        if ui.button("New query").on_hover_text("Open a SQL editor for this database").clicked() {
                            side.push(SideAct::NewQuery(db_name.clone()));
                        }
                        if ui.button("Refresh").clicked() {
                            side.push(SideAct::Refresh(db_name.clone()));
                        }
                        if ui.button(if engine == db::sql::Engine::Postgres { "Roles and privileges…" } else { "Users and privileges…" }).clicked() {
                            side.push(SideAct::SqlUsers);
                        }
                        {
                            ui.separator();
                            if ui.button("Export as SQL dump…").on_hover_text("Tables, views, triggers and routines as a script, with the rows").clicked() {
                                side.push(SideAct::SqlDump(db_name.clone(), None));
                            }
                            if ui.button("Run SQL file…").on_hover_text("Runs a dump or any script file here").clicked() {
                                side.push(SideAct::SqlRunFile(db_name.clone()));
                            }
                        }
                        if pg {
                            // PostgreSQL's items stay enabled on a locked connection and ask to unlock, like the structure editor does.
                            let own = c.sql.as_ref().and_then(|s| s.opts.database()) == Some(db_name.as_str());
                            let (sys_db, sys_schema) = (db::sql::pg::create::system_db(db_name), db::sql::pg::create::system_schema(picked));
                            ui.separator();
                            if ui.button("New schema…").clicked() {
                                side.push(SideAct::NewSchema(db_name.clone()));
                            }
                            if ui.add_enabled(!sys_schema, egui::Button::new("New table…")).on_disabled_hover_text("Belongs to the server").clicked() {
                                side.push(SideAct::NewTable(db_name.clone()));
                            }
                            if ui.add_enabled_ui(!sys_schema && !picked.is_empty(), |ui| danger_item(ui, format!("Drop schema {picked}…"))).inner.on_disabled_hover_text("Belongs to the server").clicked() {
                                side.push(SideAct::DropSchema(db_name.clone(), picked.to_string()));
                            }
                            if ui.add_enabled_ui(!sys_db && !own, |ui| danger_item(ui, "Drop database…")).inner.on_disabled_hover_text(if own { "This connection opens on it" } else { "Belongs to the server" }).clicked() {
                                side.push(SideAct::SqlDrop(db_name.clone(), String::new(), DropKind::Database));
                            }
                        } else {
                            // Creating and dropping are writes, so they stay disabled until the connection is unlocked.
                            let system = db::sql::create::SYSTEM_SCHEMAS.contains(&db_name.as_str());
                            ui.separator();
                            if ui.add_enabled(writable && !system, egui::Button::new("New table…")).on_disabled_hover_text(if system { "Belongs to the server" } else { "Read-only: unlock writes to create a table" }).clicked() {
                                side.push(SideAct::NewTable(db_name.clone()));
                            }
                            if ui.add_enabled_ui(writable && !system, |ui| danger_item(ui, "Drop database…")).inner.on_disabled_hover_text(if system { "Belongs to the server" } else { "Read-only: unlock writes to drop a database" }).clicked() {
                                side.push(SideAct::SqlDrop(db_name.clone(), String::new(), DropKind::Database));
                            }
                        }
                        return;
                    }
                    if ui.button("New collection…").clicked() {
                        side.push(SideAct::NewColl(db_name.clone(), false));
                    }
                    if ui.button("New view…").clicked() {
                        side.push(SideAct::NewColl(db_name.clone(), true));
                    }
                    if ui.button("Refresh").clicked() {
                        side.push(SideAct::Refresh(db_name.clone()));
                    }
                    if ui.button("Users and roles…").clicked() {
                        side.push(SideAct::Users);
                    }
                    if ui.button("Server status…").clicked() {
                        side.push(SideAct::Server);
                    }
                    if ui.button("Data relations…").clicked() {
                        side.push(SideAct::Relations(db_name.clone()));
                    }
                    ui.separator();
                    if ui.button("Dump database…").clicked() {
                        side.push(SideAct::Dump(db_name.clone(), String::new()));
                    }
                    if ui.button("Restore into this database…").clicked() {
                        side.push(SideAct::Restore(db_name.clone()));
                    }
                    ui.separator();
                    if danger(ui, "Drop database…").clicked() {
                        side.push(SideAct::DropDb(db_name.clone()));
                    }
                });
                if is_open {
                    if pg && let Some(p) = self.pg_schema_row(ui, c, db_name, picked) {
                        pick_schema = Some((db_name.clone(), p));
                    }
                    // Nothing listed yet: a row says the list is on its way, or why it failed with a retry, so an open
                    // database never sits empty. A PostgreSQL database loads its schemas, then the picked schema's tables.
                    let loaded = if pg { c.pg.contains_key(db_name) && (picked.is_empty() || c.tables_in(db_name, picked).is_some()) } else { colls.is_some() };
                    let failed = self.side_err.get(&(id, db_name.clone())).or_else(|| self.side_err.get(&(id, ns.clone())));
                    if let Some(e) = failed {
                        let r = side_row(ui, SIDE_ROW_H, 18.0, false, "Could not list. Retry", |p, r| {
                            p.text(egui::pos2(r.left() + 6.0, r.center().y), egui::Align2::LEFT_CENTER, "Could not list. Retry", FontId::proportional(TXT_SMALL), ALERT());
                        });
                        if r.on_hover_text(e.as_str()).clicked() {
                            retry = Some(db_name.clone());
                        }
                    } else if !loaded && c.status == Status::Ready {
                        ui.horizontal(|ui| {
                            ui.add_space(24.0);
                            spin(ui);
                            ui.label(RichText::new("Loading…").size(TXT_SMALL).color(MUTED()));
                        });
                    }
                    let matches_len = matches.len();
                    let schema = if sql { self.sql_schema.get(&(id, ns.clone())) } else { None };
                    let est_rows = self.sql_rows.get(&(id, ns.clone()));
                    let group_hidden = [false, true].map(|v| sql && self.side_closed.contains(&(id, ns.clone(), v)));
                    for coll in matches {
                        let active = cur_sel.as_ref().is_some_and(|s| s.conn == id && s.db == *db_name && s.schema == picked && s.coll == *coll);
                        let is_view = is_view_of(coll);
                        let busy = pending.contains(&coll);
                        if sql && group != Some(is_view) {
                            group = Some(is_view);
                            let (label, n) = if is_view { ("VIEWS", n_views) } else { ("TABLES", matches_len - n_views) };
                            let (cap, hr) = ui.allocate_exact_size(egui::vec2(ui.available_width(), SIDE_ROW_H - 4.0), Sense::click());
                            let key = (id, ns.clone(), is_view);
                            if hr.clicked() {
                                folded.push(key.clone());
                            }
                            let hidden = group_hidden[is_view as usize];
                            let g = ui.painter().layout_job(LayoutJob::single_section(label.into(), ui_fmt(10.0, FAINT(), W_SEMIBOLD, 0.6)));
                            if state_of(&hr) > 0 {
                                ui.painter().rect_filled(cap, R_SM, hover_fill(state_of(&hr)));
                            }
                            // A painted chevron, since the glyph sits off center.
                            let c = egui::pos2(cap.left() + 12.0, cap.center().y + 1.0);
                            let tri = if hidden { vec![c + egui::vec2(-2.0, -3.5), c + egui::vec2(-2.0, 3.5), c + egui::vec2(2.5, 0.0)] } else { vec![c + egui::vec2(-3.5, -2.0), c + egui::vec2(3.5, -2.0), c + egui::vec2(0.0, 2.5)] };
                            ui.painter().add(egui::Shape::convex_polygon(tri, FAINT(), Stroke::NONE));
                            ui.painter().galley(egui::pos2(cap.left() + 22.0, cap.center().y - g.size().y / 2.0 + 2.0), g, FAINT());
                            ui.painter().text(egui::pos2(cap.right() - 8.0, cap.center().y + 2.0), egui::Align2::RIGHT_CENTER, n.to_string(), FontId::monospace(TXT_CAPTION), FAINT());
                            hr.on_hover_cursor(egui::CursorIcon::PointingHand);
                        }
                        if group_hidden[is_view as usize] {
                            continue;
                        }
                        let mut glyph_at = egui::Rect::NOTHING;
                        let mut cut = false;
                        let r = side_row(ui, SIDE_ROW_H, 18.0, active, coll, |p, r| {
                            // Collections are marked as documents, views with a hollow diamond so
                            // they read as derived data. A collection still being copied gets a spinner.
                            glyph_at = egui::Rect::from_center_size(egui::pos2(r.left() + 6.0, r.center().y), egui::Vec2::splat(TXT_SMALL));
                            if !busy && sql && !is_view {
                                table_glyph(p, egui::pos2(r.left() + 6.0, r.center().y), if active { PRIMARY() } else { FAINT() });
                            } else if !busy {
                                let (glyph, gc) = if is_view { ("◇", C_DATE()) } else { ("{}", if active { PRIMARY() } else { FAINT() }) };
                                p.text(egui::pos2(r.left(), r.center().y), egui::Align2::LEFT_CENTER, glyph, FontId::monospace(10.0), gc);
                            }
                            // The server's estimate, once the schema is loaded for autocomplete.
                            let est = if sql && !is_view { est_rows.and_then(|m| m.get(coll.as_str()).copied()).or_else(|| schema.and_then(|sc| sc.table(coll)).and_then(|t| t.rows)) } else { None };
                            let mut w = r.width() - 20.0;
                            if let Some(n) = est {
                                let g = p.layout_no_wrap(rows_short(n), FontId::monospace(TXT_CAPTION), FAINT());
                                p.galley(egui::pos2(r.right() - 8.0 - g.size().x, r.center().y - g.size().y / 2.0 + optical_dy(&g)), g.clone(), FAINT());
                                w -= g.size().x + 12.0;
                            }
                            // One line cut with an ellipsis: the row has a fixed height, so a wrapped name drew over its neighbours.
                            let mut job = egui::text::LayoutJob::simple_singleline(coll.clone(), FontId::proportional(TXT_TITLE), if active { BRIGHT() } else if is_view { C_DATE() } else { TEXT() });
                            job.wrap = egui::text::TextWrapping::truncate_at_width(w.max(0.0));
                            let galley = p.layout_job(job);
                            cut = galley.elided;
                            p.galley(egui::pos2(r.left() + 20.0, r.center().y - galley.size().y / 2.0), galley, TEXT());
                        });
                        // A collection still being copied does not exist yet, so it neither opens nor offers actions.
                        if busy {
                            spin_at(ui, glyph_at);
                            r.on_hover_text("Being copied");
                            continue;
                        }
                        let r = match (is_view, cut) {
                            (true, true) => r.on_hover_text(format!("{coll}\nView (read-only)")),
                            (true, false) => r.on_hover_text("View (read-only)"),
                            (false, true) => r.on_hover_text(coll.as_str()),
                            (false, false) => r,
                        };
                        if matches!(&self.reveal, Some((c, d, Some(k))) if *c == id && d == db_name && k == coll) {
                            r.scroll_to_me(Some(egui::Align::Center));
                            revealed = true;
                        }
                        let this = || Sel { conn: id, db: db_name.clone(), schema: picked.to_string(), coll: coll.clone() };
                        if new_tab_click(&r) {
                            open = Some((this(), Open::NewTab));
                        } else if r.clicked() {
                            open = Some((this(), Open::Here));
                        }
                        r.context_menu(|ui| {
                            menu_look(ui);
                            ui.label(RichText::new(format!("{}: {coll}", if is_view { "View" } else if sql { "Table" } else { "Collection" })).size(TXT_SMALL).color(MUTED()));
                            if ui.button("Open").clicked() {
                                open = Some((this(), Open::Here));
                            }
                            if ui.button("Open in new tab").clicked() {
                                open = Some((this(), Open::NewTab));
                            }
                            if sql {
                                if !is_view {
                                    ui.separator();
                                    if ui.button("Import CSV…").on_hover_text("Adds the rows of a CSV file to this table").clicked() {
                                        side.push(SideAct::SqlImport(db_name.clone(), coll.clone()));
                                    }
                                }
                                if ui.button("Export as SQL dump…").clicked() {
                                    side.push(SideAct::SqlDump(db_name.clone(), Some(coll.clone())));
                                }
                                if pg {
                                    let system = db::sql::pg::create::system_schema(picked);
                                    ui.separator();
                                    if ui.add_enabled_ui(!system, |ui| danger_item(ui, if is_view { "Drop view…" } else { "Drop table…" })).inner.on_disabled_hover_text("Belongs to the server").clicked() {
                                        side.push(SideAct::SqlDrop(db_name.clone(), coll.clone(), if is_view { DropKind::View } else { DropKind::Table }));
                                    }
                                } else {
                                    let system = db::sql::create::SYSTEM_SCHEMAS.contains(&db_name.as_str());
                                    let on = writable && !system;
                                    let note = if system { "Belongs to the server" } else { "Read-only: unlock writes to change this table" };
                                    ui.separator();
                                    if !is_view && ui.add_enabled(on, egui::Button::new("Empty table…")).on_disabled_hover_text(note).on_hover_text("Deletes every row and keeps the table").clicked() {
                                        side.push(SideAct::SqlDrop(db_name.clone(), coll.clone(), DropKind::Truncate));
                                    }
                                    if ui.add_enabled_ui(on, |ui| danger_item(ui, if is_view { "Drop view…" } else { "Drop table…" })).inner.on_disabled_hover_text(note).clicked() {
                                        side.push(SideAct::SqlDrop(db_name.clone(), coll.clone(), if is_view { DropKind::View } else { DropKind::Table }));
                                    }
                                }
                                return;
                            }
                            if ui.button("Statistics").clicked() {
                                side.push(SideAct::Stats(db_name.clone(), coll.clone()));
                            }
                            if is_view && ui.button("View definition").clicked() {
                                side.push(SideAct::ViewDef(db_name.clone(), coll.clone()));
                            }
                            if !is_view && ui.button("Analyze data relations").on_hover_text("Find fields that hold ids of other collections' documents").clicked() {
                                side.push(SideAct::Analyze(db_name.clone(), coll.clone()));
                            }
                            if ui.button("Compare with…").clicked() {
                                side.push(SideAct::Compare(db_name.clone(), coll.clone()));
                            }
                            if !is_view && ui.button("Dump collection…").clicked() {
                                side.push(SideAct::Dump(db_name.clone(), coll.clone()));
                            }
                            ui.separator();
                            if !is_view && ui.button("Rename…").clicked() {
                                side.push(SideAct::Rename(db_name.clone(), coll.clone()));
                            }
                            if !is_view && ui.button("Duplicate…").on_hover_text("Copies the documents and indexes under a new name").clicked() {
                                side.push(SideAct::Duplicate(db_name.clone(), coll.clone()));
                            }
                            if danger(ui, if is_view { "Drop view…" } else { "Drop collection…" }).clicked() {
                                side.push(SideAct::DropColl(db_name.clone(), coll.clone()));
                            }
                        });
                    }
                    ui.add_space(4.0);
                }
            }
        }
        if revealed {
            self.reveal = None;
        }
        for k in folded {
            if !self.side_closed.remove(&k) {
                self.side_closed.insert(k);
            }
        }
        for d in lazy {
            self.sql_asked.insert((id, d.clone()));
            self.side_act(id, SideAct::Refresh(d));
        }
        if connect {
            self.connect(id);
        }
        if let Some(d) = retry {
            let pre = ns_key(&d, "x");
            let pre = &pre[..pre.len() - 1];
            self.side_err.retain(|k, _| k.0 != id || (k.1 != d && !k.1.starts_with(pre)));
            self.sql_asked.insert((id, d.clone()));
            self.side_act(id, SideAct::Refresh(d));
        }
        if let Some((d, schema)) = pick_schema {
            self.pg_set_schema(id, &d, schema);
        }
        if let Some(d) = toggle && !self.open_dbs.remove(&(id, d.clone())) {
            self.open_dbs.insert((id, d));
        }
        if let Some((s, how)) = open {
            self.open_collection(s, how);
        }
        for a in side {
            self.side_act(id, a);
        }
    }

    /// The switcher row of an open PostgreSQL database, with the pick made this frame. Nothing until its schemas arrived.
    fn pg_schema_row(&self, ui: &mut egui::Ui, c: &Conn, db_name: &str, picked: &str) -> Option<String> {
        let list = &c.pg.get(db_name)?.schemas;
        let pick = schema_picker(ui, db_name, picked, list, self.demo_opens_schema_menu(ui.ctx()));
        ui.add_space(2.0);
        pick
    }

    /// Refresh on a database: its collections or tables listed again, and on SQL what autocomplete knew of it dropped.
    /// `db` is the sidebar's database, on PostgreSQL never a schema. Agents that change structure use it too. False
    /// when the connection is not open.
    pub(super) fn refresh_db(&mut self, conn: u64, db: &str) -> bool {
        let Some(c) = self.conn(conn) else { return false };
        match (c.sql.clone(), c.client.clone()) {
            (Some(sql), _) if sql.dialect == db::sql::Dialect::Postgres => self.pg_refresh(conn, db),
            (Some(sql), _) => {
                self.sql_refresh_schema(conn, db, "");
                self.worker.sql_tables(conn, sql, db.to_string());
            }
            (None, Some(client)) => self.worker.collections(conn, client, db.to_string()),
            (None, None) => return false,
        }
        true
    }

    pub(super) fn side_act(&mut self, conn: u64, a: SideAct) {
        let a = match a {
            SideAct::NewQuery(db) => return self.sql_new_query(conn, db),
            SideAct::NewDb => return self.sql_new_db(conn),
            SideAct::SqlUsers => return self.open_sql_users(conn),
            SideAct::NewTable(db) if self.is_pg(conn) => return self.pg_new_table(conn, db),
            SideAct::NewTable(db) => return self.sql_new_table(conn, db),
            SideAct::SqlDrop(db, table, kind) if self.is_pg(conn) => return self.pg_drop_ask(conn, db, table, kind),
            SideAct::SqlDrop(db, table, kind) => return self.sql_drop_ask(conn, db, table, kind),
            SideAct::NewSchema(db) => return self.pg_new_schema(conn, db),
            SideAct::DropSchema(db, schema) => return self.pg_drop_ask(conn, db, schema, DropKind::Schema),
            SideAct::SqlImport(db, table) => {
                // The schema the sidebar shows is the one the menu was opened on.
                let schema = self.sel_schema(&Sel { conn, db: db.clone(), schema: String::new(), coll: table.clone() });
                return self.open_sql_import(conn, db, schema, table);
            }
            SideAct::SqlDump(db, table) => return self.open_sql_dump(conn, db, table),
            SideAct::SqlRunFile(db) => return self.open_sql_run_file(conn, db),
            a => a,
        };
        if let SideAct::Refresh(db) = &a
            && self.conn(conn).is_some_and(|c| c.sql.is_some())
        {
            self.refresh_db(conn, db);
            return;
        }
        if self.conn(conn).is_some_and(|c| c.meta.engine.is_sql()) {
            return self.warn(if self.conn(conn).is_some_and(|c| c.sql.is_some()) { "Not available for SQL connections yet." } else { "Not connected. Connect to this server first." });
        }
        let Some(client) = self.conn(conn).and_then(|c| c.client.clone()) else {
            return self.warn_offline();
        };
        let writable = self.conn(conn).is_some_and(|c| c.writable);
        let needs_write = matches!(a, SideAct::NewColl(..) | SideAct::DropDb(_) | SideAct::Rename(..) | SideAct::Duplicate(..) | SideAct::DropColl(..) | SideAct::Restore(_));
        if needs_write && !writable {
            return self.modal = Some(Modal::Unlock(conn, None));
        }
        match a {
            SideAct::NewColl(db, view) => self.modal = Some(Modal::NewCollection { conn, db, name: String::new(), view, source: String::new(), pipeline: "[\n  { $match: {} }\n]".into() }),
            SideAct::Refresh(db) => {
                self.refresh_db(conn, &db);
            }
            SideAct::Users => {
                self.users = Some(UsersWin { conn, new_db: "admin".into(), ..Default::default() });
                self.load_users(conn);
            }
            SideAct::Server => {
                self.req += 1;
                self.server = Some(ServerWin::new(conn, self.req));
            }
            SideAct::DropDb(db) => {
                self.modal = Some(Modal::Typed { title: format!("Drop the whole database {db} with all its collections? This cannot be undone."), expect: db.clone(), typed: String::new(), action: Danger::DropDatabase { conn, db } })
            }
            SideAct::Stats(db, coll) => self.worker.coll_stats(client, db, coll, "stats".into()),
            SideAct::ViewDef(db, coll) => self.worker.command(client, db, doc! { "listCollections": 1, "filter": { "name": coll } }, "viewdef"),
            SideAct::Rename(db, coll) => self.modal = Some(Modal::RenameColl { conn, db, name: coll.clone(), coll }),
            SideAct::Duplicate(db, coll) => self.modal = Some(Modal::DuplicateColl { conn, db, name: format!("{coll}_copy"), coll }),
            SideAct::Dump(db, coll) => {
                let name = if coll.is_empty() { db.clone() } else { format!("{db}.{coll}") };
                let path = dirs::download_dir().unwrap_or_default().join(format!("{name}.archive.gz")).display().to_string();
                self.modal = Some(Modal::Tool(Box::new(ToolForm { conn, from_db: db.clone(), db, coll, path, gzip: true, ..Default::default() })));
            }
            SideAct::Restore(db) => self.modal = Some(Modal::Tool(Box::new(ToolForm { conn, from_db: db.clone(), db, restore: true, gzip: true, ..Default::default() }))),
            SideAct::Compare(db, coll) => {
                self.open_collection(Sel { conn, db: db.clone(), coll, ..Default::default() }, Open::Here);
                let t = self.tm().unwrap();
                t.view = View::Compare;
                if t.compare.conn == 0 {
                    (t.compare.conn, t.compare.db) = (conn, db);
                }
            }
            SideAct::Relations(db) => self.modal = Some(Modal::Relations { conn, db }),
            SideAct::Analyze(db, coll) => self.analyze_relations(conn, client, db, coll),
            SideAct::NewQuery(_) | SideAct::SqlImport(..) | SideAct::SqlDump(..) | SideAct::SqlRunFile(_) | SideAct::NewDb | SideAct::NewTable(_) | SideAct::SqlDrop(..) | SideAct::NewSchema(_) | SideAct::DropSchema(..) | SideAct::SqlUsers => {}
            SideAct::DropColl(db, coll) => {
                self.modal = Some(Modal::Typed { title: format!("Drop {db}.{coll} with all its documents and indexes? This cannot be undone."), expect: coll.clone(), typed: String::new(), action: Danger::DropCollAt { conn, db, coll } })
            }
        }
    }
}

/// Whether `name` holds `needle`, which is already lower case, ignoring case. Names are mostly ASCII, so this compares bytes
/// instead of lower-casing every name on every frame the filter is on.
fn contains_ci(name: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if name.is_ascii() && needle.is_ascii() {
        return name.as_bytes().windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle.as_bytes()));
    }
    name.to_lowercase().contains(needle)
}

/// The schema switcher under an open PostgreSQL database: the schema shown in the tree, and a menu to pick another with
/// each one's table count and a switch for the system schemas. Returns the schema picked this frame.
fn schema_picker(ui: &mut egui::Ui, db_name: &str, picked: &str, list: &[db::sql::PgSchema], auto_open: bool) -> Option<String> {
    // One switch for every switcher, kept in egui memory since only this widget reads it.
    let system_id = egui::Id::NULL.with("pg-show-system");
    let mut show_system: bool = ui.data(|d| d.get_temp(system_id)).unwrap_or(false);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), PICK_H), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if auto_open {
        egui::Popup::open_id(ui.ctx(), egui::Popup::default_response_id(&resp));
    }
    let mut pick = None;
    let menu = egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).width(rect.width() - PICK_INDENT).show(|ui| {
        menu_look(ui);
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(PICK_PAD);
            caption(ui, format!("SCHEMAS IN {db_name}"));
        });
        ui.add_space(2.0);
        for s in super::sql::ns::listed_schemas(list, show_system) {
            if menu_row_in(ui, &s.name, if s.system { MUTED() } else { TEXT() }, s.name == picked, &s.tables.to_string(), "", false).clicked() {
                pick = Some(s.name.clone());
                ui.close();
            }
        }
        ui.add_space(2.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.add_space(PICK_PAD);
            ui.label(RichText::new("Show system schemas").size(TXT_SMALL).color(MUTED()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(PICK_PAD);
                toggle(ui, &mut show_system);
            });
        });
        ui.add_space(2.0);
    });
    ui.data_mut(|d| d.insert_temp(system_id, show_system));
    let open = menu.is_some();
    let body = egui::Rect::from_min_max(egui::pos2(rect.left() + PICK_INDENT, rect.top() + 1.0), egui::pos2(rect.right(), rect.bottom() - 1.0));
    field_frame(ui, body, &resp, open);
    let p = ui.painter();
    let label = p.layout_no_wrap("schema".into(), FontId::proportional(TXT_CAPTION), FAINT());
    p.galley(egui::pos2(body.left() + PICK_PAD, body.center().y - label.size().y / 2.0 + optical_dy(&label)), label.clone(), FAINT());
    let name = truncated(p, picked, FontId::proportional(TXT_BODY), BRIGHT(), body.width() - PICK_PAD - label.size().x - PICK_GAP - PICK_CHEVRON);
    p.galley(egui::pos2(body.left() + PICK_PAD + label.size().x + PICK_GAP, body.center().y - name.size().y / 2.0 + optical_dy(&name)), name, BRIGHT());
    chevron(p, egui::pos2(body.right() - 12.0, body.center().y), true, if resp.hovered() || open { MUTED() } else { FAINT() });
    focus_ring(ui, &resp, body, R_MD as f32);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, format!("Schema {picked}")));
    pick
}

/// A small painted plus, since the glyph from a fallback font sits off center. Dimmed while writes are locked.
fn plus_button(ui: &mut egui::Ui, enabled: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), Sense::click());
    let state = if enabled { state_of(&r) } else { 0 };
    let color = if !enabled { tint(FAINT(), 120) } else if state > 0 { BRIGHT() } else { MUTED() };
    ui.painter().rect_filled(rect, R_SM, hover_fill(state));
    let (c, s) = (rect.center(), Stroke::new(1.4, color));
    ui.painter().line_segment([c + egui::vec2(-4.5, 0.0), c + egui::vec2(4.5, 0.0)], s);
    ui.painter().line_segment([c + egui::vec2(0.0, -4.5), c + egui::vec2(0.0, 4.5)], s);
    focus_ring(ui, &r, rect, R_SM as f32);
    r.on_hover_cursor(if enabled { egui::CursorIcon::PointingHand } else { egui::CursorIcon::NotAllowed })
}

/// A row estimate in few characters: 842, 4.4k, 1.2M. Under 10,000 it is exact, so it never rounds up a count.
fn rows_short(n: u64) -> String {
    match n {
        0..=9_999 => n.to_string(),
        10_000..=999_999 => format!("{}k", n / 1000),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_estimates_are_short() {
        assert_eq!([842, 9_999, 12_345, 1_250_000].map(rows_short), ["842", "9999", "12k", "1.2M"]);
    }

    #[test]
    fn postgres_jumps_name_the_schema() {
        let mut c = Conn { dbs: vec![("shop".into(), Some(vec!["public".into(), "billing".into()]))], ..Conn::new(ConnMeta { id: 1, name: "pg".into(), engine: db::sql::Engine::Postgres, ..Default::default() }, Status::Ready) };
        c.tables.entry("shop".into()).or_default().insert("billing".into(), vec!["invoices".into()]);
        c.tables.entry("shop".into()).or_default().insert("public".into(), vec!["orders".into()]);
        let all = jumps(&[c], Some(1), "");
        let labels: Vec<&str> = all.iter().map(|j| j.0.as_str()).collect();
        assert_eq!(labels, ["pg", "shop", "shop.billing.invoices", "shop.public.orders"]);
        let Jump::Coll(sel) = &all[2].2 else { panic!("a table jumps to a table") };
        assert_eq!((sel.db.as_str(), sel.schema.as_str(), sel.coll.as_str()), ("shop", "billing", "invoices"));
    }

    #[test]
    fn palette_jumps() {
        let conn = |id, name: &str, status, dbs| Conn { dbs, ..Conn::new(ConnMeta { id, name: name.into(), ..Default::default() }, status) };
        let conns = [conn(1, "prod", Status::Ready, vec![("shop".into(), Some(vec!["users".into(), "orders".into()]))]), conn(2, "staging", Status::Idle, Vec::new())];
        let labels = |active, q| jumps(&conns, active, q).into_iter().map(|j| j.0).collect::<Vec<_>>();
        assert_eq!(labels(Some(2), ""), ["staging", "prod", "shop", "shop.users", "shop.orders"]);
        assert_eq!(labels(Some(1), "users"), ["shop.users"]);
        // Letters scattered across names ("o" in shop, "r" in users) are no match.
        assert_eq!(labels(Some(1), "or"), ["shop.orders"]);
        assert_eq!(labels(Some(1), "prod users")[0], "shop.users");
        assert_eq!(labels(Some(1), "shop")[0], "shop");
        // Typed without a separator, a word may still run across names from the start of one.
        assert_eq!(labels(Some(1), "shopord"), ["shop.orders"]);

        // "ord" is scattered through "shop-prod": the connection itself matches, every collection under it does not.
        let conns = [conn(1, "shop-prod", Status::Ready, vec![("shop".into(), Some(vec!["customers".into(), "orders".into()]))])];
        assert_eq!(jumps(&conns, Some(1), "ord").into_iter().map(|j| j.0).collect::<Vec<_>>(), ["shop.orders", "shop-prod"]);

        let conns = [conn(1, "local", Status::Ready, vec![("thetreep-api-main".into(), Some(vec!["travels".into()])), ("thetreep-api-vairon".into(), Some(vec!["travels".into(), "users".into()]))])];
        let labels = |q| jumps(&conns, Some(1), q).into_iter().map(|j| j.0).collect::<Vec<_>>();
        assert_eq!(labels("vairon travels"), ["thetreep-api-vairon.travels"]);
        assert_eq!(labels("travels")[..2], ["thetreep-api-main.travels", "thetreep-api-vairon.travels"]);
    }
}
