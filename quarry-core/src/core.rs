//! Everything the app holds that is not drawing: connections, tabs, windows, preferences, the worker and its answers.
//! The egui app keeps only what needs egui types, so the state can be built and tested without a window.

pub mod notify;
pub mod drafts;
pub mod front;

pub use front::{Front, TextRef};

use crate::db::{self, Msg, UndoAction, Worker};
use crate::state::cloud_sql;
use crate::state::complete::Fields;
use crate::state::config::UiPrefs;
use crate::state::errors::{ErrEntry, Toast};
use crate::state::groups::Groups;
use crate::state::restore::Restore;
use crate::state::server::ServerWin;
use crate::state::sql::edit::CellEdit;
use crate::state::sql::pg_server::PgServerWin;
use crate::state::sql::routines::Routines;
use crate::state::sql::server::SqlServerWin;
use crate::state::*;
use mongodb::bson::Bson;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::Receiver;

pub struct Core {
    pub(crate) worker: Worker,
    pub(crate) rx: Receiver<Msg>,
    pub conns: Vec<Conn>,
    pub active: Option<u64>,
    pub open_dbs: HashSet<(u64, String)>,
    /// Sidebar TABLES / VIEWS groups folded shut, by connection, database and whether it is the views group.
    pub side_closed: HashSet<(u64, String, bool)>,
    /// SQL databases whose table list was asked for; lists load when a database opens, not all at connect.
    pub sql_asked: HashSet<(u64, String)>,
    /// PostgreSQL: (connection, database, schema) whose table list was asked for.
    pub pg_tables_asked: Asked,
    /// The last plan of each statement explained this session, for the explain dialog's Compare tab.
    /// The Explain dialog's latest run: its connection and the handle Stop ends it by.
    pub explain_run: Option<(db::sql::Db, db::sql::RunHandle)>,
    /// Sidebar lists that failed to load, by connection and namespace key (a database, or a PostgreSQL database and
    /// schema), so the sidebar shows the failure with a retry instead of an empty list that never fills.
    pub side_err: HashMap<(u64, String), String>,
    pub tabs: Vec<Tab>,
    pub cur: usize,
    pub req: u64,
    pub json_edit: Option<JsonEdit>,
    pub drafts: HashMap<String, Draft>,
    pub saving: HashMap<String, Draft>,
    pub inline: Option<Inline>,
    /// The SQL grid cell being edited in place.
    pub sql_cell: Option<CellEdit>,
    /// A MySQL duplicate-key toast's Show the other row, waiting for the table's indexes: (req, table, index, value).
    pub dup_wait: Option<(u64, Sel, String, String)>,
    pub new_field: Option<NewField>,
    pub insert: Option<String>,
    /// Database hidden from the sidebar while its drop runs, to show again if the drop fails.
    /// Collections being written by a duplicate, as (connection, database, new name), shown with a spinner.
    pub copying: std::collections::HashSet<(u64, String, String)>,
    /// A collection to open once the create that makes it is answered.
    pub open_after: Option<(u64, String, String)>,
    /// The Query result being streamed to a file or the clipboard, shown in the status bar with a Cancel.
    pub export_job: Option<export::ExportJob>,
    /// A CSV import or a script file running, shown in the status bar with a Cancel.
    pub sql_job: Option<sql::dump::SqlJob>,
    pub prefs: UiPrefs,
    /// Column data actions cover the whole query instead of the loaded page.
    pub col_scope_query: bool,
    /// Views per namespace, shown differently in the sidebar.
    pub views: Views,
    /// Tables and columns of SQL databases, per (connection, database), for autocomplete. Loaded when a
    /// SQL input there first has focus, and again on the sidebar's Refresh.
    pub sql_schema: HashMap<(u64, String), Arc<db::sql::schema::Schema>>,
    /// The row estimate of each table of `sql_schema`, built once when the schema arrives, for the sidebar.
    pub sql_rows: HashMap<(u64, String), HashMap<String, u64>>,
    pub sql_schema_asked: HashSet<(u64, String)>,
    /// Looked up column values per (connection, database), keyed `table.column`, and the lookups already sent.
    pub sql_values: HashMap<(u64, String), Fields>,
    pub sql_values_asked: HashMap<(u64, String, String, String), HashSet<Option<String>>>,
    /// What the filter panel knows of a table's columns, by (connection, namespace, table), then column.
    pub sql_facts: HashMap<(u64, String, String), HashMap<String, db::sql::facts::ColFacts>>,
    pub sql_facts_asked: HashSet<(u64, String, String, String)>,
    pub users: Option<UsersWin>,
    pub sql_users: Option<sql::users::SqlUsersWin>,
    pub pg_roles: Option<sql::pg_roles::PgRolesWin>,
    pub server: Option<ServerWin>,
    pub sql_server: Option<SqlServerWin>,
    pub pg_server: Option<PgServerWin>,
    /// The routines of each SQL schema the sidebar lists, and the window of the one that is open.
    pub routines: Routines,
    /// Kill requests sent from the Server window and not answered yet. Kept here so an answer that arrives after
    /// the window was closed still reaches the person.
    pub sql_kills: HashSet<u64>,
    /// The answer to a connection form Test, with the request id it answers.
    pub conn_test: Option<(u64, db::Res<String>)>,
    /// When the connection form's Test was sent, to say how long it took.
    pub test_started: Option<std::time::Instant>,
    /// What the last Test learned from a SQL server, for the Advanced tab to offer.
    pub test_info: Option<(u64, db::sql::TestInfo)>,
    /// Usage and suggestions of the collection "db.coll" the Indexes view last asked about.
    pub idx_info: Option<(String, db::IndexInfo)>,
    pub undo: Vec<UndoAction>,
    /// Bytes of each undo entry by its time, measured the first time the memory tooltip needs them.
    pub undo_bytes: std::cell::RefCell<HashMap<std::time::Instant, usize>>,
    pub explain_req: u64,
    pub status: String,
    pub toasts: Vec<Toast>,
    /// This session's errors, oldest first, for the status bar list.
    pub errors: Vec<ErrEntry>,
    /// Errors since the list was last opened, the count the status bar shows.
    pub errors_unseen: usize,
    pub err_seq: u64,
    /// Field values fetched from the server for suggestions, per collection key then path. Kept on
    /// disk, so a new session suggests them before its own fetch comes back.
    pub values: HashMap<String, HashMap<String, Vec<Bson>>>,
    /// (collection key, path, typed text) already fetched this session; no text for the sample.
    pub values_asked: HashSet<(String, String, Option<String>)>,
    /// (collection key, path) asked and not answered yet.
    pub values_pending: HashSet<(String, String)>,
    /// (collection key, path) whose typed text is not looked up, and why.
    pub values_note: HashMap<(String, String), &'static str>,
    /// A string value being typed: when to look up values starting with it, collection key, path, text.
    pub values_typed: Option<(std::time::Instant, String, String, String)>,
    pub tree_filter: String,
    pub tools_installing: bool,
    /// What gcloud told this session about Cloud SQL, so the browser reopens at once.
    pub cloud: cloud_sql::CloudCache,
    /// The "Add from Cloud SQL" browser, when open.
    pub cloud_browser: Option<cloud_sql::CloudBrowser>,
    /// The Settings category shown, kept while the app runs.
    pub settings_cat: settings::SettingsCat,
    /// Tree row the palette jumped to, as (connection, database, collection), scrolled into view once drawn.
    pub reveal: Option<(u64, String, Option<String>)>,
    /// Ids of the open connections in the order they were first seen open, for the rail shortcuts.
    pub rail_order: Vec<u64>,
    /// Connection groups, from groups.json. Never part of ConnMeta, which an older Quarry rewrites without them.
    pub groups: Groups,
    /// The script library, from library.json.
    pub library: crate::library::Library,
    /// What the Library panel remembers while the app runs.
    pub lib_ui: library::LibUi,
    /// What the Undo of a toast puts back, by toast id, for changes that are not database edits.
    pub restores: Vec<(u64, Restore)>,
    pub demo: bool,
    /// Nothing is read from or written to disk or the keychain. True in the demo and in tests; `demo` alone means fake offline data.
    pub ephemeral: bool,
    pub tool: Option<ToolRun>,
    /// AI agents: saved settings, the link listener and connected sessions.
    pub agents: agents::AgentsState,
    /// Recently closed tabs, newest first.
    pub closed: Vec<Tab>,
    /// Uids of tabs left and tabs gone back from, for Cmd+[ and Cmd+], newest last.
    pub nav_back: Vec<u64>,
    pub nav_fwd: Vec<u64>,
    pub upd: Upd,
    /// Why the last automatic check failed, shown in Settings; those failures stay quiet otherwise.
    pub update_note: String,
    pub update_checked: Option<std::time::Instant>,
    pub started: std::time::Instant,
    /// What the last actions left for the UI to do, drained by the app.
    pub ui_fx: Vec<crate::action::UiFx>,
    /// Preferences changed since ui.json was last written; `flush_prefs` writes them.
    pub prefs_dirty: bool,
}

impl Core {
    /// Queues an answer as if the worker had sent it; the startup fixture shows its documents through it.
    pub fn loopback(&self, m: Msg) {
        self.worker.send(m)
    }

    /// The empty state the app and the tests start from; `demo` means fake offline data, and nothing touches disk.
    pub fn new(worker: Worker, rx: Receiver<Msg>, demo: bool) -> Self {
        Self {
            worker,
            rx,
            conns: Vec::new(),
            active: None,
            open_dbs: HashSet::new(),
            side_closed: HashSet::new(),
            sql_asked: HashSet::new(),
            pg_tables_asked: Asked::default(),
            explain_run: None,
            side_err: HashMap::new(),
            tabs: Vec::new(),
            cur: 0,
            req: 0,
            json_edit: None,
            drafts: HashMap::new(),
            saving: HashMap::new(),
            inline: None,
            sql_cell: None,
            dup_wait: None,
            new_field: None,
            insert: None,
            copying: Default::default(),
            open_after: None,
            export_job: None,
            sql_job: None,
            prefs: UiPrefs::default(),
            col_scope_query: false,
            views: Views::default(),
            sql_schema: HashMap::new(),
            sql_rows: HashMap::new(),
            sql_schema_asked: HashSet::new(),
            sql_values: HashMap::new(),
            sql_values_asked: HashMap::new(),
            sql_facts: HashMap::new(),
            sql_facts_asked: HashSet::new(),
            users: None,
            sql_users: None,
            pg_roles: None,
            server: None,
            sql_server: None,
            pg_server: None,
            routines: Routines::default(),
            sql_kills: HashSet::new(),
            conn_test: None,
            test_started: None,
            test_info: None,
            idx_info: None,
            undo: Vec::new(),
            undo_bytes: Default::default(),
            explain_req: 0,
            status: "Ready. All connections start read-only.".into(),
            toasts: Vec::new(),
            errors: Vec::new(),
            errors_unseen: 0,
            err_seq: 0,
            values: HashMap::new(),
            values_asked: HashSet::new(),
            values_pending: HashSet::new(),
            values_note: HashMap::new(),
            values_typed: None,
            tree_filter: String::new(),
            tools_installing: false,
            cloud: Default::default(),
            cloud_browser: None,
            settings_cat: Default::default(),
            reveal: None,
            rail_order: Vec::new(),
            groups: Groups::default(),
            library: Default::default(),
            lib_ui: Default::default(),
            restores: Vec::new(),
            demo,
            ephemeral: demo,
            tool: None,
            agents: Default::default(),
            closed: Vec::new(),
            nav_back: Vec::new(),
            nav_fwd: Vec::new(),
            upd: Upd::Idle,
            update_note: String::new(),
            update_checked: None,
            started: std::time::Instant::now(),
            ui_fx: Vec::new(),
            prefs_dirty: false,
        }
    }

    /// A core with a worker that wakes nobody, nothing on disk and default preferences, for headless tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut core = Self::new(Worker::for_test(tx), rx, false);
        core.ephemeral = true;
        core
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_test_starts_empty() {
        let c = Core::for_test();
        assert!(c.ephemeral && !c.demo);
        assert!(c.conns.is_empty() && c.tabs.is_empty() && c.toasts.is_empty() && c.undo.is_empty());
        assert_eq!((c.active, c.req), (None, 0));
        assert!(c.rx.try_recv().is_err());
    }
}
