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
    /// Environment label shown next to the name, such as Production.
    #[serde(default)]
    pub(super) tag: String,
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
    pub(super) dbs: Vec<(String, Option<Vec<String>>)>,
    pub(super) masked_uri: String,
    /// Kept alive while connected through SSH; dropping it closes the tunnel.
    pub(super) tunnel: Option<crate::ssh::Tunnel>,
    /// Shown in the status bar and under the sidebar; filled after connecting.
    pub(super) server: ServerInfo,
}

/// The host part of a connection string: after the scheme and any credentials, before the path
/// or options (which may themselves hold an @).
pub(super) fn host_of(uri: &str) -> String {
    let rest = uri.split_once("://").map_or(uri, |(_, r)| r);
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    authority.rsplit('@').next().unwrap_or_default().to_string()
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

#[derive(Clone, PartialEq)]
pub(super) struct Sel {
    pub(super) conn: u64,
    pub(super) db: String,
    pub(super) coll: String,
}

impl Sel {
    /// "db.coll", how index stats are keyed.
    pub(super) fn key(&self) -> String {
        format!("{}.{}", self.db, self.coll)
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

/// Everything that belongs to one open tab.
pub(super) struct Tab {
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
    pub(super) selected: Option<(bool, usize)>,
    /// Rows of the current page checked for bulk actions, and the last one clicked for Shift ranges.
    pub(super) checked: BTreeSet<usize>,
    pub(super) check_anchor: Option<usize>,
    /// Checked rows of the Shell result's page, which has its own table and so its own checks.
    pub(super) shell_checked: BTreeSet<usize>,
    /// Rows open inline besides the selected one, keyed like `selected`.
    pub(super) expanded: BTreeSet<(bool, usize)>,
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
}

/// What a SQL tab keeps besides the shared row fields of `Tab`.
#[derive(Default)]
pub(super) struct SqlTab {
    /// Columns in table order, from the last page answer.
    pub(super) cols: Vec<db::sql::Col>,
    /// Primary key column names; empty means the rows cannot be edited.
    pub(super) pk: Vec<String>,
    /// The row count is the server's table estimate, shown with a ~.
    pub(super) estimated: bool,
    /// Text for the Query view, set by "Edit in Query" on checked rows.
    pub(super) query: String,
    /// Rows before grouping when the last answer was grouped; None for an ordinary answer.
    pub(super) matched: Option<u64>,
    /// The Content bar: rules, clause chips and their text.
    pub(super) bar: super::sql_bar::Bar,
    /// What Stop reaches of the running Custom Query.
    pub(super) run: db::sql::RunHandle,
    /// The outcome of each statement of the last run, the one shown, and the request still awaited (0 when idle).
    pub(super) sets: Vec<db::sql::ResultSet>,
    pub(super) set_sel: usize,
    pub(super) run_req: u64,
    /// Database the Query view runs in when it is not the tab's own.
    pub(super) run_db: String,
    /// Caret of the Query editor as a char index.
    pub(super) caret: usize,
    /// Results as text instead of a grid, and that text for the set it was built from.
    pub(super) text_mode: bool,
    pub(super) text_cache: Option<(usize, String)>,
    /// Checked rows of the shown page of the selected result, and the anchor of a Shift range. A page change
    /// or a new run clears them: checks belong to the page they were made on.
    pub(super) qchecked: BTreeSet<usize>,
    pub(super) qanchor: Option<usize>,
    /// The run the shown results belong to, which their row counts answer to, and the result tab being
    /// read at another page (its answer replaces only that tab).
    pub(super) sets_req: u64,
    pub(super) paging: Option<usize>,
    /// What the Structure, Relations, Triggers and Info views show.
    pub(super) st: super::sql_structure::StructState,
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
}

/// Which of `len` rows a page shows (all of them for None), and the number of the first one minus one.
pub(super) fn page_window(page: Option<usize>, len: usize) -> (std::ops::Range<usize>, u64) {
    let Some(page) = page else { return (0..len, 0) };
    let start = (page * PAGE_SIZE as usize).min(len);
    (start..(start + PAGE_SIZE as usize).min(len), start as u64)
}

impl Tab {
    /// "conn/db/coll", the key of per-collection settings.
    pub(super) fn coll_key(&self) -> String {
        format!("{}/{}/{}", self.sel.conn, self.sel.db, self.sel.coll)
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
            selected: None,
            checked: BTreeSet::new(),
            check_anchor: None,
            shell_checked: BTreeSet::new(),
            expanded: BTreeSet::new(),
            agg_txt: "[\n  { $match: {} },\n  { $limit: 20 }\n]".into(),
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
            shell: ShellTab::default(),
            schema: SchemaTab::default(),
            import: ImportTab::default(),
            compare: CompareTab { insert: true, replace: true, ..Default::default() },
            watch: WatchTab::default(),
            related: HashMap::new(),
            stats: None,
            stats_asked: false,
            sql: None,
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
    /// Rows of the open SQL table, by key.
    DeleteRows(Vec<Document>),
}

/// What to run once writes are allowed.
pub(super) type Retry = Box<dyn FnOnce(&mut App)>;

pub(super) enum Modal {
    Conn(Box<ConnForm>),
    Settings,
    /// The second field reruns what asked for writes once they are allowed.
    Unlock(u64, Option<Retry>),
    Save { tag: String, changes: Vec<(String, Option<Bson>, Option<Bson>)> },
    SaveAll(Vec<String>),
    Delete(Vec<Document>),
    SqlSet(Box<SetForm>),
    SqlRow(Box<RowForm>),
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
    /// `code` highlights the body as JSON.
    Text { title: String, body: String, code: bool },
    Rename { doc: Document, path: Vec<String>, name: String },
    /// `preview` keeps the stages it was made from; Apply only runs when they equal the current
    /// ones, so a late reply for older inputs cannot approve newer stages.
    UpdateMany { set_txt: String, unset_txt: String, preview: Option<(Vec<Document>, db::Res<(Vec<(Document, Document)>, u64)>)> },
    Tool(Box<ToolForm>),
    Relations { conn: u64, db: String },
    Palette { query: String, sel: usize },
    /// Export or copy-to-file of the rows of a result, in any of the Copy as formats.
    Export(Box<super::export::ExportForm>),
    /// Values of a field with how many documents hold each, and the pipeline that counted them.
    Grouped { sel: Sel, path: String, pipeline: Vec<Document>, rows: Vec<Document> },
    /// The column header's Customize window; `key` is the column prefs key it edits.
    ColumnCustomize { key: String, agg: bool, col: String, label: String, path: String },
}

/// A value being edited in place in the document tree.
pub(super) struct Inline {
    pub(super) path: Vec<String>,
    pub(super) text: String,
    pub(super) kind: &'static str,
    pub(super) focus: bool,
}

/// The large JSON editor for a whole document (empty path) or one object/array field.
pub(super) struct JsonEdit {
    pub(super) base: Document,
    pub(super) path: Vec<String>,
    pub(super) text: String,
    pub(super) kind: &'static str,
    pub(super) focus: bool,
}

/// One document's edits that are not written yet. `original` is what was loaded, so saving can
/// refuse to overwrite someone else's change.
#[derive(Clone)]
pub(super) struct Draft {
    pub(super) conn: u64,
    pub(super) db: String,
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
}

/// The new/edit connection form.
#[derive(Default)]
pub(super) struct ConnForm {
    pub(super) id: Option<u64>,
    pub(super) engine: db::sql::Engine,
    pub(super) addr: db::sql::Addr,
    pub(super) tab: ConnTab,
    pub(super) name: String,
    pub(super) tag: String,
    pub(super) uri: String,
    pub(super) show: bool,
    pub(super) ssh_on: bool,
    pub(super) ssh: crate::ssh::SshConf,
    pub(super) ssh_pwd: String,
    pub(super) ssh_pass: String,
    pub(super) proxy_on: bool,
    pub(super) proxy: db::Proxy,
    pub(super) proxy_pwd: String,
    pub(super) testing: bool,
}

impl ConnForm {
    pub(super) fn net(&self) -> db::Net {
        db::Net {
            ssh: self.ssh_on.then(|| (crate::ssh::SshConf { host: self.ssh.host.trim().into(), user: self.ssh.user.trim().into(), ..self.ssh.clone() }, crate::ssh::SshSecrets { password: Some(self.ssh_pwd.clone()), passphrase: Some(self.ssh_pass.clone()) })),
            // SQL drivers cannot use SOCKS, so a stale proxy setting must not apply.
            proxy: (self.proxy_on && !self.ssh_on && !self.engine.is_sql()).then(|| db::Proxy { host: self.proxy.host.trim().into(), password: Some(self.proxy_pwd.clone()), ..self.proxy.clone() }),
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
}

pub(super) enum QAct {
    Filter(String),
    Sort(String),
    Project(String),
    Load(SavedQuery),
    Save(String),
    Delete(String),
}

pub(super) enum TreeAct {
    Start(Vec<String>, Bson),
    Commit,
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
    }
}

impl App {
    pub(super) fn ok(&mut self, s: impl Into<String>) {
        self.status = (s.into(), false);
        self.status_at = std::time::Instant::now();
    }

    pub(super) fn err(&mut self, s: impl Into<String>) {
        self.status = (s.into(), true);
        self.status_at = std::time::Instant::now();
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
        self.t().is_some_and(|t| self.views.contains(&(t.sel.conn, t.sel.db.clone(), t.sel.coll.clone())))
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
            self.err("This connection is read-only. Unlock writes in the top bar first.");
            return None;
        }
        match self.target() {
            Some(t) => Some((conn, t)),
            None => {
                self.err("Not connected.");
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
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        let uri = match stored_uri(id) {
            Ok(u) => u,
            Err(e) => return self.err(uri_err(e)),
        };
        let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == id) else { return };
        c.status = Status::Connecting;
        c.client = None;
        c.sql = None;
        c.tunnel = None;
        // Asked again once connected, so a changed or failed server never shows the old one.
        c.server = ServerInfo::default();
        let net = net_of(&c.meta);
        if c.meta.engine.is_sql() {
            self.worker.sql_connect(id, uri, self.timeout(), net);
        } else {
            self.worker.connect(id, uri, self.timeout(), net);
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
