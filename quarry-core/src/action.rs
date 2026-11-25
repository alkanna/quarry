//! Typed actions: the one way the UI, and later the menus and the agents, change Core. `Core::execute` runs one and
//! answers `Reply` or `Refusal`, so every flow can run in a test without a window. How to put an area on it:
//!
//! - **Area enum.** Make `action/<area>.rs` with `pub enum <Area>A` (derive Serialize, Deserialize, Debug, Clone), add one
//!   variant to `Action` and one arm to `execute`. Payloads carry ids (conn `u64`, tab uid), names, strings and small
//!   enums, never indices into a Vec, `Row`, `Arc` or `Client`. A password goes in a `Secret`.
//! - **Handler.** The same file holds `impl Core { fn <area>(&mut self, a: <Area>A) -> Result<Reply, Refusal> }` and the
//!   area's non-drawing methods (loads, answers, staging, apply), moved from `impl App` with `self.core.x` made `self.x`.
//! - **Refusals.** Return `self.refuse_ro(conn, None)`, `refuse_offline(conn)`, `refuse(msg)` or `refuse_settings(msg)`:
//!   each pushes the toast `warn_ro`, `warn_offline`, `warn` or `warn_settings` pushed before. On `ReadOnly` the app also
//!   opens Unlock with a retry that runs the same action again, so no closure lives here.
//! - **Toasts, errors, undo.** `self.ok`, `self.err`, `self.warn*` and `self.push_undo` are on Core (`core/notify.rs`).
//! - **UI effects.** What only the UI can do (open one of its dialogs) is pushed on `self.ui_fx` as a `UiFx`; `App::exec`
//!   and `logic()` drain it.
//! - **Answers.** Add the area's `Msg` variants to `Core::handles` and route them in `Core::on_msg`; keep the request id
//!   guards as they were. `App::poll` hands every message `handles` accepts to `on_msg`.
//! - **Drawing.** A window takes its state out for the frame, draws it (a `TextEdit` may bind a `&mut String` of it),
//!   pushes `<Area>A` values for clicks, toggles and choices, puts the state back, then calls `self.exec(..)` on each.
//! - **Tests.** `Core::for_test()`, then `execute`, `inject` fake answers or `step_until` real ones; no egui needed. See
//!   `action/roles_tests.rs`.

use crate::core::Core;
use crate::db::Msg;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

pub mod roles;
pub mod server;
pub mod users;
pub mod sql;
pub mod structure;
pub mod sql_server;
pub mod shell;
pub mod tools;
pub mod watch;
pub mod conn;
pub mod side;
pub mod update;
pub mod toasts;
pub mod prefs;
pub mod library;
pub mod tabs;
pub mod docs;
pub mod undo;
pub mod indexes;
pub mod aggregate;
pub mod lib_dialogs;
pub mod mongo;
pub mod grid;
pub mod routines;
#[cfg(test)]
mod roles_tests;
#[cfg(test)]
mod users_tests;
#[cfg(test)]
mod sql_server_tests;
#[cfg(test)]
mod shell_tests;
#[cfg(test)]
mod conn_tests;
#[cfg(test)]
mod prefs_tests;
#[cfg(test)]
mod sql_tests;
#[cfg(test)]
mod docs_tests;
#[cfg(test)]
mod mongo_tests;
#[cfg(test)]
mod structure_tests;
#[cfg(test)]
mod ddl_tests;

pub use roles::RolesA;
pub use server::{MongoServerA, ServerA};
pub use users::{MongoUsersA, SqlUsersA, UsersA};
pub use sql_server::{PgServerA, SqlServerA};
pub use shell::ShellA;
pub use tools::ToolsA;
pub use watch::WatchA;
pub use conn::{ConnA, GroupA, RailDrop};
pub use side::SideA;
pub use update::{UpdBtn, UpdateA};
pub use toasts::AppA;
pub use prefs::{Pref, PrefVals, PrefsA};
pub use library::LibA;
pub use sql::SqlA;
pub use structure::StructA;
pub use tabs::TabA;
pub use docs::{BarField, DocsA, EditA, ValueA};
pub use undo::UndoA;
pub use aggregate::{AggA, StageA};
pub use indexes::IdxA;
pub use mongo::{CmpA, FieldA, ImportA, MongoA, SchemaA};
pub use grid::GridA;
pub use routines::RoutineA;
pub use sql::ddl::{DdlA, SqlForm};
pub use sql::ops::SqlOpsA;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Action {
    Roles(RolesA),
    Users(UsersA),
    Server(ServerA),
    Shell(ShellA),
    Watch(WatchA),
    Tools(ToolsA),
    Conn(ConnA),
    Side(SideA),
    Update(UpdateA),
    App(AppA),
    Prefs(PrefsA),
    Library(LibA),
    Sql(SqlA),
    Structure(StructA),
    Tab(TabA),
    Docs(DocsA),
    Undo(UndoA),
    Aggregate(AggA),
    Indexes(IdxA),
    Mongo(MongoA),
    Grid(GridA),
    Routines(RoutineA),
    Ddl(DdlA),
    SqlOps(SqlOpsA),
}

/// What an action did: finished, or started worker call `req` whose answer comes back through `on_msg`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    Done,
    Pending(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub kind: RefusalKind,
    pub msg: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalKind {
    /// Writes are locked on this connection; the UI offers Unlock and runs the action again after it.
    ReadOnly { conn: u64 },
    Offline,
    Invalid,
    Settings,
}

/// Effects only the UI can carry out, drained by the app after each action and each frame.
pub enum UiFx {
    Open(Dialog),
    /// A connection opened: the app reloads its tabs that hold no rows, Mongo ones through their query bar.
    Connected { conn: u64, sql: bool },
    /// Closes the tabs of a dropped or renamed database or collection, every collection of `db` when `coll` is None.
    CloseTabs { conn: u64, db: String, coll: Option<String> },
    /// Restart into the installed update, once the app's own dialogs hold nothing.
    Restart,
    /// Open a library script on this connection (`App::open_script_on`), once its connection is ready.
    OpenScript { r: crate::library::ScriptRef, how: crate::state::library::open::How, conn: u64 },
    /// Open a linked file just read (`App::open_resolved`), with the time and length it had.
    OpenResolved { r: crate::library::ScriptRef, how: crate::state::library::open::How, script: Box<crate::library::Script>, meta: Option<(std::time::SystemTime, u64)> },
    /// A dialog an answer opens, shown once the dialog already open (which may hold typing) closes.
    Late(Dialog),
    /// A dialog shown only if no other is open.
    IfFree(Dialog),
    /// Puts this text on the clipboard.
    CopyText(String),
}

/// The app's dialogs an action opens. The app keeps `Modal`, which holds its own closures, so Core names them here.
pub enum Dialog {
    /// Unlock with nothing to run after it, for a banner's Unlock button.
    Unlock(u64),
    PgNewRole(Box<crate::state::sql::pg_roles::NewRoleForm>),
    PgPassword(Box<crate::state::sql::pg_roles::PgPwForm>),
    PgDropRole(Box<crate::state::sql::pg_roles::DropForm>),
    SqlNewUser(Box<crate::state::sql::users::NewUser>),
    SqlPassword(Box<crate::state::sql::users::PwForm>),
    /// A confirmation that asks for a name to be typed.
    Typed { title: String, expect: String, action: crate::state::Danger },
    /// The Shell's confirm before the script of tab `tab` runs, with these possibly destructive words.
    ShellWrite { tab: u64, ops: Vec<String> },
    /// MongoDB's New database or New collection dialog.
    MongoNewColl(Box<crate::state::mongo_create::MongoNewColl>),
    Settings,
    WhatsNew(Box<crate::state::whats_new::WhatsNew>),
    ErrorDetails(Box<crate::state::errors::ErrEntry>),
    /// A text window, such as a linked file against the tab that edits it.
    Text { title: String, body: String, code: bool },
    /// The Explain dialog of a SQL statement, which the app builds and holds with its first request.
    SqlExplain(String),
    /// Save all's confirmation of these staged drafts.
    SaveAll(Vec<String>),
    /// The insert form of a SQL table.
    SqlRow(Box<crate::state::sql::edit::RowForm>),
    /// Save a draft's changes, when Ask before saving is on.
    Save { tag: String, changes: Vec<(String, Option<mongodb::bson::Bson>, Option<mongodb::bson::Bson>)> },
    Delete(crate::state::Sel, Vec<mongodb::bson::Document>),
    /// A pipeline with $out or $merge, which writes.
    AggWrite(Vec<mongodb::bson::Document>),
    /// The value counts of a field, from the server.
    Grouped { sel: crate::state::Sel, path: String, pipeline: Vec<mongodb::bson::Document>, rows: Vec<mongodb::bson::Document> },
    /// The data relations of a database.
    Relations { conn: u64, db: String },
    /// Rename field `path` of document `doc`, with `name` to start from.
    Rename { doc: mongodb::bson::Document, path: Vec<String>, name: String },
    CreateIndex(Box<crate::state::indexes::IndexForm>),
    /// Import's confirmation of inserting this many documents.
    ImportInsert(usize),
    /// The Structure view's column, index, CHECK, table options and relation dialogs.
    ColEdit(Box<crate::state::sql::alter::ColForm>),
    IdxEdit(Box<crate::state::sql::alter::IdxForm>),
    CheckEdit(Box<crate::state::sql::checks::CheckForm>),
    TableOpts(Box<crate::state::sql::table_opts::TableForm>),
    FkEdit(Box<crate::state::sql::rel::FkForm>),
    /// A maintenance statement of the Info page that locks or rebuilds the table.
    SqlMaintain { what: crate::db::sql::Maint, stmt: String },
    /// A SQL create, drop, rename, duplicate, empty, view or bulk dialog.
    SqlForm(SqlForm),
}

/// A password in an action. It is wiped on drop, prints as *** and serializes as null, so a journal or an echo never
/// holds it; a deserialized one is absent, and `execute` refuses an action whose secret is absent.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(Option<String>);

impl Secret {
    pub fn new(s: String) -> Secret {
        Secret(Some(s))
    }

    /// The text, or None when it did not come through (a deserialized action).
    pub fn expose(&self) -> Option<&str> {
        self.0.as_deref()
    }

    /// Hands the text over, leaving nothing behind.
    pub fn take(&mut self) -> Option<String> {
        self.0.take()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "***" } else { "(absent)" })
    }
}

impl Serialize for Secret {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_none()
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Secret, D::Error> {
        // Whatever was written stands for a password that was never sent, so it reads back as absent.
        serde::de::IgnoredAny::deserialize(d)?;
        Ok(Secret(None))
    }
}

impl Core {
    /// Runs one action. Refusals have already told the person why, with the toast that goes with their kind.
    pub fn execute(&mut self, a: Action) -> Result<Reply, Refusal> {
        match a {
            Action::Roles(a) => self.roles(a),
            Action::Users(a) => self.users(a),
            Action::Server(a) => self.server(a),
            Action::Shell(a) => self.shell(a),
            Action::Watch(a) => self.watch(a),
            Action::Tools(a) => self.tools(a),
            Action::Conn(a) => self.conn_act(a),
            Action::Side(a) => self.side(a),
            Action::Update(a) => self.update(a),
            Action::App(a) => self.app(a),
            Action::Prefs(a) => self.prefs_act(a),
            Action::Library(a) => self.library_act(a),
            Action::Sql(a) => self.sql(a),
            Action::Structure(a) => self.structure_act(a),
            Action::Tab(a) => self.tab(a),
            Action::Docs(a) => self.docs(a),
            Action::Undo(a) => self.undo(a),
            Action::Aggregate(a) => self.aggregate(a),
            Action::Indexes(a) => self.indexes(a),
            Action::Mongo(a) => self.mongo(a),
            Action::Grid(a) => self.grid(a),
            Action::Routines(a) => self.routine_act(a),
            Action::Ddl(a) => self.ddl(a),
            Action::SqlOps(a) => self.sql_ops(a),
        }
    }

    pub fn take_ui_fx(&mut self) -> Vec<UiFx> {
        std::mem::take(&mut self.ui_fx)
    }

    /// Writes are locked on `conn`: the read-only toast, or `note` in its place where a window words it its own way.
    pub fn refuse_ro(&mut self, conn: u64, note: Option<&str>) -> Refusal {
        match note {
            Some(n) => self.warn(n),
            None => self.warn_ro_on(Some(conn)),
        }
        Refusal { kind: RefusalKind::ReadOnly { conn }, msg: note.map_or_else(|| self.last_warning(), str::to_string) }
    }

    /// Writes are locked on `conn`, for a site that opens Unlock without a toast first.
    pub fn refuse_ro_quiet(&mut self, conn: u64) -> Refusal {
        Refusal { kind: RefusalKind::ReadOnly { conn }, msg: String::new() }
    }

    pub fn refuse_offline(&mut self, conn: u64) -> Refusal {
        self.warn_offline_on(Some(conn));
        Refusal { kind: RefusalKind::Offline, msg: self.last_warning() }
    }

    pub fn refuse(&mut self, msg: impl Into<String>) -> Refusal {
        let msg = msg.into();
        self.warn(msg.clone());
        Refusal { kind: RefusalKind::Invalid, msg }
    }

    pub fn refuse_settings(&mut self, msg: impl Into<String>) -> Refusal {
        let msg = msg.into();
        self.warn_settings(msg.clone());
        Refusal { kind: RefusalKind::Settings, msg }
    }

    fn last_warning(&self) -> String {
        self.toasts.last().map(|t| if t.body.is_empty() { t.title.clone() } else { format!("{}. {}", t.title, t.body) }).unwrap_or_default()
    }

    /// Whether `on_msg` takes this answer. The app's own poll handles the rest.
    pub fn handles(m: &Msg) -> bool {
        use crate::db::sql::pg::roles::Answer;
        match m {
            // The drop dialog's answer fills a form the app's Modal holds.
            Msg::PgRoles { ans: Answer::DropInfo(_), .. } => false,
            Msg::PgRoles { .. } | Msg::SqlUsers { .. } | Msg::SqlUserGrants { .. } | Msg::SqlUserDone { .. } => true,
            Msg::Connected { .. } | Msg::SqlConnected { .. } | Msg::Tested { .. } | Msg::Collections { .. } | Msg::SqlSchemas { .. } | Msg::SqlTables { .. } => true,
            Msg::Command { tag, .. } if tag.starts_with("srv-") => true,
            Msg::Command { tag, .. } => matches!(tag.as_str(), "users" | "users-self" | "roles") || tag.starts_with("server:"),
            Msg::SqlServer { .. } | Msg::PgServer { .. } => true,
            Msg::ShellLog { .. } | Msg::ShellQuery { .. } | Msg::ShellDone { .. } | Msg::WatchEvent { .. } | Msg::WatchDone { .. } | Msg::ToolLog(_) | Msg::ToolDone(_) | Msg::ToolsInstalled(_) => true,
            Msg::UpdateChecked(_) | Msg::UpdateInstalled(_) => true,
            Msg::LibScan { link, .. } => *link != 0,
            Msg::LibFile { .. } => true,
            Msg::SqlRan { .. } | Msg::SqlCount { .. } | Msg::SqlLint { .. } => true,
            Msg::SqlCols { .. } | Msg::SqlFilterCount { .. } | Msg::SqlKeys { .. } | Msg::SqlValues { .. } | Msg::SqlFacts { .. } => true,
            Msg::SqlAltered { .. } => true,
            // Msg::Docs stays in the app's poll: the page's field suggestions are built by its autocomplete.
            Msg::Indexes { .. } | Msg::IndexInfo { .. } | Msg::Written { .. } | Msg::Saved { .. } => true,
            Msg::Aggregated { .. } | Msg::StagePreview { .. } => true,
            Msg::Compared { .. } | Msg::Schema { .. } | Msg::Analyzed { .. } | Msg::Grouped { .. } | Msg::ColumnValues { .. } | Msg::Related { .. } | Msg::RefFound { .. } => true,
            Msg::SqlRoutine { .. } => true,
            // SqlDropInfo, SqlViewDef and SqlBulkPrep fill a form the app's Modal holds; SqlViewSaved does too, and hands the rest to `sql_view_saved`.
            Msg::SqlDdl { .. } | Msg::SqlRenamed { .. } => true,
            _ => false,
        }
    }

    /// Applies one worker answer that `handles` accepts; any other is dropped.
    pub fn on_msg(&mut self, m: Msg) {
        match m {
            Msg::PgRoles { conn, req, ans } => self.pg_roles_answer(conn, req, ans),
            Msg::SqlUsers { conn, req, res } => self.sql_users_loaded(conn, req, res),
            Msg::SqlUserGrants { conn, req, res } => self.sql_user_grants_loaded(conn, req, res),
            Msg::SqlUserDone { conn, account, op, res, undo } => self.sql_user_done(conn, account, op, res, undo),
            Msg::Command { tag, res } if tag.starts_with("server:") => self.server_reply(&tag, res),
            Msg::Connected { conn, attempt, res } => self.on_connected(conn, attempt, res),
            Msg::SqlConnected { conn, attempt, res } => self.on_sql_connected(conn, attempt, res),
            Msg::Tested { req, res, info } => self.tested(req, res, info),
            Msg::Collections { conn, db, res } => self.collections_done(conn, db, res),
            Msg::Command { tag, res } if tag.starts_with("srv-") => self.srv_reply(&tag, res),
            Msg::SqlSchemas { conn, database, res } => self.pg_schemas_done(conn, database, res),
            Msg::SqlTables { conn, database, schema, res } => self.pg_tables_done(conn, database, schema, res),
            Msg::Command { tag, res } => self.mongo_users_reply(&tag, res),
            Msg::SqlServer { req, ans } => self.sql_server_done(req, ans),
            Msg::PgServer { req, ans } => self.pg_server_done(req, ans),
            Msg::ShellLog { req, level, text } => self.shell_log(req, level, text),
            Msg::ShellQuery { req, q } => self.shell_query(req, q),
            Msg::ShellDone { req, res, ms, query_ms } => self.shell_done(req, res, ms, query_ms),
            Msg::WatchEvent { req, doc } => self.watch_event(req, doc),
            Msg::WatchDone { req, res } => self.watch_done(req, res),
            Msg::ToolLog(line) => self.tool_log(line),
            Msg::ToolDone(res) => self.tool_done(res),
            Msg::ToolsInstalled(res) => self.tools_installed(res),
            Msg::UpdateChecked(res) => self.on_update_checked(res),
            Msg::UpdateInstalled(res) => self.on_update_installed(res),
            Msg::LibScan { link, scan } => self.apply_scan(link, scan),
            Msg::LibFile { job, out } => self.file_done(job, out),
            Msg::SqlRan { req, sets, .. } => self.sql_ran(req, sets),
            Msg::SqlCount { req, idx, res } => self.sql_counted(req, idx, res),
            Msg::SqlLint { req, lint } => self.sql_linted(req, lint),
            Msg::SqlCols { req, cols, pk, estimated, matched } => self.sql_cols(req, cols, pk, estimated, matched),
            Msg::SqlFilterCount { req, res } => self.sql_filter_counted(req, res),
            Msg::SqlKeys { req, res } => self.sql_keys_done(req, res),
            Msg::SqlValues { conn, database, schema, table, column, prefix, res } => self.sql_values_done(conn, database, schema, table, column, prefix, res),
            Msg::SqlFacts { conn, database, schema, table, column, res } => self.sql_facts_done(conn, database, schema, table, column, res),
            Msg::SqlAltered { conn, database, schema, table, res, undo, sql_mode, alter, unsure } => self.sql_altered(conn, database, schema, table, res, undo, sql_mode, alter, unsure),
            Msg::Indexes { req, res } => self.indexes_loaded(req, res),
            Msg::IndexInfo { key, res } => self.index_info_loaded(key, res),
            Msg::Written { at, res, undo, reload, retry } => self.written(at, res, undo, reload, retry),
            Msg::Saved { tag, res, undo } => self.saved(tag, res, undo),
            Msg::Aggregated { req, res } => self.aggregated(req, res),
            Msg::StagePreview { req, res } => self.stage_previewed(req, res),
            Msg::Compared { req, res } => self.compared(req, res),
            Msg::Schema { req, res } => self.schema_done(req, res),
            Msg::Analyzed { conn, db, res } => self.analyzed(conn, db, res),
            Msg::Grouped { conn, db, coll, path, pipeline, res } => self.grouped(conn, db, coll, path, pipeline, res),
            Msg::ColumnValues { conn, op, coll, path, res } => self.column_values_done(conn, op, coll, path, res),
            Msg::Related { req, res } => self.related_done(req, res),
            Msg::RefFound { conn, db, coll, path, value, res } => self.ref_found(conn, db, coll, path, value, res),
            Msg::SqlRoutine { conn, req, ans } => self.routine_done(conn, req, ans),
            Msg::SqlDdl { conn, database, schema, table, what, res, undo, dbs, sql_mode } => self.sql_ddl_done(conn, database, schema, table, what, res, undo, dbs, sql_mode),
            Msg::SqlRenamed { conn, database, schema, from, to, whole, res, undo } => self.sql_renamed(conn, database, schema, from, to, whole, res, undo),
            _ => {}
        }
    }

    /// Drains the worker's answers into `on_msg` and hands back the ones Core does not take.
    pub fn tick(&mut self) -> Vec<Msg> {
        let mut rest = Vec::new();
        while let Ok(m) = self.rx.try_recv() {
            if Self::handles(&m) {
                self.on_msg(m);
            } else {
                rest.push(m);
            }
        }
        rest
    }

    /// Runs `tick` until `cond` holds, like the UI tests' `wait`. Panics with `what` after 40 s.
    #[cfg(any(test, feature = "test-support"))]
    pub fn step_until(&mut self, what: &str, cond: impl Fn(&Core) -> bool) {
        let t = std::time::Instant::now();
        while !cond(self) {
            assert!(t.elapsed() < std::time::Duration::from_secs(40), "timed out waiting for {what}; status: {}", self.status);
            self.tick();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// A worker answer made up by a test, applied as if it had come over the channel.
    #[cfg(any(test, feature = "test-support"))]
    pub fn inject(&mut self, m: Msg) {
        assert!(Self::handles(&m), "Core does not take this answer");
        self.on_msg(m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_never_shows_in_debug_or_json() {
        let a = Action::Roles(RolesA::SetPassword { conn: 1, role: "jane".into(), oid: "16405".into(), password: Secret::new("Zq9secret".into()) });
        assert!(!format!("{a:?}").contains("Zq9secret"));
        let json = serde_json::to_string(&a).unwrap();
        assert!(!json.contains("Zq9secret"), "{json}");
        let back: Action = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, Action::Roles(RolesA::SetPassword { password, .. }) if password.expose().is_none()), "a password never comes back from JSON");
    }
}
