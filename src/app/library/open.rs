//! Opening a library script into a tab, and writing a tab's text back to the script it came from. The panel and the
//! linked folders call `open_script` and `save_script`.

use super::*;
use crate::library::ScriptRef;

/// How a script leaves the library: Return opens it, Alt+Return opens it in a new tab, Cmd+Enter runs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum How {
    Here,
    NewTab,
    Run,
}

/// What the view on screen holds, as a script of the library would hold it.
pub(in crate::app) struct TabText {
    pub kind: &'static str,
    pub body: String,
    pub sort: String,
    pub project: String,
    pub view: View,
    pub db: String,
    pub schema: String,
    pub coll: String,
}

/// The view whose text is a script of `kind`.
pub(in crate::app) fn view_of_kind(kind: &str) -> Option<View> {
    Some(match kind {
        "sql" => View::Sql,
        "shell" => View::Shell,
        "filter" => View::Documents,
        "pipeline" => View::Aggregate,
        _ => return None,
    })
}


/// Where a script runs, as `App::script_target` works it out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) enum Target {
    On(u64),
    /// No connection is the obvious one: the person picks from these.
    Ask(Vec<u64>),
    /// Nothing can run it, and why.
    None(String),
}

/// Whether a script of `kind` can run on a connection of `engine`: SQL on MySQL, MariaDB and PostgreSQL, filters and
/// pipelines on MongoDB, Shell scripts on any.
pub(in crate::app) fn kind_fits(kind: &str, engine: db::sql::Engine) -> bool {
    match kind {
        "sql" => engine.is_sql(),
        "filter" | "pipeline" => !engine.is_sql(),
        "shell" => true,
        _ => false,
    }
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "sql" => "SQL script",
        "filter" => "query",
        "pipeline" => "pipeline",
        _ => "Shell script",
    }
}

/// The database named in a connection address, such as shop in mysql://app@host:3306/shop?ssl-mode=REQUIRED. Empty
/// when the address names none.
pub(in crate::app) fn uri_db(uri: &str) -> String {
    let Some((_, rest)) = uri.split_once("://") else { return String::new() };
    // A masked or encoded password may hold a slash, so the host starts after the last @.
    let host_on = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    let Some((_, path)) = host_on.split_once('/') else { return String::new() };
    path.split(['?', '#']).next().unwrap_or_default().trim_matches('/').to_string()
}

/// A script waiting for its connection to be ready, which `library_tick` opens once it is.
#[derive(Clone, Debug)]
pub(in crate::app) struct Pending {
    pub r: ScriptRef,
    pub how: How,
    pub conn: u64,
    /// The tab that was current when it was asked for: a Run becomes a plain open if another tab is current by then.
    pub uid: Option<u64>,
    /// The connection's connect attempt when it was asked for, so the failure it had then is not taken for a new one.
    pub attempt: u64,
    /// Egui time the wait started while the connection was not connecting, for the time limit.
    pub since: Option<f64>,
    /// The connection was seen connecting since, so a failure now is news even on the same attempt.
    pub seen_connecting: bool,
    pub name: String,
}

/// The Open on dialog: the connections a script can run on when none is the obvious one.
pub(in crate::app) struct OpenOnForm {
    pub r: ScriptRef,
    pub how: How,
    pub conns: Vec<u64>,
    pub sel: Option<u64>,
    pub name: String,
}

impl App {
    /// The text of the current tab's view as a script, or None for a view that holds no script (Indexes, Schema, a table's
    /// Content grid and the like).
    pub(in crate::app) fn tab_text(&self) -> Option<TabText> {
        Self::text_of(self.t()?)
    }

    /// Whether the current tab's view holds a script, as `tab_text` would find, without copying its text: the keys ask
    /// every frame.
    pub(in crate::app) fn tab_has_text(&self) -> bool {
        self.t().is_some_and(|t| matches!((t.view, t.sql.is_some()), (View::Sql, true) | (View::Shell, _) | (View::Documents | View::Aggregate, false)))
    }

    fn text_of(t: &Tab) -> Option<TabText> {
        let (db, coll) = (t.sel.db.clone(), t.sel.coll.clone());
        Some(match (t.view, t.sql.as_ref()) {
            (View::Sql, Some(s)) => {
                let run_db = if s.run_db.is_empty() { db } else { s.run_db.clone() };
                TabText { kind: "sql", body: s.query.clone(), sort: String::new(), project: String::new(), view: View::Sql, db: run_db, schema: s.run_schema.trim().to_string(), coll: String::new() }
            }
            (View::Shell, _) => TabText { kind: "shell", body: t.shell.code.clone(), sort: String::new(), project: String::new(), view: View::Shell, db, schema: String::new(), coll: String::new() },
            (View::Documents, None) => TabText { kind: "filter", body: t.filter_txt.clone(), sort: t.sort_txt.clone(), project: t.proj_txt.clone(), view: View::Documents, db, schema: t.sel.schema.clone(), coll },
            (View::Aggregate, None) => TabText { kind: "pipeline", body: t.agg_txt.clone(), sort: String::new(), project: String::new(), view: View::Aggregate, db, schema: t.sel.schema.clone(), coll },
            _ => return None,
        })
    }

    /// The binding a restored tab gets back: the script is still in the library and the tab shows the view it is written in.
    /// Its base is the saved text, so edits made before the restart still show as edits. A linked file is bound with the
    /// text the tab holds; the linked folders' code adjusts its disk state when it rescans.
    pub(in crate::app) fn rebind(&self, t: &Tab, r: ScriptRef) -> Option<Bound> {
        if !r.is_safe() {
            return None;
        }
        let (view, base) = match &r {
            ScriptRef::Saved(id) => {
                let s = self.library.scripts.iter().find(|s| s.id == *id)?;
                (view_of_kind(&s.kind)?, s.body.clone())
            }
            ScriptRef::File { .. } => {
                let tt = Self::text_of(t)?;
                (tt.view, tt.body)
            }
        };
        (t.view == view).then_some(Bound { r, view, base, disk: None, conflict: None })
    }

    /// The database a SQL or Shell script opens in: its own when it names one, else the current tab's on the same
    /// connection, else the connection's default.
    fn script_db(&self, conn: u64, wanted: &str) -> String {
        if !wanted.is_empty() {
            return wanted.to_string();
        }
        if let Some(t) = self.t().filter(|t| t.sel.conn == conn && !t.sel.db.is_empty()) {
            return t.sel.db.clone();
        }
        self.default_db(conn)
    }

    /// The database a connection opens in when nothing names one: the one in its address, else the first that is not the
    /// server's own (information_schema, mysql, admin and the like), never simply the first in the list.
    pub(in crate::app) fn default_db(&self, conn: u64) -> String {
        let Some(c) = self.conn(conn) else { return String::new() };
        let dbs: Vec<&str> = c.dbs.iter().map(|d| d.0.as_str()).collect();
        let named = uri_db(&c.masked_uri);
        if !named.is_empty() && (dbs.is_empty() || dbs.contains(&named.as_str())) {
            return named;
        }
        let system = |d: &str| ["admin", "local", "config"].contains(&d) || db::sql::create::SYSTEM_SCHEMAS.contains(&d);
        dbs.iter().find(|d| !system(d)).or(dbs.first()).map(|d| d.to_string()).unwrap_or_default()
    }

    /// Where a script runs: a saved script on its own connection, a linked file on the current tab's connection when the
    /// link is used there, else on the active one. When neither fits, the person picks from the connections that do; one is
    /// never taken at random, since it could be production.
    pub(in crate::app) fn script_target(&self, r: &ScriptRef) -> Target {
        match r {
            ScriptRef::Saved(id) => {
                let Some(s) = self.library.scripts.iter().find(|s| s.id == *id) else { return Target::None("That script is no longer in the library.".into()) };
                if self.conn(s.conn).is_some() {
                    return Target::On(s.conn);
                }
                let fit: Vec<u64> = self.conns.iter().filter(|c| kind_fits(&s.kind, c.meta.engine)).map(|c| c.meta.id).collect();
                if fit.is_empty() { Target::None("The connection of this script was removed, and no other connection can run it.".into()) } else { Target::Ask(fit) }
            }
            ScriptRef::File { link, rel } => {
                let Some(l) = self.library.links.iter().find(|l| l.id == *link) else { return Target::None("That folder is no longer linked.".into()) };
                let kind = self.file_kind(*link, rel);
                let fits = |id: u64| self.conn(id).is_some_and(|c| crate::library::format::fits(l, &kind, id, c.meta.engine));
                if let Some(c) = self.t().map(|t| t.sel.conn).into_iter().chain(self.active).find(|c| fits(*c)) {
                    return Target::On(c);
                }
                let fit: Vec<u64> = self.conns.iter().map(|c| c.meta.id).filter(|c| fits(*c)).collect();
                if fit.is_empty() { Target::None("No connection this folder is used on can run that file.".into()) } else { Target::Ask(fit) }
            }
        }
    }

    /// "Runs on shop-prod  ·  shop  ·  read-only": where the script would run now, for the preview card and the Run tips.
    pub(in crate::app) fn runs_on(&self, r: &ScriptRef) -> String {
        match self.script_target(r) {
            Target::On(c) => {
                let wanted = match r {
                    ScriptRef::Saved(id) => self.library.scripts.iter().find(|s| s.id == *id).map(|s| s.db.clone()).unwrap_or_default(),
                    ScriptRef::File { .. } => String::new(),
                };
                let db = self.script_db(c, &wanted);
                let lock = if self.conn_writable(c) { "writable" } else { "read-only" };
                [format!("Runs on {}", self.conn_name(c)), db, lock.to_string()].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("  \u{b7}  ")
            }
            Target::Ask(_) => "Asks which connection to run on".into(),
            Target::None(why) => why,
        }
    }

    /// Whether running this script now could change data: a SQL statement that is not a read, a pipeline with a write stage,
    /// or any Shell script, whose code can do anything.
    fn script_writes(&self, s: &crate::library::Script) -> bool {
        match s.kind.as_str() {
            "sql" => db::sql::lex::read_only_check(&s.body, self.dialect_of(s.conn)).is_err(),
            "shell" => true,
            "pipeline" if db::is_shell(&s.body) => db::parse_shell(&s.body).is_ok_and(|sh| matches!(sh.kind, db::ShellKind::Aggregate(p) if db::has_write_stage(&p))),
            "pipeline" => db::parse_pipeline(&s.body).is_ok_and(|p| db::has_write_stage(&p)),
            _ => false,
        }
    }

    /// Whether tab `i` is one that opening a script can replace: it holds no text of the person's beyond the script it
    /// was opened from, and that is unchanged.
    fn tab_clean(&self, i: usize, kind: &str) -> bool {
        let Some(t) = self.tabs.get(i) else { return false };
        let text = match (kind, t.sql.as_ref()) {
            ("sql", Some(s)) if t.view == View::Sql && t.sel.coll.is_empty() => s.query.as_str(),
            _ => return false,
        };
        let staged = t.sql.as_ref().is_some_and(|s| !s.st.staged.is_empty());
        let unchanged = match &t.script {
            Some(b) => b.view == View::Sql && b.base == text,
            None => text.trim().is_empty(),
        };
        unchanged && !staged
    }

    /// Scripts that left the library (deleted, or moved into a linked folder): tabs bound to them keep their text but are
    /// unbound, so their next Cmd+S opens the Save dialog, and they leave the recent list and a pending open.
    pub(in crate::app) fn forget_scripts(&mut self, ids: &[u64]) {
        let gone = |r: &ScriptRef| matches!(r, ScriptRef::Saved(id) if ids.contains(id));
        for t in &mut self.tabs {
            if t.script.as_ref().is_some_and(|b| gone(&b.r)) {
                t.script = None;
            }
        }
        self.library.recent.retain(|r| !gone(r));
        if self.lib_ui.pending.as_ref().is_some_and(|p| gone(&p.r)) {
            self.lib_ui.pending = None;
        }
    }

    /// Ties the current tab to a script, so Cmd+S writes it back, and puts it first in the recent list.
    pub(in crate::app) fn bind_script(&mut self, r: ScriptRef, view: View, base: String) {
        if let Some(t) = self.tm() {
            t.script = Some(Bound { r: r.clone(), view, base, disk: None, conflict: None });
        }
        self.library.push_recent(r);
        self.save_library();
    }

    /// Opens the script `r` in a tab, the way `how` says. A plain open takes the current tab only when it is a clean one
    /// of the same kind and connection, like a click on a table, and otherwise opens a tab beside it. A script whose
    /// connection is not ready waits in `lib_ui.pending` and opens when it is. Run goes through the gated run paths.
    pub(in crate::app) fn open_script(&mut self, r: &ScriptRef, how: How) {
        match self.script_target(r) {
            Target::On(c) => self.open_script_on(r, how, c),
            Target::Ask(conns) => {
                let name = self.script_name(r);
                self.modal = Some(Modal::OpenOn(Box::new(OpenOnForm { r: r.clone(), how, conns, name, sel: None })));
            }
            Target::None(why) => self.warn(why),
        }
    }

    /// `open_script` on the connection `conn`, which the person picked or the target rule gave.
    pub(in crate::app) fn open_script_on(&mut self, r: &ScriptRef, how: How, conn: u64) {
        match r {
            ScriptRef::Saved(id) => match self.library.scripts.iter().find(|s| s.id == *id).cloned() {
                Some(mut s) => {
                    s.conn = conn;
                    self.open_resolved(r, how, s, None)
                }
                None => self.warn("That script is no longer in the library."),
            },
            // A linked file is read on the worker, and opens on `conn` when it is back.
            ScriptRef::File { link, rel } => self.read_file_job(*link, rel, super::linked::Job::Open { r: r.clone(), how, conn }),
        }
    }

    /// The name a script shows: a saved script's own, a linked file's file name.
    pub(in crate::app) fn script_name(&self, r: &ScriptRef) -> String {
        match r {
            ScriptRef::Saved(id) => self.library.scripts.iter().find(|s| s.id == *id).map(|s| s.name.clone()).unwrap_or_default(),
            ScriptRef::File { rel, .. } => rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        }
    }

    /// `open_script` once the script is known, with `s.conn` the connection it opens on. `file_meta` is the time and length
    /// of a linked file as it was read, which go on the tab's binding, so a change on disk is told from the text just read.
    pub(in crate::app) fn open_resolved(&mut self, r: &ScriptRef, how: How, s: crate::library::Script, file_meta: Option<Option<(std::time::SystemTime, u64)>>) {
        let file = match (r, file_meta) {
            (ScriptRef::File { link, rel }, Some(meta)) => Some((*link, rel.clone(), meta)),
            _ => None,
        };
        let Some((status_ready, engine, cname, attempt)) = self.conn(s.conn).map(|c| (c.status == Status::Ready, c.meta.engine, c.meta.name.clone(), c.attempt)) else {
            return self.warn("The connection of this script was removed. The script stays in the library, but it needs a connection to open.");
        };
        // Checked before anything opens, so a script of another engine never leaves a half-built tab behind.
        if view_of_kind(&s.kind).is_none() {
            return self.warn(format!("This build does not know scripts of kind \"{}\".", s.kind));
        }
        if !kind_fits(&s.kind, engine) {
            let what = if engine.is_sql() { "SQL" } else { "MongoDB" };
            return self.warn(format!("Not opened: \"{}\" is a {}, and {cname} is a {what} connection.", s.name, kind_label(&s.kind)));
        }
        if matches!(s.kind.as_str(), "filter" | "pipeline") && (s.coll.is_empty() || s.db.is_empty()) {
            return self.warn(format!("\"{}\" names no collection. Save it again from the collection it belongs to.", s.name));
        }
        if !status_ready && !self.demo {
            let uid = self.t().map(|t| t.uid);
            let seen_connecting = self.conn(s.conn).is_some_and(|c| c.status == Status::Connecting);
            self.lib_ui.pending = Some(Pending { r: r.clone(), how, conn: s.conn, uid, attempt, since: None, seen_connecting, name: s.name.clone() });
            // activate connects only an idle connection; a failed one is tried again.
            if self.conn(s.conn).is_some_and(|c| matches!(c.status, Status::Failed(_))) {
                self.connect(s.conn);
            }
            self.activate(s.conn);
            self.toast_acts(format!("Opening {} when {cname} connects", s.name), vec![("Cancel".into(), crate::app::errors::Act::CancelOpen)]);
            return;
        }
        self.lib_ui.pending = None;
        let new_tab = how == How::NewTab;
        // Run is "Run all". On a writable connection a script that can change data is opened, not run, with a notice
        // above the editor, and the person runs it from there. A read-only connection keeps its own gates.
        let writes = how == How::Run && self.conn_writable(s.conn) && self.script_writes(&s);
        let run = how == How::Run && !writes;
        match s.kind.as_str() {
            "sql" => {
                let db = self.script_db(s.conn, &s.db);
                let reuse = !new_tab && self.tab_clean(self.cur, "sql") && self.t().is_some_and(|t| t.sel.conn == s.conn && t.sel.db == db);
                if !reuse {
                    self.sql_new_query(s.conn, db);
                }
                if let Some(q) = self.tm().and_then(|t| t.sql.as_mut()) {
                    q.query = s.body.clone();
                    q.caret = 0;
                    q.run_schema = s.schema.clone();
                    // A reused tab may have been switched to another database; the script runs in its own.
                    q.run_db = String::new();
                }
                self.bind_script(r.clone(), View::Sql, s.body);
                if run {
                    self.sql_run(true, false);
                }
            }
            "shell" => {
                let db = self.script_db(s.conn, &s.db);
                if engine.is_sql() {
                    self.sql_new_query(s.conn, db);
                } else {
                    // The Shell needs no collection, so its tab has none and is named for the database.
                    let sel = Sel { conn: s.conn, db, schema: String::new(), coll: String::new() };
                    self.open_tab(sel, Open::NewTab);
                }
                self.open_in_shell(s.body.clone());
                self.bind_script(r.clone(), View::Shell, s.body);
                if run {
                    self.run_shell_script();
                }
            }
            "filter" => {
                let sel = Sel { conn: s.conn, db: s.db.clone(), schema: s.schema.clone(), coll: s.coll.clone() };
                self.open_tab(sel, if new_tab { Open::NewTab } else { Open::Link });
                if let Some(t) = self.tm() {
                    t.clear_options();
                    (t.filter_txt, t.sort_txt, t.proj_txt) = (s.body.clone(), s.sort.clone(), s.project.clone());
                    t.view = View::Documents;
                }
                self.bind_script(r.clone(), View::Documents, s.body);
                // A filter only reads, so it runs on open, as a click on a collection does.
                self.run_query();
            }
            "pipeline" => {
                let sel = Sel { conn: s.conn, db: s.db.clone(), schema: s.schema.clone(), coll: s.coll.clone() };
                self.open_tab(sel, if new_tab { Open::NewTab } else { Open::Link });
                if let Some(t) = self.tm() {
                    t.agg_txt = s.body.clone();
                    t.view = View::Aggregate;
                }
                self.bind_script(r.clone(), View::Aggregate, s.body.clone());
                // A pipeline can be heavy, so only Run runs it; Open loads it.
                if run {
                    self.run_script_pipeline(&s.body);
                }
            }
            other => return self.warn(format!("This build does not know scripts of kind \"{other}\".")),
        }
        if writes && let Some(uid) = self.t().map(|t| t.uid) {
            self.lib_ui.writes.insert(uid);
        }
        if let Some((link, rel, meta)) = file {
            if let Some(b) = self.tm().and_then(|t| t.script.as_mut()) {
                b.disk = meta;
            }
            self.mark_seen(link, &rel, meta);
        }
    }

    /// Runs a pipeline opened from the library through the Aggregate view's own gate, which asks before a write stage.
    pub(in crate::app) fn run_script_pipeline(&mut self, text: &str) {
        if db::is_shell(text) {
            return self.run_shell(text);
        }
        match db::parse_pipeline(text) {
            Ok(p) => self.run_pipeline(p),
            Err(e) => self.err(e),
        }
    }

    /// Cmd+S and the Save button: writes the view's text back to the script the tab is bound to, or opens the Save to
    /// library dialog when there is none. Page handlers consume Cmd+S first (staged grid edits, document drafts), so this
    /// only runs when the person means the script.
    pub(in crate::app) fn save_script(&mut self) {
        let Some(tt) = self.tab_text() else { return self.warn("There is no script to save in this view. Open a Query, Documents, Aggregate or Shell view.") };
        if tt.body.trim().is_empty() && tt.sort.trim().is_empty() && tt.project.trim().is_empty() {
            return self.warn("Nothing to save: the script is empty.");
        }
        // A tab bound to a linked file writes that file, or raises the conflict when it changed on disk.
        if self.save_linked() {
            return;
        }
        let bound = self.t().and_then(|t| t.script.as_ref()).filter(|b| b.view == tt.view).map(|b| b.r.clone());
        // A bound linked file is written by the linked folders' code; until then it takes the dialog.
        if let Some(ScriptRef::Saved(id)) = bound
            && let Some(i) = self.library.scripts.iter().position(|s| s.id == id)
        {
            let tab_conn = self.t().map(|t| t.sel.conn);
            let sc = &mut self.library.scripts[i];
            (sc.body, sc.sort, sc.project, sc.updated) = (tt.body.clone(), tt.sort.clone(), tt.project.clone(), crate::library::now_secs());
            // The database and collection the tab runs on now are part of what is saved, as the Save dialog keeps them. A script
            // opened on another connection keeps its own, which name places on its own server.
            let own = tab_conn == Some(sc.conn);
            if own && !tt.db.is_empty() {
                (sc.db, sc.schema) = (tt.db.clone(), tt.schema.clone());
            }
            if own && !tt.coll.is_empty() {
                sc.coll = tt.coll.clone();
            }
            let name = sc.name.clone();
            self.bind_script(ScriptRef::Saved(id), tt.view, tt.body.clone());
            self.ok(format!("Saved \"{name}\""));
            self.warn_plain_password(&tt);
            return;
        }
        self.open_save_dialog(tt);
    }

    /// Cmd+Shift+S and the chip's Save as new: the Save dialog for the tab's text as a new script, whatever the tab is bound
    /// to, so the bound one stays as it was.
    pub(in crate::app) fn save_script_as(&mut self) {
        let Some(tt) = self.tab_text() else { return };
        if tt.body.trim().is_empty() && tt.sort.trim().is_empty() && tt.project.trim().is_empty() {
            return self.warn("Nothing to save: the script is empty.");
        }
        let name = self.t().and_then(|t| t.script.as_ref()).map(|b| self.script_name(&b.r));
        self.open_save_dialog(tt);
        if let (Some(Modal::SaveScript(f)), Some(n)) = (&mut self.modal, name) {
            let stem = std::path::Path::new(&n).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(n);
            f.name = format!("{stem} copy");
        }
    }

    /// Cmd+Shift+L: the Library panel on screen with the keys on its tree.
    pub(in crate::app) fn focus_library(&mut self) {
        if self.prefs.sidebar_hidden || self.prefs.side_mode() != SideMode::Library {
            self.prefs.set_side_mode(SideMode::Library);
            self.prefs.sidebar_hidden = false;
            self.save_prefs();
        }
        self.lib_ui.focus_tree = true;
    }

    /// The chip's Show in library: the panel on screen with the script selected and the folders above it open.
    pub(in crate::app) fn show_in_library(&mut self, r: &ScriptRef) {
        use crate::library::tree::Node;
        self.focus_library();
        let st = &mut self.lib_ui.panel;
        st.query.clear();
        let node = match r {
            ScriptRef::Saved(id) => {
                let lib = &self.library;
                let mut at = lib.scripts.iter().find(|s| s.id == *id).and_then(|s| crate::library::tree::live_parent(lib, s.folder));
                // Bounded, since a hand-edited file could hold a loop of folders.
                for _ in 0..32 {
                    let Some(f) = at else { break };
                    st.open.insert(Node::Folder(f));
                    at = lib.folders.iter().find(|x| x.id == f).and_then(|x| crate::library::tree::live_parent(lib, x.parent));
                }
                // A script of another connection shows only in the All connections scope.
                let conn = lib.scripts.iter().find(|s| s.id == *id).map(|s| s.conn);
                if conn.is_some() && conn != self.t().map(|t| t.sel.conn).or(self.active) {
                    self.prefs.lib_scope_all = true;
                }
                Node::Script(*id)
            }
            ScriptRef::File { link, rel } => {
                st.open.insert(Node::Link(*link));
                let mut at = std::path::PathBuf::new();
                for c in rel.parent().into_iter().flat_map(|p| p.components()) {
                    at.push(c);
                    st.open.insert(Node::Dir { link: *link, rel: at.clone() });
                }
                Node::File { link: *link, rel: rel.clone() }
            }
        };
        let st = &mut self.lib_ui.panel;
        st.sel = Some(node);
        st.focus = true;
    }

    /// The chip's Unbind: the tab keeps its text but no longer writes back to the script, so Cmd+S asks again.
    pub(in crate::app) fn unbind_script(&mut self) {
        if let Some(t) = self.tm() {
            t.script = None;
        }
    }

    /// Whether tab `i` holds changes to the script it is bound to that are not saved to it yet.
    pub(in crate::app) fn tab_edited(&self, i: usize) -> bool {
        self.tabs.get(i).is_some_and(|t| t.script.as_ref().is_some_and(|b| super::linked::bound_text(t).is_some_and(|x| x != b.base)))
    }

    /// The connection the Library panel scopes to: the current tab's, else the active one.
    pub(in crate::app) fn lib_conn(&self) -> Option<u64> {
        self.t().map(|t| t.sel.conn).filter(|c| self.conn(*c).is_some()).or(self.active.filter(|c| self.conn(*c).is_some()))
    }

    /// A SQL script with a password in it is saved as typed, since one with the password cut out would not run, and the
    /// person is told where it now sits.
    pub(in crate::app) fn warn_plain_password(&mut self, tt: &TabText) {
        if tt.kind == "sql" && !db::redact_sql(&tt.body).1.is_empty() {
            self.warn("Saved, but the script holds a password in plain text in the library file.");
        }
    }

    /// What shows above a script's editor: the conflict of a linked file changed on disk, and the notice of a script that
    /// Run opened instead of running because it can write.
    pub(in crate::app) fn script_banners(&mut self, ui: &mut egui::Ui) {
        self.conflict_banner(ui);
        self.writes_banner(ui);
    }

    /// "This script writes": Run all from the library opened the script on a writable connection without running it. Run all
    /// here runs it through the view's own gates (the Shell and a pipeline with a write stage still ask first).
    fn writes_banner(&mut self, ui: &mut egui::Ui) {
        let Some(t) = self.t() else { return };
        if !self.lib_ui.writes.contains(&t.uid) {
            return;
        }
        let (uid, view, conn) = (t.uid, t.view, t.sel.conn);
        let cname = self.conn_name(conn);
        let mut act = 0;
        banner_frame(WARNING()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.set_min_height(BUTTON_H);
                glyph_icon(ui, glyphs::warn, WARNING(), 16.0);
                ui.add_space(4.0);
                ui.label(semibold(RichText::new("This script writes").size(TXT_BODY).color(BRIGHT())));
                ui.label(RichText::new(format!("{cname} is writable, so it was opened, not run.")).size(TXT_BODY).color(TEXT()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if quiet(ui, "Dismiss").clicked() {
                        act = 2;
                    }
                    if primary(ui, "Run all", WARNING()).on_hover_text("Runs the whole script through the usual checks").clicked() {
                        act = 1;
                    }
                });
            });
        });
        ui.add_space(10.0);
        if act == 0 {
            return;
        }
        self.lib_ui.writes.remove(&uid);
        if act == 1 {
            match view {
                View::Sql => self.sql_run(true, false),
                View::Shell => self.run_shell_script(),
                View::Aggregate => {
                    let text = self.t().map(|t| t.agg_txt.clone()).unwrap_or_default();
                    self.run_script_pipeline(&text);
                }
                _ => {}
            }
        }
    }

    /// Open on: the connections a script can run on, grouped as the Connections panel groups them, when no tab or active
    /// connection is the obvious one. Nothing is preselected, so a production connection is never a default.
    pub(in crate::app) fn open_on_dialog(&mut self, ui: &mut egui::Ui, f: &mut OpenOnForm, keep: &mut bool) {
        ui.set_width(460.0);
        let verb = if f.how == How::Run { "Run all" } else { "Open" };
        dialog_head(ui, &Head::new("Which connection?").sub(f.name.clone()).glyph(glyphs::plug));
        // Grouped by the connection groups, in their order, then the connections in no group.
        let mut sections: Vec<(String, Vec<u64>)> = Vec::new();
        for g in &self.groups.groups {
            let ids: Vec<u64> = g.members.iter().copied().filter(|c| f.conns.contains(c)).collect();
            if !ids.is_empty() {
                sections.push((g.name.to_uppercase(), ids));
            }
        }
        let loose: Vec<u64> = f.conns.iter().copied().filter(|c| self.groups.group_of(*c).is_none()).collect();
        if !loose.is_empty() {
            sections.push((if sections.is_empty() { String::new() } else { "NOT IN A GROUP".into() }, loose));
        }
        let mut go = false;
        dialog_body(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("open-on").max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
                let w = ui.available_width();
                for (title, ids) in &sections {
                    if !title.is_empty() {
                        card_caption(ui, title, "");
                    }
                    for id in ids {
                        let Some(c) = self.conn(*id) else { continue };
                        let lock = if c.writable { "writable" } else { "read-only" };
                        let desc = [c.meta.tag.as_str(), c.masked_uri.as_str(), lock].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("  \u{b7}  ");
                        let r = radio_card(ui, w, f.sel == Some(*id), &c.meta.name, &desc);
                        if r.clicked() {
                            f.sel = Some(*id);
                        }
                        if r.double_clicked() {
                            (f.sel, go) = (Some(*id), true);
                        }
                        ui.add_space(6.0);
                    }
                }
            });
        });
        dialog_foot(ui, |ui| {
            go |= ui.add_enabled(f.sel.is_some(), primary_button(ui, verb, PRIMARY())).clicked();
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
        });
        if let (true, Some(c)) = (go, f.sel) {
            *keep = false;
            let (r, how) = (f.r.clone(), f.how);
            self.open_script_on(&r, how, c);
        }
    }
}
