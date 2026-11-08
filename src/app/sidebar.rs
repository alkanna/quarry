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

/// Scores an entry's lowercase names (connection, database, collection) against the query
/// tokens, or None when a token matches nowhere. Each token counts its best name, so
/// "vairon travels" finds thetreep-api-vairon.travels. A collection whose own name matches
/// the last token gets a bonus, as people usually type the collection last.
pub(super) fn jump_score(tokens: &[String], segs: &[&str]) -> Option<u32> {
    let mut total = 0;
    for (n, t) in tokens.iter().enumerate() {
        let best = segs.iter().enumerate().filter_map(|(i, s)| seg_score(t, s).map(|sc| (sc, i))).max();
        let (sc, i) = match best {
            Some(b) => b,
            // Typed without separators, like "shopord", a token can span names.
            None if fuzzy(t, &segs.join("."), false) => (1, 0),
            None => return None,
        };
        total += sc;
        if n + 1 == tokens.len() && segs.len() == 3 && i == 2 {
            total += 3;
        }
    }
    Some(total)
}

/// Palette entries matching `q` as (label, hint, target), best match first. With an empty query
/// they keep tree order, active connection first.
pub(super) fn jumps(conns: &[Conn], active: Option<u64>, q: &str) -> Vec<(String, String, Jump)> {
    const MAX: usize = 50;
    let q = q.to_lowercase();
    let tokens: Vec<String> = q.split(|c: char| c.is_whitespace() || c == '.' || c == '/').filter(|t| !t.is_empty()).map(String::from).collect();
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
            for coll in colls.iter().flatten() {
                push(&[&name, &dbl, &coll.to_lowercase()], format!("{db}.{coll}"), &c.meta.name, Jump::Coll(Sel { conn: id, db: db.clone(), coll: coll.clone() }));
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
    side_row(ui, 48.0, 4.0, active, |p, r| {
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
            .frame(egui::Frame::new().fill(PANEL()).inner_margin(egui::Margin { left: 10, right: 10, top: 12, bottom: 10 }))
            .show(ui, |ui| {
                self.conn_picker(ui);
                ui.add_space(10.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.tree_filter)
                        .id(egui::Id::unique("tree-filter"))
                        .hint_text("Find a collection")
                        .desired_width(f32::INFINITY)
                        .margin(egui::Margin::symmetric(10, 6)),
                );
                ui.add_space(12.0);
                caption(ui, "DATABASES");
                ui.add_space(2.0);
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.db_tree(ui));
            });
    }

    pub(super) fn conn_picker(&mut self, ui: &mut egui::Ui) {
        let cur = self.active.and_then(|id| self.conn(id));
        let (name, tag, dot) = match cur {
            Some(c) => (c.meta.name.clone(), c.meta.tag.clone(), status_color(&c.status)),
            None => ("Add a connection".to_string(), String::new(), FAINT()),
        };
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), Sense::click());
        let p = ui.painter();
        let fill = if resp.is_pointer_button_down_on() { shade(HOVER(), 0.08) } else if resp.hovered() { HOVER() } else { RAISED() };
        p.rect(rect, R_MD, fill, Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
        p.circle_filled(egui::pos2(rect.left() + 16.0, rect.center().y), 4.0, dot);
        chevron(p, egui::pos2(rect.right() - 16.0, rect.center().y), true, MUTED());
        paint_name_tag(p, egui::Rangef::new(rect.left() + 28.0, rect.right() - 30.0), rect.center().y, &name, FontId::proportional(TXT_TITLE), BRIGHT(), &tag);
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        if cur.is_none() && resp.clicked() {
            return self.new_conn_modal();
        }
        let mut pick = None;
        let mut new = false;
        let mut remove = None;
        let mut edit = None;
        let mut refresh = None;
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
            for c in &self.conns {
                if conn_row(ui, c, Some(c.meta.id) == self.active).clicked() {
                    pick = Some(c.meta.id);
                }
            }
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(2.0);
            if ui.button("+  New connection…").clicked() {
                new = true;
            }
            if let Some(c) = cur {
                if ui.button(format!("Edit \"{}\"…", c.meta.name)).clicked() {
                    edit = Some(c.meta.id);
                }
                if ui.button("Refresh databases").on_hover_text("Reconnect and reload the list of databases and collections").clicked() {
                    refresh = Some(c.meta.id);
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
            if let Some(meta) = self.conn(id).map(|c| c.meta.clone()) {
                self.conn_test = None;
                // Opening the form with an empty address would overwrite the stored one on save,
                // unless there is none, which is when re-entering it is the way out.
                // Demo ids overlap real ones, so the demo must not show the user's real secrets.
                let uri = if self.demo {
                    self.conn(id).map(|c| c.masked_uri.clone()).unwrap_or_default()
                } else {
                    match stored_uri(id) {
                        Ok(u) => u,
                        Err(keyring::Error::NoEntry) => String::new(),
                        Err(e) => return self.err(uri_err(e)),
                    }
                };
                let stored = |what: &str| if self.demo { String::new() } else { secret(id, what).unwrap_or_default() };
                self.modal = Some(Modal::Conn(Box::new(ConnForm {
                    id: Some(id),
                    name: meta.name.clone(),
                    tag: meta.tag.clone(),
                    uri,
                    ssh_on: meta.ssh.is_some(),
                    ssh: meta.ssh.clone().unwrap_or_default(),
                    ssh_pwd: stored("ssh-password"),
                    ssh_pass: stored("ssh-passphrase"),
                    proxy_on: meta.proxy.is_some(),
                    proxy: meta.proxy.clone().unwrap_or_default(),
                    proxy_pwd: stored("proxy-password"),
                    show: false,
                    testing: false,
                })));
            }
        }
        if let Some(id) = refresh {
            if !self.demo {
                self.connect(id);
            }
        }
        if let Some(id) = pick {
            self.activate(id);
        }
        if new {
            self.new_conn_modal();
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

    pub(super) fn activate(&mut self, id: u64) {
        self.active = Some(id);
        if self.conn(id).is_some_and(|c| c.status == Status::Idle) && !self.demo {
            self.connect(id);
        }
    }

    pub(super) fn jump(&mut self, j: Jump) {
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
            self.open_collection(sel, false);
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
                    if let Status::Failed(e) = st {
                        ui.add(egui::Label::new(RichText::new(e).size(TXT_SMALL).color(ALERT())).wrap());
                    }
                    connect = primary(ui, "Connect", PRIMARY()).clicked();
                }
            }
            for (db_name, colls) in &c.dbs {
                let matches: Vec<&String> = colls.iter().flatten().filter(|n| filter.is_empty() || n.to_lowercase().contains(&filter)).collect();
                if !filter.is_empty() && matches.is_empty() {
                    continue;
                }
                let is_open = !filter.is_empty() || self.open_dbs.contains(&(id, db_name.clone()));
                let dim = SYSTEM_DBS.contains(&db_name.as_str());
                let count = colls.as_ref().map(|v| v.len().to_string()).unwrap_or_else(|| "…".into());
                let r = side_row(ui, SIDE_ROW_H, 0.0, false, |p, r| {
                    chevron(p, egui::pos2(r.left() + 4.0, r.center().y), is_open, MUTED());
                    p.text(egui::pos2(r.left() + 16.0, r.center().y), egui::Align2::LEFT_CENTER, db_name, FontId::proportional(TXT_TITLE), if dim { FAINT() } else { BRIGHT() });
                    p.text(egui::pos2(r.right(), r.center().y), egui::Align2::RIGHT_CENTER, &count, FontId::proportional(TXT_SMALL), FAINT());
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
                    if ui.button("New collection…").clicked() {
                        side.push(SideAct::NewColl(db_name.clone(), false));
                    }
                    if ui.button("New view…").clicked() {
                        side.push(SideAct::NewColl(db_name.clone(), true));
                    }
                    if ui.button("⟳ Refresh").clicked() {
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
                    for coll in matches {
                        let sel = Sel { conn: id, db: db_name.clone(), coll: coll.clone() };
                        let active = cur_sel.as_ref() == Some(&sel);
                        let is_view = self.views.contains(&(id, db_name.clone(), coll.clone()));
                        let r = side_row(ui, SIDE_ROW_H, 16.0, active, |p, r| {
                            let c = egui::pos2(r.left() + 4.0, r.center().y);
                            if is_view {
                                // Views get a hollow marker so they read as derived data.
                                p.circle_stroke(c, 3.5, Stroke::new(1.5, if active { PRIMARY() } else { C_DATE() }));
                            } else {
                                p.circle_filled(c, 3.5, if active { PRIMARY() } else { WARNING() });
                            }
                            let w = r.width() - 16.0;
                            let galley = p.layout(coll.clone(), FontId::proportional(TXT_TITLE), if active { BRIGHT() } else if is_view { C_DATE() } else { TEXT() }, w);
                            p.galley(egui::pos2(r.left() + 16.0, r.center().y - galley.size().y / 2.0), galley, TEXT());
                        });
                        let r = if is_view { r.on_hover_text("View (read-only)") } else { r };
                        if matches!(&self.reveal, Some((c, d, Some(k))) if *c == id && d == db_name && k == coll) {
                            r.scroll_to_me(Some(egui::Align::Center));
                            revealed = true;
                        }
                        if r.clicked() {
                            open = Some(sel.clone());
                        }
                        r.context_menu(|ui| {
                            menu_look(ui);
                            ui.label(RichText::new(format!("{}: {coll}", if is_view { "View" } else { "Collection" })).size(TXT_SMALL).color(MUTED()));
                            if ui.button("Open").clicked() {
                                open = Some(sel.clone());
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
        if connect {
            self.connect(id);
        }
        if let Some(d) = toggle {
            if !self.open_dbs.remove(&(id, d.clone())) {
                self.open_dbs.insert((id, d));
            }
        }
        if let Some(s) = open {
            self.open_collection(s, false);
        }
        for a in side {
            self.side_act(id, a);
        }
    }

    pub(super) fn side_act(&mut self, conn: u64, a: SideAct) {
        let Some(client) = self.conn(conn).and_then(|c| c.client.clone()) else {
            return self.err("Connect first.");
        };
        let writable = self.conn(conn).is_some_and(|c| c.writable);
        let needs_write = matches!(a, SideAct::NewColl(..) | SideAct::DropDb(_) | SideAct::Rename(..) | SideAct::DropColl(..) | SideAct::Restore(_));
        if needs_write && !writable {
            return self.modal = Some(Modal::Unlock(conn, None));
        }
        match a {
            SideAct::NewColl(db, view) => self.modal = Some(Modal::NewCollection { conn, db, name: String::new(), view, source: String::new(), pipeline: "[\n  { $match: {} }\n]".into() }),
            SideAct::Refresh(db) => self.worker.collections(conn, client, db),
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
            SideAct::Stats(db, coll) => self.worker.coll_stats(client, db, coll),
            SideAct::ViewDef(db, coll) => self.worker.command(client, db, doc! { "listCollections": 1, "filter": { "name": coll } }, "viewdef"),
            SideAct::Rename(db, coll) => self.modal = Some(Modal::RenameColl { conn, db, name: coll.clone(), coll }),
            SideAct::Dump(db, coll) => {
                let name = if coll.is_empty() { db.clone() } else { format!("{db}.{coll}") };
                let path = dirs::download_dir().unwrap_or_default().join(format!("{name}.archive.gz")).display().to_string();
                self.modal = Some(Modal::Tool(Box::new(ToolForm { conn, from_db: db.clone(), db, coll, path, gzip: true, ..Default::default() })));
            }
            SideAct::Restore(db) => self.modal = Some(Modal::Tool(Box::new(ToolForm { conn, from_db: db.clone(), db, restore: true, gzip: true, ..Default::default() }))),
            SideAct::Compare(db, coll) => {
                self.open_collection(Sel { conn, db: db.clone(), coll }, false);
                let t = self.tm().unwrap();
                t.view = View::Compare;
                if t.compare.conn == 0 {
                    (t.compare.conn, t.compare.db) = (conn, db);
                }
            }
            SideAct::Relations(db) => self.modal = Some(Modal::Relations { conn, db }),
            SideAct::Analyze(db, coll) => self.analyze_relations(conn, client, db, coll),
            SideAct::DropColl(db, coll) => {
                self.modal = Some(Modal::Typed { title: format!("Drop {db}.{coll} with all its documents and indexes? This cannot be undone."), expect: coll.clone(), typed: String::new(), action: Danger::DropCollAt { conn, db, coll } })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_jumps() {
        let conn = |id, name: &str, status, dbs| Conn { meta: ConnMeta { id, name: name.into(), ..Default::default() }, client: None, status, writable: false, dbs, masked_uri: String::new(), tunnel: None };
        let conns = [conn(1, "prod", Status::Ready, vec![("shop".into(), Some(vec!["users".into(), "orders".into()]))]), conn(2, "staging", Status::Idle, Vec::new())];
        let labels = |active, q| jumps(&conns, active, q).into_iter().map(|j| j.0).collect::<Vec<_>>();
        assert_eq!(labels(Some(2), ""), ["staging", "prod", "shop", "shop.users", "shop.orders"]);
        assert_eq!(labels(Some(1), "users"), ["shop.users"]);
        // "or" fuzzily matches shop.users too, but the substring match ranks first.
        assert_eq!(labels(Some(1), "or"), ["shop.orders", "shop.users"]);
        assert_eq!(labels(Some(1), "prod users")[0], "shop.users");
        assert_eq!(labels(Some(1), "shop")[0], "shop");

        let conns = [conn(1, "local", Status::Ready, vec![("thetreep-api-main".into(), Some(vec!["travels".into()])), ("thetreep-api-vairon".into(), Some(vec!["travels".into(), "users".into()]))])];
        let labels = |q| jumps(&conns, Some(1), q).into_iter().map(|j| j.0).collect::<Vec<_>>();
        assert_eq!(labels("vairon travels"), ["thetreep-api-vairon.travels"]);
        assert_eq!(labels("travels")[..2], ["thetreep-api-main.travels", "thetreep-api-vairon.travels"]);
    }
}
