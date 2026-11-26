//! Opening a library script into a tab, and writing a tab's text back to the script it came from. The panel and the
//! linked folders call `open_script` and `save_script`.

use super::*;
use crate::library::ScriptRef;
use quarry_core::action::{Action, LibA};
pub(crate) use quarry_core::state::library::open::*;
pub(crate) use quarry_core::state::library::open::Target;

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "sql" => "SQL script",
        "filter" => "query",
        "where" => "table filter",
        "pipeline" => "pipeline",
        _ => "Shell script",
    }
}

impl App {
    /// The text of the current tab's view as a script, or None for a view that holds no script (Indexes, Schema, a SQL
    /// table's Structure and the like). A SQL table's Content view holds its WHERE and ORDER BY as a table filter.
    pub(in crate::app) fn tab_text(&self) -> Option<TabText> {
        self.core.tab_text()
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
                let s = self.core.library.scripts.iter().find(|s| s.id == *id)?;
                (view_of_kind(&s.kind)?, s.body.clone())
            }
            ScriptRef::File { .. } => {
                let tt = text_of(t)?;
                (tt.view, tt.body)
            }
        };
        (t.view == view).then_some(Bound { r, view, base, disk: None, conflict: None })
    }

    /// The database a SQL or Shell script opens in: its own when it names one, else the current tab's on the same
    /// connection, else the connection's default.
    pub(in crate::app) fn script_db(&self, conn: u64, wanted: &str) -> String {
        if !wanted.is_empty() {
            return wanted.to_string();
        }
        if let Some(t) = self.t().filter(|t| t.sel.conn == conn && !t.sel.db.is_empty()) {
            return t.sel.db.clone();
        }
        self.default_db(conn)
    }

    /// Whether a saved script already runs at this target, so Make default has nothing to change. A script saved without
    /// a database or schema runs in the tab's own: `own_schema` is that schema.
    pub(in crate::app) fn runs_at(&self, s: &crate::library::Script, conn: u64, db: &str, schema: &str, own_schema: &str) -> bool {
        s.conn == conn && self.script_db(conn, &s.db) == db && (s.schema == schema || s.schema.is_empty() && schema == own_schema)
    }

    pub(in crate::app) fn default_db(&self, conn: u64) -> String {
        self.core.default_db(conn)
    }

    /// Where a script runs: a saved script on its own connection, a linked file on the current tab's connection when the
    /// link is used there, else on the active one. When neither fits, the person picks from the connections that do; one is
    /// never taken at random, since it could be production.
    pub(in crate::app) fn script_target(&self, r: &ScriptRef) -> Target {
        match r {
            ScriptRef::Saved(id) => {
                let Some(s) = self.core.library.scripts.iter().find(|s| s.id == *id) else { return Target::None("That script is no longer in the library.".into()) };
                if self.conn(s.conn).is_some() {
                    return Target::On(s.conn);
                }
                let fit: Vec<u64> = self.core.conns.iter().filter(|c| kind_fits(&s.kind, c.meta.engine)).map(|c| c.meta.id).collect();
                if fit.is_empty() { Target::None("The connection of this script was removed, and no other connection can run it.".into()) } else { Target::Ask(fit) }
            }
            ScriptRef::File { link, rel } => {
                let Some(l) = self.core.library.links.iter().find(|l| l.id == *link) else { return Target::None("That folder is no longer linked.".into()) };
                let kind = self.core.file_kind(*link, rel);
                let fits = |id: u64| self.conn(id).is_some_and(|c| crate::library::format::fits(l, &kind, id, c.meta.engine));
                if let Some(c) = self.t().map(|t| t.sel.conn).into_iter().chain(self.core.active).find(|c| fits(*c)) {
                    return Target::On(c);
                }
                let fit: Vec<u64> = self.core.conns.iter().map(|c| c.meta.id).filter(|c| fits(*c)).collect();
                if fit.is_empty() { Target::None("No connection this folder is used on can run that file.".into()) } else { Target::Ask(fit) }
            }
        }
    }

    /// "Runs on shop-prod  ·  shop  ·  read-only": where the script would run now, for the preview card and the Run tips.
    pub(in crate::app) fn runs_on(&self, r: &ScriptRef) -> String {
        match self.script_target(r) {
            Target::On(c) => {
                let wanted = match r {
                    ScriptRef::Saved(id) => self.core.library.scripts.iter().find(|s| s.id == *id).map(|s| s.db.clone()).unwrap_or_default(),
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

    /// The `{{...}}` values of a SQL or Shell script, or of a filter with its sort and projection, or None for one without
    /// any. A plain script costs one scan for `{{`.
    pub(in crate::app) fn script_snippet(&self, s: &crate::library::Script) -> Option<crate::library::snippet::Parsed> {
        snippet_of(s, || self.dialect_of(s.conn))
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
    pub(in crate::app) fn tab_clean(&self, i: usize, kind: &str) -> bool {
        let Some(t) = self.core.tabs.get(i) else { return false };
        let text = match (kind, t.sql.as_ref()) {
            ("sql", Some(s)) if t.view == View::Sql && t.sel.coll.is_empty() => s.query.as_str(),
            _ => return false,
        };
        // A tab still running is busy too, even with its text cleared: its answer would land under the new script.
        let staged = t.sql.as_ref().is_some_and(|s| !s.st.staged.is_empty() || s.run_req != 0);
        let unchanged = match &t.script {
            Some(b) => b.view == View::Sql && b.base == text,
            None => text.trim().is_empty(),
        };
        unchanged && !staged
    }

    /// Opens the script `r` in a tab, the way `how` says. A plain open takes the current tab only when it is a clean one
    /// of the same kind and connection, like a click on a table, and otherwise opens a tab beside it. A script whose
    /// connection is not ready waits in `lib_ui.pending` and opens when it is. Run goes through the gated run paths.
    pub(in crate::app) fn open_script(&mut self, r: &ScriptRef, how: How) {
        // Run on a snippet asks for its values first, whether from Cmd+K, the Library panel or a picker.
        if how == How::Run
            && let ScriptRef::Saved(id) = r
            && self.core.library.scripts.iter().any(|s| s.id == *id && self.script_snippet(s).is_some())
        {
            return self.open_snippet_form(*id, None);
        }
        // A script already open in another tab is switched to: a second copy comes only from New tab or a duplicated tab.
        // Opened again on the tab it is in, it takes its saved text back, and Undo keeps the edits it replaced.
        if how == How::Here
            && let Some(i) = self.core.tabs.iter().position(|t| t.script.as_ref().is_some_and(|b| &b.r == r))
        {
            if i != self.core.cur || !self.tab_edited(i) {
                return self.set_cur(i);
            }
            if let ScriptRef::Saved(id) = r
                && let Some(s) = self.core.library.scripts.iter().find(|s| s.id == *id).cloned()
            {
                let Some(tab) = self.core.tabs.get(i).map(|t| t.uid) else { return };
                return self.exec(Action::Library(LibA::ReloadBound { tab, script: s.id }));
            }
        }
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
            ScriptRef::Saved(id) => match self.core.library.scripts.iter().find(|s| s.id == *id).cloned() {
                Some(mut s) => {
                    s.conn = conn;
                    self.open_resolved(r, how, s, None)
                }
                None => self.warn("That script is no longer in the library."),
            },
            // A linked file is read on the worker, and opens on `conn` when it is back.
            ScriptRef::File { .. } => self.exec(Action::Library(LibA::OpenFile { r: r.clone(), how, conn })),
        }
    }

    /// The name a script shows: a saved script's own, a linked file's file name.
    pub(in crate::app) fn script_name(&self, r: &ScriptRef) -> String {
        match r {
            ScriptRef::Saved(id) => self.core.library.scripts.iter().find(|s| s.id == *id).map(|s| s.name.clone()).unwrap_or_default(),
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
        if matches!(s.kind.as_str(), "filter" | "pipeline" | "where") && (s.coll.is_empty() || s.db.is_empty()) {
            return self.warn(format!("\"{}\" names no collection. Save it again from the collection it belongs to.", s.name));
        }
        if !status_ready && !self.core.demo {
            let uid = self.cur_uid();
            let seen_connecting = self.conn(s.conn).is_some_and(|c| c.status == Status::Connecting);
            self.exec(Action::Library(LibA::Wait { r: r.clone(), how, conn: s.conn, uid, attempt, seen_connecting, name: s.name.clone() }));
            // activate connects only an idle connection; a failed one is tried again.
            if self.conn(s.conn).is_some_and(|c| matches!(c.status, Status::Failed(_))) {
                self.connect(s.conn);
            }
            self.activate(s.conn);
            self.toast_acts(format!("Opening {} when {cname} connects", s.name), vec![("Cancel".into(), crate::app::errors::Act::CancelOpen)]);
            return;
        }
        self.exec(Action::Library(LibA::CancelPending));
        let new_tab = how == How::NewTab;
        // Run is "Run all". On a writable connection a script that can change data is opened, not run, with a notice
        // above the editor, and the person runs it from there. A read-only connection keeps its own gates.
        let writes = how == How::Run && self.conn_writable(s.conn) && self.script_writes(&s);
        let run = how == How::Run && !writes;
        match s.kind.as_str() {
            "sql" => {
                let db = self.script_db(s.conn, &s.db);
                let reuse = !new_tab && self.tab_clean(self.core.cur, "sql") && self.t().is_some_and(|t| t.sel.conn == s.conn && t.sel.db == db);
                if !reuse {
                    self.sql_new_query(s.conn, db);
                }
                // A linked file with {{...}} values opens to be filled in; it never runs with them empty.
                let snippet = self.script_snippet(&s).is_some();
                self.exec(Action::Library(LibA::Fill { r: r.clone(), script: Box::new(s) }));
                if run && !snippet {
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
                // The values of what the tab held before are not this script's, and a linked file with values opens to
                // be filled in.
                let snippet = self.script_snippet(&s).is_some();
                self.exec(Action::Library(LibA::Fill { r: r.clone(), script: Box::new(s) }));
                if run && !snippet {
                    self.run_shell_script();
                }
            }
            "filter" => {
                let sel = Sel { conn: s.conn, db: s.db.clone(), schema: s.schema.clone(), coll: s.coll.clone() };
                self.open_tab(sel, if new_tab { Open::NewTab } else { Open::Link });
                self.exec(Action::Library(LibA::Fill { r: r.clone(), script: Box::new(s) }));
                // A filter only reads, so it runs on open, as a click on a collection does.
                self.run_query();
            }
            "where" => {
                let sel = Sel { conn: s.conn, db: s.db.clone(), schema: s.schema.clone(), coll: s.coll.clone() };
                self.open_tab(sel, if new_tab { Open::NewTab } else { Open::Link });
                self.exec(Action::Library(LibA::Fill { r: r.clone(), script: Box::new(s) }));
                // A filter only reads, so it runs on open, as a click on the table does.
                self.reload();
            }
            "pipeline" => {
                let sel = Sel { conn: s.conn, db: s.db.clone(), schema: s.schema.clone(), coll: s.coll.clone() };
                self.open_tab(sel, if new_tab { Open::NewTab } else { Open::Link });
                let body = s.body.clone();
                self.exec(Action::Library(LibA::Fill { r: r.clone(), script: Box::new(s) }));
                // A pipeline can be heavy, so only Run runs it; Open loads it.
                if run {
                    self.run_script_pipeline(&body);
                }
            }
            other => return self.warn(format!("This build does not know scripts of kind \"{other}\".")),
        }
        if writes && let Some(uid) = self.cur_uid() {
            self.exec(Action::Library(LibA::Writes { tab: uid, on: true }));
        }
        if let Some((link, rel, meta)) = file {
            self.exec(Action::Library(LibA::Seen { link, rel, meta }));
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
        self.exec(Action::Library(LibA::SaveTab));
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
        if self.core.prefs.sidebar_hidden || self.core.prefs.side_mode() != SideMode::Library {
            self.set_side(SideMode::Library, false);
        }
        self.ui.lib_focus_tree = true;
    }

    /// The chip's Show in library: the panel on screen with the script selected and the folders above it open.
    pub(in crate::app) fn show_in_library(&mut self, r: &ScriptRef) {
        self.focus_library();
        self.exec(Action::Library(LibA::Show(r.clone())));
    }

    /// The chip's Unbind: the tab keeps its text but no longer writes back to the script, so Cmd+S asks again.
    pub(in crate::app) fn unbind_script(&mut self) {
        if let Some(tab) = self.cur_uid() {
            self.exec(Action::Library(LibA::Unbind { tab }));
        }
    }

    /// Closes tab `i`, or asks first when it holds changes to its script that are not saved.
    pub(in crate::app) fn ask_close_tab(&mut self, i: usize) {
        match self.core.tabs.get(i) {
            Some(t) if self.tab_edited(i) => self.modal = Some(Modal::CloseEdited(t.uid)),
            _ => self.close_tab(i),
        }
    }

    /// Close a tab with unsaved script changes: save them first, close anyway, or keep the tab.
    pub(in crate::app) fn close_edited_dialog(&mut self, ui: &mut egui::Ui, uid: u64, keep: &mut bool) {
        let Some(i) = self.core.tab_by_uid(uid) else {
            *keep = false;
            return;
        };
        let name = self.tab_name(i);
        ui.set_width(fit_width(ui.ctx(), 460.0));
        dialog_head(ui, &Head::new(format!("Close \"{name}\"?")).sub("Its changes to the script are not saved").glyph(glyphs::warn).tone(WARNING()));
        dialog_body(ui, |ui| {
            ui.label(RichText::new("Closed without saving, it comes back with its text from Reopen closed tab until Quarry quits.").color(MUTED()));
        });
        let (mut save, mut discard) = (false, false);
        dialog_foot(ui, |ui| {
            if primary(ui, "Save and close", PRIMARY()).clicked() {
                save = true;
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
            foot_left(ui, |ui| {
                if danger(ui, "Close without saving").clicked() {
                    discard = true;
                }
            });
        });
        if save {
            *keep = false;
            self.set_cur(i);
            self.save_script();
            // A linked file is written in the background and a script the library lost asks where to save: both leave
            // the tab open, still edited, so nothing closes before it is saved.
            if !self.tab_edited(i) && self.modal.is_none() {
                self.close_tab(i);
            }
        }
        if discard {
            *keep = false;
            self.close_tab(i);
        }
    }

    /// Whether tab `i` holds changes to the script it is bound to that are not saved to it yet.
    pub(in crate::app) fn tab_edited(&self, i: usize) -> bool {
        self.core.tabs.get(i).is_some_and(|t| t.script.as_ref().is_some_and(|b| super::linked::bound_text(t).is_some_and(|x| x != b.base)))
    }

    /// The connection the Library panel scopes to: the one picked in the rail, like the rest of the sidebar, else the
    /// current tab's.
    pub(in crate::app) fn lib_conn(&self) -> Option<u64> {
        self.core.active.filter(|c| self.conn(*c).is_some()).or(self.t().map(|t| t.sel.conn).filter(|c| self.conn(*c).is_some()))
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
        if !self.core.lib_ui.writes.contains(&t.uid) {
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
        self.exec(Action::Library(LibA::Writes { tab: uid, on: false }));
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
        ui.set_width(fit_width(ui.ctx(), 460.0));
        let verb = if f.how == How::Run { "Run all" } else { "Open" };
        dialog_head(ui, &Head::new("Which connection?").sub(f.name.clone()).glyph(glyphs::plug));
        // Grouped by the connection groups, in their order, then the connections in no group.
        let mut sections: Vec<(String, Vec<u64>)> = Vec::new();
        for g in &self.core.groups.groups {
            let ids: Vec<u64> = g.members.iter().copied().filter(|c| f.conns.contains(c)).collect();
            if !ids.is_empty() {
                sections.push((g.name.to_uppercase(), ids));
            }
        }
        let loose: Vec<u64> = f.conns.iter().copied().filter(|c| self.core.groups.group_of(*c).is_none()).collect();
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
