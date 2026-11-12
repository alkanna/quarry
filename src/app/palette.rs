//! The Cmd+K palette: one search over the databases, tables and collections of every connection,
//! and the app's commands after ">". Matching and ranking are in `sidebar::jumps`.

use super::*;
use super::rail::{RailIcon, engine_logo, engine_mark, mark_color, paint_icon};

const PAL_W: f32 = 640.0;
const LIST_MAX_H: f32 = 440.0;

pub(super) const MONGO_VIEWS: [(View, &str); 8] = [(View::Documents, "Documents"), (View::Aggregate, "Aggregate"), (View::Indexes, "Indexes"), (View::Schema, "Schema"), (View::Import, "Import"), (View::Compare, "Compare"), (View::Shell, "Shell"), (View::Watch, "Watch")];
pub(super) const SQL_VIEWS: [(View, &str); 7] = [(View::Documents, "Content"), (View::Structure, "Structure"), (View::Relations, "Relations"), (View::Triggers, "Triggers"), (View::Info, "Info"), (View::Sql, "Query"), (View::Shell, "Shell")];

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Cmd {
    Reload,
    Explain,
    CloseTab,
    View(View),
    NewQuery,
    Connect,
    AllowWrites,
    LockWrites,
    Server,
    Users,
    EditConn,
    Disconnect,
    NewConn,
    CloudSql,
    Settings,
    Log,
    Reopen,
    Undo,
    Shortcuts,
}

#[derive(Clone)]
enum Act {
    Jump(Jump),
    Cmd(Cmd),
}

#[derive(Clone, Copy, PartialEq)]
enum Glyph {
    /// The engine logo of `Row::mark`.
    Engine,
    Db,
    Table,
    View,
    Coll,
    Reload,
    Lock,
    Tab,
    Prompt,
}

struct Row {
    name: String,
    /// The second line: where the item lives. Commands have none and get a one-line row.
    sub: String,
    glyph: Glyph,
    /// The engine mark that tints the tile; commands have none.
    mark: Option<&'static str>,
    tag: String,
    meta: String,
    keys: String,
    /// A connection's status dot, painted before `meta`.
    status: Option<Color32>,
    act: Act,
}

impl Row {
    fn cmd(name: &str, glyph: Glyph, keys: impl Into<String>, cmd: Cmd) -> Self {
        Row { name: name.into(), sub: String::new(), glyph, mark: None, tag: String::new(), meta: String::new(), keys: keys.into(), status: None, act: Act::Cmd(cmd) }
    }

    /// Allowing writes is the one risky command, drawn in the warning color.
    fn warn(&self) -> bool {
        matches!(self.act, Act::Cmd(Cmd::AllowWrites))
    }
}

type Section = (String, Vec<Row>);

/// A shortcut as macOS draws it, or spelled out on the other systems.
pub(super) fn keys(shift: bool, alt: bool, key: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("{}{}\u{2318}{key}", if alt { "\u{2325}" } else { "" }, if shift { "\u{21e7}" } else { "" })
    } else {
        format!("Ctrl+{}{}{key}", if shift { "Shift+" } else { "" }, if alt { "Alt+" } else { "" })
    }
}

fn status_label(s: &Status) -> &'static str {
    match s {
        Status::Ready => "Connected",
        Status::Connecting => "Connecting\u{2026}",
        Status::Idle => "Not connected",
        Status::Failed(_) => "Failed",
    }
}

/// How well `name` alone matches every token, for commands.
fn name_score(tok: &[String], name: &str) -> Option<u32> {
    jump_score(tok, &[&name.to_lowercase()])
}

impl App {
    pub(super) fn open_palette(&mut self) {
        self.open_palette_with(String::new());
    }

    /// A browser's new tab: the palette, whose pick opens next to the current tab.
    pub(super) fn open_new_tab(&mut self) {
        self.modal = Some(Modal::Palette { query: String::new(), sel: 0, cmds: false, new_tab: true });
    }

    /// The palette with `query` typed in, where a leading ">" switches to commands as typing it would.
    pub(super) fn open_palette_with(&mut self, query: String) {
        let (query, cmds) = match query.strip_prefix('>') {
            Some(rest) => (rest.to_string(), true),
            None => (query, false),
        };
        self.modal = Some(Modal::Palette { query, sel: 0, cmds, new_tab: false });
    }

    fn jump_row(&self, j: Jump) -> Option<Row> {
        let id = match &j {
            Jump::Conn(id) | Jump::Db(id, _) => *id,
            Jump::Coll(s) => s.conn,
        };
        let c = self.conn(id)?;
        let (mark, _) = engine_mark(c.meta.engine, &c.server.product);
        let sql = c.meta.engine.is_sql();
        let (name, sub, glyph, meta, status) = match &j {
            Jump::Conn(_) => {
                let uri = if c.masked_uri.is_empty() { "no address saved".to_string() } else { c.masked_uri.clone() };
                (c.meta.name.clone(), uri, Glyph::Engine, status_label(&c.status).to_string(), Some(status_color(&c.status)))
            }
            Jump::Db(_, db) => {
                let n = c.dbs.iter().find(|d| d.0 == *db).and_then(|d| d.1.as_ref()).map(|v| v.len());
                // A PostgreSQL database lists its schemas, and its tables sit one level down.
                let unit = if c.meta.engine == db::sql::Engine::Postgres { "schemas" } else if sql { "tables" } else { "collections" };
                let meta = n.map(|n| format!("{n} {unit}")).unwrap_or_default();
                (db.clone(), c.meta.name.clone(), Glyph::Db, meta, None)
            }
            Jump::Coll(s) => {
                let view = self.views.has(s.conn, &s.nskey(), &s.coll);
                let glyph = if view { Glyph::View } else if sql { Glyph::Table } else { Glyph::Coll };
                let place = if s.schema.is_empty() { s.db.clone() } else { format!("{} \u{203a} {}", s.db, s.schema) };
                (s.coll.clone(), format!("{} \u{203a} {}{}", c.meta.name, place, if view { " \u{b7} view" } else { "" }), glyph, String::new(), None)
            }
        };
        Some(Row { name, sub, glyph, mark: Some(mark), tag: c.meta.tag.clone(), meta, keys: String::new(), status, act: Act::Jump(j) })
    }

    /// Every command that applies right now, in groups: the current tab, the active connection, the app.
    fn commands(&self) -> Vec<(String, Row)> {
        let mut out = Vec::new();
        let mut add = |group: &str, row: Row| out.push((group.to_string(), row));
        if let Some(t) = self.t() {
            let sql = self.conn(t.sel.conn).is_some_and(|c| c.meta.engine.is_sql());
            let g = format!("This tab \u{b7} {}", self.tab_name(self.cur));
            add(&g, Row::cmd("Reload tab", Glyph::Reload, keys(false, false, "R"), Cmd::Reload));
            add(&g, Row::cmd("Explain", Glyph::Prompt, keys(false, false, "E"), Cmd::Explain));
            if !t.sel.coll.is_empty() {
                let views: &[(View, &str)] = if sql { &SQL_VIEWS } else { &MONGO_VIEWS };
                for (i, (v, label)) in views.iter().enumerate().filter(|(_, (v, _))| *v != t.view) {
                    // Cmd+Alt+digit picks the Mongo views only; SQL tabs have no such shortcut.
                    add(&g, Row::cmd(&format!("Show {label}"), Glyph::Prompt, if sql { String::new() } else { keys(false, true, &(i + 1).to_string()) }, Cmd::View(*v)));
                }
            }
            if sql {
                add(&g, Row::cmd(&format!("New query in {}", t.sel.db), Glyph::Prompt, "", Cmd::NewQuery));
            }
            add(&g, Row::cmd("Close tab", Glyph::Tab, keys(false, false, "W"), Cmd::CloseTab));
        }
        if let Some(c) = self.active.and_then(|id| self.conn(id)) {
            let (_, product) = engine_mark(c.meta.engine, &c.server.product);
            let g = format!("{} \u{b7} {product}", c.meta.name);
            let lockable = c.status == Status::Ready && !self.prefs.always_writable;
            if c.status == Status::Ready {
                add(&g, Row::cmd("Refresh connection", Glyph::Reload, keys(true, false, "R"), Cmd::Connect));
                if lockable && c.writable {
                    add(&g, Row::cmd("Lock writes", Glyph::Lock, "", Cmd::LockWrites));
                }
                add(&g, Row::cmd("Server status", Glyph::Prompt, "", Cmd::Server));
                add(&g, Row::cmd(if c.meta.engine == db::sql::Engine::Postgres { "Roles" } else { "Users" }, Glyph::Prompt, "", Cmd::Users));
                add(&g, Row::cmd("Disconnect", Glyph::Prompt, "", Cmd::Disconnect));
            } else if c.status != Status::Connecting {
                add(&g, Row::cmd("Connect", Glyph::Reload, keys(true, false, "R"), Cmd::Connect));
            }
            add(&g, Row::cmd("Edit connection\u{2026}", Glyph::Prompt, "", Cmd::EditConn));
            if lockable && !c.writable {
                add(&g, Row { tag: c.meta.tag.clone(), ..Row::cmd("Read-only: allow writes\u{2026}", Glyph::Lock, "", Cmd::AllowWrites) });
            }
        }
        let g = "Quarry";
        add(g, Row::cmd("New connection\u{2026}", Glyph::Prompt, keys(false, false, "N"), Cmd::NewConn));
        add(g, Row::cmd("Add from Cloud SQL\u{2026}", Glyph::Prompt, "", Cmd::CloudSql));
        if !self.closed.is_empty() {
            add(g, Row::cmd("Reopen closed tab", Glyph::Tab, keys(true, false, "T"), Cmd::Reopen));
        }
        if !self.undo.is_empty() {
            add(g, Row { meta: format!("{} in 24 h", self.undo.len()), ..Row::cmd("Recent changes", Glyph::Prompt, "", Cmd::Undo) });
        }
        add(g, Row::cmd("Query log", Glyph::Prompt, keys(false, false, "L"), Cmd::Log));
        add(g, Row::cmd("Settings", Glyph::Prompt, keys(false, false, ","), Cmd::Settings));
        add(g, Row::cmd("Keyboard shortcuts", Glyph::Prompt, keys(false, false, "/"), Cmd::Shortcuts));
        out
    }

    /// The palette's sections for the query: commands in command mode; open tabs and
    /// connections before typing; otherwise the best match, then the rest by kind, then commands.
    fn sections(&self, q: &str, tok: &[String], cmd_mode: bool) -> Vec<Section> {
        let mut out: Vec<Section> = Vec::new();
        let push = |out: &mut Vec<Section>, title: &str, r: Row| match out.last_mut() {
            Some(s) if s.0 == title => s.1.push(r),
            _ => out.push((title.to_string(), vec![r])),
        };
        if cmd_mode {
            for (g, r) in self.commands() {
                if tok.is_empty() || name_score(tok, &r.name).is_some() {
                    push(&mut out, &g, r);
                }
            }
            return out;
        }
        if tok.is_empty() {
            let order = std::iter::once(self.cur).chain((0..self.tabs.len()).filter(|&i| i != self.cur));
            for i in order.filter(|&i| i < self.tabs.len()).take(6) {
                let sel = self.tabs[i].sel.clone();
                if !sel.coll.is_empty()
                    && let Some(r) = self.jump_row(Jump::Coll(sel))
                {
                    push(&mut out, "Open tabs", r);
                }
            }
            for c in &self.conns {
                if let Some(r) = self.jump_row(Jump::Conn(c.meta.id)) {
                    push(&mut out, "Connections", r);
                }
            }
            return out;
        }
        let mut rows = jumps(&self.conns, self.active, q).into_iter().filter_map(|j| self.jump_row(j.2));
        if let Some(top) = rows.next() {
            push(&mut out, "Top hit", top);
        }
        // Stable, so each kind keeps the ranking `jumps` gave it.
        let kind = |r: &Row| match r.act {
            Act::Jump(Jump::Coll(_)) => (0, "Tables and collections"),
            Act::Jump(Jump::Db(..)) => (1, "Databases"),
            _ => (2, "Connections"),
        };
        let mut rest: Vec<Row> = rows.collect();
        rest.sort_by_key(|r| kind(r).0);
        for r in rest {
            push(&mut out, kind(&r).1, r);
        }
        let mut cmds: Vec<(u32, Row)> = self.commands().into_iter().filter_map(|(_, r)| name_score(tok, &r.name).map(|s| (s, r))).collect();
        cmds.sort_by_key(|c| std::cmp::Reverse(c.0));
        for (_, r) in cmds.into_iter().take(4) {
            push(&mut out, "Commands", r);
        }
        out
    }

    pub(super) fn run_cmd(&mut self, c: Cmd) {
        let active = self.active;
        match c {
            Cmd::Reload if !self.tabs.is_empty() => self.reload(),
            Cmd::Explain if self.t().is_some_and(|t| t.view == View::Sql && t.sql.is_some()) => self.sql_run(false, true),
            Cmd::Explain => self.explain(),
            Cmd::CloseTab => self.close_tab(self.cur),
            Cmd::View(v) => self.set_view(v),
            Cmd::NewQuery => {
                if let Some(s) = self.t().map(|t| t.sel.clone()) {
                    self.sql_new_query(s.conn, s.db);
                    // The new tab starts in the schema of the one it was opened from, not the sidebar's.
                    if !s.schema.is_empty() && let Some(t) = self.tm() {
                        t.sel.schema = s.schema;
                    }
                }
            }
            Cmd::Connect if !self.demo => {
                if let Some(id) = active {
                    self.connect(id);
                }
            }
            Cmd::AllowWrites => self.modal = active.map(|id| Modal::Unlock(id, None)),
            Cmd::LockWrites => {
                if let Some(c) = self.conns.iter_mut().find(|c| Some(c.meta.id) == active) {
                    c.writable = false;
                    self.ok("Writes locked");
                }
            }
            Cmd::Server => {
                if let Some(id) = active {
                    self.open_server(id);
                }
            }
            Cmd::Users => match active {
                Some(id) if self.conn(id).is_some_and(|c| c.meta.engine.is_sql()) => self.open_sql_users(id),
                Some(id) => self.side_act(id, SideAct::Users),
                None => {}
            },
            Cmd::EditConn => {
                if let Some(id) = active {
                    self.edit_conn(id);
                }
            }
            Cmd::Disconnect => {
                if let Some(id) = active {
                    self.disconnect(id);
                }
            }
            Cmd::NewConn => self.new_conn_modal(),
            Cmd::CloudSql => self.open_cloud_browser(false),
            Cmd::Settings => self.modal = Some(Modal::Settings),
            Cmd::Log => self.log.open = !self.log.open,
            Cmd::Reopen => self.reopen_tab(),
            Cmd::Undo => self.modal = Some(Modal::Undo),
            Cmd::Shortcuts => self.shortcuts_modal(),
            _ => {}
        }
    }

    pub(super) fn palette(&mut self, ctx: &egui::Context) {
        let Some(Modal::Palette { mut query, mut sel, mut cmds, new_tab }) = self.modal.take() else { return };
        // Anchored near the top, so the box stays put while the list grows and shrinks.
        let top = ctx.content_rect().height() * 0.14;
        let id = egui::Id::unique(("quarry-layer", "palette"));
        let area = egui::Modal::default_area(id).anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top));
        // The pick, and whether it opens in a new tab.
        let mut pick: Option<(Act, bool)> = None;
        let r = egui::Modal::new(id).area(area).frame(dialog_frame()).show(ctx, |ui| {
            ui.set_width(PAL_W);
            ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
            // Taken before the text field sees them, so arrows move the selection and Enter picks.
            let (up, down, enter, back, cmd_enter) = ui.input_mut(|i| {
                let back = cmds && query.is_empty() && i.consume_key(egui::Modifiers::NONE, Key::Backspace);
                // Cmd+Enter first, or the plain Enter below would take it.
                let cmd_enter = i.consume_key(egui::Modifiers::COMMAND, Key::Enter);
                (i.consume_key(egui::Modifiers::NONE, Key::ArrowUp), i.consume_key(egui::Modifiers::NONE, Key::ArrowDown), i.consume_key(egui::Modifiers::NONE, Key::Enter), back, cmd_enter)
            });
            if back {
                cmds = false;
                sel = 0;
            }
            palette_header(ui, &mut query, &mut sel, &mut cmds);
            let tok = tokens(&query);
            let secs = self.sections(&query, &tok, cmds);
            let n: usize = secs.iter().map(|s| s.1.len()).sum();
            if down {
                sel = (sel + 1).min(n.saturating_sub(1));
            }
            if up {
                sel = sel.saturating_sub(1);
            }
            ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top() - 0.5, Stroke::new(1.0, RAISED_LINE()));
            egui::ScrollArea::vertical().max_height(LIST_MAX_H).min_scrolled_height(LIST_MAX_H).auto_shrink([false, true]).show(ui, |ui| {
                ui.add_space(6.0);
                if n == 0 {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), EMPTY_LIST_H), Sense::hover());
                    let msg = if query.trim().is_empty() { "No commands" } else { "No matches" };
                    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, msg, FontId::proportional(TXT_TITLE), MUTED());
                }
                let mut i = 0;
                for (title, rows) in &secs {
                    section_caption(ui, title, (rows.len() > 1).then_some(rows.len()));
                    for row in rows {
                        let r = paint_row(ui, row, i == sel, &tok);
                        if i == sel && (up || down) {
                            r.scroll_to_me(None);
                        }
                        if new_tab_click(&r) {
                            pick = Some((row.act.clone(), true));
                        } else if r.clicked() {
                            pick = Some((row.act.clone(), new_tab));
                        }
                        i += 1;
                    }
                    ui.add_space(6.0);
                }
            });
            if enter || cmd_enter {
                pick = secs.iter().flat_map(|s| &s.1).nth(sel).map(|r| (r.act.clone(), new_tab || cmd_enter));
            }
            palette_footer(ui, cmds, new_tab);
        });
        if let Some((a, new)) = pick {
            match a {
                Act::Jump(j) => {
                    // A collection already open is switched to, as a browser offers its open tab.
                    let open = matches!(&j, Jump::Coll(s) if self.tabs.iter().any(|t| t.sel == *s));
                    self.jump(j, if new && !open { Open::NewTab } else { Open::Here })
                }
                Act::Cmd(c) => self.run_cmd(c),
            }
        } else if !r.should_close() && self.modal.is_none() {
            self.modal = Some(Modal::Palette { query, sel, cmds, new_tab });
        }
    }
}

impl App {
    /// Every open tab grouped by connection, with a filter, for when the strip overflows. A click
    /// or Enter sets `pick`, the cross on a hovered row sets `close`, and Enter with no match hands
    /// the query to the Cmd+K palette through `search`.
    pub(super) fn tab_list(&self, ui: &mut egui::Ui, pick: &mut Option<usize>, close: &mut Option<usize>, search: &mut Option<String>) {
        let id = egui::Id::unique("tab-filter");
        let n = self.tabs.len();
        sticky_menu(ui, format!("{n} tabs"), |ui| {
            ui.set_width(TAB_LIST_W);
            ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
            let margin = ui.spacing().menu_margin;
            // The filter and highlight live only while the menu stays open: a gap of a frame means it was closed.
            // No highlight yet means it was just opened, and it goes on the current tab once the rows are known.
            let frame = ui.ctx().cumulative_frame_nr();
            let (mut q, mut sel) = ui.data(|d| d.get_temp::<(String, Option<usize>, u64)>(id)).filter(|s| s.2 + 1 >= frame).map(|s| (s.0, s.1)).unwrap_or_default();
            // Taken before the text field sees them, so arrows move the highlight and Enter picks.
            let (up, down, enter) = ui.input_mut(|i| (i.consume_key(egui::Modifiers::NONE, Key::ArrowUp), i.consume_key(egui::Modifiers::NONE, Key::ArrowDown), i.consume_key(egui::Modifiers::NONE, Key::Enter)));
            let (head, _) = ui.allocate_exact_size(egui::vec2(TAB_LIST_W, TAB_LIST_HEAD_H), Sense::hover());
            let cy = head.center().y;
            // The field ends before room for the "n of m" count, so it runs before the rows are filtered
            // and a keystroke and Enter in the same frame both see the new query.
            let field = egui::Rect::from_min_max(egui::pos2(head.left() + 30.0, cy - BUTTON_H / 2.0), egui::pos2(head.right() - 70.0, cy + BUTTON_H / 2.0));
            let hint = RichText::new(format!("Filter {n} tabs")).color(FAINT());
            let r = ui.new_child(egui::UiBuilder::new().max_rect(field)).add(egui::TextEdit::singleline(&mut q).frame(egui::Frame::NONE).margin(egui::Margin::ZERO).font(FontId::proportional(TXT_SUBHEAD)).text_color(BRIGHT()).hint_text(hint).desired_width(field.width()).align(egui::Align2::LEFT_CENTER));
            r.request_focus();
            if r.changed() {
                sel = Some(0);
            }
            let tok = tokens(&q);
            // Once per frame: a query tab's name counts the query tabs before it.
            let names: Vec<String> = (0..n).map(|i| self.tab_name(i)).collect();
            // Grouped by connection id, so same-named connections stay apart.
            let mut groups: Vec<(u64, Vec<usize>)> = Vec::new();
            for (i, t) in self.tabs.iter().enumerate() {
                if !tok.is_empty() {
                    let conn = self.conn(t.sel.conn).map_or("", |c| c.meta.name.as_str());
                    let hay = format!("{} {} {} {conn}", names[i], t.sel.db, t.sel.schema).to_lowercase();
                    if !tok.iter().all(|w| hay.contains(w.as_str())) {
                        continue;
                    }
                }
                match groups.iter_mut().find(|g| g.0 == t.sel.conn) {
                    Some(g) => g.1.push(i),
                    None => groups.push((t.sel.conn, vec![i])),
                }
            }
            let flat: Vec<usize> = groups.iter().flat_map(|g| g.1.iter().copied()).collect();
            let p = ui.painter();
            paint_icon(p, egui::pos2(head.left() + 16.0, cy), RailIcon::Search, Stroke::new(1.5, FAINT()));
            if !tok.is_empty() {
                p.text(egui::pos2(head.right() - 8.0, cy), egui::Align2::RIGHT_CENTER, format!("{} of {n}", flat.len()), FontId::monospace(TXT_CAPTION), FAINT());
            }
            p.hline(head.x_range().expand(margin.left as f32), head.bottom() - 0.5, Stroke::new(1.0, RAISED_LINE()));
            let opened = sel.is_none();
            let mut sel = sel.unwrap_or_else(|| flat.iter().position(|&i| i == self.cur).unwrap_or(0));
            if down {
                sel += 1;
            }
            if up {
                sel = sel.saturating_sub(1);
            }
            sel = sel.min(flat.len().saturating_sub(1));

            let max_h = ui.ctx().content_rect().height() - TAB_LIST_HEAD_H - PALETTE_FOOT_H - 80.0;
            egui::ScrollArea::vertical().max_height(max_h).show(ui, |ui| {
                ui.add_space(4.0);
                if flat.is_empty() {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(TAB_LIST_W, EMPTY_LIST_H), Sense::hover());
                    let p = ui.painter();
                    let g = p.layout_job(LayoutJob::single_section("No open tab matches".into(), ui_fmt(TXT_TITLE, TEXT(), W_SEMIBOLD, 0.0)));
                    p.galley(rect.center() - egui::vec2(g.size().x / 2.0, 9.0 + g.size().y / 2.0), g, TEXT());
                    p.text(rect.center() + egui::vec2(0.0, 11.0), egui::Align2::CENTER_CENTER, format!("\u{21b5} searches every connection with {}", keys(false, false, "K")), FontId::proportional(TXT_SMALL), MUTED());
                }
                let mut k = 0;
                for (conn, idx) in &groups {
                    self.tab_group_head(ui, *conn, idx.len());
                    let mut prev_db = None;
                    for &i in idx {
                        let db = self.tabs[i].sel.scope_label();
                        let show_db = prev_db != Some(db);
                        prev_db = Some(db);
                        let (r, x) = self.tab_list_row(ui, i, &names[i], k == sel, show_db, &tok);
                        if k == sel && (up || down || opened) {
                            r.scroll_to_me(None);
                        }
                        if x {
                            *close = Some(i);
                        } else if r.clicked() {
                            *pick = Some(i);
                            ui.close();
                        }
                        k += 1;
                    }
                    ui.add_space(4.0);
                }
            });
            if enter {
                match flat.get(sel) {
                    Some(&i) => *pick = Some(i),
                    None => *search = Some(q.clone()),
                }
                ui.close();
            }
            tab_list_footer(ui, margin);
            ui.data_mut(|d| d.insert_temp(id, (q, Some(sel), frame)));
        });
    }

    /// A connection's header in the tab list: engine tile, name, tag and how many tabs it has open.
    fn tab_group_head(&self, ui: &mut egui::Ui, conn: u64, count: usize) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(TAB_LIST_W, TAB_GROUP_H), Sense::hover());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let cy = rect.center().y;
        let p = ui.painter();
        let count = p.layout_no_wrap(count.to_string(), FontId::monospace(TXT_CAPTION), FAINT());
        let right = rect.right() - 8.0 - count.size().x;
        p.galley(egui::pos2(right, cy - count.size().y / 2.0), count, FAINT());
        let Some(c) = self.conn(conn) else { return };
        let (mark, _) = engine_mark(c.meta.engine, &c.server.product);
        paint_tile(ui, egui::Rect::from_min_size(egui::pos2(rect.left() + 6.0, cy - 10.0), egui::Vec2::splat(20.0)), Glyph::Engine, Some(mark), false);
        let x = rect.left() + 34.0;
        // The name and tag share the room before the count, the tag taking at most 40 % of it.
        let room = right - 10.0 - x;
        let p = ui.painter();
        let tag_w = if c.meta.tag.is_empty() { 0.0 } else { (p.layout_no_wrap(c.meta.tag.to_uppercase(), FontId::proportional(TXT_CAPTION), FAINT()).size().x + 14.0).min(room * 0.4) };
        let mut job = LayoutJob::single_section(c.meta.name.clone(), ui_fmt(TXT_SMALL, TEXT(), W_SEMIBOLD, 0.0));
        job.wrap = egui::text::TextWrapping::truncate_at_width(room - if tag_w > 0.0 { tag_w + 8.0 } else { 0.0 });
        let g = p.layout_job(job);
        let w = g.size().x;
        p.galley(egui::pos2(x, cy - g.size().y / 2.0 + optical_dy(&g)), g, TEXT());
        if tag_w > 0.0 {
            paint_tag(p, x + w + 8.0 + tag_w, cy, &c.meta.tag, tag_w);
        }
    }

    /// One tab in the list. Returns the row's response and whether its close cross was clicked.
    fn tab_list_row(&self, ui: &mut egui::Ui, i: usize, name: &str, lit: bool, show_db: bool, tok: &[String]) -> (egui::Response, bool) {
        let (rect, r) = ui.allocate_exact_size(egui::vec2(TAB_LIST_W, POP_ROW_H), Sense::click());
        if !ui.is_rect_visible(rect) {
            return (r, false);
        }
        let cur = i == self.cur;
        // contains_pointer, since the cross on top of the row takes the hover itself.
        let over = r.contains_pointer();
        // As in the palette: SEL marks the row Enter acts on, MENU_HOVER only the pointer.
        let fill = match (lit, state_of(&r)) {
            (true, 1 | 2) => SEL_HOVER(),
            (true, _) => SEL(),
            (false, 2) => menu_pressed(),
            _ if over => MENU_HOVER(),
            _ => Color32::TRANSPARENT,
        };
        ui.painter().rect_filled(rect.shrink2(egui::vec2(0.0, 1.0)), R_SM, fill);
        let cy = rect.center().y;
        let mut right = rect.right() - 6.0;
        let mut x_clicked = false;
        if over {
            x_clicked = row_close(ui, rect.shrink2(egui::vec2(6.0, 0.0)), cy, "Close tab");
            right -= CLOSE_W + 8.0;
        } else {
            // Cmd+1 to 8 jump to those tabs and Cmd+9 to the last one, as in browsers.
            let key = if i + 1 == self.tabs.len() { Some(9) } else if i < 8 { Some(i + 1) } else { None };
            if let Some(k) = key {
                right = kbd(ui.painter(), right, cy, &keys(false, false, &k.to_string())) - 8.0;
            }
        }
        let p = ui.painter();
        if cur {
            p.circle_filled(egui::pos2(rect.left() + 16.0, cy), 3.0, PRIMARY());
        }
        let nx = rect.left() + 34.0;
        let color = if cur || lit { BRIGHT() } else { TEXT() };
        let g = p.layout_job(name_job(name, tok, color, right - nx));
        let w = g.size().x;
        p.galley(egui::pos2(nx, cy - g.size().y / 2.0 + optical_dy(&g)), g, color);
        if show_db {
            let db = truncated(p, self.tabs[i].sel.scope_label(), FontId::monospace(TXT_CAPTION), FAINT(), right - nx - w - 8.0);
            p.galley(egui::pos2(nx + w + 8.0, cy - db.size().y / 2.0), db, FAINT());
        }
        (r.on_hover_cursor(egui::CursorIcon::PointingHand), x_clicked)
    }
}

/// The footer band of the palette and the tab list, `rect` widened to `band`, with its top line.
fn paint_footer_band(p: &egui::Painter, band: egui::Rect, corner: u8) {
    p.rect_filled(band, egui::CornerRadius { nw: 0, ne: 0, sw: corner, se: corner }, RAISED().lerp_to_gamma(PANEL(), 0.5));
    p.hline(band.x_range(), band.top() + 0.5, Stroke::new(1.0, RAISED_LINE()));
}

/// Key caps and their labels from `x` along a footer, the labels muted. Returns where they end.
fn key_hints(p: &egui::Painter, mut x: f32, cy: f32, hints: &[(&[&str], &str)]) -> f32 {
    for (ks, text) in hints {
        for k in *ks {
            let w = kbd_w(p, k);
            kbd(p, x + w, cy, k);
            x += w + 3.0;
        }
        let g = p.layout_no_wrap(text.to_string(), FontId::proportional(TXT_SMALL), MUTED());
        let w = g.size().x;
        p.galley(egui::pos2(x + 3.0, cy - g.size().y / 2.0 + optical_dy(&g)), g, MUTED());
        x += 3.0 + w + 16.0;
    }
    x
}

/// One key cap and its label ending at `right`.
fn key_hint_right(p: &egui::Painter, right: f32, cy: f32, key: &str, text: &str) {
    let g = p.layout_no_wrap(text.into(), FontId::proportional(TXT_SMALL), MUTED());
    let lx = right - g.size().x;
    p.galley(egui::pos2(lx, cy - g.size().y / 2.0 + optical_dy(&g)), g, MUTED());
    kbd(p, lx - 6.0, cy, key);
}

/// Key hints along the bottom of the tab list, on the palette's footer band.
fn tab_list_footer(ui: &mut egui::Ui, margin: egui::Margin) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(TAB_LIST_W, PALETTE_FOOT_H), Sense::hover());
    // Out through the menu's margin, so the band meets the popup's edges and bottom corners.
    let band = egui::Rect::from_min_max(rect.min - egui::vec2(margin.left as f32, 0.0), rect.max + egui::vec2(margin.right as f32, margin.bottom as f32));
    let p = ui.painter();
    paint_footer_band(p, band, ui.visuals().menu_corner_radius.sw);
    let cy = rect.center().y + margin.bottom as f32 / 2.0;
    key_hints(p, rect.left() + 8.0, cy, &[(&["\u{2191}", "\u{2193}"], "move"), (&["\u{21b5}"], "switch")]);
    key_hint_right(p, rect.right() - 8.0, cy, &keys(false, false, "K"), "search everything");
}

fn palette_header(ui: &mut egui::Ui, query: &mut String, sel: &mut usize, cmds: &mut bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(PAL_W, PALETTE_HEAD_H), Sense::hover());
    let p = ui.painter().clone();
    let cy = rect.center().y;
    paint_icon(&p, egui::pos2(rect.left() + 26.5, cy), RailIcon::Search, Stroke::new(1.5, FAINT()));
    let mut x = rect.left() + 44.0;
    if *cmds {
        let g = p.layout_job(LayoutJob::single_section("Commands".into(), ui_fmt(TXT_SMALL, PRIMARY(), W_MEDIUM, 0.0)));
        let chip = egui::Rect::from_min_size(egui::pos2(x, cy - 12.0), egui::vec2(g.size().x + 30.0, 24.0));
        p.rect(chip, R_MD, tint(PRIMARY(), 36), Stroke::new(1.0, tint(PRIMARY(), 90)), egui::StrokeKind::Inside);
        let gt = p.layout_job(LayoutJob::single_section(">".into(), mono_fmt(TXT_SMALL, PRIMARY(), W_SEMIBOLD)));
        p.galley(egui::pos2(chip.left() + 9.0, cy - gt.size().y / 2.0 + optical_dy(&gt)), gt, PRIMARY());
        p.galley(egui::pos2(chip.left() + 21.0, cy - g.size().y / 2.0 + optical_dy(&g)), g, PRIMARY());
        x = chip.right() + 10.0;
    }
    let esc = kbd(&p, rect.right() - 16.0, cy, "esc");
    let field = egui::Rect::from_min_max(egui::pos2(x, cy - 12.0), egui::pos2(esc - 12.0, cy + 12.0));
    let hint = if *cmds { "Run a command" } else { "Search tables, collections and commands" };
    // A child Ui, which unlike `ui.put` leaves the header's own allocation alone.
    let r = ui.new_child(egui::UiBuilder::new().max_rect(field)).add(egui::TextEdit::singleline(query).frame(egui::Frame::NONE).margin(egui::Margin::ZERO).font(FontId::proportional(TXT_SEARCH)).text_color(BRIGHT()).hint_text(RichText::new(hint).color(FAINT())).desired_width(field.width()).align(egui::Align2::LEFT_CENTER));
    r.request_focus();
    if r.changed() {
        *sel = 0;
        // ">" typed first switches to commands; Backspace on the empty field leaves them.
        if !*cmds && query.starts_with('>') {
            query.remove(0);
            *cmds = true;
        }
    }
}

fn kbd_w(p: &egui::Painter, text: &str) -> f32 {
    (p.layout_no_wrap(text.into(), FontId::proportional(TXT_CAPTION), MUTED()).size().x + 10.0).max(18.0)
}

/// A key cap whose right edge is at `right`; returns its left edge.
fn kbd(p: &egui::Painter, right: f32, cy: f32, text: &str) -> f32 {
    let r = egui::Rect::from_min_max(egui::pos2(right - kbd_w(p, text), cy - 9.0), egui::pos2(right, cy + 9.0));
    paint_kbd(p, r, p.layout_no_wrap(text.into(), FontId::proportional(TXT_CAPTION), MUTED()));
    r.left()
}

fn section_caption(ui: &mut egui::Ui, title: &str, count: Option<usize>) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), PALETTE_CAPTION_H), Sense::hover());
    let p = ui.painter();
    let g = p.layout_job(LayoutJob::single_section(title.to_uppercase(), ui_fmt(TXT_CAPTION, FAINT(), W_SEMIBOLD, 0.5)));
    p.galley(egui::pos2(rect.left() + 20.0, rect.center().y - g.size().y / 2.0 + optical_dy(&g)), g, FAINT());
    if let Some(n) = count {
        p.text(egui::pos2(rect.right() - 20.0, rect.center().y), egui::Align2::RIGHT_CENTER, n.to_string(), FontId::monospace(TXT_CAPTION), FAINT());
    }
}

/// `name` with the letters that match a query token in the accent color.
fn name_job(name: &str, tok: &[String], color: Color32, width: f32) -> LayoutJob {
    let lower = name.to_lowercase();
    let mut hit = vec![false; name.len()];
    // Byte offsets of the lowercase text only line up when lowercasing kept every length.
    if lower.len() == name.len() {
        for t in tok {
            if let Some(i) = lower.find(t.as_str()) {
                hit[i..i + t.len()].fill(true);
            }
        }
    }
    let accent = if is_light() { PRIMARY() } else { C_LINK() };
    let mut job = LayoutJob::default();
    let mut start = 0;
    for (i, _) in name.char_indices().skip(1).chain([(name.len(), ' ')]) {
        if i == name.len() || hit[i] != hit[start] {
            let on = hit[start];
            job.append(&name[start..i], 0.0, ui_fmt(TXT_TITLE, if on { accent } else { color }, if on { W_SEMIBOLD } else { W_MEDIUM }, 0.0));
            start = i;
        }
    }
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(0.0));
    job
}

fn paint_row(ui: &mut egui::Ui, row: &Row, sel: bool, tok: &[String]) -> egui::Response {
    let two = !row.sub.is_empty();
    let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), if two { PALETTE_ROW_H } else { PALETTE_CMD_H }), Sense::click());
    // Up to 50 results; only the ones in view are worth painting.
    if !ui.is_rect_visible(rect) {
        return r;
    }
    let fill = match (sel, state_of(&r)) {
        (true, 2) => SEL_HOVER(),
        (true, _) => SEL(),
        (false, 2) => menu_pressed(),
        (false, 1) => MENU_HOVER(),
        _ => Color32::TRANSPARENT,
    };
    ui.painter().rect_filled(rect.shrink2(egui::vec2(8.0, 1.0)), R_SM, fill);
    let cy = rect.center().y;
    paint_tile(ui, egui::Rect::from_center_size(egui::pos2(rect.left() + 29.0, cy), egui::Vec2::splat(if two { 26.0 } else { 24.0 })), row.glyph, row.mark, row.warn());
    let p = ui.painter();
    let nx = rect.left() + 52.0;
    let mut xr = rect.right() - 20.0;
    if sel {
        xr = kbd(p, xr, cy, "\u{21b5}") - 6.0;
        let g = p.layout_job(LayoutJob::single_section(if matches!(row.act, Act::Cmd(_)) { "Run" } else { "Open" }.into(), ui_fmt(TXT_SMALL, MUTED(), W_MEDIUM, 0.0)));
        xr -= g.size().x;
        p.galley(egui::pos2(xr, cy - g.size().y / 2.0 + optical_dy(&g)), g, MUTED());
        xr -= 12.0;
    }
    if !row.meta.is_empty() {
        let g = p.layout_no_wrap(row.meta.clone(), FontId::monospace(TXT_CAPTION), FAINT());
        xr -= g.size().x;
        p.galley(egui::pos2(xr, cy - g.size().y / 2.0), g, FAINT());
        xr -= 10.0;
        if let Some(c) = row.status {
            p.circle_filled(egui::pos2(xr - 3.0, cy), 3.5, c);
            xr -= 14.0;
        }
    }
    if !row.keys.is_empty() {
        xr = kbd(p, xr, cy, &row.keys) - 8.0;
    }
    if !row.tag.is_empty() {
        xr = paint_tag(p, xr, cy, &row.tag, (xr - nx) * 0.4) - 8.0;
    }
    let color = if row.warn() { WARNING() } else if sel { BRIGHT() } else { TEXT() };
    let g = p.layout_job(name_job(&row.name, tok, color, xr - nx));
    if two {
        p.galley(egui::pos2(nx, cy - 8.0 - g.size().y / 2.0), g, color);
        let s = truncated(p, &row.sub, FontId::monospace(TXT_CAPTION), MUTED(), xr - nx);
        p.galley(egui::pos2(nx, cy + 9.0 - s.size().y / 2.0), s, MUTED());
    } else {
        p.galley(egui::pos2(nx - 2.0, cy - g.size().y / 2.0 + optical_dy(&g)), g, color);
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Points along an ellipse around `c` from angle `a0` to `a1` (radians, y down).
fn arc(c: egui::Pos2, rx: f32, ry: f32, a0: f32, a1: f32, n: usize) -> Vec<egui::Pos2> {
    (0..=n).map(|k| a0 + (a1 - a0) * k as f32 / n as f32).map(|a| c + egui::vec2(rx * a.cos(), ry * a.sin())).collect()
}

/// The square at the start of a row: tinted by the engine, with the item's kind as a line glyph.
fn paint_tile(ui: &egui::Ui, rect: egui::Rect, glyph: Glyph, mark: Option<&'static str>, warn: bool) {
    use std::f32::consts::PI;
    let p = ui.painter();
    let color = if warn { WARNING() } else { mark.map(mark_color).unwrap_or(MUTED()) };
    p.rect_filled(rect, R_MD, tint(color, if mark.is_some() || warn { 40 } else { 28 }));
    let c = rect.center();
    let s = Stroke::new(1.3, color);
    let v = egui::vec2;
    let r = |w: f32, h: f32| egui::Rect::from_center_size(c, v(w, h));
    match glyph {
        Glyph::Engine => {
            if let Some(tex) = mark.and_then(|m| engine_logo(ui.ctx(), m)) {
                p.image(tex.id(), r(15.0, 15.0), egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), color);
            }
        }
        Glyph::Table | Glyph::View => {
            let b = r(13.0, 11.0);
            p.rect_stroke(b, 1.5, s, egui::StrokeKind::Middle);
            p.hline(b.x_range(), b.top() + 3.5, s);
            if glyph == Glyph::Table {
                p.vline(b.left() + 4.5, egui::Rangef::new(b.top() + 3.5, b.bottom()), s);
            } else {
                p.hline(egui::Rangef::new(b.left() + 3.0, b.right() - 3.0), b.top() + 7.0, s);
            }
        }
        Glyph::Coll => {
            let g = p.layout_job(LayoutJob::single_section("{}".into(), mono_fmt(TXT_SMALL, color, W_SEMIBOLD)));
            p.galley(c - g.size() / 2.0 + v(0.0, optical_dy(&g)), g, color);
        }
        Glyph::Db => {
            p.add(egui::Shape::ellipse_stroke(c - v(0.0, 4.0), v(5.5, 2.0), s));
            p.vline(c.x - 5.5, egui::Rangef::new(c.y - 4.0, c.y + 4.0), s);
            p.vline(c.x + 5.5, egui::Rangef::new(c.y - 4.0, c.y + 4.0), s);
            p.line(arc(c + v(0.0, 4.0), 5.5, 2.0, 0.0, PI, 12), s);
        }
        Glyph::Reload => {
            let pts = arc(c, 5.0, 5.0, -0.6, 4.3, 16);
            let end = pts[0];
            p.line(pts, s);
            p.line(vec![end + v(-3.0, -0.5), end, end + v(0.5, -3.0)], s);
        }
        Glyph::Lock => {
            let b = egui::Rect::from_center_size(c + v(0.0, 2.0), v(10.0, 7.0));
            p.rect_stroke(b, 1.5, s, egui::StrokeKind::Middle);
            p.line(arc(egui::pos2(c.x, b.top()), 2.8, 2.8, PI, 2.0 * PI, 10), s);
        }
        Glyph::Tab => {
            let b = r(12.0, 9.0).translate(v(0.0, 1.0));
            p.rect_stroke(b, 1.5, s, egui::StrokeKind::Middle);
            p.line(vec![egui::pos2(b.left(), b.top() + 0.5), egui::pos2(b.left(), b.top() - 2.5), egui::pos2(b.left() + 5.0, b.top() - 2.5), egui::pos2(b.left() + 6.0, b.top())], s);
        }
        Glyph::Prompt => {
            p.line(vec![c + v(-4.5, -3.5), c + v(-1.0, 0.0), c + v(-4.5, 3.5)], s);
            p.hline(egui::Rangef::new(c.x + 0.5, c.x + 5.0), c.y + 3.5, s);
        }
    }
}

fn palette_footer(ui: &mut egui::Ui, cmds: bool, new_tab: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), PALETTE_FOOT_H), Sense::hover());
    let p = ui.painter();
    paint_footer_band(p, rect, R_LG);
    let cy = rect.center().y;
    const CMD_ENTER: [&str; 2] = [if cfg!(target_os = "macos") { "\u{2318}" } else { "Ctrl" }, "\u{21b5}"];
    let mut left: Vec<(&[&str], &str)> = vec![(&["\u{2191}", "\u{2193}"], "move"), (&["\u{21b5}"], if cmds { "run" } else if new_tab { "open in new tab" } else { "open" })];
    if cmds {
        left.push((&["\u{232b}"], "leave commands"));
    } else if !new_tab {
        left.push((&CMD_ENTER, "new tab"));
    }
    key_hints(p, rect.left() + 16.0, cy, &left);
    if !cmds {
        key_hint_right(p, rect.right() - 16.0, cy, ">", "commands");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_job_tints_only_the_matched_letters() {
        let job = name_job("order_items", &["ord".into(), "item".into()], Color32::WHITE, 300.0);
        let parts: Vec<&str> = job.sections.iter().map(|s| &job.text[s.byte_range.start.0..s.byte_range.end.0]).collect();
        assert_eq!(parts, ["ord", "er_", "item", "s"]);
        // A name whose lowercase form changes length keeps one section rather than panicking.
        let job = name_job("\u{130}stanbul", &["st".into()], Color32::WHITE, 300.0);
        assert_eq!(job.sections.len(), 1);
    }

    #[test]
    fn shortcut_spelling() {
        let k = keys(true, false, "R");
        assert!(k == "\u{21e7}\u{2318}R" || k == "Ctrl+Shift+R", "{k}");
    }
}
