//! The state the views draw from: connections, tabs, modals, and the actions views hand back.

use super::*;

impl ShellTab {
    pub(super) fn set_value(&mut self, v: Bson) {
        self.docs = match &v {
            Bson::Array(a) if !a.is_empty() && a.iter().all(|x| matches!(x, Bson::Document(_))) => a.iter().filter_map(|x| x.as_document().cloned()).collect(),
            Bson::Document(d) => vec![d.clone()],
            _ => Vec::new(),
        };
        self.fields = auto_fields(&self.docs);
        self.count = match &v {
            Bson::Array(a) => Some(a.len()),
            _ => None,
        };
        self.page = 0;
        self.raw = self.docs.is_empty();
        self.raw_text = None;
        self.value = Some(v);
        self.error = None;
    }
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub(super) struct ConnMeta {
    pub(super) id: u64,
    pub(super) name: String,
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
    pub(super) status: Status,
    /// Writes are locked at every launch and only unlocked for this session.
    pub(super) writable: bool,
    pub(super) dbs: Vec<(String, Option<Vec<String>>)>,
    pub(super) masked_uri: String,
    /// Kept alive while connected through SSH; dropping it closes the tunnel.
    pub(super) tunnel: Option<crate::ssh::Tunnel>,
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
}

/// Everything that belongs to one open tab.
pub(super) struct Tab {
    pub(super) sel: Sel,
    pub(super) view: View,
    pub(super) filter_txt: String,
    pub(super) sort_txt: String,
    pub(super) proj_txt: String,
    pub(super) query: Query,
    pub(super) page: u64,
    pub(super) docs: Vec<Document>,
    pub(super) fields: Vec<String>,
    /// Every dotted field path seen in the loaded documents, for autocomplete.
    pub(super) paths: Vec<String>,
    /// Main kind and frequent values of each path in the loaded documents, for Query suggestions.
    pub(super) field_info: crate::ui::complete::Fields,
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
    /// Rows open inline besides the selected one, keyed like `selected`.
    pub(super) expanded: BTreeSet<(bool, usize)>,
    pub(super) agg_txt: String,
    pub(super) agg_docs: Vec<Document>,
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
}

impl Default for SchemaTab {
    fn default() -> Self {
        Self { req: 0, limit: SCHEMA_DEFAULT_LIMIT, running: false, stats: Vec::new(), docs: 0, ms: None, err: None, vtext: String::new(), vlevel: 2, vwarn: false, vreq: 0, vloaded: false }
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
    /// The returned value; documents also go to `docs` for the table.
    pub(super) value: Option<Bson>,
    pub(super) docs: Vec<Document>,
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
}

/// Which of `len` rows a page shows (all of them for None), and the number of the first one minus one.
pub(super) fn page_window(page: Option<usize>, len: usize) -> (std::ops::Range<usize>, u64) {
    let Some(page) = page else { return (0..len, 0) };
    let start = (page * PAGE_SIZE as usize).min(len);
    (start..(start + PAGE_SIZE as usize).min(len), start as u64)
}

impl Tab {
    /// The result table that is not the Documents one, as this view keeps it: all its rows, their columns,
    /// the page on screen (None shows every row) and the suffix of its column-settings key.
    pub(super) fn agg_source(&self) -> (&[Document], &[String], Option<usize>, &'static str) {
        match self.view {
            View::Shell => (&self.shell.docs, &self.shell.fields, Some(self.shell.page), "shell"),
            // Import previews its first page.
            View::Import => match &self.import.parsed {
                Some(Ok(d)) => (d, &self.import.fields, Some(0), "import"),
                _ => (&[], &[], Some(0), "import"),
            },
            // Field rows and document rows have different columns.
            View::Compare => (self.compare.rows(), &self.compare.fields[self.compare.list as usize], Some(self.compare.page), if self.compare.list == CmpList::Changed { "compare-diff" } else { "compare" }),
            View::Indexes => (&self.idx_rows, &IDX_FIELDS, None, "indexes"),
            View::Watch => (&self.watch.rows, &WATCH_FIELDS, None, "watch"),
            _ => (&self.agg_docs, &self.agg_fields, None, "agg"),
        }
    }

    /// Where `view` keeps those rows, so the table can take them out while it draws and put them back.
    pub(super) fn agg_store(&mut self, view: View) -> Option<&mut Vec<Document>> {
        match view {
            View::Shell => Some(&mut self.shell.docs),
            View::Import => self.import.parsed.as_mut()?.as_mut().ok(),
            View::Compare => self.compare.rows_mut(),
            View::Indexes => Some(&mut self.idx_rows),
            View::Watch => Some(&mut self.watch.rows),
            _ => Some(&mut self.agg_docs),
        }
    }

    /// The rows on screen in that table, their columns, and the number of the first one minus one.
    pub(super) fn agg_page(&self) -> (&[Document], &[String], u64) {
        let (docs, fields, page, _) = self.agg_source();
        let (range, base) = page_window(page, docs.len());
        (&docs[range], fields, base)
    }

    /// Whether the rows on screen are documents of this collection, so actions on the collection apply to them.
    pub(super) fn in_coll(&self) -> bool {
        !matches!(self.view, View::Compare | View::Import | View::Indexes | View::Watch)
    }

    pub(super) fn new(sel: Sel) -> Self {
        Self {
            sel,
            view: View::Documents,
            filter_txt: String::new(),
            sort_txt: String::new(),
            proj_txt: String::new(),
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
            idx_rows: Vec::new(),
            shell: ShellTab::default(),
            schema: SchemaTab::default(),
            import: ImportTab::default(),
            compare: CompareTab { insert: true, replace: true, ..Default::default() },
            watch: WatchTab::default(),
            related: HashMap::new(),
        }
    }
}

pub(super) enum Danger {
    DeleteMatching(Document),
    DropCollection,
    DropIndex(String),
    RemoveConnection(u64),
    DropDatabase { conn: u64, db: String },
    DropCollAt { conn: u64, db: String, coll: String },
    DropUser { conn: u64, db: String, user: String },
    RenameMany { filter: Document, from: String, to: String },
    UpdateMany { filter: Document, stages: Vec<Document> },
    Upsert { key: String },
    Sync { conn: u64 },
    Restore { conn: u64, args: Vec<String> },
    Validator(Document),
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
    Typed { title: String, expect: String, typed: String, action: Danger },
    CreateIndex(Box<IndexForm>),
    AggWrite(Vec<Document>),
    ShellWrite(Vec<String>),
    ImportInsert(usize),
    NewCollection { conn: u64, db: String, name: String, view: bool, source: String, pipeline: String },
    RenameColl { conn: u64, db: String, coll: String, name: String },
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

/// The new/edit connection form.
#[derive(Default)]
pub(super) struct ConnForm {
    pub(super) id: Option<u64>,
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
            proxy: (self.proxy_on && !self.ssh_on).then(|| db::Proxy { host: self.proxy.host.trim().into(), password: Some(self.proxy_pwd.clone()), ..self.proxy.clone() }),
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
    DropColl(String, String),
    Dump(String, String),
    Restore(String),
    Compare(String, String),
    Relations(String),
    /// Analyze relations of one collection, or of every collection when empty.
    Analyze(String, String),
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
        c.tunnel = None;
        let net = net_of(&c.meta);
        self.worker.connect(id, uri, self.timeout(), net);
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
