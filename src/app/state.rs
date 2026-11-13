//! The state the views draw from: connections, tabs, modals, and the actions views hand back.

use super::*;

impl ShellTab {
    pub(super) fn set_value(&mut self, v: Bson) {
        self.count = match &v {
            Bson::Array(a) => Some(a.len()),
            _ => None,
        };
        // Documents are kept once, as BSON bytes; anything else stays a value. Rows fail only for
        // keys BSON cannot hold, and such a result stays a value too.
        let rows = match &v {
            Bson::Array(a) if !a.is_empty() && a.iter().all(|x| matches!(x, Bson::Document(_))) => a.iter().filter_map(Bson::as_document).map(db::Row::from_doc).collect(),
            Bson::Document(d) => db::Row::from_doc(d).map(|r| vec![r]),
            _ => Err(String::new()),
        };
        (self.docs, self.value) = match rows {
            Ok(rows) => (rows, None),
            Err(_) => (Vec::new(), Some(v)),
        };
        self.fields = auto_fields(&self.docs);
        self.page = 0;
        self.raw = self.docs.is_empty();
        self.raw_text = None;
        self.error = None;
    }

    /// SQL rows have no `_id` unless the query selects one, so the column the Mongo layout adds first
    /// is dropped when no row holds it.
    pub(super) fn hide_absent_id(&mut self) {
        if !self.docs.iter().any(|r| r.value("_id").is_some()) {
            self.fields.retain(|f| f != "_id");
        }
    }

    /// The whole result as one value, for Copy, Export and the JSON view. Documents are decoded
    /// from their rows for the moment.
    pub(super) fn result(&self) -> Option<Bson> {
        match (&self.value, self.docs.as_slice(), self.count) {
            (Some(v), ..) => Some(v.clone()),
            (None, [], _) => None,
            (None, [d], None) => Some(Bson::Document(d.owned())),
            (None, rows, _) => Some(Bson::Array(rows.iter().map(|r| Bson::Document(r.owned())).collect())),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub(super) struct ConnMeta {
    pub(super) id: u64,
    pub(super) name: String,
    /// Old saved connections have no engine and are Mongo.
    #[serde(default)]
    pub(super) engine: db::sql::Engine,
    /// Reach the server through an SSH tunnel; passwords and passphrases live in the Keychain.
    #[serde(default)]
    pub(super) ssh: Option<crate::ssh::SshConf>,
    /// Reach the server through a SOCKS5 proxy; its password lives in the Keychain.
    #[serde(default)]
    pub(super) proxy: Option<db::Proxy>,
    /// Reach a PostgreSQL server through Google's Cloud SQL proxy, which Quarry starts. Nothing in it is secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cloud_sql: Option<crate::cloudsql::CloudSql>,
    /// Environment label shown next to the name, such as Production.
    #[serde(default)]
    pub(super) tag: String,
    /// Session settings, init command and timeouts of a SQL connection. None of it is secret.
    #[serde(default, skip_serializing_if = "db::sql::Advanced::is_default")]
    pub(super) advanced: db::sql::Advanced,
}

#[derive(PartialEq)]
pub(super) enum Status {
    Idle,
    Connecting,
    Ready,
    Failed(String),
}

pub(super) struct Conn {
    pub(super) meta: ConnMeta,
    pub(super) client: Option<Client>,
    /// Set instead of `client` for SQL connections.
    pub(super) sql: Option<db::sql::Db>,
    pub(super) status: Status,
    /// Writes are locked at every launch and only unlocked for this session.
    pub(super) writable: bool,
    /// On PostgreSQL the inner list holds the database's user schemas, and `tables` the tables of each one.
    pub(super) dbs: Vec<(String, Option<Vec<String>>)>,
    /// PostgreSQL only: the tables and views of each schema that was loaded, by database then schema. Nested so a lookup
    /// can borrow both names.
    pub(super) tables: HashMap<String, HashMap<String, Vec<String>>>,
    /// PostgreSQL only: per database the schemas the server listed and the one the sidebar shows. `dbs` holds the user
    /// schema names of the same list, written together with it by `set_schemas`.
    pub(super) pg: HashMap<String, PgNs>,
    pub(super) masked_uri: String,
    /// Kept alive while connected through SSH or Cloud SQL; dropping it closes the tunnel or stops the proxy.
    pub(super) tunnel: Option<db::Way>,
    /// The connect in flight, so an answer to one the user stopped or replaced is dropped.
    pub(super) attempt: u64,
    /// Shown in the status bar and under the sidebar; filled after connecting.
    pub(super) server: ServerInfo,
}

/// What a PostgreSQL database holds beside its tables: every schema with its table count and whether it is a system one,
/// and the schema the sidebar lists. The pick is kept for the session only, since the open tabs carry their own schema
/// and the list of schemas can change between launches. An empty pick means none was made yet.
#[derive(Default)]
pub(super) struct PgNs {
    pub(super) schemas: Vec<db::sql::PgSchema>,
    pub(super) picked: String,
}

impl Conn {
    pub(super) fn new(meta: ConnMeta, status: Status) -> Conn {
        Conn { meta, client: None, sql: None, status, writable: false, dbs: Vec::new(), tables: Default::default(), pg: Default::default(), masked_uri: String::new(), tunnel: None, attempt: 0, server: Default::default() }
    }

    /// Whether a connect answer for `attempt` is still wanted: this connection is connecting, for that attempt.
    pub(super) fn waits_for(&self, attempt: u64) -> bool {
        self.status == Status::Connecting && self.attempt == attempt
    }

    /// Forgets every list read from the server, for a new session or a closed one.
    pub(super) fn forget_lists(&mut self) {
        self.dbs.clear();
        self.tables.clear();
        self.pg.clear();
    }

    /// The tables and views of one PostgreSQL schema, once loaded.
    pub(super) fn tables_in(&self, db: &str, schema: &str) -> Option<&Vec<String>> {
        self.tables.get(db)?.get(schema)
    }

    /// The schema the sidebar lists for a PostgreSQL database: the one picked, else `public`, else the first user schema.
    pub(super) fn pg_picked(&self, db: &str) -> &str {
        match self.pg.get(db) {
            Some(p) if !p.picked.is_empty() => &p.picked,
            _ => default_schema(self.dbs.iter().find(|d| d.0 == db).and_then(|d| d.1.as_deref()).unwrap_or_default()),
        }
    }

    /// The schemas of a database arrived: the user names go to `dbs` next to the full list.
    pub(super) fn set_schemas(&mut self, db: &str, list: Vec<db::sql::PgSchema>, picked: String) {
        let user: Vec<String> = list.iter().filter(|s| !s.system).map(|s| s.name.clone()).collect();
        if let Some(d) = self.dbs.iter_mut().find(|d| d.0 == db) {
            d.1 = Some(user);
        }
        self.pg.insert(db.to_string(), PgNs { schemas: list, picked });
    }
}

/// The (connection, database, schema) whose PostgreSQL table list was asked for, so a list loads once.
#[derive(Default)]
pub(super) struct Asked(HashSet<(u64, String, String)>);

impl Asked {
    /// Marks a list as asked; false when it already was.
    pub(super) fn insert(&mut self, conn: u64, db: &str, schema: &str) -> bool {
        self.0.insert((conn, db.to_string(), schema.to_string()))
    }

    /// The schemas of a database that were asked for.
    pub(super) fn held(&self, conn: u64, db: &str) -> Vec<String> {
        self.0.iter().filter(|k| k.0 == conn && k.1 == db).map(|k| k.2.clone()).collect()
    }

    /// Forgets that lists were asked for, so they load again: one schema's, one database's or the whole connection's.
    /// A new session, a refresh and a failed list each need it.
    pub(super) fn forget(&mut self, conn: u64, db: Option<&str>, schema: Option<&str>) {
        self.0.retain(|k| !(k.0 == conn && db.is_none_or(|d| k.1 == d) && schema.is_none_or(|s| k.2 == s)));
    }
}

/// The views of each namespace, by connection and then the namespace key (`ns_key`), for the sidebar, the palette and
/// autocomplete. A set is shared out by `Arc`, so a scope keeps it without copying.
#[derive(Default)]
pub(super) struct Views(HashMap<u64, HashMap<String, Arc<HashSet<String>>>>);

impl Views {
    pub(super) fn of(&self, conn: u64, key: &str) -> Option<&Arc<HashSet<String>>> {
        self.0.get(&conn)?.get(key)
    }

    pub(super) fn has(&self, conn: u64, key: &str, name: &str) -> bool {
        self.of(conn, key).is_some_and(|s| s.contains(name))
    }

    /// Replaces the views of a namespace with what the server listed.
    pub(super) fn set(&mut self, conn: u64, key: &str, names: impl IntoIterator<Item = String>) {
        self.0.entry(conn).or_default().insert(key.to_string(), Arc::new(names.into_iter().collect()));
    }

    pub(super) fn add(&mut self, conn: u64, key: &str, name: &str) {
        let set = self.0.entry(conn).or_default().entry(key.to_string()).or_default();
        Arc::make_mut(set).insert(name.to_string());
    }
}

/// The host part of a connection string: after the scheme and any credentials, before the path
/// or options (which may themselves hold an @).
pub(super) fn host_of(uri: &str) -> String {
    let rest = uri.split_once("://").map_or(uri, |(_, r)| r);
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    authority.rsplit('@').next().unwrap_or_default().to_string()
}

/// The user part of a connection string's credentials, "ana" in "mongodb://ana:****@host".
pub(super) fn user_of(uri: &str) -> String {
    let rest = uri.split_once("://").map_or(uri, |(_, r)| r);
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    authority.rsplit_once('@').map(|(cred, _)| cred.split(':').next().unwrap_or_default().to_string()).unwrap_or_default()
}

#[derive(Default, Clone)]
pub(super) struct ServerInfo {
    pub(super) version: String,
    pub(super) set_name: String,
    pub(super) host: String,
    /// "MySQL" or "MariaDB" for SQL connections; empty for Mongo, which shows "mongod".
    pub(super) product: String,
    pub(super) tls: bool,
}

impl ServerInfo {
    /// The status bar's "host · mongod version · set", with the host from `uri` until the server says.
    pub(super) fn status_line(&self, uri: &str) -> String {
        let host = if self.host.is_empty() { host_of(uri) } else { self.host.clone() };
        let version = (!self.version.is_empty()).then(|| format!("{} {}", if self.product.is_empty() { "mongod" } else { &self.product }, self.version));
        [Some(host), version, Some(self.set_name.clone()), self.tls.then(|| "TLS".to_string())].into_iter().flatten().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("  ·  ")
    }

}

#[derive(Clone, PartialEq, Default)]
pub(super) struct Sel {
    pub(super) conn: u64,
    pub(super) db: String,
    /// PostgreSQL only: the schema inside the database. Empty for Mongo and MySQL, where `db` is the whole namespace.
    pub(super) schema: String,
    pub(super) coll: String,
}

/// The schema a PostgreSQL database opens on when it has one.
pub(super) const PG_PUBLIC: &str = super::shell_attach::PG_SCHEMA;

/// Key of one namespace in the caches that hold a list per database (autocomplete schema, values, views). A PostgreSQL
/// schema name repeats across databases, so the database is part of the key; MySQL has no schema and keeps the bare name.
pub(super) fn ns_key(db: &str, schema: &str) -> String {
    if schema.is_empty() { db.to_string() } else { format!("{db}\u{1f}{schema}") }
}

/// The schema a database opens on: `public` when the user can see it, else the first user schema, else none.
pub(super) fn default_schema(schemas: &[String]) -> &str {
    if schemas.iter().any(|s| s == PG_PUBLIC) { PG_PUBLIC } else { schemas.first().map_or("", String::as_str) }
}

impl Sel {
    /// The namespace the worker calls take: the schema on PostgreSQL, the database on MySQL. A PostgreSQL connection is
    /// bound to one database, so `db` only picks the pool there (`Db::at`).
    pub(super) fn space(&self) -> &str {
        if self.schema.is_empty() { &self.db } else { &self.schema }
    }

    /// The cache key of this selection's namespace, see `ns_key`.
    pub(super) fn nskey(&self) -> String {
        ns_key(&self.db, &self.schema)
    }

    /// The faint label beside a tab's name: the schema of a PostgreSQL table, else the database. A Query tab has no
    /// table, so it names its database.
    pub(super) fn scope_label(&self) -> &str {
        if !self.schema.is_empty() && !self.coll.is_empty() { &self.schema } else { &self.db }
    }

    /// "schema.table" outside `public`, the plain table name otherwise, for places with a single name.
    pub(super) fn shown(&self) -> String {
        if self.schema.is_empty() || self.schema == PG_PUBLIC { self.coll.clone() } else { format!("{}.{}", self.schema, self.coll) }
    }

    /// "db.coll", how index stats are keyed.
    pub(super) fn key(&self) -> String {
        format!("{}.{}", self.db, self.coll)
    }

    /// "db.coll", or the database alone when no collection is picked.
    pub(super) fn ns(&self) -> String {
        if self.coll.is_empty() { self.db.clone() } else { self.key() }
    }
}

/// Where the Cmd+K palette can jump to.
#[derive(Clone)]
pub(super) enum Jump {
    Conn(u64),
    Db(u64, String),
    Coll(Sel),
}

#[derive(PartialEq, Clone, Copy)]
pub(super) enum View {
    Documents,
    Aggregate,
    Indexes,
    Shell,
    Schema,
    Import,
    Compare,
    Watch,
    /// SQL tabs only. Documents is their Content view.
    Structure,
    Relations,
    Triggers,
    Info,
    /// The SQL Custom Query.
    Sql,
}

/// Where a results view keeps its rows: decoded, or as BSON bytes like the Documents view
/// (Aggregate and Watch, whose results can be large; see `db::Row`).
pub(super) enum Store<'a> {
    Docs(&'a mut Vec<Document>),
    Raw(&'a mut Vec<db::Row>),
}

static NEXT_TAB: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Everything that belongs to one open tab.
pub(super) struct Tab {
    /// Unique for the session. Back and forward history keys on it, since Query tabs and copies share a `Sel`.
    pub(super) uid: u64,
    pub(super) sel: Sel,
    pub(super) view: View,
    pub(super) filter_txt: String,
    pub(super) sort_txt: String,
    pub(super) proj_txt: String,
    // ponytail: limit, skip, hint and collation are not saved with open tabs or the library; add them there if asked.
    pub(super) limit_txt: String,
    pub(super) skip_txt: String,
    pub(super) hint_txt: String,
    pub(super) collation_txt: String,
    pub(super) query: Query,
    pub(super) page: u64,
    pub(super) docs: Vec<db::Row>,
    pub(super) fields: Vec<String>,
    /// Every dotted field path seen in the loaded documents, for autocomplete.
    pub(super) paths: Vec<String>,
    /// Main kind and frequent values of each path in the loaded documents, for Query suggestions.
    /// Shared with the Shell, which holds the Arcs its types were built from: a change through
    /// Arc::make_mut then makes a new pointer, so the Shell can tell a change by identity.
    pub(super) field_info: Arc<crate::ui::complete::Fields>,
    pub(super) count: u64,
    pub(super) ms: u128,
    pub(super) req: u64,
    /// Index list requests have their own id, so they never cancel a documents load.
    pub(super) idx_req: u64,
    pub(super) loading: bool,
    /// The last read of one of this tab's views failed; that view shows why instead of results.
    pub(super) failed: Vec<super::errors::Failed>,
    /// Why the query bar's text could not be run, shown under it; the rows from before stay.
    pub(super) bar_err: Option<String>,
    /// An agent changed this collection or table since the tab loaded; cleared by a reload.
    pub(super) agent_changed: bool,
    pub(super) selected: Option<(bool, usize)>,
    /// Rows of the current page checked for bulk actions, and the last one clicked for Shift ranges.
    pub(super) checked: BTreeSet<usize>,
    pub(super) check_anchor: Option<usize>,
    /// Checked rows of the Shell result's page, which has its own table and so its own checks.
    pub(super) shell_checked: BTreeSet<usize>,
    /// Rows open inline besides the selected one, keyed like `selected`.
    pub(super) expanded: BTreeSet<(bool, usize)>,
    /// A SQL grid shows its selected row as a record under it, toggled by Tab.
    pub(super) record: bool,
    pub(super) agg_txt: String,
    pub(super) agg_docs: Vec<db::Row>,
    pub(super) agg_fields: Vec<String>,
    pub(super) agg_ms: Option<u128>,
    /// The running pipeline's request, 0 when idle. Kept apart from `req` so a page load and a
    /// pipeline never discard each other's answer.
    pub(super) agg_req: u64,
    // Stage whose output the results pane shows; None after a full run.
    pub(super) agg_stage: Option<usize>,
    /// Pipeline builder stages and the pipeline text they match; when `agg_txt` changes
    /// elsewhere (Text mode, library, a pasted query) the stages are rebuilt from it.
    pub(super) stages: Vec<Stage>,
    pub(super) stages_src: String,
    /// The collection's first documents and estimated count, shown above the first stage.
    pub(super) agg_input: Stage,
    pub(super) agg_text_mode: bool,
    pub(super) indexes: Vec<Document>,
    /// Each indexed field path with the names of its indexes, built when the indexes arrive since
    /// the table and document marks look it up on every frame.
    pub(super) indexed: HashMap<String, String>,
    /// The page size the documents on screen were loaded with, to reload them after it changes.
    pub(super) size: u64,
    /// The Indexes view's table rows: each index with its usage merged in. Rebuilt only when the
    /// indexes or their stats arrive, since column widths are cached by the rows' address.
    pub(super) idx_rows: Vec<Document>,
    /// Names the rows on screen, changed by `rows_replaced` whenever an answer replaces them. Measured column
    /// widths are kept per generation, because the allocator may give a new answer an old one's address.
    pub(super) rows_gen: u64,
    pub(super) shell: ShellTab,
    pub(super) schema: SchemaTab,
    pub(super) import: ImportTab,
    pub(super) compare: CompareTab,
    pub(super) watch: WatchTab,
    /// Preview text of referenced documents, keyed by `compact(_id)`.
    pub(super) related: HashMap<String, String>,
    /// Document count, data size in bytes and index count of the whole collection, shown under
    /// its name; asked once per tab.
    pub(super) stats: Option<(u64, u64, u64)>,
    pub(super) stats_asked: bool,
    /// Set for tabs of a SQL connection, whose rows come from `sql_page` instead of `find`.
    pub(super) sql: Option<Box<SqlTab>>,
    /// The library script or linked file this tab's editor holds, so Cmd+S writes it back.
    pub(super) script: Option<Bound>,
}

/// The tie between a tab and the library script or linked file it was opened from.
#[allow(dead_code)]
pub(super) struct Bound {
    pub(super) r: crate::library::ScriptRef,
    /// The view whose text is the script. Cmd+S in another view of the tab does not write it.
    pub(super) view: View,
    /// The text as last opened or saved, to tell an edited tab from a clean one.
    pub(super) base: String,
    /// Modified time and length of a linked file when it was last read or written.
    pub(super) disk: Option<(std::time::SystemTime, u64)>,
    /// A linked file that changed on disk under an edited tab: the text found there, until the person picks Reload,
    /// Keep mine or Compare.
    pub(super) conflict: Option<String>,
}

/// What a row of the sidebar carries while it is dragged. Dropped on another row, or on a rail tile.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DragItem {
    Conn(u64),
    Script(crate::library::ScriptRef),
    Folder(u64),
}

/// What a SQL tab keeps besides the shared row fields of `Tab`.
#[derive(Default)]
pub(super) struct SqlTab {
    /// Columns in table order, from the last page answer.
    pub(super) cols: Arc<Vec<db::sql::Col>>,
    /// Primary key column names; empty means the rows cannot be edited.
    pub(super) pk: Vec<String>,
    /// The row count is the server's table estimate, shown with a ~.
    pub(super) estimated: bool,
    /// Text for the Query view, set by "Edit in Query" on checked rows.
    pub(super) query: String,
    /// The query of the last page asked for, which whole-query column actions repeat.
    pub(super) ran: db::sql::PageQuery,
    /// Rows before grouping when the last answer was grouped; None for an ordinary answer.
    pub(super) matched: Option<u64>,
    /// The Content bar: rules, clause chips and their text.
    pub(super) bar: super::sql::bar::Bar,
    /// What Stop reaches of the running Custom Query.
    pub(super) run: db::sql::RunHandle,
    /// The outcome of each statement of the last run, the one shown, and the request still awaited (0 when idle).
    pub(super) sets: Vec<db::sql::ResultSet>,
    pub(super) set_sel: usize,
    pub(super) run_req: u64,
    /// Database the Query view runs in when it is not the tab's own.
    pub(super) run_db: String,
    /// PostgreSQL only: the schema the Query view puts first in search_path. Empty keeps the server's own.
    pub(super) run_schema: String,
    /// Caret of the Query editor as a char index, and what the script says about it.
    pub(super) caret: usize,
    pub(super) caret_cache: std::cell::RefCell<Option<super::sql::query::CaretInfo>>,
    /// Results as text instead of a grid, and that text for the set it was built from.
    pub(super) text_mode: bool,
    pub(super) text_cache: Option<(usize, String)>,
    /// Checked rows of the shown page of the selected result, and the anchor of a Shift range. A page change
    /// or a new run clears them: checks belong to the page they were made on.
    pub(super) qchecked: BTreeSet<usize>,
    pub(super) qanchor: Option<usize>,
    /// The selected row of that page, whose record Tab opens. Cleared with the checks.
    pub(super) qsel: Option<usize>,
    /// The run the shown results belong to, which their row counts answer to, and the result tab being
    /// read at another page (its answer replaces only that tab).
    pub(super) sets_req: u64,
    pub(super) paging: Option<usize>,
    /// What the Structure, Relations, Triggers and Info views show.
    pub(super) st: super::sql::structure::StructState,
}

/// The Compare view: this tab's collection (source) against a chosen target.
#[derive(Default)]
pub(super) struct CompareTab {
    pub(super) conn: u64,
    pub(super) db: String,
    pub(super) coll: String,
    pub(super) req: u64,
    pub(super) running: bool,
    pub(super) result: Option<db::Compared>,
    /// The target `result` was computed against.
    pub(super) ran: (u64, String, String),
    pub(super) insert: bool,
    pub(super) replace: bool,
    pub(super) delete: bool,
    pub(super) list: CmpList,
    pub(super) page: usize,
    /// Columns of each list, indexed by `CmpList`, so they stay put from page to page.
    pub(super) fields: [Vec<String>; 3],
}

#[derive(Default, Clone, Copy, PartialEq)]
pub(super) enum CmpList {
    #[default]
    Changed,
    OnlySrc,
    OnlyDst,
}

impl CompareTab {
    /// Opens on the first list with rows, since an empty Different list is the common case.
    pub(super) fn set_result(&mut self, r: Option<db::Compared>) {
        if let Some(r) = &r {
            let lens = [r.changed.len(), r.only_src.len(), r.only_dst.len()];
            self.list = [CmpList::Changed, CmpList::OnlySrc, CmpList::OnlyDst][lens.iter().position(|&n| n > 0).unwrap_or(0)];
            // ponytail: columns from the first 5000 documents, a field only seen later is reachable from the Columns menu.
            let sample = |d: &[Document]| auto_fields(&d[..d.len().min(5000)]);
            self.fields = [db::DIFF_FIELDS.map(String::from).to_vec(), sample(&r.only_src), sample(&r.only_dst)];
        }
        (self.result, self.page) = (r, 0);
    }

    /// Every row of the list being shown.
    pub(super) fn rows(&self) -> &[Document] {
        self.result.as_ref().map_or(&[], |r| match self.list {
            CmpList::Changed => &r.diffs,
            CmpList::OnlySrc => &r.only_src,
            CmpList::OnlyDst => &r.only_dst,
        })
    }

    fn rows_mut(&mut self) -> Option<&mut Vec<Document>> {
        let r = self.result.as_mut()?;
        Some(match self.list {
            CmpList::Changed => &mut r.diffs,
            CmpList::OnlySrc => &mut r.only_src,
            CmpList::OnlyDst => &mut r.only_dst,
        })
    }
}

/// A field holding ids of another collection's documents (the original's dataRelations).
#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
#[serde(default)]
pub(super) struct Relation {
    pub(super) coll: String,
    pub(super) path: String,
    pub(super) target: String,
    pub(super) preview: String,
}

/// The Dump or Restore form.
#[derive(Default)]
pub(super) struct ToolForm {
    pub(super) conn: u64,
    pub(super) db: String,
    pub(super) coll: String,
    pub(super) restore: bool,
    pub(super) path: String,
    pub(super) gzip: bool,
    pub(super) drop: bool,
    /// Database name inside the archive; restoring renames it to `db` when different.
    pub(super) from_db: String,
}

/// A running (or finished) mongodump or mongorestore.
pub(super) struct ToolRun {
    pub(super) title: String,
    pub(super) log: Vec<String>,
    pub(super) cancel: Option<tokio::sync::oneshot::Sender<()>>,
    pub(super) done: Option<db::Res<String>>,
}

/// The Schema view of one tab.
pub(super) struct SchemaTab {
    pub(super) req: u64,
    pub(super) limit: i64,
    /// The limit counts from the most recently inserted documents instead of the first.
    pub(super) last: bool,
    pub(super) running: bool,
    pub(super) stats: Vec<db::FieldStat>,
    pub(super) docs: usize,
    pub(super) ms: Option<u128>,
    /// Why the last analysis failed; also stops the automatic analysis from retrying every frame.
    pub(super) err: Option<String>,
    /// Validator editor: the JSON, the level (0 off, 1 moderate, 2 strict) and whether failures only warn.
    pub(super) vtext: String,
    pub(super) vlevel: u8,
    pub(super) vwarn: bool,
    pub(super) vreq: u64,
    pub(super) vloaded: bool,
    /// Field paths whose children are folded away.
    pub(super) closed: HashSet<String>,
}

impl Default for SchemaTab {
    fn default() -> Self {
        Self { req: 0, limit: SCHEMA_DEFAULT_LIMIT, last: false, running: false, stats: Vec::new(), docs: 0, ms: None, err: None, vtext: String::new(), vlevel: 2, vwarn: false, vreq: 0, vloaded: false, closed: HashSet::new() }
    }
}

/// The original analyses every matching document (limit 0); on large production collections that
/// is slow, so the rebuild starts with the first 1000 and offers "All".
pub(super) const SCHEMA_DEFAULT_LIMIT: i64 = 1000;

/// The Import view of one tab: the text to import, how to read it and the parsed preview.
pub(super) struct ImportTab {
    pub(super) text: String,
    pub(super) file: Option<String>,
    pub(super) kind: db::ImportKind,
    pub(super) separator: Option<char>,
    pub(super) header: bool,
    pub(super) camel: bool,
    pub(super) upsert: bool,
    pub(super) upsert_by: String,
    pub(super) parsed: Option<db::Res<Vec<Document>>>,
    /// What `parsed` was made from, so it is redone only when something changes.
    pub(super) parsed_key: u64,
    pub(super) fields: Vec<String>,
}

impl Default for ImportTab {
    fn default() -> Self {
        Self { text: String::new(), file: None, kind: db::ImportKind::Json, separator: None, header: true, camel: false, upsert: false, upsert_by: "_id".into(), parsed: None, parsed_key: 0, fields: Vec::new() }
    }
}

/// The Shell view of one tab: the script, its logs and its result.
#[derive(Default)]
pub(super) struct ShellTab {
    pub(super) code: String,
    /// Other saved connections whose data is a global of the script, saved with the tab.
    pub(super) attached: Vec<Attached>,
    pub(super) logs: Vec<(String, String)>,
    /// The returned value when it is not documents; see `result` for the whole of it.
    pub(super) value: Option<Bson>,
    /// Returned documents, one or an array of them, as BSON bytes for the table.
    pub(super) docs: Vec<db::Row>,
    pub(super) fields: Vec<String>,
    pub(super) page: usize,
    /// Show the result as JSON text instead of the table.
    pub(super) raw: bool,
    pub(super) raw_text: Option<String>,
    pub(super) error: Option<String>,
    pub(super) req: u64,
    pub(super) cancel: Option<tokio::sync::oneshot::Sender<()>>,
    pub(super) help: bool,
    pub(super) started: Option<std::time::Instant>,
    pub(super) ms: Option<u128>,
    pub(super) count: Option<usize>,
    /// Every query a SQL script ran, in order.
    pub(super) queries: Vec<db::ShellQuery>,
    /// The bottom panel shows the Queries tab instead of the Console.
    pub(super) queries_tab: bool,
    /// The server connection of each SQL relay of the running script (app connection id, its thread id), so the
    /// Server window can mark them as Quarry's own. Ids are only read while the script runs.
    pub(super) relays: Vec<(u64, std::sync::Arc<std::sync::atomic::AtomicU64>)>,
}

/// Which of `len` rows a page shows (all of them for None), and the number of the first one minus one.
pub(super) fn page_window(page: Option<usize>, len: usize) -> (std::ops::Range<usize>, u64) {
    let Some(page) = page else { return (0..len, 0) };
    let start = (page * PAGE_SIZE as usize).min(len);
    (start..(start + PAGE_SIZE as usize).min(len), start as u64)
}

impl Tab {
    /// Drops the error panel of `view`, once its read worked or is asked again.
    pub(super) fn clear_failed(&mut self, view: View) {
        self.failed.retain(|f| f.view != view);
    }

    /// Call wherever an answer replaces the rows this tab shows.
    pub(super) fn rows_replaced(&mut self) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.rows_gen = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// "conn/db/coll", the key of per-collection settings.
    pub(super) fn coll_key(&self) -> String {
        if self.sel.schema.is_empty() { format!("{}/{}/{}", self.sel.conn, self.sel.db, self.sel.coll) } else { format!("{}/{}/{}.{}", self.sel.conn, self.sel.db, self.sel.schema, self.sel.coll) }
    }

    /// The page decoded, for a scan of every row; the tab itself keeps only BSON bytes.
    pub(super) fn decoded_docs(&self) -> Vec<Document> {
        self.docs.iter().map(db::Row::owned).collect()
    }

    /// The result table that is not the Documents one, as this view keeps it: all its rows, their columns,
    /// the page on screen (None shows every row) and the suffix of its column-settings key.
    pub(super) fn agg_source(&self) -> (Rows<'_>, &[String], Option<usize>, &'static str) {
        match self.view {
            View::Shell => (Rows::Raw(&self.shell.docs), &self.shell.fields, Some(self.shell.page), "shell"),
            // Import previews its first page.
            View::Import => match &self.import.parsed {
                Some(Ok(d)) => (Rows::Docs(d), &self.import.fields, Some(0), "import"),
                _ => (Rows::Docs(&[]), &[], Some(0), "import"),
            },
            // Field rows and document rows have different columns.
            View::Compare => (Rows::Docs(self.compare.rows()), &self.compare.fields[self.compare.list as usize], Some(self.compare.page), if self.compare.list == CmpList::Changed { "compare-diff" } else { "compare" }),
            View::Indexes => (Rows::Docs(&self.idx_rows), &IDX_FIELDS, None, "indexes"),
            View::Watch => (Rows::Raw(&self.watch.rows), &WATCH_FIELDS, None, "watch"),
            _ => (Rows::Raw(&self.agg_docs), &self.agg_fields, None, "agg"),
        }
    }

    /// Where `view` keeps those rows, so the table can take them out while it draws and put them back.
    pub(super) fn agg_store(&mut self, view: View) -> Option<Store<'_>> {
        Some(match view {
            View::Shell => Store::Raw(&mut self.shell.docs),
            View::Import => Store::Docs(self.import.parsed.as_mut()?.as_mut().ok()?),
            View::Compare => Store::Docs(self.compare.rows_mut()?),
            View::Indexes => Store::Docs(&mut self.idx_rows),
            View::Watch => Store::Raw(&mut self.watch.rows),
            _ => Store::Raw(&mut self.agg_docs),
        })
    }

    /// Whether the grid on screen has a selected row for Tab to open as a record. The Query view keeps its own,
    /// because its rows are none of the ones `selected` points into.
    pub(super) fn record_row(&self) -> bool {
        match (self.view, &self.sql) {
            // Text mode draws no grid, so no record either.
            (View::Sql, Some(s)) => s.qsel.is_some() && !s.text_mode,
            _ => self.selected.is_some(),
        }
    }

    /// The rows on screen in that table, their columns, and the number of the first one minus one.
    pub(super) fn agg_page(&self) -> (Rows<'_>, &[String], u64) {
        let (docs, fields, page, _) = self.agg_source();
        let (range, base) = page_window(page, docs.len());
        (docs.slice(range), fields, base)
    }

    /// Whether the rows on screen are documents of this collection, so actions on the collection apply to them.
    pub(super) fn in_coll(&self) -> bool {
        !matches!(self.view, View::Compare | View::Import | View::Indexes | View::Watch)
    }

    pub(super) fn set_indexes(&mut self, v: Vec<Document>) {
        self.indexed.clear();
        for ix in &v {
            let name = ix.get_str("name").unwrap_or_default();
            // Text indexes keep their fields under weights; their keys are internal.
            for path in ["keys", "weights"].iter().filter_map(|k| ix.get_document(k).ok()).flat_map(|d| d.keys()) {
                let names = self.indexed.entry(path.clone()).or_default();
                if !names.is_empty() {
                    names.push_str(", ");
                }
                names.push_str(name);
            }
        }
        self.indexes = v;
    }

    /// Names of the indexes whose keys include this field, in the Documents view only. Array
    /// positions in `path` are skipped, since index keys name a field inside an array without them.
    pub(super) fn indexes_on(&self, path: &str) -> Option<&str> {
        if self.view != View::Documents || self.indexed.is_empty() {
            return None;
        }
        let hit = if path.split('.').any(|s| s.parse::<usize>().is_ok()) { self.indexed.get(&db::strip_indices(path)) } else { self.indexed.get(path) };
        hit.map(String::as_str)
    }

    /// Empties the sort, project, limit, skip, hint and collation boxes, for a query that brings its own.
    pub(super) fn clear_options(&mut self) {
        for s in [&mut self.sort_txt, &mut self.proj_txt, &mut self.limit_txt, &mut self.skip_txt, &mut self.hint_txt, &mut self.collation_txt] {
            s.clear();
        }
    }

    /// How many documents an action on every match of the filter reaches, as a phrase. The results
    /// count stops at the limit and skip, which such actions ignore.
    pub(super) fn bulk_count(&self) -> String {
        if self.query.limit.is_some() || self.query.skip > 0 { "every match, ignoring the limit and skip".into() } else { format!("about {}", self.count) }
    }

    /// Marks the tab as one of a SQL connection.
    pub(super) fn for_engine(mut self, engine: db::sql::Engine) -> Self {
        self.sql = engine.is_sql().then(Default::default);
        self
    }

    pub(super) fn new(sel: Sel) -> Self {
        Self {
            uid: NEXT_TAB.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            sel,
            view: View::Documents,
            filter_txt: String::new(),
            sort_txt: String::new(),
            proj_txt: String::new(),
            limit_txt: String::new(),
            skip_txt: String::new(),
            hint_txt: String::new(),
            collation_txt: String::new(),
            query: Query::default(),
            page: 0,
            docs: Vec::new(),
            fields: Vec::new(),
            paths: Vec::new(),
            field_info: Default::default(),
            count: 0,
            ms: 0,
            req: 0,
            idx_req: 0,
            loading: false,
            failed: Vec::new(),
            bar_err: None,
            agent_changed: false,
            selected: None,
            checked: BTreeSet::new(),
            check_anchor: None,
            shell_checked: BTreeSet::new(),
            expanded: BTreeSet::new(),
            record: false,
            agg_txt: NEW_PIPELINE.into(),
            agg_docs: Vec::new(),
            agg_fields: Vec::new(),
            agg_ms: None,
            agg_req: 0,
            agg_stage: None,
            stages: Vec::new(),
            stages_src: String::new(),
            agg_input: Stage::new("", "", true),
            agg_text_mode: false,
            indexes: Vec::new(),
            indexed: HashMap::new(),
            size: 0,
            idx_rows: Vec::new(),
            rows_gen: 0,
            shell: ShellTab::default(),
            schema: SchemaTab::default(),
            import: ImportTab::default(),
            compare: CompareTab { insert: true, replace: true, ..Default::default() },
            watch: WatchTab::default(),
            related: HashMap::new(),
            stats: None,
            stats_asked: false,
            sql: None,
            script: None,
        }
    }
}

pub(super) enum Danger {
    DeleteMatching(db::Scope),
    DropCollection,
    DropIndex(String),
    RemoveConnection(u64),
    DropDatabase { conn: u64, db: String },
    DropCollAt { conn: u64, db: String, coll: String },
    DropUser { conn: u64, db: String, user: String },
    RenameMany { scope: db::Scope, from: String, to: String },
    UpdateMany { scope: db::Scope, stages: Vec<Document> },
    Upsert { key: String },
    Sync { conn: u64 },
    Restore { conn: u64, args: Vec<String> },
    Validator(Document),
    /// Rows of a SQL table, by key, with the table they were picked in.
    DeleteRows(Sel, Vec<Document>),
    /// The staged ALTER TABLE of the open SQL table, confirmed because it drops a column or cannot be undone.
    ApplyAlter(Sel),
    /// The structure undo recorded at this time, confirmed because it drops a column or converts a type back.
    UndoAlter(std::time::Instant),
    /// Drop a SQL database, table or view, or empty a table; `table` is empty for a database.
    SqlDrop { conn: u64, database: String, schema: String, table: String, kind: DropKind, opt: bool },
    /// A trigger of a SQL table, by name.
    DropTrigger(Sel, String),
    /// Drop a SQL user or role.
    SqlDropUser { conn: u64, acct: db::sql::users::Account },
}

/// What to run once writes are allowed.
pub(super) type Retry = Box<dyn FnOnce(&mut App)>;

#[allow(dead_code)]
pub(super) enum Modal {
    Conn(Box<ConnForm>),
    Settings,
    /// The second field reruns what asked for writes once they are allowed.
    Unlock(u64, Option<Retry>),
    Save { tag: String, changes: Vec<(String, Option<Bson>, Option<Bson>)> },
    SaveAll(Vec<String>),
    /// The rows or documents, with the table or collection they were picked in.
    Delete(Sel, Vec<Document>),
    SqlSet(Box<SetForm>),
    SqlRow(Box<RowForm>),
    /// Add or edit a column of the open SQL table, with its ALTER preview.
    ColEdit(Box<ColForm>),
    IdxEdit(Box<IdxForm>),
    SqlNewDb(Box<NewDb>),
    SqlNewTable(Box<NewTable>),
    SqlDrop(Box<DropForm>),
    PgNewDb(Box<super::sql::create_pg::PgNewDb>),
    PgNewSchema(Box<super::sql::create_pg::PgNewSchema>),
    PgNewTable(Box<super::sql::create_pg::PgNewTable>),
    SqlNewUser(Box<super::sql::users::NewUser>),
    SqlPassword(Box<super::sql::users::PwForm>),
    PgNewRole(Box<super::sql::pg_roles::NewRoleForm>),
    PgPassword(Box<super::sql::pg_roles::PgPwForm>),
    PgDropRole(Box<super::sql::pg_roles::DropForm>),
    /// Add a foreign key to the open SQL table, with its ALTER preview.
    FkEdit(Box<FkForm>),
    Typed { title: String, expect: String, typed: String, action: Danger },
    CreateIndex(Box<IndexForm>),
    AggWrite(Vec<Document>),
    ShellWrite(Vec<String>),
    ImportInsert(usize),
    NewCollection { conn: u64, db: String, name: String, view: bool, source: String, pipeline: String },
    RenameColl { conn: u64, db: String, coll: String, name: String },
    DuplicateColl { conn: u64, db: String, coll: String, name: String },
    Undo,
    Explain { summary: db::ExplainSummary, raw: String, show_raw: bool },
    ErrorDetails(Box<super::errors::ErrEntry>),
    /// `code` highlights the body as JSON.
    Text { title: String, body: String, code: bool },
    Rename { doc: Document, path: Vec<String>, name: String },
    /// `preview` keeps the stages it was made from; Apply only runs when they equal the current
    /// ones, so a late reply for older inputs cannot approve newer stages.
    UpdateMany { set_txt: String, unset_txt: String, preview: Option<(Vec<Document>, db::Res<(Vec<(Document, Document)>, u64)>)> },
    Tool(Box<ToolForm>),
    Relations { conn: u64, db: String },
    /// `cmds` once ">" switched the palette to commands.
    Palette { query: String, sel: usize, cmds: bool, new_tab: bool },
    /// Export or copy-to-file of the rows of a result, in any of the Copy as formats.
    Export(Box<super::export::ExportForm>),
    /// Import a CSV file into a SQL table.
    SqlImport(Box<super::sql::import::ImportForm>),
    /// Export a SQL database or some of its tables as a dump script.
    SqlDump(Box<super::sql::dump::DumpForm>),
    /// Run a script file on a SQL database.
    SqlRunFile(Box<super::sql::dump::RunForm>),
    /// Values of a field with how many documents hold each, and the pipeline that counted them.
    Grouped { sel: Sel, path: String, pipeline: Vec<Document>, rows: Vec<Document> },
    /// The column header's Customize window; `key` is the column prefs key it edits.
    ColumnCustomize { key: String, agg: bool, col: String, label: String, path: String },
    /// Attach another saved connection to the current Shell tab.
    Attach(Box<super::shell_attach::AttachForm>),
    /// Edit a connection group.
    GroupEdit(Box<super::group_dialog::GroupForm>),
    /// Save the current script to the library.
    SaveScript(Box<super::library::save_dialog::SaveForm>),
    /// Which connection a library script opens on, when none is the obvious one.
    OpenOn(Box<super::library::open::OpenOnForm>),
    /// Link a folder on disk to the library.
    LinkFolder(Box<super::library::link_dialog::LinkForm>),
}

/// A value being edited in place in the document tree.
pub(super) struct Inline {
    pub(super) path: Vec<String>,
    pub(super) text: String,
    pub(super) kind: &'static str,
    pub(super) focus: bool,
    /// The calendar of a date is open, holding the text from before it opened for Cancel.
    pub(super) pick: Option<String>,
    /// Dates are shown and typed in this computer's zone instead of UTC.
    pub(super) local: bool,
}

/// The large JSON editor for a whole document (empty path) or one object/array field.
pub(super) struct JsonEdit {
    pub(super) base: Document,
    pub(super) path: Vec<String>,
    pub(super) text: String,
    pub(super) kind: &'static str,
    pub(super) focus: bool,
    /// Edited since it opened, so the side panel must not drop it for another value.
    pub(super) dirty: bool,
}

impl JsonEdit {
    pub(super) fn new(base: Document, path: Vec<String>, text: String, kind: &'static str) -> Self {
        Self { base, path, text, kind, focus: true, dirty: false }
    }
}

/// One document's edits that are not written yet. `original` is what was loaded, so saving can
/// refuse to overwrite someone else's change.
#[derive(Clone)]
pub(super) struct Draft {
    pub(super) conn: u64,
    pub(super) db: String,
    pub(super) schema: String,
    pub(super) coll: String,
    pub(super) original: Document,
    pub(super) current: Document,
}

/// A field being added inside an object (or appended to an array).
pub(super) struct NewField {
    pub(super) parent: Vec<String>,
    pub(super) name: String,
    pub(super) text: String,
    pub(super) kind: &'static str,
    /// Why Add was refused, shown under the inputs until the name changes.
    pub(super) err: Option<String>,
}

/// One stage of the pipeline builder with its own preview, like the original's AggregatorStage
/// (prepareNewStage, renderer line 112193).
pub(super) struct Stage {
    /// Stable id for the editor, so reordering keeps each text box's cursor and focus.
    pub(super) uid: u64,
    pub(super) op: String,
    pub(super) text: String,
    pub(super) on: bool,
    pub(super) req: u64,
    pub(super) count: Option<u64>,
    pub(super) sample: Vec<Document>,
    pub(super) ms: Option<u128>,
    pub(super) error: Option<String>,
    pub(super) limit: i64,
    /// When the preview should run; edits wait a second like the original's 1000 ms debounce
    /// (updateStage, renderer line 112016).
    pub(super) due: Option<std::time::Instant>,
    /// Sample rows shown whole.
    pub(super) open: HashSet<usize>,
}

impl Stage {
    pub(super) fn new(op: &str, text: &str, on: bool) -> Self {
        static UID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            uid: UID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            op: op.into(),
            text: text.into(),
            on,
            req: 0,
            count: None,
            sample: Vec::new(),
            ms: None,
            error: None,
            limit: db::STAGE_SAMPLE,
            due: Some(std::time::Instant::now()),
            open: HashSet::new(),
        }
    }
}

/// Stages from pipeline text; each stage must hold exactly one operator.
pub(super) fn stages_from_text(text: &str) -> db::Res<Vec<Stage>> {
    db::parse_pipeline(text)?
        .iter()
        .enumerate()
        .map(|(i, d)| match (d.len(), d.iter().next()) {
            (1, Some((op, v))) => Ok(Stage::new(op, &db::pretty_shell(v), true)),
            _ => Err(format!("Stage {} must have exactly one operator, like {{ $match: {{}} }}", i + 1)),
        })
        .collect()
}

/// Pipeline text of the stages that are on, so Run, the library and code export see them.
pub(super) fn text_from_stages(stages: &[Stage]) -> String {
    let body: Vec<String> = stages.iter().filter(|s| s.on).map(|s| format!("  {{ {}: {} }}", s.op, s.text.trim().replace('\n', "\n  "))).collect();
    if body.is_empty() { "[]".into() } else { format!("[\n{}\n]", body.join(",\n")) }
}

/// The pipeline that ends with stage `i`, stages that are off left out.
pub(super) fn stage_pipeline(stages: &[Stage], i: usize) -> db::Res<Vec<Document>> {
    stages[..=i]
        .iter()
        .enumerate()
        .filter(|(_, s)| s.on)
        .map(|(k, s)| db::parse_value(&s.text).map(|v| doc! { s.op.clone(): v }).map_err(|e| format!("Stage {} ({}): {e}", k + 1, s.op)))
        .collect()
}

/// The tabs of the connection form. Proxy is Mongo only and TLS is SQL only.
#[derive(Default, Clone, Copy, PartialEq)]
pub(super) enum ConnTab {
    #[default]
    Standard,
    Ssh,
    Proxy,
    Tls,
    Advanced,
}

/// The time zone choice of the Advanced tab; the Custom text lives beside it.
#[derive(Default, Clone, Copy, PartialEq)]
pub(super) enum ZoneKind {
    #[default]
    Server,
    Utc,
    System,
    Custom,
}

/// The new/edit connection form.
#[derive(Default)]
pub(super) struct ConnForm {
    pub(super) id: Option<u64>,
    /// A new connection joins this group when it is saved, from "New connection in <group>" in the Connections panel.
    pub(super) group: Option<u64>,
    pub(super) engine: db::sql::Engine,
    pub(super) addr: db::sql::Addr,
    pub(super) tab: ConnTab,
    pub(super) name: String,
    pub(super) tag: String,
    pub(super) uri: String,
    pub(super) show: bool,
    pub(super) ssh_on: bool,
    /// Set when the connection goes through Cloud SQL; refused together with SSH.
    pub(super) cloud_sql: Option<crate::cloudsql::CloudSql>,
    /// The instance as the browser listed it, for its state and the IP kinds it has; None until gcloud lists it.
    pub(super) cloud_inst: Option<crate::cloudsql::Instance>,
    pub(super) ssh: crate::ssh::SshConf,
    pub(super) ssh_pwd: String,
    pub(super) ssh_pass: String,
    pub(super) proxy_on: bool,
    pub(super) proxy: db::Proxy,
    pub(super) proxy_pwd: String,
    /// PostgreSQL: the passphrase of an encrypted client key, kept in the Keychain.
    pub(super) pg_key_pass: String,
    pub(super) testing: bool,
    /// Charset, collation and init command are edited in place; the rest has form state of its own below.
    pub(super) adv: db::sql::Advanced,
    pub(super) zone_kind: ZoneKind,
    pub(super) zone_text: String,
    pub(super) mode_custom: bool,
    pub(super) mode_on: [bool; 8],
    pub(super) mode_extra: String,
    pub(super) t_connect: String,
    pub(super) t_query: String,
    pub(super) t_idle: String,
    /// (charset, collation) pairs the last Test read from the server.
    pub(super) collations: Vec<(String, String)>,
    /// The request id of this form's last Test, so an answer meant for another form or an older Test is ignored.
    pub(super) test_req: u64,
    /// The server's own sql_mode as the last Test read it, where Custom starts from.
    pub(super) server_mode: Option<String>,
    /// The Custom mode holds a choice (saved, clicked, or the server's once), so a Test never replaces it, even
    /// when that choice is no flags at all.
    pub(super) mode_seeded: bool,
    /// PostgreSQL: the rows of the session settings as (name, value). The three timeout and name rows are always first.
    pub(super) pg_rows: Vec<(String, String)>,
    /// PostgreSQL: the saved search_path list, in order.
    pub(super) pg_path: Vec<String>,
    /// PostgreSQL: the "Add setting" picker, with the text typed in its filter, when it is open.
    pub(super) pg_picker: Option<String>,
    /// PostgreSQL: the text of the schema box beside the "Add schema" list.
    pub(super) pg_schema_text: String,
    /// PostgreSQL: the schemas of the default database as the last Test found them, and whether a Test ran at all.
    pub(super) pg_schemas: Vec<String>,
    pub(super) pg_tested: bool,
    /// PostgreSQL: (setting, the value that was tested, the server's refusal) from the last Test.
    pub(super) pg_refused: Vec<(String, String, String)>,
    /// PostgreSQL: `advanced()` for the inputs it was made from, since the form asks every frame and validating every row is not free.
    pub(super) pg_adv: Option<(PgAdvKey, Option<String>)>,
    /// PostgreSQL: the shape check and allow-list entry of each setting row, kept while the row's name and value stay as they are.
    pub(super) pg_checks: Vec<RowCheck>,
}

/// What the PostgreSQL Advanced tab is made from.
#[derive(PartialEq)]
pub(super) struct PgAdvKey {
    zone_kind: ZoneKind,
    zone_text: String,
    rows: Vec<(String, String)>,
    path: Vec<String>,
    connect: String,
    idle: String,
}

/// The verdict of the form on one session setting row.
pub(super) struct RowCheck {
    pub name: String,
    pub value: String,
    pub error: Option<String>,
    pub def: Option<&'static db::sql::pg::advanced::Def>,
}

/// The host saved in a Cloud SQL connection's address, which the proxy's local port replaces when it connects.
pub(super) const CLOUD_HOST: &str = "127.0.0.1";

/// The rows of the PostgreSQL session settings that are always there, before the ones added from the picker.
pub(super) const PG_FIXED: [&str; 3] = ["statement_timeout", "lock_timeout", "application_name"];

impl ConnForm {
    /// Applies what a Test learned, if it answers this form's last Test.
    pub(super) fn take_test_info(&mut self, req: u64, info: db::sql::TestInfo) -> bool {
        if req == 0 || req != self.test_req {
            return false;
        }
        self.server_mode = Some(info.sql_mode);
        self.collations = info.collations;
        self.seed_mode();
        if self.engine == db::sql::Engine::Postgres {
            self.pg_schemas = info.schemas;
            self.pg_tested = true;
            // Kept with the value that was tested, so a row the user changed since no longer shows the old refusal.
            let value = |f: &ConnForm, n: &str| f.pg_rows.iter().find(|(r, _)| r == n).map(|(_, v)| v.trim().to_string()).unwrap_or_default();
            self.pg_refused = info.setting_errors.into_iter().map(|(n, m)| (n.clone(), value(self, &n), m)).collect();
        }
        true
    }

    /// Starts Custom from the server's mode, once.
    pub(super) fn seed_mode(&mut self) {
        use db::sql::advanced::{parse_flags, MODE_FLAGS};
        let Some(mode) = self.server_mode.as_deref().filter(|_| self.mode_custom && !self.mode_seeded) else { return };
        let flags = parse_flags(mode).unwrap_or_default();
        self.mode_on = MODE_FLAGS.map(|m| flags.iter().any(|x| x == m));
        self.mode_extra = flags.iter().filter(|x| !MODE_FLAGS.contains(&x.as_str())).cloned().collect::<Vec<_>>().join(", ");
        self.mode_seeded = true;
    }

    /// Makes sure the always-present PostgreSQL rows exist, in front of the added ones.
    pub(super) fn pg_ensure_rows(&mut self) {
        for (i, n) in PG_FIXED.iter().enumerate() {
            if self.pg_rows.get(i).is_none_or(|(r, _)| r != n) {
                let v = self.pg_rows.iter().position(|(r, _)| r == n).map(|j| self.pg_rows.remove(j).1).unwrap_or_default();
                self.pg_rows.insert(i, (n.to_string(), v));
            }
        }
    }

    pub(super) fn time_zone(&self) -> db::sql::Zone {
        match self.zone_kind {
            ZoneKind::Server => db::sql::Zone::Server,
            ZoneKind::Utc => db::sql::Zone::Utc,
            ZoneKind::System => db::sql::Zone::System,
            ZoneKind::Custom => db::sql::Zone::Custom(self.zone_text.trim().into()),
        }
    }

    /// The PostgreSQL Advanced tab as saved settings: an empty fixed row sends nothing, any other row must hold a value.
    fn pg_advanced(&self) -> Result<db::sql::Advanced, String> {
        let secs = |text: &str, name: &str| -> Result<Option<u32>, String> {
            let t = text.trim();
            if t.is_empty() { Ok(None) } else { t.parse().map(Some).map_err(|_| format!("{name} timeout: {t:?} is not a number of seconds")) }
        };
        let pg_settings = self.pg_rows.iter().filter(|(n, v)| !(PG_FIXED.contains(&n.as_str()) && v.trim().is_empty())).map(|(n, v)| (n.clone(), v.trim().to_string())).collect();
        let a = db::sql::Advanced { zone: self.time_zone(), pg_settings, search_path: self.pg_path.clone(), connect_secs: secs(&self.t_connect, "Connect")?, idle_secs: secs(&self.t_idle, "Idle")?, ..Default::default() };
        db::sql::pg::advanced::PgSession::new(&a, db::sql::advanced::system_zone_name())?;
        Ok(a)
    }

    /// What is wrong with the Advanced tab, if anything. PostgreSQL validates every row, so it runs again only when what the tab is made from changed.
    pub(super) fn advanced_error(&mut self) -> Option<String> {
        if self.engine != db::sql::Engine::Postgres {
            return self.advanced().err();
        }
        let same = self.pg_adv.as_ref().is_some_and(|(k, _)| k.zone_kind == self.zone_kind && k.zone_text == self.zone_text && k.rows == self.pg_rows && k.path == self.pg_path && k.connect == self.t_connect && k.idle == self.t_idle);
        if !same {
            let key = PgAdvKey { zone_kind: self.zone_kind, zone_text: self.zone_text.clone(), rows: self.pg_rows.clone(), path: self.pg_path.clone(), connect: self.t_connect.clone(), idle: self.t_idle.clone() };
            self.pg_adv = Some((key, self.advanced().err()));
        }
        self.pg_adv.as_ref().and_then(|(_, e)| e.clone())
    }

    /// The form's Advanced tab as saved settings, or the first thing wrong with it.
    pub(super) fn advanced(&self) -> Result<db::sql::Advanced, String> {
        use db::sql::advanced::{parse_flags, MODE_FLAGS};
        if !self.engine.is_sql() {
            return Ok(Default::default());
        }
        if self.engine == db::sql::Engine::Postgres {
            return self.pg_advanced();
        }
        let zone = match self.zone_kind {
            ZoneKind::Server => db::sql::Zone::Server,
            ZoneKind::Utc => db::sql::Zone::Utc,
            ZoneKind::System => db::sql::Zone::System,
            ZoneKind::Custom => db::sql::Zone::Custom(self.zone_text.trim().into()),
        };
        let sql_mode = if self.mode_custom {
            let mut flags: Vec<String> = MODE_FLAGS.iter().zip(self.mode_on).filter(|(_, on)| *on).map(|(f, _)| f.to_string()).collect();
            for f in parse_flags(&self.mode_extra)? {
                if !flags.contains(&f) {
                    flags.push(f);
                }
            }
            Some(flags)
        } else {
            None
        };
        let secs = |text: &str, name: &str| -> Result<Option<u32>, String> {
            let t = text.trim();
            if t.is_empty() { Ok(None) } else { t.parse().map(Some).map_err(|_| format!("{name} timeout: {t:?} is not a number of seconds")) }
        };
        let a = db::sql::Advanced { zone, collation: self.adv.collation.trim().into(), sql_mode, connect_secs: secs(&self.t_connect, "Connect")?, query_secs: secs(&self.t_query, "Query")?, idle_secs: secs(&self.t_idle, "Idle")?, ..self.adv.clone() };
        a.validate()?;
        Ok(a)
    }

    /// Fills the form from saved settings.
    pub(super) fn set_advanced(&mut self, a: &db::sql::Advanced) {
        use db::sql::advanced::MODE_FLAGS;
        self.adv = a.clone();
        (self.zone_kind, self.zone_text) = match &a.zone {
            db::sql::Zone::Server => (ZoneKind::Server, String::new()),
            db::sql::Zone::Utc => (ZoneKind::Utc, String::new()),
            db::sql::Zone::System => (ZoneKind::System, String::new()),
            db::sql::Zone::Custom(z) => (ZoneKind::Custom, z.clone()),
        };
        self.mode_custom = a.sql_mode.is_some();
        self.mode_seeded = a.sql_mode.is_some();
        let flags = a.sql_mode.clone().unwrap_or_default();
        self.mode_on = MODE_FLAGS.map(|f| flags.iter().any(|x| x == f));
        self.mode_extra = flags.iter().filter(|f| !MODE_FLAGS.contains(&f.as_str())).cloned().collect::<Vec<_>>().join(", ");
        let t = |s: Option<u32>| s.map(|s| s.to_string()).unwrap_or_default();
        (self.t_connect, self.t_query, self.t_idle) = (t(a.connect_secs), t(a.query_secs), t(a.idle_secs));
        self.pg_rows = a.pg_settings.clone();
        self.pg_path = a.search_path.clone();
        self.pg_ensure_rows();
    }

    /// The SQL address Test and Save use. Behind Cloud SQL the proxy replaces host and port, and an IAM sign-in sends no password.
    pub(super) fn sql_addr(&self) -> db::sql::Addr {
        let mut a = db::sql::Addr { host: self.addr.host.trim().into(), user: self.addr.user.trim().into(), database: self.addr.database.trim().into(), ..self.addr.clone() };
        if let Some(c) = self.cloud_on() {
            if a.host.is_empty() {
                a.host = CLOUD_HOST.into();
            }
            if c.iam {
                a.password.clear();
            }
        }
        a
    }

    /// The Cloud SQL settings when they apply: only PostgreSQL goes through the proxy.
    pub(super) fn cloud_on(&self) -> Option<&crate::cloudsql::CloudSql> {
        self.cloud_sql.as_ref().filter(|_| self.engine == db::sql::Engine::Postgres)
    }

    pub(super) fn net(&self) -> db::Net {
        db::Net {
            ssh: self.ssh_on.then(|| (crate::ssh::SshConf { host: self.ssh.host.trim().into(), user: self.ssh.user.trim().into(), ..self.ssh.clone() }, crate::ssh::SshSecrets { password: Some(self.ssh_pwd.clone()), passphrase: Some(self.ssh_pass.clone()) })),
            // SQL drivers cannot use SOCKS, so a stale proxy setting must not apply.
            proxy: (self.proxy_on && !self.ssh_on && !self.engine.is_sql()).then(|| db::Proxy { host: self.proxy.host.trim().into(), password: Some(self.proxy_pwd.clone()), ..self.proxy.clone() }),
            cloud_sql: self.cloud_sql.clone().filter(|_| self.engine == db::sql::Engine::Postgres),
            tools: Default::default(),
            key_passphrase: (self.engine == db::sql::Engine::Postgres && !self.pg_key_pass.is_empty()).then(|| self.pg_key_pass.clone()),
        }
    }
}

/// The Users and roles window of one connection.
#[derive(Default)]
pub(super) struct UsersWin {
    pub(super) conn: u64,
    pub(super) users: Option<db::Res<Vec<Document>>>,
    /// usersInfo was refused, so the list holds only the logged-in user.
    pub(super) self_only: bool,
    pub(super) roles: Option<db::Res<Vec<Document>>>,
    pub(super) new_name: String,
    pub(super) new_pwd: String,
    pub(super) new_db: String,
    pub(super) new_roles: String,
    /// (user, database, new password or empty, roles text)
    pub(super) edit: Option<(String, String, String, String)>,
}

pub(super) enum SideAct {
    NewColl(String, bool),
    Refresh(String),
    Users,
    Server,
    DropDb(String),
    Stats(String, String),
    ViewDef(String, String),
    Rename(String, String),
    Duplicate(String, String),
    DropColl(String, String),
    Dump(String, String),
    Restore(String),
    Compare(String, String),
    Relations(String),
    /// Analyze relations of one collection, or of every collection when empty.
    Analyze(String, String),
    /// A Query tab for a SQL database.
    NewQuery(String),
    /// Import a CSV file into a SQL table (database, table).
    SqlImport(String, String),
    /// Export a SQL dump of a database, with the table to start from when there is one.
    SqlDump(String, Option<String>),
    /// Run a script file on a SQL database.
    SqlRunFile(String),
    /// SQL create and drop: a database, a table in a database, and a drop or empty of a table or database (name empty).
    NewDb,
    NewTable(String),
    SqlDrop(String, String, DropKind),
    /// PostgreSQL: a schema of a database, and dropping one (database, schema).
    NewSchema(String),
    DropSchema(String, String),
    /// The Users and privileges window of a SQL connection.
    SqlUsers,
}

pub(super) enum QAct {
    Filter(String),
    Sort(String),
    Project(String),
}

pub(super) enum TreeAct {
    Start(Vec<String>, Bson),
    Commit,
    /// Move the inline edit into the large editor.
    Expand,
    Cancel,
    Remove(Vec<String>),
    AddField(Vec<String>),
    CommitNew,
    ShowFull(String, Bson),
    PinColumn(String),
    Highlight(String),
    Filter(String, Bson, bool),
    Group(String, Bson, bool),
    Distinct(String),
    Index(String),
    Rename(Vec<String>),
    GoRef(String, Bson),
    EditDoc,
    /// Open or close every container under the path; an empty path means the whole document.
    Fold(Vec<String>, bool),
}

/// The pipeline a new tab starts with, which is not work of the person's.
pub(super) const NEW_PIPELINE: &str = "[\n  { $match: {} },\n  { $limit: 20 }\n]";

/// Where a collection opens. `Here` reuses the current tab unless work in it would be lost.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Open {
    Here,
    NewTab,
    /// A link in data: a tab showing it without work in it, else a new tab. The current tab is never replaced.
    Link,
    /// The current tab whatever it holds: the Shell switching its own collection.
    Replace,
}

#[derive(Clone, Copy)]
pub(super) enum TabClose {
    This,
    Others,
    Left,
    Right,
    All,
}

pub(super) enum ColAct {
    Pin(String),
    Hide(String),
    Move(String, i32),
    /// Drag and drop: the column goes before the one at this position.
    MoveTo(String, usize),
    Show(String),
    Highlight(String),
    Align(String, u8),
    /// Sets the header label (empty clears it) and moves the column to another field path.
    Customize { col: String, label: String, path: String },
    Reset,
}

/// The query log panel: docked above the status bar, or in its own window.
#[derive(Default)]
pub(super) struct LogUi {
    pub(super) open: bool,
    pub(super) detached: bool,
    pub(super) filter: String,
    pub(super) errors: bool,
    /// Commands shown in full, by connection and start time.
    pub(super) expanded: HashSet<(u64, std::time::SystemTime)>,
}

pub(super) fn net_of(meta: &ConnMeta) -> db::Net {
    db::Net {
        ssh: meta.ssh.clone().map(|c| (c, crate::ssh::SshSecrets { password: secret(meta.id, "ssh-password"), passphrase: secret(meta.id, "ssh-passphrase") })),
        proxy: meta.proxy.clone().map(|p| db::Proxy { password: secret(meta.id, "proxy-password"), ..p }),
        cloud_sql: meta.cloud_sql.clone(),
        tools: Default::default(),
        key_passphrase: None,
    }
}

impl App {
    pub(super) fn ok(&mut self, s: impl Into<String>) {
        self.status = s.into();
    }

    pub(super) fn conn(&self, id: u64) -> Option<&Conn> {
        self.conns.iter().find(|c| c.meta.id == id)
    }

    pub(super) fn t(&self) -> Option<&Tab> {
        self.tabs.get(self.cur)
    }

    pub(super) fn tm(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.cur)
    }

    pub(super) fn sel_is_view(&self) -> bool {
        self.t().is_some_and(|t| self.views.has(t.sel.conn, &t.sel.nskey(), &t.sel.coll))
    }

    pub(super) fn sel_writable(&self) -> bool {
        if self.sel_is_view() {
            return false;
        }
        self.t().and_then(|t| self.conn(t.sel.conn)).is_some_and(|c| c.writable)
    }

    pub(super) fn timeout(&self) -> Duration {
        Duration::from_millis(self.prefs.timeout_ms)
    }

    pub(super) fn target_for(&self, s: &Sel) -> Option<Target> {
        let client = self.conn(s.conn)?.client.clone()?;
        Some(Target { client, db: s.db.clone(), coll: s.coll.clone(), timeout: self.timeout() })
    }

    pub(super) fn target(&self) -> Option<Target> {
        self.target_for(&self.t()?.sel)
    }

    /// The single gate every write goes through.
    pub(super) fn writable_target(&mut self) -> Option<(u64, Target)> {
        let conn = self.t()?.sel.conn;
        if !self.sel_writable() {
            self.warn_ro();
            return None;
        }
        match self.target() {
            Some(t) => Some((conn, t)),
            None => {
                self.warn_offline();
                None
            }
        }
    }

    pub(super) fn ask_unlock(&mut self) {
        if let Some(t) = self.t() {
            self.modal = Some(Modal::Unlock(t.sel.conn, None));
        }
    }

    /// Asks to allow writes, then runs `then` if they were allowed.
    pub(super) fn ask_unlock_then(&mut self, then: impl FnOnce(&mut App) + 'static) {
        if let Some(t) = self.t() {
            self.modal = Some(Modal::Unlock(t.sel.conn, Some(Box::new(then))));
        }
    }

    pub(super) fn connect(&mut self, id: u64) {
        // Demo ids overlap real ones, so reading the Keychain here would connect to a real server.
        if self.ephemeral {
            return self.ok("Offline demo: no database is connected.");
        }
        let uri = match stored_uri(id) {
            Ok(u) => u,
            Err(e) => return self.err(uri_err(e)),
        };
        self.connect_uri(id, uri);
    }

    /// Connects with a known address, so tests need no keychain.
    pub(super) fn connect_uri(&mut self, id: u64, uri: String) {
        let tools = self.cloud_tools();
        self.req += 1;
        let attempt = self.req;
        let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == id) else { return };
        c.status = Status::Connecting;
        c.attempt = attempt;
        c.client = None;
        c.sql = None;
        c.tunnel = None;
        // Asked again once connected, so a changed or failed server never shows the old one.
        c.server = ServerInfo::default();
        let mut net = net_of(&c.meta);
        let adv = c.meta.advanced.clone();
        net.tools = tools;
        // Only a connection with a client key can have a passphrase, so the others never ask the Keychain for one.
        if c.meta.engine == db::sql::Engine::Postgres && db::sql::Addr::parse(&uri).is_some_and(|a| !a.ssl_key.is_empty()) {
            net.key_passphrase = secret(id, "pg-key-passphrase");
        }
        if c.meta.engine.is_sql() {
            self.worker.sql_connect_with(id, attempt, uri, self.timeout(), net, adv);
        } else {
            self.worker.connect(id, attempt, uri, self.timeout(), net);
        }
    }

    /// Something a restart would lose: unsaved edits, writes in flight, running scripts or tools.
    pub(super) fn busy(&self) -> bool {
        !self.drafts.is_empty()
            || !self.saving.is_empty()
            || self.json_edit.is_some()
            || self.insert.is_some()
            || self.inline.is_some()
            || self.new_field.is_some()
            || self.modal.is_some()
            || self.tool.as_ref().is_some_and(|t| t.done.is_none())
            || self.tabs.iter().any(|t| t.shell.cancel.is_some() || t.compare.running || t.loading)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_answer_seeds_custom_mode_once_and_only_for_its_own_form() {
        let info = |mode: &str| db::sql::TestInfo { sql_mode: mode.into(), collations: vec![("utf8mb4".into(), "utf8mb4_bin".into())], ..Default::default() };
        let mut f = ConnForm { engine: db::sql::Engine::MySql, mode_custom: true, test_req: 7, ..Default::default() };
        // Another form's or an older Test's answer changes nothing, and a form that never tested takes none.
        assert!(!f.take_test_info(6, info("ANSI_QUOTES")));
        assert!(!ConnForm::default().take_test_info(0, info("ANSI_QUOTES")));
        assert!(f.collations.is_empty() && f.mode_on.iter().all(|on| !on));
        assert!(f.take_test_info(7, info("STRICT_TRANS_TABLES,IGNORE_SPACE")));
        assert_eq!(f.advanced().unwrap().sql_mode, Some(vec!["STRICT_TRANS_TABLES".into(), "IGNORE_SPACE".into()]));
        assert_eq!(f.collations.len(), 1);
        // The user turns every flag off on purpose; a second Test keeps that.
        f.mode_on = [false; 8];
        f.mode_extra.clear();
        f.test_req = 8;
        assert!(f.take_test_info(8, info("STRICT_TRANS_TABLES")));
        assert_eq!(f.advanced().unwrap().sql_mode, Some(vec![]));
        // A saved Custom mode with no flags is a choice too.
        let mut g = ConnForm { engine: db::sql::Engine::MySql, test_req: 3, ..Default::default() };
        g.set_advanced(&db::sql::Advanced { sql_mode: Some(vec![]), ..Default::default() });
        assert!(g.take_test_info(3, info("ANSI_QUOTES")));
        assert_eq!(g.advanced().unwrap().sql_mode, Some(vec![]));
        // Server default at Test time, Custom picked later: Custom still starts from the server's mode.
        let mut h = ConnForm { engine: db::sql::Engine::MySql, test_req: 4, ..Default::default() };
        assert!(h.take_test_info(4, info("NO_ZERO_DATE")));
        h.mode_custom = true;
        h.seed_mode();
        assert_eq!(h.advanced().unwrap().sql_mode, Some(vec!["NO_ZERO_DATE".into()]));
    }

    #[test]
    fn shell_keeps_documents_once_as_rows() {
        let mut sh = ShellTab::default();
        let docs = vec![Bson::Document(doc! { "_id": 1, "a": "x" }), Bson::Document(doc! { "_id": 2 })];
        sh.set_value(Bson::Array(docs.clone()));
        assert!(sh.value.is_none());
        assert_eq!((sh.docs.len(), sh.count, sh.fields.clone()), (2, Some(2), vec!["_id".to_string(), "a".to_string()]));
        assert_eq!(sh.result(), Some(Bson::Array(docs)));
        sh.set_value(Bson::Document(doc! { "n": 3 }));
        assert_eq!((sh.docs.len(), sh.count, sh.result()), (1, None, Some(Bson::Document(doc! { "n": 3 }))));
        for v in [Bson::Int32(7), Bson::Array(vec![]), Bson::Array(vec![Bson::Int32(1), Bson::Document(doc! {})])] {
            sh.set_value(v.clone());
            assert!(sh.docs.is_empty() && sh.raw);
            assert_eq!(sh.result(), Some(v));
        }
    }

    #[test]
    fn user_comes_from_the_masked_address() {
        assert_eq!(user_of("mongodb://ana:****@h:27017/?tls=true"), "ana");
        assert_eq!(user_of("mysql://root@db:3306"), "root");
        assert_eq!(user_of("mongodb://localhost/?x=a@b"), "");
    }

    #[test]
    fn sql_result_hides_missing_id() {
        let mut sh = ShellTab::default();
        sh.set_value(Bson::Array(vec![Bson::Document(doc! { "a": 1 })]));
        sh.hide_absent_id();
        assert_eq!(sh.fields, ["a"]);
        sh.set_value(Bson::Array(vec![Bson::Document(doc! { "_id": 1, "a": 1 })]));
        sh.hide_absent_id();
        assert_eq!(sh.fields, ["_id", "a"]);
    }

    #[test]
    fn pipeline_builder_text() {
        let mut st = stages_from_text("[{ $match: { a: 1 } }, { $limit: 5 }, { $project: { a: 1 } }]").unwrap();
        assert_eq!(st.iter().map(|s| s.op.as_str()).collect::<Vec<_>>(), ["$match", "$limit", "$project"]);
        st[1].on = false;
        // Stages that are off are left out of the text and of every preview pipeline.
        let text = text_from_stages(&st);
        assert_eq!(db::parse_pipeline(&text).unwrap().len(), 2);
        assert_eq!(stage_pipeline(&st, 2).unwrap(), vec![doc! { "$match": { "a": 1 } }, doc! { "$project": { "a": 1 } }]);
        st[0].text = "{ a: ".into();
        assert!(stage_pipeline(&st, 2).unwrap_err().starts_with("Stage 1 ($match)"));
        assert!(stages_from_text("[{ $match: {}, $limit: 1 }]").is_err());
        assert_eq!(text_from_stages(&[]), "[]");
    }
}
