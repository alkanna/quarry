//! Everything that talks to MongoDB or converts its data. The UI never blocks: each call is a
//! tokio task that sends one `Msg` back and asks for a repaint.

use futures_util::{StreamExt, TryStreamExt};
use mongodb::bson::{Bson, Document, doc};
use mongodb::options::{ClientOptions, IndexOptions};
use mongodb::{Client, IndexModel};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

pub const PAGE_SIZE: u64 = 50; // paginationLimit in the original
pub const DEFAULT_TIMEOUT_MS: u64 = 5000; // Settings.connectionTimeout in the original
pub const UNDO_MAX: usize = 50; // maxActionsLength
pub const UNDO_MAX_AGE_S: u64 = 86_400; // maxActionsAge

pub type Res<T> = Result<T, String>;

#[derive(Clone)]
pub struct Target {
    pub client: Client,
    pub db: String,
    pub coll: String,
    pub timeout: Duration,
}

impl Target {
    fn c(&self) -> mongodb::Collection<Document> {
        self.client.database(&self.db).collection(&self.coll)
    }
}

#[derive(Clone, Default)]
pub struct Query {
    pub filter: Document,
    pub sort: Option<Document>,
    pub projection: Option<Document>,
    /// From a pasted shell query such as `.skip(10).limit(5)`; paging happens inside this window.
    pub skip: u64,
    pub limit: Option<u64>,
}

pub enum UndoKind {
    Insert { id: Bson },
    /// An import: undo deletes these ids.
    InsertMany { ids: Vec<Bson> },
    Update { old: Document, new: Document },
    Delete { old: Document },
}

pub struct UndoAction {
    pub at: Instant,
    pub conn: u64,
    pub db: String,
    pub coll: String,
    pub kind: UndoKind,
}

pub enum ExplainOf {
    Find(Query),
    Aggregate(Vec<Document>),
}

pub enum Msg {
    /// Connected (or not); a tunnel, if any, must be kept alive as long as the client is used.
    Connected { conn: u64, res: Res<(Client, Vec<String>, Option<crate::ssh::Tunnel>)> },
    /// Result of the connection form's Test button.
    Tested { res: Res<String> },
    /// Collection names (sorted) and which of them are views.
    Collections { conn: u64, db: String, res: Res<(Vec<String>, Vec<String>)> },
    /// A read command answered (users, roles, stats, view definition); `tag` says which.
    Command { tag: String, res: Res<Document> },
    /// A database-level change finished; the collection list of `db` is reloaded.
    DbChanged { conn: u64, db: String, res: Res<String> },
    Docs { req: u64, res: Res<(Vec<Document>, u64, u128)> },
    Indexes { req: u64, res: Res<Vec<Document>> },
    /// Usage and suggestions for the collection named `key` ("db.coll").
    IndexInfo { key: String, res: Res<IndexInfo> },
    Aggregated { req: u64, res: Res<(Vec<Document>, u128)> },
    /// The server's explain output for a query or pipeline.
    Explained { req: u64, res: Res<Document> },
    /// Up to 5 matching documents as (before, after) the given update stages, and the total match count.
    UpdatePreview { stages: Vec<Document>, res: Res<(Vec<(Document, Document)>, u64)> },
    /// A write finished. `undo` is filled when the write can be reverted. `retry` is the insert
    /// dialog's (db, coll, text), to reopen it if the server refused the document.
    Written { res: Res<String>, undo: Option<UndoAction>, reload: bool, retry: Option<(String, String, String)> },
    /// A staged document edit was written (or refused); `tag` names the draft.
    Saved { tag: String, res: Res<String>, undo: Option<UndoAction> },
    Info(Res<String>),
    /// Schema analysis of `docs` documents.
    Schema { req: u64, res: Res<(Vec<FieldStat>, usize, u128)> },
    /// A line the Shell script printed (console.log/warn/error), or a progress note.
    ShellLog { req: u64, level: String, text: String },
    /// The Shell script finished: its return value as canonical Extended JSON `{"v": ...}`.
    ShellDone { req: u64, res: Res<String>, ms: u128 },
    /// A result to show in a dialog, such as a distinct-values count.
    Notice { title: String, body: String },
    /// Preview values of referenced documents, keyed by `compact(_id)`.
    Related { req: u64, res: Res<Vec<(String, String)>> },
    /// Which collection of `db` holds `value` as an `_id`, and the field to preview it with.
    RefFound { conn: u64, db: String, coll: String, path: String, value: Bson, res: Res<Option<(String, String)>> },
    Compared { req: u64, res: Res<Compared> },
    /// Relations found by analysis: (collection, path, referenced collection, preview field).
    Analyzed { conn: u64, db: String, res: Res<Vec<(String, String, String, String)>> },
    /// A line printed by mongodump or mongorestore.
    ToolLog(String),
    ToolDone(Res<String>),
    /// Update check: Some(version) once a newer version is installed.
    Update(Res<Option<String>>),
    /// The MongoDB Database Tools were installed or updated to this version; None when already current.
    ToolsInstalled(Res<Option<String>>),
}

/// Two collections compared by `_id`.
#[derive(Default)]
pub struct Compared {
    pub only_src: Vec<Document>,
    pub only_dst: Vec<Document>,
    /// (source, target) pairs with the same `_id` and different content.
    pub changed: Vec<(Document, Document)>,
    pub same: usize,
}

/// The original skips collections above this many documents (maxDocuments, renderer line 132512).
pub const COMPARE_MAX: u64 = 1_000_000;
/// Ids are copied and deleted in chunks of this size (chunkSize, renderer line 132513).
pub const SYNC_CHUNK: usize = 1000;

/// Field order is ignored, as the original compares document hashes of sorted fields.
pub fn compare_docs(src: Vec<Document>, dst: Vec<Document>) -> Compared {
    let mut other: std::collections::HashMap<String, Document> = dst.into_iter().map(|d| (compact(d.get("_id").unwrap_or(&Bson::Null)), d)).collect();
    let mut r = Compared::default();
    for d in src {
        match other.remove(&compact(d.get("_id").unwrap_or(&Bson::Null))) {
            None => r.only_src.push(d),
            Some(o) if o == d => r.same += 1,
            Some(o) => r.changed.push((d, o)),
        }
    }
    r.only_dst = other.into_values().collect();
    r.only_dst.sort_by_key(|d| compact(d.get("_id").unwrap_or(&Bson::Null)));
    r
}

/// First top-level text field that is not itself an id, like the original's getFirstStringField
/// (renderer line 105602); `_id` when there is none.
pub fn preview_field(d: &Document) -> String {
    let id_like = |s: &str| s.len() == 24 && s.chars().all(|c| c.is_ascii_hexdigit());
    d.iter().find(|(k, v)| *k != "_id" && matches!(v, Bson::String(s) if !id_like(s))).map(|(k, _)| k.clone()).unwrap_or_else(|| "_id".into())
}

/// Every value at `path`, looking inside arrays at any level, for looking up referenced documents.
pub fn values_at(docs: &[Document], path: &str) -> Vec<Bson> {
    fn walk(v: &Bson, segs: &[&str], out: &mut Vec<Bson>) {
        match (v, segs.split_first()) {
            (Bson::Array(a), _) => a.iter().for_each(|x| walk(x, segs, out)),
            (Bson::Document(d), Some((k, rest))) => {
                if let Some(x) = d.get(*k) {
                    walk(x, rest, out);
                }
            }
            (v, None) => out.push(v.clone()),
            _ => {}
        }
    }
    let segs: Vec<&str> = path.split('.').collect();
    let mut out = Vec::new();
    for d in docs {
        walk(&Bson::Document(d.clone()), &segs, &mut out);
    }
    out.sort_by_key(compact);
    out.dedup();
    out
}

/// Fields that look like references, with sample ids (the original's _analyzeDocument, renderer
/// line 105562): ObjectIds, or alphanumeric strings as long as this collection's own string ids.
/// Arrays are looked into through their first element; objects with 100 fields or more are skipped.
pub fn proto_relations(docs: &[Document], id_len: usize) -> Vec<(String, Vec<Bson>)> {
    fn is_key(v: &Bson, id_len: usize) -> bool {
        match v {
            Bson::ObjectId(_) => true,
            Bson::String(s) => id_len > 0 && s.len() == id_len && s.chars().all(|c| c.is_ascii_alphanumeric()),
            _ => false,
        }
    }
    fn walk(d: &Document, prefix: &str, id_len: usize, out: &mut Vec<(String, Vec<Bson>)>) {
        for (k, v) in d {
            let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
            match v {
                Bson::Array(a) => match a.first() {
                    Some(f) if is_key(f, id_len) => out.push((path, a.clone())),
                    Some(Bson::Document(x)) => walk(x, &path, id_len, out),
                    _ => {}
                },
                v if is_key(v, id_len) => out.push((path, vec![v.clone()])),
                Bson::Document(x) if x.len() < 100 => walk(x, &path, id_len, out),
                _ => {}
            }
        }
    }
    let mut out: Vec<(String, Vec<Bson>)> = Vec::new();
    for d in docs {
        let mut d = d.clone();
        d.remove("_id");
        let mut found = Vec::new();
        walk(&d, "", id_len, &mut found);
        for (p, ids) in found {
            if !out.iter().any(|(q, _)| *q == p) {
                out.push((p, ids));
            }
        }
    }
    out
}

/// The address without a database path: mongorestore treats one as an implicit --db and then
/// ignores --nsInclude (getMongorestoreCommand, renderer line 135880).
pub fn uri_without_db(uri: &str) -> String {
    let Some(s) = uri.find("://").map(|i| i + 3) else { return uri.into() };
    let Some(slash) = uri[s..].find('/').map(|i| s + i) else { return uri.into() };
    let q = uri[slash..].find('?').map_or(uri.len(), |i| slash + i);
    format!("{}/{}", &uri[..slash], &uri[q..])
}

/// mongodump arguments as the original builds them (getMongodumpCommand, renderer line 135838).
/// They are passed straight to the program, so no shell quoting is needed.
pub fn dump_args(uri: &str, db: &str, coll: Option<&str>, archive: &str, gzip: bool) -> Vec<String> {
    let mut a = vec![format!("--uri={uri}"), format!("--archive={archive}"), format!("--db={db}")];
    if let Some(c) = coll {
        a.push(format!("--collection={c}"));
    }
    if gzip {
        a.push("--gzip".into());
    }
    a.push("--forceTableScan".into());
    a
}

/// mongorestore arguments (getMongorestoreCommand, renderer line 135878): only `from_db` (or one
/// collection of it) is restored, renamed into `to_db` when the names differ.
pub fn restore_args(uri: &str, archive: &str, from_db: &str, coll: Option<&str>, to_db: &str, drop: bool, gzip: bool) -> Vec<String> {
    let ns = coll.map_or("*".to_string(), str::to_string);
    let mut a = vec![format!("--uri={}", uri_without_db(uri)), format!("--archive={archive}"), format!("--nsInclude={from_db}.{ns}")];
    if from_db != to_db {
        a.push(format!("--nsFrom={from_db}.{ns}"));
        a.push(format!("--nsTo={to_db}.{ns}"));
    }
    if drop {
        a.push("--drop".into());
    }
    if gzip {
        a.push("--gzip".into());
    }
    a
}

pub struct Worker {
    rt: tokio::runtime::Runtime,
    tx: Sender<Msg>,
    ctx: eframe::egui::Context,
}

fn e(err: impl std::fmt::Display) -> String {
    let s = err.to_string();
    // Driver errors append their labels and the raw reply bytes, which only hide the message.
    let s = match s.strip_prefix("Kind: ") {
        Some(r) => r.split(", labels: ").next().unwrap_or(r).to_string(),
        None => s,
    };
    if s.contains("imeout") || s.contains("exceeded time limit") {
        // Same advice the original gives on timeouts.
        format!("{s}\nThis usually means a query on a non-indexed field of a big collection. Raise the timeout or add an index.")
    } else {
        s
    }
}

impl Worker {
    pub fn new(tx: Sender<Msg>, ctx: eframe::egui::Context) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime");
        Self { rt, tx, ctx }
    }

    /// Runs a Shell script in a separate Bun process; `cancel` stops it.
    pub fn shell(&self, req: u64, job: ShellJob, cancel: tokio::sync::oneshot::Receiver<()>) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let send = move |m: Msg| {
                let _ = tx.send(m);
                ctx.request_repaint();
            };
            let start = Instant::now();
            let res = run_shell(req, &job, &send, cancel).await;
            send(Msg::ShellDone { req, res, ms: start.elapsed().as_millis() });
        });
    }

    /// Runs blocking work (such as the update check) off the UI thread.
    pub fn blocking(&self, f: impl FnOnce() -> Msg + Send + 'static) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.rt.spawn_blocking(move || {
            let _ = tx.send(f());
            ctx.request_repaint();
        });
    }

    /// Delivers a message through the same channel real results use (test fixtures only).
    pub fn send(&self, m: Msg) {
        self.spawn(async move { m });
    }

    fn spawn(&self, f: impl Future<Output = Msg> + Send + 'static) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        self.rt.spawn(async move {
            // A panic in the task would otherwise end it silently; say so instead.
            let msg = tokio::spawn(f).await.unwrap_or_else(|err| Msg::Info(Err(format!("Internal error: {err}"))));
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    pub fn connect(&self, conn: u64, uri: String, timeout: Duration, net: Net) {
        self.spawn(async move {
            let res = open_client(conn, &uri, timeout, &net).await;
            Msg::Connected { conn, res }
        });
    }

    /// Connects and lists databases without keeping anything, for the connection form.
    pub fn test(&self, uri: String, timeout: Duration, net: Net) {
        self.spawn(async move {
            let res = open_client(0, &uri, timeout, &net).await.map(|(_, dbs, t)| {
                let via = match (&t, &net.proxy) {
                    (Some(t), _) => format!(" through SSH (host key {})", t.fingerprint),
                    (None, Some(p)) => format!(" through the proxy {}:{}", p.host, p.port),
                    _ => String::new(),
                };
                format!("Connected{via}: {} databases.", dbs.len())
            });
            Msg::Tested { res }
        });
    }

    pub fn collections(&self, conn: u64, client: Client, db: String) {
        self.spawn(async move {
            let res = async {
                let specs: Vec<mongodb::results::CollectionSpecification> = client.database(&db).list_collections().await.map_err(e)?.try_collect().await.map_err(e)?;
                let mut names: Vec<String> = specs.iter().map(|s| s.name.clone()).collect();
                names.sort();
                let views = specs.iter().filter(|s| s.collection_type == mongodb::results::CollectionType::View).map(|s| s.name.clone()).collect();
                Ok((names, views))
            }
            .await;
            Msg::Collections { conn, db, res }
        });
    }

    /// Runs a read command and returns its answer under `tag`.
    pub fn command(&self, client: Client, db: String, cmd: Document, tag: &str) {
        let tag = tag.to_string();
        self.spawn(async move {
            let res = client.database(&db).run_command(cmd).await.map_err(e);
            Msg::Command { tag, res }
        });
    }

    /// Runs a command that changes a database (create, rename, drop, users) and reports `done`.
    pub fn db_change(&self, conn: u64, client: Client, db: String, cmd: Document, done: String) {
        self.spawn(async move {
            // renameCollection only runs on the admin database.
            let on = if cmd.contains_key("renameCollection") { "admin".to_string() } else { db.clone() };
            let res = client.database(&on).run_command(cmd).await.map(|_| done).map_err(e);
            Msg::DbChanged { conn, db, res }
        });
    }

    /// Size and count of one collection, from $collStats (the collStats command is deprecated).
    pub fn coll_stats(&self, client: Client, db: String, coll: String) {
        self.spawn(async move {
            let res = async {
                let c = client.database(&db).collection::<Document>(&coll);
                let mut cur = c.aggregate(vec![doc! { "$collStats": { "storageStats": {} } }]).await.map_err(e)?;
                let d: Document = cur.try_next().await.map_err(e)?.ok_or("No statistics returned")?;
                let s = d.get_document("storageStats").cloned().unwrap_or_default();
                let mut out = doc! { "collection": format!("{db}.{coll}") };
                for k in ["count", "size", "avgObjSize", "storageSize", "nindexes", "totalIndexSize", "capped"] {
                    if let Some(v) = s.get(k) {
                        out.insert(k, v.clone());
                    }
                }
                if let Ok(sizes) = s.get_document("indexSizes") {
                    out.insert("indexSizes", sizes.clone());
                }
                Ok(out)
            }
            .await;
            Msg::Command { tag: "stats".into(), res }
        });
    }

    pub fn find(&self, req: u64, t: Target, q: Query, page: u64) {
        self.spawn(async move {
            let res = async {
                let start = Instant::now();
                let c = t.c();
                // An empty filter on a huge collection is the common case; the estimate is instant.
                let count = if q.filter.is_empty() {
                    c.estimated_document_count().max_time(t.timeout).await
                } else {
                    c.count_documents(q.filter.clone()).max_time(t.timeout).await
                }
                .map_err(e)?;
                let count = q.limit.map_or(count.saturating_sub(q.skip), |l| count.saturating_sub(q.skip).min(l));
                let take = count.saturating_sub(page * PAGE_SIZE).min(PAGE_SIZE).max(1);
                let mut f = c.find(q.filter).skip(q.skip + page * PAGE_SIZE).limit(take as i64).max_time(t.timeout);
                if let Some(s) = q.sort {
                    f = f.sort(s);
                }
                if let Some(p) = q.projection {
                    f = f.projection(p);
                }
                let docs: Vec<Document> = f.await.map_err(e)?.try_collect().await.map_err(e)?;
                Ok((docs, count, start.elapsed().as_millis()))
            }
            .await;
            Msg::Docs { req, res }
        });
    }

    pub fn indexes(&self, req: u64, t: Target) {
        self.spawn(async move {
            let res = async {
                let v: Vec<IndexModel> = t.c().list_indexes().await.map_err(e)?.try_collect().await.map_err(e)?;
                Ok(v.into_iter()
                    .map(|m| {
                        let o = m.options.unwrap_or_default();
                        let mut d = doc! { "name": o.name.unwrap_or_default(), "keys": m.keys };
                        if o.unique == Some(true) {
                            d.insert("unique", true);
                        }
                        if let Some(ttl) = o.expire_after {
                            d.insert("expireAfterSeconds", ttl.as_secs() as i64);
                        }
                        if let Some(pf) = o.partial_filter_expression {
                            d.insert("partialFilterExpression", pf);
                        }
                        d
                    })
                    .collect())
            }
            .await;
            Msg::Indexes { req, res }
        });
    }

    /// Usage, size and hidden state per index, plus the profiler's unindexed or slow queries.
    pub fn index_info(&self, t: Target) {
        self.spawn(async move {
            let key = format!("{}.{}", t.db, t.coll);
            let res = async {
                let db = t.client.database(&t.db);
                let num = |b: Option<&Bson>| match b {
                    Some(Bson::Int32(n)) => *n as i64,
                    Some(Bson::Int64(n)) => *n,
                    Some(Bson::Double(n)) => *n as i64,
                    _ => 0,
                };
                // Sizes are best effort: views and some deployments refuse $collStats.
                let sizes = async {
                    let mut cur = t.c().aggregate(vec![doc! { "$collStats": { "storageStats": {} } }]).await.ok()?;
                    let d = cur.try_next().await.ok()??;
                    d.get_document("storageStats").ok()?.get_document("indexSizes").ok().cloned()
                }
                .await
                .unwrap_or_default();
                let rows: Vec<Document> = t.c().aggregate(vec![doc! { "$indexStats": {} }]).await.map_err(e)?.try_collect().await.map_err(e)?;
                let stats = rows
                    .iter()
                    .map(|d| {
                        let name = d.get_str("name").unwrap_or_default().to_string();
                        let acc = d.get_document("accesses").cloned().unwrap_or_default();
                        IndexStat {
                            ops: num(acc.get("ops")),
                            since: acc.get_datetime("since").ok().map(|t| t.timestamp_millis()),
                            hidden: d.get_document("spec").is_ok_and(|s| s.get_bool("hidden") == Ok(true)),
                            size: sizes.get(&name).map(|b| num(Some(b)) as usize),
                            name,
                        }
                    })
                    .collect();
                let prof = db.run_command(doc! { "profile": -1 }).await.ok();
                let level = prof.as_ref().map_or(0, |p| num(p.get("was")) as i32);
                let slowms = prof.as_ref().map_or(100, |p| num(p.get("slowms")));
                // Newest first so the cap keeps recent traffic.
                let entries: Vec<Document> = match db
                    .collection::<Document>("system.profile")
                    .aggregate(vec![
                        doc! { "$match": { "ns": &key, "op": "query", "command.find": { "$exists": true }, "$or": [{ "planSummary": { "$regex": "COLLSCAN" } }, { "millis": { "$gt": slowms } }] } },
                        doc! { "$sort": { "ts": -1 } },
                        doc! { "$limit": 1000 },
                        doc! { "$project": { "filter": "$command.filter", "sort": "$command.sort", "millis": 1 } },
                    ])
                    .await
                {
                    Ok(c) => c.try_collect().await.unwrap_or_default(),
                    Err(_) => Vec::new(),
                };
                Ok(IndexInfo { stats, level, slowms, shapes: group_profile(&entries) })
            }
            .await;
            Msg::IndexInfo { key, res }
        });
    }

    pub fn aggregate(&self, req: u64, t: Target, pipeline: Vec<Document>) {
        self.spawn(async move {
            let res = async {
                let start = Instant::now();
                let cur = t.c().aggregate(pipeline).max_time(t.timeout).allow_disk_use(true).await.map_err(e)?;
                // ponytail: results capped at 1000 in memory; stream to a file via export for more.
                let docs: Vec<Document> = cur.take(1000).try_collect().await.map_err(e)?;
                Ok((docs, start.elapsed().as_millis()))
            }
            .await;
            Msg::Aggregated { req, res }
        });
    }

    pub fn explain(&self, req: u64, t: Target, what: ExplainOf) {
        self.spawn(async move {
            let inner = match what {
                ExplainOf::Find(q) => {
                    let mut d = doc! { "find": &t.coll, "filter": q.filter, "limit": q.limit.map_or(PAGE_SIZE, |l| l.min(PAGE_SIZE)) as i64 };
                    if q.skip > 0 {
                        d.insert("skip", q.skip as i64);
                    }
                    if let Some(s) = q.sort {
                        d.insert("sort", s);
                    }
                    if let Some(p) = q.projection {
                        d.insert("projection", p);
                    }
                    d
                }
                ExplainOf::Aggregate(p) => doc! { "aggregate": &t.coll, "pipeline": p, "cursor": {} },
            };
            let cmd = doc! { "explain": inner, "verbosity": "executionStats", "maxTimeMS": t.timeout.as_millis() as i64 };
            let res = t.client.database(&t.db).run_command(cmd).await.map_err(e);
            Msg::Explained { req, res }
        });
    }

    fn write(&self, f: impl Future<Output = (Res<String>, Option<UndoAction>)> + Send + 'static) {
        self.spawn(async move {
            let (res, undo) = f.await;
            Msg::Written { res, undo, reload: true, retry: None }
        });
    }

    pub fn insert(&self, conn: u64, t: Target, d: Document, text: Option<String>) {
        self.spawn(async move {
            let retry = text.map(|x| (t.db.clone(), t.coll.clone(), x));
            let (res, undo) = match t.c().insert_one(d).await {
                Ok(r) => (
                    Ok(format!("Inserted document {}", short(&r.inserted_id))),
                    Some(UndoAction { at: Instant::now(), conn, db: t.db, coll: t.coll, kind: UndoKind::Insert { id: r.inserted_id } }),
                ),
                Err(err) => (Err(e(err)), None),
            };
            Msg::Written { res, undo, reload: true, retry }
        });
    }

    /// Replaces `old` with `new`, but only if the server still holds exactly `old`.
    pub fn replace(&self, conn: u64, t: Target, old: Document, new: Document, record_undo: bool) {
        self.write(replace_doc(conn, t, old, new, record_undo));
    }

    /// Writes one document's staged edits; the answer names the draft so it can be kept on failure.
    pub fn save(&self, tag: String, conn: u64, t: Target, old: Document, new: Document) {
        self.spawn(async move {
            let (res, undo) = replace_doc(conn, t, old, new, true).await;
            Msg::Saved { tag, res, undo }
        });
    }

    pub fn delete(&self, conn: u64, t: Target, old: Document, record_undo: bool) {
        self.write(async move {
            match t.c().delete_one(unchanged(&old)).await {
                Ok(r) if r.deleted_count == 0 => (Err(CONFLICT.into()), None),
                Ok(_) => (
                    Ok("Deleted 1 document".into()),
                    record_undo.then(|| UndoAction { at: Instant::now(), conn, db: t.db, coll: t.coll, kind: UndoKind::Delete { old } }),
                ),
                Err(err) => (Err(e(err)), None),
            }
        });
    }

    pub fn delete_by_id(&self, t: Target, id: Bson) {
        self.write(async move {
            match t.c().delete_one(doc! { "_id": id }).await {
                Ok(r) => (Ok(format!("Undo removed {} document", r.deleted_count)), None),
                Err(err) => (Err(e(err)), None),
            }
        });
    }

    /// Preview values for the referenced ids shown in the table (relatedDocuments, renderer line 107207).
    pub fn related(&self, req: u64, t: Target, ids: Vec<Bson>, preview: String) {
        self.spawn(async move {
            let res = async {
                let c = t.c();
                let f = c.find(doc! { "_id": { "$in": ids } }).projection(doc! { &preview: 1 }).max_time(t.timeout);
                let docs: Vec<Document> = f.await.map_err(e)?.try_collect().await.map_err(e)?;
                Ok(docs.iter().filter_map(|d| Some((compact(d.get("_id")?), value_text(get_path(d, &preview)?)))).collect())
            }
            .await;
            Msg::Related { req, res }
        });
    }

    /// Tries each collection of the database for a document with this `_id` (findReference,
    /// renderer line 105588). `t.coll` is the collection the id was found in.
    pub fn find_ref(&self, conn: u64, t: Target, colls: Vec<String>, path: String, value: Bson) {
        self.spawn(async move {
            let res = async {
                let base = t.client.database(&t.db);
                for c in colls.iter().filter(|c| !c.starts_with("system.")) {
                    if let Some(d) = base.collection::<Document>(c).find_one(doc! { "_id": value.clone() }).max_time(t.timeout).await.map_err(e)? {
                        return Ok(Some((c.clone(), preview_field(&d))));
                    }
                }
                Ok(None)
            }
            .await;
            Msg::RefFound { conn, db: t.db, coll: t.coll, path, value, res }
        });
    }

    /// Finds relations of `colls` by sampling their 10 newest documents and looking their id-like
    /// values up in every collection of the database (analyzeDataRelations, renderer line 105620).
    pub fn analyze_relations(&self, conn: u64, t: Target, colls: Vec<String>, all: Vec<String>) {
        self.spawn(async move {
            let res = async {
                let base = t.client.database(&t.db);
                let mut found = Vec::new();
                for coll in &colls {
                    let docs: Vec<Document> = base.collection::<Document>(coll).find(doc! {}).sort(doc! { "$natural": -1 }).limit(10).max_time(t.timeout).await.map_err(e)?.try_collect().await.map_err(e)?;
                    // String ids count only when this collection itself uses string ids of that length.
                    let id_len = match docs.first().and_then(|d| d.get("_id")) {
                        Some(Bson::String(s)) => s.len(),
                        _ => 0,
                    };
                    for (path, ids) in proto_relations(&docs, id_len) {
                        for target in &all {
                            let hit = base.collection::<Document>(target).find_one(doc! { "_id": { "$in": ids.clone() } }).max_time(t.timeout).await;
                            if let Ok(Some(d)) = hit {
                                found.push((coll.clone(), path.clone(), target.clone(), preview_field(&d)));
                                break;
                            }
                        }
                    }
                }
                Ok(found)
            }
            .await;
            Msg::Analyzed { conn, db: t.db, res }
        });
    }

    /// Loads both collections whole and compares them by `_id`.
    // ponytail: full documents are fetched; hash them server-side if collections near COMPARE_MAX get slow
    pub fn compare(&self, req: u64, src: Target, dst: Target) {
        self.spawn(async move {
            let res = async {
                let mut sides = Vec::new();
                for t in [&src, &dst] {
                    let n = t.c().estimated_document_count().await.map_err(e)?;
                    if n > COMPARE_MAX {
                        return Err(format!("{}.{} has {n} documents; comparing is limited to {COMPARE_MAX}.", t.db, t.coll));
                    }
                    let docs: Vec<Document> = t.c().find(doc! {}).await.map_err(e)?.try_collect().await.map_err(e)?;
                    sides.push(docs);
                }
                let dst_docs = sides.pop().unwrap_or_default();
                Ok(compare_docs(sides.pop().unwrap_or_default(), dst_docs))
            }
            .await;
            Msg::Compared { req, res }
        });
    }

    /// Writes chosen differences into the target. A changed document is only replaced while it is
    /// still as compared, so edits made meanwhile are kept.
    pub fn sync(&self, t: Target, insert: Vec<Document>, replace: Vec<(Document, Document)>, delete: Vec<Bson>) {
        self.write(async move {
            let r = async {
                let c = t.c();
                for ch in insert.chunks(SYNC_CHUNK) {
                    c.insert_many(ch.to_vec()).await.map_err(e)?;
                }
                let mut skipped = 0;
                for (new, old) in &replace {
                    if c.replace_one(unchanged(old), new.clone()).await.map_err(e)?.matched_count == 0 {
                        skipped += 1;
                    }
                }
                let mut deleted = 0;
                for ch in delete.chunks(SYNC_CHUNK) {
                    deleted += c.delete_many(doc! { "_id": { "$in": ch.to_vec() } }).await.map_err(e)?.deleted_count;
                }
                let mut msg = format!("Sync done: {} inserted, {} replaced, {deleted} deleted", insert.len(), replace.len() - skipped);
                if skipped > 0 {
                    msg += &format!(", {skipped} skipped because they changed since the comparison");
                }
                Ok(msg)
            }
            .await;
            (r, None)
        });
    }

    /// Runs mongodump or mongorestore, streaming its output; `cancel` stops it.
    pub fn tool(&self, exe: std::path::PathBuf, args: Vec<String>, cancel: tokio::sync::oneshot::Receiver<()>) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let send = move |m: Msg| {
                let _ = tx.send(m);
                ctx.request_repaint();
            };
            let name = exe.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let run = async {
                use tokio::io::AsyncBufReadExt;
                let mut cmd = tokio::process::Command::new(&exe);
                cmd.args(&args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
                #[cfg(windows)]
                cmd.creation_flags(crate::update::CREATE_NO_WINDOW);
                let mut child = cmd
                    .spawn()
                    .map_err(|err| format!("Could not start {} ({err})", exe.display()))?;
                // Both tools write their progress to stderr.
                let mut err = tokio::io::BufReader::new(child.stderr.take().ok_or("no stderr")?).lines();
                let mut out = tokio::io::BufReader::new(child.stdout.take().ok_or("no stdout")?).lines();
                let (mut err_open, mut out_open) = (true, true);
                while err_open || out_open {
                    tokio::select! {
                        l = err.next_line(), if err_open => match l.map_err(e)? { Some(l) => send(Msg::ToolLog(l)), None => err_open = false },
                        l = out.next_line(), if out_open => match l.map_err(e)? { Some(l) => send(Msg::ToolLog(l)), None => out_open = false },
                    }
                }
                let st = child.wait().await.map_err(e)?;
                if st.success() { Ok(format!("{name} finished")) } else { Err(format!("{name} failed ({st})")) }
            };
            let res = tokio::select! {
                r = run => r,
                _ = cancel => Err(format!("{name} stopped")),
            };
            send(Msg::ToolDone(res));
        });
    }

    /// Like the original's "Count distinct values": distinct(path, current filter).len().
    pub fn distinct_count(&self, t: Target, path: String, filter: Document) {
        self.spawn(async move {
            match t.c().distinct(&path, filter).max_time(t.timeout).await {
                Ok(v) => Msg::Notice { title: "Number of distinct values".into(), body: format!("{}\n\ndistinct values in \"{path}\" (the current query is applied)", v.len()) },
                Err(err) => Msg::Info(Err(e(err))),
            }
        });
    }

    /// Runs the real update stages on a sample, so the preview cannot differ from the write.
    pub fn update_preview(&self, t: Target, filter: Document, stages: Vec<Document>) {
        self.spawn(async move {
            let res = async {
                let before: Vec<Document> = t.c().aggregate(vec![doc! { "$match": &filter }, doc! { "$limit": 5 }]).max_time(t.timeout).await.map_err(e)?.try_collect().await.map_err(e)?;
                // The same documents by _id, so each pair is one document even if others change meanwhile.
                let ids: Vec<Bson> = before.iter().filter_map(|d| d.get("_id").cloned()).collect();
                let after_pipe = std::iter::once(doc! { "$match": { "_id": { "$in": ids } } }).chain(stages.iter().cloned()).collect::<Vec<_>>();
                let mut after: std::collections::HashMap<String, Document> = t.c().aggregate(after_pipe).max_time(t.timeout).await.map_err(e)?.try_collect::<Vec<_>>().await.map_err(e)?.into_iter().map(|d| (d.get("_id").map(compact).unwrap_or_default(), d)).collect();
                let pairs = before.into_iter().filter_map(|b| after.remove(&b.get("_id").map(compact).unwrap_or_default()).map(|a| (b, a))).collect();
                let n = t.c().count_documents(filter).max_time(t.timeout).await.map_err(e)?;
                Ok((pairs, n))
            }
            .await;
            Msg::UpdatePreview { stages, res }
        });
    }

    pub fn update_many(&self, t: Target, filter: Document, stages: Vec<Document>) {
        self.write(async move {
            match t.c().update_many(filter, stages).await {
                Ok(r) => (Ok(format!("Updated {} documents", r.modified_count)), None),
                Err(err) => (Err(e(err)), None),
            }
        });
    }

    pub fn rename_many(&self, t: Target, filter: Document, from: String, to: String) {
        self.write(async move {
            match t.c().update_many(filter, doc! { "$rename": { &from: &to } }).await {
                Ok(r) => (Ok(format!("Renamed {from} to {to} in {} documents", r.modified_count)), None),
                Err(err) => (Err(e(err)), None),
            }
        });
    }

    /// Writes imported documents: insert all (ordered, in batches), or upsert each by `key` with
    /// $set like the original's import (bulkWrite of updateOne upserts).
    pub fn import(&self, conn: u64, t: Target, docs: Vec<Document>, upsert_by: Option<String>) {
        const BATCH: usize = 1000;
        self.write(async move {
            let c = t.c();
            let mut docs = docs;
            match upsert_by {
                None => {
                    // Like the original's generateNewDocumentId: documents without _id get one of the
                    // kind the collection already uses (ObjectIds are left to the driver).
                    if docs.iter().any(|d| !d.contains_key("_id")) {
                        let last = c.find_one(doc! {}).sort(doc! { "_id": -1 }).await.ok().flatten().and_then(|d| d.get("_id").cloned());
                        let mut next = match &last {
                            Some(Bson::Int32(n)) => Some(*n as i64 + 1),
                            Some(Bson::Int64(n)) => Some(n + 1),
                            Some(Bson::Double(f)) => Some(*f as i64 + 1),
                            _ => None,
                        };
                        for d in docs.iter_mut().filter(|d| !d.contains_key("_id")) {
                            let id = match (&last, next.as_mut()) {
                                (Some(Bson::String(s)), _) => Bson::String(random_id(s.chars().count())),
                                (_, Some(n)) => {
                                    *n += 1;
                                    Bson::Int64(*n - 1)
                                }
                                _ => continue,
                            };
                            let mut with_id = doc! { "_id": id };
                            with_id.extend(std::mem::take(d));
                            *d = with_id;
                        }
                    }
                    let mut ids = Vec::with_capacity(docs.len());
                    for chunk in docs.chunks(BATCH) {
                        match c.insert_many(chunk.to_vec()).await {
                            Ok(r) => ids.extend(r.inserted_ids.into_values()),
                            Err(err) => {
                                let msg = format!("Stopped after {} documents: {}", ids.len(), e(err));
                                let undo = (!ids.is_empty()).then(|| UndoAction { at: Instant::now(), conn, db: t.db.clone(), coll: t.coll.clone(), kind: UndoKind::InsertMany { ids } });
                                return (Err(msg), undo);
                            }
                        }
                    }
                    let n = ids.len();
                    (Ok(format!("Imported {n} documents")), Some(UndoAction { at: Instant::now(), conn, db: t.db, coll: t.coll, kind: UndoKind::InsertMany { ids } }))
                }
                Some(key) => {
                    let (mut matched, mut upserted) = (0u64, 0u64);
                    for d in docs {
                        let Some(v) = d.get(&key).cloned() else {
                            return (Err(format!("A document has no {key}; nothing after it was written ({matched} updated, {upserted} added).")), None);
                        };
                        match c.update_one(doc! { &key: v }, doc! { "$set": d }).upsert(true).await {
                            Ok(r) => {
                                matched += r.matched_count;
                                upserted += r.upserted_id.is_some() as u64;
                            }
                            Err(err) => return (Err(format!("Stopped ({matched} updated, {upserted} added): {}", e(err))), None),
                        }
                    }
                    (Ok(format!("Import done: {matched} updated, {upserted} added")), None)
                }
            }
        });
    }

    pub fn delete_ids(&self, t: Target, ids: Vec<Bson>) {
        self.write(async move {
            match t.c().delete_many(doc! { "_id": { "$in": ids } }).await {
                Ok(r) => (Ok(format!("Undo removed {} imported documents", r.deleted_count)), None),
                Err(err) => (Err(e(err)), None),
            }
        });
    }

    /// Reads up to `limit` documents (0 = all) matching `filter` and summarises their fields.
    pub fn schema(&self, req: u64, t: Target, filter: Document, limit: i64) {
        self.spawn(async move {
            let res = async {
                let start = Instant::now();
                let c = t.c();
                let mut f = c.find(filter).max_time(t.timeout);
                if limit > 0 {
                    f = f.limit(limit);
                }
                let docs: Vec<Document> = f.await.map_err(e)?.try_collect().await.map_err(e)?;
                let stats = analyze_schema(&docs);
                Ok((stats, docs.len(), start.elapsed().as_millis()))
            }
            .await;
            Msg::Schema { req, res }
        });
    }

    pub fn delete_many(&self, t: Target, filter: Document) {
        self.write(async move {
            match t.c().delete_many(filter).await {
                Ok(r) => (Ok(format!("Deleted {} documents", r.deleted_count)), None),
                Err(err) => (Err(e(err)), None),
            }
        });
    }

    pub fn create_index(&self, t: Target, keys: Document, unique: bool) {
        self.spawn(async move {
            let m = IndexModel::builder().keys(keys).options(IndexOptions::builder().unique(unique).build()).build();
            let res = t.c().create_index(m).await.map(|r| format!("Created index {}", r.index_name)).map_err(e);
            Msg::Written { res, undo: None, reload: false, retry: None }
        });
    }

    pub fn drop_index(&self, t: Target, name: String) {
        self.spawn(async move {
            let res = t.c().drop_index(&name).await.map(|_| format!("Dropped index {name}")).map_err(e);
            Msg::Written { res, undo: None, reload: false, retry: None }
        });
    }

    pub fn export(&self, t: Target, q: Query, path: std::path::PathBuf) {
        self.spawn(async move {
            let res = async {
                let c = t.c();
                let mut f = c.find(q.filter).skip(q.skip).max_time(t.timeout);
                if let Some(l) = q.limit {
                    f = f.limit(l as i64);
                }
                if let Some(s) = q.sort {
                    f = f.sort(s);
                }
                if let Some(p) = q.projection {
                    f = f.projection(p);
                }
                // ponytail: whole result held in memory (CSV needs every column first); stream if exports get huge.
                let docs: Vec<Document> = f.await.map_err(e)?.try_collect().await.map_err(e)?;
                let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("json").to_ascii_lowercase();
                let body = match ext.as_str() {
                    "csv" => to_table(&docs, ','),
                    "tsv" => to_table(&docs, '\t'),
                    _ => serde_json::to_string_pretty(&docs.iter().map(|d| Bson::Document(d.clone()).into_relaxed_extjson()).collect::<Vec<_>>())
                        .map_err(e)?,
                };
                std::fs::write(&path, body).map_err(|x| format!("Could not write {}: {x}", path.display()))?;
                Ok(format!("Exported {} documents to {}", docs.len(), path.display()))
            }
            .await;
            Msg::Info(res)
        });
    }
}

pub struct ShellJob {
    pub runtime: std::path::PathBuf,
    /// Folder holding the runner script and its installed MongoDB driver.
    pub dir: std::path::PathBuf,
    /// JSON for the runner: uri, db, code, collections, readOnly, timeoutMs.
    pub input: String,
    /// Extra npm packages the person added ("name" or "name@version").
    pub packages: Vec<String>,
}

/// Splits "name@version" (scoped names keep their leading @).
pub fn package_spec(p: &str) -> (String, String) {
    match p.rfind('@') {
        Some(i) if i > 0 => (p[..i].to_string(), p[i + 1..].to_string()),
        _ => (p.to_string(), "latest".to_string()),
    }
}

/// npm package names: lowercase letters, digits, - . _ ~ and one optional @scope/.
pub fn valid_package(p: &str) -> bool {
    let (name, version) = package_spec(p);
    let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-._~".contains(c));
    let name_ok = match name.strip_prefix('@').and_then(|r| r.split_once('/')) {
        Some((scope, n)) => ok(scope) && ok(n),
        None => !name.starts_with('@') && ok(&name),
    };
    name_ok && !version.is_empty() && version.chars().all(|c| c.is_ascii_alphanumeric() || "-._^~<>=*|".contains(c))
}

/// Always available in the Shell, like the original NodeShell's context (renderer ~131560:
/// db, ObjectId, dayjs, _, console, log, randomId, UUIDv1, UUIDv4). Versions follow the
/// original's package.json where it lists them (lodash ^4.17.21, dayjs ^1.11.13).
pub const SHELL_BUILTINS: [(&str, &str); 4] = [("lodash", "^4.17.21"), ("dayjs", "^1.11.13"), ("uuid", "^11.0.0"), ("socks", "^2.8.7")];

fn runner_package(packages: &[String]) -> String {
    let mut deps = serde_json::Map::new();
    deps.insert("mongodb".into(), "^6.10.0".into());
    for (name, version) in SHELL_BUILTINS {
        deps.insert(name.into(), version.into());
    }
    for p in packages.iter().filter(|p| valid_package(p)) {
        let (name, version) = package_spec(p);
        deps.insert(name, version.into());
    }
    serde_json::json!({ "name": "quarry-shell", "private": true, "type": "module", "dependencies": deps }).to_string()
}

const RUNNER: &str = include_str!("shell_runner.mjs");

async fn run_shell(req: u64, job: &ShellJob, send: &(impl Fn(Msg) + Send + Sync), cancel: tokio::sync::oneshot::Receiver<()>) -> Res<String> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    use tokio::process::Command;
    let note = |t: &str| send(Msg::ShellLog { req, level: "info".into(), text: t.into() });
    std::fs::create_dir_all(&job.dir).map_err(e)?;
    let package = runner_package(&job.packages);
    let mut deps_changed = false;
    for (name, body) in [("runner.mjs", RUNNER), ("package.json", package.as_str())] {
        let path = job.dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(body) {
            std::fs::write(&path, body).map_err(e)?;
            deps_changed |= name == "package.json";
        }
    }
    let missing = !job.dir.join("node_modules").join("mongodb").exists()
        || SHELL_BUILTINS.iter().any(|(n, _)| !job.dir.join("node_modules").join(n).exists())
        || job.packages.iter().any(|p| !job.dir.join("node_modules").join(package_spec(p).0).exists());
    if deps_changed || missing {
        note("Installing packages for the shell with Bun…");
        let mut cmd = Command::new(&job.runtime);
        cmd.arg("install").current_dir(&job.dir);
        #[cfg(windows)]
        cmd.creation_flags(crate::update::CREATE_NO_WINDOW);
        let out = cmd.output().await.map_err(|err| format!("Could not start Bun ({}): {err}", job.runtime.display()))?;
        if !out.status.success() {
            return Err(format!("bun install failed: {}", String::from_utf8_lossy(&out.stderr)));
        }
        note("Packages ready.");
    }
    let mut cmd = Command::new(&job.runtime);
    cmd.arg("runner.mjs")
        .current_dir(&job.dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(crate::update::CREATE_NO_WINDOW);
    let mut child = cmd
        .spawn()
        .map_err(|err| format!("Could not start Bun ({}): {err}", job.runtime.display()))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    stdin.write_all(job.input.as_bytes()).await.map_err(e)?;
    drop(stdin);
    let mut lines = tokio::io::BufReader::new(child.stdout.take().ok_or("no stdout")?).lines();
    let mut stderr = child.stderr.take().ok_or("no stderr")?;
    let read = async {
        let mut result: Option<Res<String>> = None;
        while let Some(line) = lines.next_line().await.map_err(e)? {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                send(Msg::ShellLog { req, level: "log".into(), text: line });
                continue;
            };
            let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
            match v.get("type").and_then(|t| t.as_str()) {
                Some("log") => send(Msg::ShellLog { req, level: s("level"), text: s("text") }),
                Some("result") if v.get("ok").and_then(|o| o.as_bool()) == Some(true) => result = Some(Ok(s("ejson"))),
                Some("result") => result = Some(Err(s("error"))),
                _ => {}
            }
        }
        Ok::<_, String>(result)
    };
    tokio::select! {
        r = read => {
            let _ = child.wait().await;
            match r? {
                Some(res) => res,
                None => {
                    let mut err = String::new();
                    let _ = stderr.read_to_string(&mut err).await;
                    Err(if err.trim().is_empty() { "The shell ended without a result.".into() } else { err })
                }
            }
        }
        _ = cancel => {
            let _ = child.kill().await;
            Err("Stopped.".into())
        }
    }
}

/// How to reach the server: directly, through an SSH tunnel, or through a SOCKS5 proxy.
#[derive(Clone, Default)]
pub struct Net {
    pub ssh: Option<(crate::ssh::SshConf, crate::ssh::SshSecrets)>,
    pub proxy: Option<Proxy>,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Debug)]
#[serde(default)]
pub struct Proxy {
    pub host: String,
    pub port: u16,
    pub user: String,
    #[serde(skip)]
    pub password: Option<String>,
}

/// One command sent to a server, for the query log.
pub struct LogEntry {
    pub conn: u64,
    pub at: std::time::SystemTime,
    pub db: String,
    pub name: String,
    pub cmd: String,
    /// None while the command is still running.
    pub ms: Option<u128>,
    pub err: Option<String>,
    req: i32,
}

pub const LOG_MAX: usize = 2000;

// ponytail: one process-wide log, clients of every connection write here; keyed by conn id.
pub static LOG: std::sync::Mutex<std::collections::VecDeque<LogEntry>> = std::sync::Mutex::new(std::collections::VecDeque::new());

/// Feeds the driver's command events into `LOG`. The driver already blanks the bodies of
/// authentication and user password commands, so secrets never reach the log.
fn log_event(conn: u64, ev: mongodb::event::command::CommandEvent) {
    use mongodb::event::command::CommandEvent as E;
    match ev {
        E::Started(s) => {
            // Driver housekeeping, not something the user asked for.
            if matches!(s.command_name.as_str(), "hello" | "isMaster" | "ismaster" | "endSessions" | "killCursors") {
                return;
            }
            let mut cmd = s.command;
            for k in ["lsid", "$db", "$clusterTime", "$readPreference", "txnNumber", "apiVersion"] {
                cmd.remove(k);
            }
            // Bulk writes carry thousands of documents; a few show the shape.
            for k in ["documents", "updates", "deletes"] {
                if let Some(Bson::Array(a)) = cmd.get_mut(k).filter(|a| a.as_array().is_some_and(|a| a.len() > 3)) {
                    let more = a.len() - 3;
                    a.truncate(3);
                    a.push(Bson::String(format!("… {more} more")));
                }
            }
            let mut text = compact(&Bson::Document(cmd));
            if let Some((cut, _)) = text.char_indices().nth(4000) {
                text.truncate(cut);
                text.push_str(" …");
            }
            let entry = LogEntry { conn, at: std::time::SystemTime::now(), db: s.db, name: s.command_name, cmd: text, ms: None, err: None, req: s.request_id };
            let Ok(mut log) = LOG.lock() else { return };
            if log.len() >= LOG_MAX {
                log.pop_front();
            }
            log.push_back(entry);
        }
        other => {
            let (req, ms, err) = match other {
                E::Succeeded(s) => (s.request_id, s.duration.as_millis(), None),
                E::Failed(f) => (f.request_id, f.duration.as_millis(), Some(e(f.failure))),
                _ => return,
            };
            let Ok(mut log) = LOG.lock() else { return };
            if let Some(l) = log.iter_mut().rev().find(|l| l.conn == conn && l.req == req) {
                (l.ms, l.err) = (Some(ms), err);
            }
        }
    }
}

async fn open_client(conn: u64, uri: &str, timeout: Duration, net: &Net) -> Res<(Client, Vec<String>, Option<crate::ssh::Tunnel>)> {
    use mongodb::options::Socks5Proxy;
    let tunnel = match &net.ssh {
        Some((conf, secrets)) => Some(crate::ssh::open(conf, secrets, timeout).await?),
        None => None,
    };
    let mut o = ClientOptions::parse(uri).await.map_err(e)?;
    o.connect_timeout = Some(timeout);
    o.server_selection_timeout = Some(timeout);
    o.app_name.get_or_insert_with(|| "quarry".into());
    o.command_event_handler = Some(mongodb::event::EventHandler::callback(move |ev| log_event(conn, ev)));
    if let Some(t) = &tunnel {
        o.socks5_proxy = Some(Socks5Proxy::builder().host("127.0.0.1".to_string()).port(t.port).build());
    } else if let Some(p) = &net.proxy {
        // The original's proxyHost/proxyPort/proxyUsername/proxyPassword; port 1080 when unset.
        let auth = (!p.user.is_empty()).then(|| (p.user.clone(), p.password.clone().unwrap_or_default()));
        o.socks5_proxy = Some(Socks5Proxy::builder().host(p.host.clone()).port(if p.port == 0 { 1080 } else { p.port }).authentication(auth).build());
    }
    let client = Client::with_options(o).map_err(e)?;
    let mut dbs = client.list_database_names().await.map_err(e)?;
    dbs.sort();
    Ok((client, dbs, tunnel))
}

async fn replace_doc(conn: u64, t: Target, old: Document, new: Document, record_undo: bool) -> (Res<String>, Option<UndoAction>) {
    let id = old.get("_id").cloned().unwrap_or(Bson::Null);
    match t.c().replace_one(unchanged(&old), new.clone()).await {
        Ok(r) if r.matched_count == 0 => (Err(CONFLICT.into()), None),
        Ok(_) => (
            Ok(format!("Saved document {}", short(&id))),
            record_undo.then(|| UndoAction { at: Instant::now(), conn, db: t.db, coll: t.coll, kind: UndoKind::Update { old, new } }),
        ),
        Err(err) => (Err(e(err)), None),
    }
}

pub const CONFLICT: &str = "Not saved: this document changed on the server since you opened it. Reload and try again.";

/// Matches the document only while every field is still exactly as we last read it,
/// so a concurrent change by someone else is never silently overwritten.
fn unchanged(old: &Document) -> Document {
    doc! {
        "_id": old.get("_id").cloned().unwrap_or(Bson::Null),
        "$expr": { "$eq": ["$$ROOT", { "$literal": old.clone() }] },
    }
}

pub fn short(b: &Bson) -> String {
    match b {
        Bson::ObjectId(o) => o.to_hex(),
        Bson::String(s) => s.clone(),
        other => other.clone().into_relaxed_extjson().to_string(),
    }
}

// ---------- text <-> BSON ----------

/// Accepts strict JSON, JSON5 (unquoted keys, single quotes, trailing commas) and the common
/// shell helpers ObjectId(), ISODate(), new Date(), NumberLong(), NumberInt(), NumberDecimal().
pub fn parse_value(text: &str) -> Res<Bson> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Bson::Document(Document::new()));
    }
    let v: serde_json::Value = json5::from_str(&shell_to_ejson(text)).map_err(|err| format!("Not valid JSON: {err}"))?;
    Bson::try_from(v).map_err(|err| format!("Not valid Extended JSON: {err}"))
}

pub fn parse_doc(text: &str) -> Res<Document> {
    match parse_value(text)? {
        Bson::Document(d) => Ok(d),
        _ => Err("Expected an object like { field: value }".into()),
    }
}

#[derive(Default)]
pub struct ExplainRow {
    pub depth: usize,
    pub stage: String,
    pub index: String,
    pub keys: Option<i64>,
    pub docs: Option<i64>,
    pub returned: Option<i64>,
    pub ms: Option<i64>,
}

#[derive(Default)]
pub struct ExplainSummary {
    pub rows: Vec<ExplainRow>,
    pub keys: i64,
    pub docs: i64,
    pub returned: i64,
    pub ms: i64,
    pub warnings: Vec<String>,
}

fn int(d: &Document, k: &str) -> Option<i64> {
    match d.get(k)? {
        Bson::Int32(n) => Some(*n as i64),
        Bson::Int64(n) => Some(*n),
        Bson::Double(n) => Some(*n as i64),
        _ => None,
    }
}

/// The parts of an explain reply that carry `executionStats`: the reply itself, the `$cursor`
/// stage of an aggregate, or one per shard.
fn explain_bodies<'a>(d: &'a Document, out: &mut Vec<&'a Document>) {
    if d.contains_key("executionStats") {
        out.push(d);
        return;
    }
    if let Ok(shards) = d.get_document("shards") {
        shards.values().filter_map(Bson::as_document).for_each(|s| explain_bodies(s, out));
    }
    if let Ok(stages) = d.get_array("stages") {
        stages.iter().filter_map(Bson::as_document).filter_map(|s| s.get_document("$cursor").ok()).for_each(|c| explain_bodies(c, out));
    }
}

fn explain_walk(n: &Document, depth: usize, rows: &mut Vec<ExplainRow>) {
    let stage = n.get_str("stage").unwrap_or("?");
    // The slot-based engine reports reads on its fetch and scan stages instead of docsExamined.
    let docs = int(n, "docsExamined").or_else(|| if matches!(stage, "fetch" | "scan") { int(n, "numReads") } else { None });
    rows.push(ExplainRow {
        depth,
        stage: stage.to_string(),
        index: n.get_str("indexName").unwrap_or_default().to_string(),
        keys: int(n, "keysExamined"),
        docs,
        returned: int(n, "nReturned"),
        ms: int(n, "executionTimeMillisEstimate"),
    });
    for (k, v) in n {
        match v {
            Bson::Document(c) if k.ends_with("Stage") => explain_walk(c, depth + 1, rows),
            Bson::Array(a) if k == "inputStages" => a.iter().filter_map(Bson::as_document).for_each(|c| explain_walk(c, depth + 1, rows)),
            // A sharded find lists each shard's own plan.
            Bson::Array(a) if k == "shards" => a.iter().filter_map(Bson::as_document).filter_map(|s| s.get_document("executionStages").ok()).for_each(|c| explain_walk(c, depth + 1, rows)),
            _ => {}
        }
    }
}

/// Flattens an explain reply (find or aggregate, classic or slot-based engine, sharded or not)
/// into plan stages, totals and warnings.
pub fn explain_summary(d: &Document) -> ExplainSummary {
    let mut bodies = Vec::new();
    explain_bodies(d, &mut bodies);
    let mut s = ExplainSummary::default();
    for b in bodies {
        let Ok(st) = b.get_document("executionStats") else { continue };
        s.keys += int(st, "totalKeysExamined").unwrap_or(0);
        s.docs += int(st, "totalDocsExamined").unwrap_or(0);
        s.returned += int(st, "nReturned").unwrap_or(0);
        s.ms += int(st, "executionTimeMillis").unwrap_or(0);
        if let Ok(root) = st.get_document("executionStages") {
            explain_walk(root, 0, &mut s.rows);
        }
    }
    let has = |names: &[&str]| s.rows.iter().any(|r| names.contains(&r.stage.to_lowercase().as_str()));
    if has(&["collscan", "scan"]) {
        s.warnings.push("COLLSCAN: every document is read. An index on the filtered fields would avoid it.".into());
    }
    if has(&["sort"]) {
        s.warnings.push("SORT in memory: no index provides the order. An index ending in the sort fields avoids it.".into());
    }
    let examined = s.docs.max(s.keys);
    // Grouping and counting shrink the output by design, so few results there are not a sign of a weak filter.
    if examined >= 1000 && examined > s.returned * 10 && !has(&["group", "count", "count_scan"]) {
        s.warnings.push(format!("Examined {examined} to return {}. The filter is not selective for the indexes in use.", s.returned));
    }
    s
}

pub fn parse_pipeline(text: &str) -> Res<Vec<Document>> {
    match parse_value(if text.trim().is_empty() { "[]" } else { text })? {
        Bson::Array(a) => a
            .into_iter()
            .map(|s| match s {
                Bson::Document(d) => Ok(d),
                _ => Err("Every pipeline stage must be an object".to_string()),
            })
            .collect(),
        Bson::Document(d) => Ok(vec![d]),
        _ => Err("Expected an array of stages like [{ $match: {} }]".into()),
    }
}

fn shell_to_ejson(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' || c == '\'' {
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != c {
                i += if chars[i] == '\\' { 2 } else { 1 };
            }
            i = (i + 1).min(chars.len());
            out.extend(&chars[start..i]);
            continue;
        }
        if c.is_ascii_alphabetic() && (i == 0 || !(chars[i - 1].is_ascii_alphanumeric() || chars[i - 1] == '$' || chars[i - 1] == '_')) {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphanumeric() {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if word == "new" {
                continue; // `new Date(...)` is treated like `Date(...)`
            }
            if j < chars.len() && chars[j] == '(' {
                if let Some(close) = chars[j..].iter().position(|&ch| ch == ')') {
                    let arg: String = chars[j + 1..j + close].iter().collect();
                    let arg = arg.trim().trim_matches(|ch| ch == '"' || ch == '\'').to_string();
                    let rep = match word.as_str() {
                        "ObjectId" => Some(format!("{{\"$oid\":\"{arg}\"}}")),
                        "ISODate" | "Date" => Some(format!("{{\"$date\":\"{arg}\"}}")),
                        "NumberLong" => Some(format!("{{\"$numberLong\":\"{arg}\"}}")),
                        "NumberDecimal" => Some(format!("{{\"$numberDecimal\":\"{arg}\"}}")),
                        "NumberInt" => Some(arg.clone()),
                        _ => None,
                    };
                    if let Some(rep) = rep {
                        out.push_str(&rep);
                        i = j + close + 1;
                        continue;
                    }
                }
            }
            out.push_str(&word);
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Text for the editor: relaxed Extended JSON, except 64-bit integers keep their marker so
/// saving never silently turns a long into an int (relaxed form would drop that type).
pub fn to_edit_json(d: &Document) -> String {
    to_edit_value(&Bson::Document(d.clone()))
}

/// Pretty editor text for any value, with the same long-integer rule as `to_edit_json`.
pub fn to_edit_value(b: &Bson) -> String {
    fn conv(b: &Bson) -> serde_json::Value {
        match b {
            Bson::Int64(n) => serde_json::json!({ "$numberLong": n.to_string() }),
            Bson::Document(d) => serde_json::Value::Object(d.iter().map(|(k, v)| (k.clone(), conv(v))).collect()),
            Bson::Array(a) => serde_json::Value::Array(a.iter().map(conv).collect()),
            other => other.clone().into_relaxed_extjson(),
        }
    }
    serde_json::to_string_pretty(&conv(b)).unwrap_or_default()
}

/// What to turn into driver code: a find with its options, or a pipeline.
pub enum CodeOf {
    Find(Query),
    Aggregate(Vec<Document>),
}

#[derive(Clone, Copy, PartialEq)]
pub enum Lang {
    Mongosh,
    Node,
    Python,
    Java,
    Go,
    Rust,
}

impl Lang {
    pub const ALL: [Lang; 6] = [Lang::Mongosh, Lang::Node, Lang::Python, Lang::Java, Lang::Go, Lang::Rust];

    pub fn name(self) -> &'static str {
        match self {
            Lang::Mongosh => "mongosh",
            Lang::Node => "Node.js",
            Lang::Python => "Python",
            Lang::Java => "Java",
            Lang::Go => "Go",
            Lang::Rust => "Rust",
        }
    }
}

/// A runnable snippet for the query. Values travel as Extended JSON in a raw string literal and are
/// read back by each driver's own parser, so every BSON type survives without a per-language converter.
pub fn driver_code(lang: Lang, db: &str, coll: &str, what: &CodeOf) -> String {
    let q = |s: &str| serde_json::Value::String(s.into()).to_string();
    let uri = "mongodb://localhost:27017";
    let (filter, sort, proj, skip, limit) = match what {
        CodeOf::Find(f) => (Some(to_edit_json(&f.filter)), f.sort.as_ref().map(to_edit_json), f.projection.as_ref().map(to_edit_json), f.skip, f.limit),
        CodeOf::Aggregate(_) => (None, None, None, 0, None),
    };
    let pipe = match what {
        CodeOf::Aggregate(p) => Some(to_edit_value(&Bson::Array(p.iter().cloned().map(Bson::Document).collect()))),
        CodeOf::Find(_) => None,
    };
    // Each literal escapes only what its delimiter cannot hold.
    let js = |s: &str| format!("String.raw`{}`", s.replace('`', "` + \"`\" + String.raw`").replace("${", "${'${'}"));
    let py = |s: &str| format!("r\"\"\"{}\"\"\"", s.replace("\"\"\"", "\"\"\" + '\"\"\"' + r\"\"\""));
    let java = |s: &str| format!("Document.parse(\"\"\"\n{}\n\"\"\")", s.replace('\\', "\\\\").replace("\"\"\"", "\\\"\"\""));
    // Go reads only canonical numbers when canonical is true, and the editor text is relaxed, so it passes false.
    let go = |s: &str| format!("[]byte(`{}`)", s.replace('`', "` + \"`\" + `"));
    let rs = |s: &str| {
        let hashes = "#".repeat(1 + s.matches("\"#").count().min(8));
        format!("r{hashes}\"{s}\"{hashes}")
    };
    let c = crate::app::collection_ref(coll);
    let c = if c.starts_with("db.") { c.replacen("collection", "getCollection", 1) } else { format!("db.{c}") };
    let sh = |d: &Document| shell_text(&Bson::Document(d.clone()));
    match lang {
        Lang::Mongosh => match what {
            CodeOf::Find(f) => {
                let mut s = format!("{c}.find({}", sh(&f.filter));
                if let Some(p) = &f.projection {
                    s += &format!(", {}", sh(p));
                }
                s.push(')');
                if let Some(o) = &f.sort {
                    s += &format!(".sort({})", sh(o));
                }
                if skip > 0 {
                    s += &format!(".skip({skip})");
                }
                if let Some(n) = limit {
                    s += &format!(".limit({n})");
                }
                s
            }
            CodeOf::Aggregate(p) => format!("{c}.aggregate({})", shell_text(&Bson::Array(p.iter().cloned().map(Bson::Document).collect()))),
        },
        Lang::Node => {
            let head = format!("import {{ MongoClient }} from \"mongodb\";\nimport {{ EJSON }} from \"bson\";\n\nconst client = new MongoClient({});\nconst coll = client.db({}).collection({});\n\n", q(uri), q(db), q(coll));
            let body = match (&filter, &pipe) {
                (Some(f), _) => {
                    let mut s = format!("const filter = EJSON.parse({});\n", js(f));
                    let mut opts = Vec::new();
                    for n in ["sort", "projection"] {
                        if let Some(v) = if n == "sort" { &sort } else { &proj } {
                            s += &format!("const {n} = EJSON.parse({});\n", js(v));
                            opts.push(n.to_string());
                        }
                    }
                    if skip > 0 {
                        opts.push(format!("skip: {skip}"));
                    }
                    if let Some(n) = limit {
                        opts.push(format!("limit: {n}"));
                    }
                    format!("{s}const docs = await coll.find(filter, {{ {} }}).toArray();\n", opts.join(", "))
                }
                (_, Some(p)) => format!("const pipeline = EJSON.parse({});\nconst docs = await coll.aggregate(pipeline).toArray();\n", js(p)),
                _ => String::new(),
            };
            format!("{head}{body}console.log(docs);\nawait client.close();\n")
        }
        Lang::Python => {
            let head = format!("from bson import json_util\nfrom pymongo import MongoClient\n\nclient = MongoClient({})\ncoll = client[{}][{}]\n\n", q(uri), q(db), q(coll));
            let body = match (&filter, &pipe) {
                (Some(f), _) => {
                    let mut s = format!("query = json_util.loads({})\n", py(f));
                    let mut call = "coll.find(query".to_string();
                    if let Some(v) = &proj {
                        s += &format!("projection = json_util.loads({})\n", py(v));
                        call += ", projection";
                    }
                    call.push(')');
                    if let Some(v) = &sort {
                        s += &format!("sort = json_util.loads({})\n", py(v));
                        call += ".sort(list(sort.items()))";
                    }
                    if skip > 0 {
                        call += &format!(".skip({skip})");
                    }
                    if let Some(n) = limit {
                        call += &format!(".limit({n})");
                    }
                    format!("{s}for doc in {call}:\n    print(doc)\n")
                }
                (_, Some(p)) => format!("pipeline = json_util.loads({})\nfor doc in coll.aggregate(pipeline):\n    print(doc)\n", py(p)),
                _ => String::new(),
            };
            format!("{head}{body}")
        }
        Lang::Java => {
            let head = format!("import com.mongodb.client.*;\nimport java.util.List;\nimport org.bson.Document;\n\ntry (MongoClient client = MongoClients.create({})) {{\n    MongoCollection<Document> coll = client.getDatabase({}).getCollection({});\n\n", q(uri), q(db), q(coll));
            let body = match (&filter, &pipe) {
                (Some(f), _) => {
                    let mut call = "coll.find(filter)".to_string();
                    let mut s = format!("    Document filter = {};\n", java(f));
                    if let Some(v) = &proj {
                        s += &format!("    Document projection = {};\n", java(v));
                        call += ".projection(projection)";
                    }
                    if let Some(v) = &sort {
                        s += &format!("    Document sort = {};\n", java(v));
                        call += ".sort(sort)";
                    }
                    if skip > 0 {
                        call += &format!(".skip({skip})");
                    }
                    if let Some(n) = limit {
                        call += &format!(".limit({n})");
                    }
                    format!("{s}    for (Document doc : {call}) {{\n        System.out.println(doc.toJson());\n    }}\n")
                }
                // Document.parse only reads objects, so the array rides inside one.
                (_, Some(p)) => {
                    let wrapped = java(&format!("{{ \"pipeline\": {p} }}"));
                    format!("    List<Document> pipeline = {wrapped}.getList(\"pipeline\", Document.class);\n    for (Document doc : coll.aggregate(pipeline)) {{\n        System.out.println(doc.toJson());\n    }}\n")
                }
                _ => String::new(),
            };
            format!("{head}{body}}}\n")
        }
        Lang::Go => {
            let head = format!("package main\n\nimport (\n\t\"context\"\n\t\"fmt\"\n\n\t\"go.mongodb.org/mongo-driver/v2/bson\"\n\t\"go.mongodb.org/mongo-driver/v2/mongo\"\n\t\"go.mongodb.org/mongo-driver/v2/mongo/options\"\n)\n\nfunc main() {{\n\tctx := context.TODO()\n\tclient, err := mongo.Connect(options.Client().ApplyURI({}))\n\tif err != nil {{\n\t\tpanic(err)\n\t}}\n\tdefer client.Disconnect(ctx)\n\tcoll := client.Database({}).Collection({})\n\n\tparse := func(js []byte, v any) {{\n\t\tif err := bson.UnmarshalExtJSON(js, false, v); err != nil {{\n\t\t\tpanic(err)\n\t\t}}\n\t}}\n", q(uri), q(db), q(coll));
            let body = match (&filter, &pipe) {
                (Some(f), _) => {
                    let mut s = format!("\tvar filter bson.D\n\tparse({}, &filter)\n", go(f));
                    let mut opts = Vec::new();
                    for (n, v, set) in [("sort", &sort, "SetSort"), ("projection", &proj, "SetProjection")] {
                        if let Some(v) = v {
                            s += &format!("\tvar {n} bson.D\n\tparse({}, &{n})\n", go(v));
                            opts.push(format!(".{set}({n})"));
                        }
                    }
                    if skip > 0 {
                        opts.push(format!(".SetSkip({skip})"));
                    }
                    if let Some(n) = limit {
                        opts.push(format!(".SetLimit({n})"));
                    }
                    format!("{s}\tcur, err := coll.Find(ctx, filter, options.Find(){})\n", opts.concat())
                }
                // UnmarshalExtJSON needs a document at the top, so the array rides inside one.
                (_, Some(p)) => format!("\tvar wrapped struct{{ Pipeline mongo.Pipeline }}\n\tparse({}, &wrapped)\n\tcur, err := coll.Aggregate(ctx, wrapped.Pipeline)\n", go(&format!("{{ \"pipeline\": {p} }}"))),
                _ => String::new(),
            };
            format!("{head}{body}\tif err != nil {{\n\t\tpanic(err)\n\t}}\n\tvar docs []bson.M\n\tif err := cur.All(ctx, &docs); err != nil {{\n\t\tpanic(err)\n\t}}\n\tfmt.Println(docs)\n}}\n")
        }
        Lang::Rust => {
            let head = format!("use futures_util::TryStreamExt;\nuse mongodb::{{bson::Document, Client}};\n\n#[tokio::main]\nasync fn main() -> mongodb::error::Result<()> {{\n    let client = Client::with_uri_str({}).await?;\n    let coll = client.database({}).collection::<Document>({});\n\n", q(uri), q(db), q(coll));
            let body = match (&filter, &pipe) {
                (Some(f), _) => {
                    let mut s = format!("    let filter: Document = serde_json::from_str({}).unwrap();\n", rs(f));
                    let mut call = "coll.find(filter)".to_string();
                    for (n, v) in [("sort", &sort), ("projection", &proj)] {
                        if let Some(v) = v {
                            s += &format!("    let {n}: Document = serde_json::from_str({}).unwrap();\n", rs(v));
                            call += &format!(".{n}({n})");
                        }
                    }
                    if skip > 0 {
                        call += &format!(".skip({skip})");
                    }
                    if let Some(n) = limit {
                        call += &format!(".limit({n})");
                    }
                    format!("{s}    let mut cursor = {call}.await?;\n")
                }
                (_, Some(p)) => format!("    let pipeline: Vec<Document> = serde_json::from_str({}).unwrap();\n    let mut cursor = coll.aggregate(pipeline).await?;\n", rs(p)),
                _ => String::new(),
            };
            format!("{head}{body}    while let Some(doc) = cursor.try_next().await? {{\n        println!(\"{{doc}}\");\n    }}\n    Ok(())\n}}\n")
        }
    }
}

pub fn compact(b: &Bson) -> String {
    b.clone().into_relaxed_extjson().to_string()
}

pub fn type_name(b: &Bson) -> &'static str {
    match b {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        Bson::Boolean(_) => "bool",
        Bson::Null | Bson::Undefined => "null",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::ObjectId(_) => "objectId",
        Bson::DateTime(_) => "date",
        Bson::Decimal128(_) => "decimal",
        b @ Bson::Binary(_) if uuid_of(b).is_some() => "uuid",
        Bson::Binary(_) => "binary",
        Bson::RegularExpression(_) => "regex",
        Bson::Timestamp(_) => "timestamp",
        _ => "other",
    }
}

/// Field-level changes between two documents, as dotted paths, for the save preview.
pub fn diff(old: &Document, new: &Document) -> Vec<(String, Option<Bson>, Option<Bson>)> {
    fn walk(p: &str, a: &Document, b: &Document, out: &mut Vec<(String, Option<Bson>, Option<Bson>)>) {
        for (k, va) in a {
            let path = if p.is_empty() { k.clone() } else { format!("{p}.{k}") };
            match (va, b.get(k)) {
                (Bson::Document(x), Some(Bson::Document(y))) => walk(&path, x, y, out),
                (_, Some(vb)) if vb == va => {}
                (_, vb) => out.push((path, Some(va.clone()), vb.cloned())),
            }
        }
        for (k, vb) in b {
            if !a.contains_key(k) {
                let path = if p.is_empty() { k.clone() } else { format!("{p}.{k}") };
                out.push((path, None, Some(vb.clone())));
            }
        }
    }
    let mut out = Vec::new();
    walk("", old, new, &mut out);
    out
}

/// Pipeline-form update stages. `$unset` takes an array of paths, which every server with
/// pipeline updates accepts.
pub fn update_stages(set: Document, unset: Vec<String>) -> Vec<Document> {
    let mut v = Vec::new();
    if !set.is_empty() {
        v.push(doc! { "$set": set });
    }
    if !unset.is_empty() {
        v.push(doc! { "$unset": unset });
    }
    v
}

pub fn has_write_stage(p: &[Document]) -> bool {
    p.iter().any(|s| s.contains_key("$out") || s.contains_key("$merge"))
}

/// Pipeline up to and including stage `i`, capped so a preview stays cheap.
/// Write stages are dropped so a preview can never modify data.
pub fn stage_preview(p: &[Document], i: usize) -> Option<Vec<Document>> {
    let mut out: Vec<Document> = p.get(..=i)?.iter().filter(|s| !has_write_stage(std::slice::from_ref(s))).cloned().collect();
    out.push(doc! { "$limit": 20 });
    Some(out)
}

/// CSV or TSV with every top-level field seen in any document, RFC 4180 quoting.
pub fn to_table(docs: &[Document], sep: char) -> String {
    let mut cols: Vec<String> = Vec::new();
    for d in docs {
        for k in d.keys() {
            if !cols.contains(k) {
                cols.push(k.clone());
            }
        }
    }
    let cell = |s: String| -> String {
        if sep == '\t' {
            s.replace(['\t', '\n', '\r'], " ")
        } else if s.contains([',', '"', '\n', '\r']) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s
        }
    };
    let mut out = cols.iter().map(|c| cell(c.clone())).collect::<Vec<_>>().join(&sep.to_string());
    for d in docs {
        out.push('\n');
        let row: Vec<String> = cols
            .iter()
            .map(|c| {
                cell(match d.get(c) {
                    None | Some(Bson::Null) => String::new(),
                    Some(Bson::String(s)) => s.clone(),
                    Some(Bson::ObjectId(o)) => o.to_hex(),
                    Some(Bson::DateTime(t)) => t.try_to_rfc3339_string().unwrap_or_default(),
                    Some(other) => compact(other),
                })
            })
            .collect();
        out.push_str(&row.join(&sep.to_string()));
    }
    out
}

pub fn mask_uri(uri: &str) -> String {
    // Hide the password between "user:" and "@".
    if let (Some(s), Some(at)) = (uri.find("://"), uri.rfind('@')) {
        if let Some(colon) = uri[s + 3..at].find(':') {
            return format!("{}****{}", &uri[..s + 3 + colon + 1], &uri[at..]);
        }
    }
    uri.to_string()
}

// ---------- pasted shell queries ----------

pub enum ShellKind {
    Find(Query),
    Aggregate(Vec<Document>),
}

pub struct Shell {
    pub coll: String,
    pub kind: ShellKind,
}

pub fn is_shell(text: &str) -> bool {
    text.trim_start().starts_with("db.")
}

/// Text inside the parentheses that `s` starts after, and what follows the closing one.
fn take_args(s: &str) -> Res<(&str, &str)> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut esc = false;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' if depth == 0 => return Ok((&s[..i], &s[i + 1..])),
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    Err("A parenthesis is not closed".into())
}

fn as_u64(b: &Bson) -> Res<u64> {
    match b {
        Bson::Int32(n) if *n >= 0 => Ok(*n as u64),
        Bson::Int64(n) if *n >= 0 => Ok(*n as u64),
        Bson::Double(n) if *n >= 0.0 => Ok(*n as u64),
        _ => Err("skip and limit need a positive number".into()),
    }
}

/// Understands `db.coll.find(filter, projection).sort().skip().limit()`, `findOne`,
/// `countDocuments`, `aggregate([...])` and `db.getCollection("name")...`.
pub fn parse_shell(src: &str) -> Res<Shell> {
    let s = src.trim().trim_end_matches(';').trim();
    let rest = s.strip_prefix("db.").ok_or("Shell queries start with db.")?;
    let (coll, mut rest) = if let Some(r) = rest.strip_prefix("getCollection(") {
        let (arg, r) = take_args(r)?;
        (arg.trim().trim_matches(|c| c == '"' || c == '\'').to_string(), r)
    } else {
        let end = rest.find('.').ok_or("Expected db.<collection>.<method>(...)")?;
        (rest[..end].trim().to_string(), &rest[end..])
    };
    let mut calls = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let r = rest.strip_prefix('.').ok_or_else(|| format!("Unexpected text: {}", rest.chars().take(20).collect::<String>()))?;
        let open = r.find('(').ok_or("Expected a method call like .find(...)")?;
        let (args, after) = take_args(&r[open + 1..])?;
        calls.push((r[..open].trim().to_string(), args.to_string()));
        rest = after;
    }
    let args = |a: &str| -> Res<Vec<Bson>> {
        match parse_value(&format!("[{a}]"))? {
            Bson::Array(v) => Ok(v),
            _ => unreachable!(),
        }
    };
    let as_doc = |b: Option<&Bson>| -> Res<Option<Document>> {
        match b {
            None => Ok(None),
            Some(Bson::Document(d)) => Ok(Some(d.clone())),
            Some(_) => Err("Expected an object".into()),
        }
    };
    let mut it = calls.into_iter();
    let (method, a) = it.next().ok_or("Missing a method like .find() or .aggregate()")?;
    let kind = match method.as_str() {
        "aggregate" => {
            let v = args(&a)?;
            let stages = match v.as_slice() {
                [Bson::Array(st)] => st.clone(),
                _ => v,
            };
            let p = stages
                .into_iter()
                .map(|s| match s {
                    Bson::Document(d) => Ok(d),
                    _ => Err("Every pipeline stage must be an object".to_string()),
                })
                .collect::<Res<Vec<_>>>()?;
            ShellKind::Aggregate(p)
        }
        "find" | "findOne" | "count" | "countDocuments" => {
            let v = args(&a)?;
            let mut q = Query {
                filter: as_doc(v.first())?.unwrap_or_default(),
                projection: as_doc(v.get(1))?.filter(|p| !p.is_empty()),
                limit: (method == "findOne").then_some(1),
                ..Default::default()
            };
            for (m, a) in it.by_ref() {
                let v = args(&a)?;
                match m.as_str() {
                    "sort" => q.sort = as_doc(v.first())?,
                    "projection" | "project" => q.projection = as_doc(v.first())?,
                    "skip" => q.skip = as_u64(v.first().ok_or("skip needs a number")?)?,
                    "limit" => q.limit = Some(as_u64(v.first().ok_or("limit needs a number")?)?).filter(|l| *l > 0),
                    "toArray" | "pretty" => {}
                    other => return Err(format!(".{other}() is not supported here")),
                }
            }
            ShellKind::Find(q)
        }
        other => return Err(format!("db.{coll}.{other}() is not supported. Use find, findOne, countDocuments or aggregate.")),
    };
    if let Some((m, _)) = it.find(|(m, _)| m != "toArray" && m != "pretty") {
        return Err(format!(".{m}() is not supported after this method"));
    }
    Ok(Shell { coll, kind })
}

/// The original's randomId (short-unique-id, "alphanum" dictionary): digits and letters of both cases.
pub fn random_id(len: usize) -> String {
    const ALPHANUM: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEED: AtomicU64 = AtomicU64::new(0);
    let mut x = SEED.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed)
        ^ std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1)
        | 1;
    (0..len)
        .map(|_| {
            // xorshift64: ids only need to be unlikely to repeat, not secret.
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            ALPHANUM[(x % ALPHANUM.len() as u64) as usize] as char
        })
        .collect()
}

// ---------- schema ----------

/// One field path across the analysed documents.
#[derive(Clone, Debug)]
pub struct FieldStat {
    pub path: String,
    pub depth: usize,
    /// Documents that have this field at least once.
    pub docs: usize,
    /// Type name and how many times it was seen.
    pub types: Vec<(&'static str, usize)>,
    pub examples: Vec<String>,
}

/// Field paths in first-seen order, how many documents have each, their types and a few
/// examples. Objects inside arrays are merged under the array's path.
pub fn analyze_schema(docs: &[Document]) -> Vec<FieldStat> {
    use std::collections::{HashMap, HashSet};
    fn walk(d: &Document, prefix: &str, depth: usize, stats: &mut Vec<FieldStat>, at: &mut HashMap<String, usize>, seen: &mut HashSet<usize>) {
        for (k, v) in d {
            let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
            let i = *at.entry(path.clone()).or_insert_with(|| {
                stats.push(FieldStat { path: path.clone(), depth, docs: 0, types: Vec::new(), examples: Vec::new() });
                stats.len() - 1
            });
            if seen.insert(i) {
                stats[i].docs += 1;
            }
            let tn = type_name(v);
            match stats[i].types.iter_mut().find(|(t, _)| *t == tn) {
                Some(t) => t.1 += 1,
                None => stats[i].types.push((tn, 1)),
            }
            if !matches!(v, Bson::Document(_) | Bson::Array(_)) && stats[i].examples.len() < 3 {
                let shown = match v {
                    Bson::ObjectId(o) => o.to_hex(),
                    Bson::DateTime(d) => d.try_to_rfc3339_string().unwrap_or_default(),
                    other => compact(other),
                };
                let ex: String = shown.chars().take(40).collect();
                if !stats[i].examples.contains(&ex) {
                    stats[i].examples.push(ex);
                }
            }
            match v {
                Bson::Document(c) => walk(c, &path, depth + 1, stats, at, seen),
                Bson::Array(a) => {
                    for x in a.iter().take(50) {
                        if let Bson::Document(c) = x {
                            walk(c, &path, depth + 1, stats, at, seen);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let (mut stats, mut at) = (Vec::new(), HashMap::new());
    for d in docs {
        let mut seen = HashSet::new();
        walk(d, "", 0, &mut stats, &mut at, &mut seen);
    }
    for s in &mut stats {
        s.types.sort_by(|a, b| b.1.cmp(&a.1));
    }
    stats
}

/// A `$jsonSchema` validator describing the analysed documents. A field is required when every
/// sampled document has it. Children are nested only under object values, because `properties`
/// does not constrain array elements.
pub fn schema_from_stats(stats: &[FieldStat], sampled: usize) -> Document {
    fn bson_type(t: &str) -> Option<&'static str> {
        Some(match t {
            "double" => "double",
            "string" => "string",
            "object" => "object",
            "array" => "array",
            "bool" => "bool",
            "null" => "null",
            "int" => "int",
            "long" => "long",
            "objectId" => "objectId",
            "date" => "date",
            "decimal" => "decimal",
            "uuid" | "binary" => "binData",
            "regex" => "regex",
            "timestamp" => "timestamp",
            _ => return None,
        })
    }
    fn object(stats: &[FieldStat], prefix: &str, depth: usize, present: usize) -> Document {
        let (mut props, mut required) = (Document::new(), Vec::new());
        for f in stats.iter().filter(|f| f.depth == depth && f.path.strip_prefix(prefix).is_some_and(|r| !r.contains('.'))) {
            let name = &f.path[prefix.len()..];
            let mut types: Vec<&str> = f.types.iter().filter_map(|(t, _)| bson_type(t)).collect();
            let all_mapped = types.len() == f.types.len();
            types.sort_unstable();
            types.dedup();
            let mut schema = Document::new();
            // An unmappable type leaves the field unconstrained rather than rejecting its documents.
            if all_mapped {
                match types.as_slice() {
                    [one] => schema.insert("bsonType", *one),
                    many => schema.insert("bsonType", many.iter().map(|t| Bson::String(t.to_string())).collect::<Vec<_>>()),
                };
            }
            // When the path is also an array its children are merged from elements, so their
            // counts say nothing about presence inside each object.
            if f.types.iter().any(|(t, _)| *t == "object") {
                let plain = !f.types.iter().any(|(t, _)| *t == "array");
                schema.extend(object(stats, &format!("{}.", f.path), depth + 1, if plain { f.docs } else { 0 }));
            }
            if present > 0 && f.docs == present {
                required.push(Bson::String(name.to_string()));
            }
            props.insert(name, schema);
        }
        let mut out = Document::new();
        if !props.is_empty() {
            out.insert("properties", props);
        }
        // MongoDB rejects an empty required array.
        if !required.is_empty() {
            out.insert("required", required);
        }
        out
    }
    let mut schema = doc! { "bsonType": "object" };
    schema.extend(object(stats, "", 0, sampled));
    doc! { "$jsonSchema": schema }
}

// ---------- import ----------

#[derive(Clone, Copy, PartialEq, Default)]
pub enum ImportKind {
    #[default]
    Json,
    JsonLines,
    Csv,
}

pub struct CsvOptions {
    /// None = detect among , tab | ; (the original's delimitersToGuess).
    pub separator: Option<char>,
    pub header: bool,
    pub camel_case: bool,
}

/// Parses pasted or loaded text into documents, following the original's import rules.
pub fn parse_import(text: &str, kind: ImportKind, csv: &CsvOptions) -> Res<Vec<Document>> {
    match kind {
        ImportKind::Json => match parse_value(text)? {
            Bson::Array(a) => a
                .into_iter()
                .enumerate()
                .map(|(i, x)| match x {
                    Bson::Document(d) => Ok(d),
                    _ => Err(format!("Item {} is not a document", i + 1)),
                })
                .collect(),
            Bson::Document(d) => Ok(vec![d]),
            _ => Err("Expected a JSON array of documents or one document".into()),
        },
        ImportKind::JsonLines => text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
            .map(|(i, l)| parse_doc(l).map_err(|err| format!("Line {}: {err}", i + 1)))
            .collect(),
        ImportKind::Csv => {
            let sep = csv.separator.unwrap_or_else(|| guess_separator(text));
            let mut rows = csv_rows(text, sep);
            if rows.is_empty() {
                return Ok(Vec::new());
            }
            let names: Vec<String> = if csv.header {
                rows.remove(0).into_iter().map(|h| if csv.camel_case { camel_case(&h) } else { h.trim().to_string() }).collect()
            } else {
                (1..=rows.iter().map(|r| r.len()).max().unwrap_or(0)).map(|i| format!("column{i}")).collect()
            };
            Ok(rows
                .into_iter()
                .map(|r| {
                    let mut d = Document::new();
                    for (i, cell) in r.into_iter().enumerate() {
                        let name = names.get(i).cloned().unwrap_or_else(|| format!("column{}", i + 1));
                        d.insert(name, csv_value(&cell));
                    }
                    d
                })
                .collect())
        }
    }
}

fn guess_separator(text: &str) -> char {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default();
    [',', '\t', '|', ';'].into_iter().max_by_key(|c| first.matches(*c).count()).unwrap_or(',')
}

/// RFC 4180 rows: quotes with "" escapes, separators and newlines inside quotes, empty lines skipped.
fn csv_rows(text: &str, sep: char) -> Vec<Vec<String>> {
    let (mut rows, mut row, mut cell) = (Vec::new(), Vec::new(), String::new());
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (quoted, c) {
            (true, '"') if chars.peek() == Some(&'"') => {
                cell.push('"');
                chars.next();
            }
            (true, '"') => quoted = false,
            (true, c) => cell.push(c),
            (false, '"') if cell.is_empty() => quoted = true,
            (false, c) if c == sep => row.push(std::mem::take(&mut cell)),
            (false, '\r') => {}
            (false, '\n') => {
                row.push(std::mem::take(&mut cell));
                if !(row.len() == 1 && row[0].is_empty()) {
                    rows.push(std::mem::take(&mut row));
                }
                row.clear();
            }
            (false, c) => cell.push(c),
        }
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    rows
}

/// Papa Parse's dynamicTyping, as the original imports CSV: booleans, numbers and ISO dates get
/// their type; an empty cell becomes null.
fn csv_value(cell: &str) -> Bson {
    let t = cell.trim();
    if t.is_empty() {
        return Bson::Null;
    }
    if t.eq_ignore_ascii_case("true") {
        return Bson::Boolean(true);
    }
    if t.eq_ignore_ascii_case("false") {
        return Bson::Boolean(false);
    }
    let numeric = t.chars().enumerate().all(|(i, c)| c.is_ascii_digit() || (i == 0 && c == '-') || c == '.' || c == 'e' || c == 'E' || c == '+')
        && t.chars().any(|c| c.is_ascii_digit())
        && !(t.len() > 1 && t.starts_with('0') && !t.starts_with("0."));
    if numeric {
        if let Ok(n) = t.parse::<i64>() {
            return i32::try_from(n).map(Bson::Int32).unwrap_or(Bson::Int64(n));
        }
        if let Ok(f) = t.parse::<f64>() {
            return Bson::Double(f);
        }
    }
    if t.len() >= 20 && t.as_bytes()[4] == b'-' && t.contains('T') {
        if let Ok(d) = mongodb::bson::DateTime::parse_rfc3339_str(t) {
            return Bson::DateTime(d);
        }
    }
    Bson::String(cell.to_string())
}

/// lodash camelCase for header names ("First Name" -> firstName).
fn camel_case(s: &str) -> String {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|p| !p.is_empty())
        .enumerate()
        .map(|(i, p)| {
            let lower = p.to_lowercase();
            if i == 0 { lower } else { lower[..1].to_uppercase() + &lower[1..] }
        })
        .collect()
}

// ---------- field actions ----------

/// `items.2.sku` becomes `items.sku`, which is how MongoDB queries match inside arrays.
pub fn strip_indices(path: &str) -> String {
    path.split('.').filter(|s| s.parse::<usize>().is_err()).collect::<Vec<_>>().join(".")
}

/// The value as JavaScript's JSON.stringify would write it (ObjectId and dates as strings).
fn plain_json(b: &Bson) -> serde_json::Value {
    use serde_json::Value as V;
    match b {
        Bson::ObjectId(o) => V::String(o.to_hex()),
        Bson::DateTime(t) => V::String(t.try_to_rfc3339_string().unwrap_or_default()),
        Bson::Int64(n) => V::from(*n),
        Bson::Document(d) => V::Object(d.iter().map(|(k, v)| (k.clone(), plain_json(v))).collect()),
        Bson::Array(a) => V::Array(a.iter().map(plain_json).collect()),
        other => other.clone().into_relaxed_extjson(),
    }
}

/// The original's "Field size": JSON.stringify(value).length (renderer line 101291).
pub fn field_size(b: &Bson) -> usize {
    plain_json(b).to_string().len()
}

/// Readable shell syntax that `parse_value` reads back, e.g. `{ _id: ObjectId("...") }`.
pub fn shell_text(b: &Bson) -> String {
    let key = |k: &str| {
        if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') && !k.starts_with(|c: char| c.is_ascii_digit()) {
            k.to_string()
        } else {
            serde_json::Value::String(k.into()).to_string()
        }
    };
    match b {
        Bson::ObjectId(o) => format!("ObjectId(\"{}\")", o.to_hex()),
        Bson::DateTime(t) => format!("ISODate(\"{}\")", t.try_to_rfc3339_string().unwrap_or_default()),
        Bson::Int64(n) => format!("NumberLong({n})"),
        Bson::Decimal128(d) => format!("NumberDecimal(\"{d}\")"),
        Bson::Document(d) if d.is_empty() => "{}".into(),
        Bson::Document(d) => format!("{{ {} }}", d.iter().map(|(k, v)| format!("{}: {}", key(k), shell_text(v))).collect::<Vec<_>>().join(", ")),
        Bson::Array(a) => format!("[{}]", a.iter().map(shell_text).collect::<Vec<_>>().join(", ")),
        other => compact(other),
    }
}

/// The original's groupCountField pipeline: match, unwind arrays, group by the field, sort by count.
pub fn group_count_pipeline(path: &str, value: &Bson, filter: Document) -> Res<Vec<Document>> {
    let segs: Vec<&str> = path.split('.').collect();
    let numeric: Vec<usize> = segs.iter().enumerate().filter(|(_, s)| s.parse::<usize>().is_ok()).map(|(i, _)| i).collect();
    if numeric.len() > 1 {
        return Err("Grouping does not support fields nested more than one array deep.".into());
    }
    let mut p = vec![doc! { "$match": filter }];
    let mut group_path = path.to_string();
    if let Some(&i) = numeric.first() {
        p.push(doc! { "$unwind": format!("${}", segs[..i].join(".")) });
        group_path = strip_indices(path);
    } else if matches!(value, Bson::Array(_)) {
        p.push(doc! { "$unwind": format!("${path}") });
    }
    p.push(doc! { "$group": { "_id": format!("${group_path}"), "count": { "$sum": 1 } } });
    p.push(doc! { "$sort": { "count": -1 } });
    Ok(p)
}

// ---------- typed values for inline editing ----------

// The useful subset of the original's fieldTypes table (renderer line 30729): string, number kinds,
// bool, date, ObjectId, null, object, array, regular expression and UUID.
pub const KINDS: [&str; 13] = ["string", "int", "long", "double", "decimal", "bool", "date", "objectId", "null", "object", "array", "regex", "uuid"];

fn uuid_of(b: &Bson) -> Option<mongodb::bson::Uuid> {
    match b {
        Bson::Binary(bin) if bin.subtype == mongodb::bson::spec::BinarySubtype::Uuid => bin.to_uuid().ok(),
        _ => None,
    }
}

/// The original flags whole numbers in this range as likely Unix timestamps in seconds
/// (fieldTypes.number.getInfo, renderer line 31150).
pub fn possible_timestamp(b: &Bson) -> Option<String> {
    let n = match b {
        Bson::Int32(n) => *n as i64,
        Bson::Int64(n) => *n,
        Bson::Double(f) if f.fract() == 0.0 => *f as i64,
        _ => return None,
    };
    (n > 1_000_000_000 && n < 3_000_000_000).then(|| mongodb::bson::DateTime::from_millis(n * 1000).try_to_rfc3339_string().unwrap_or_default())
}

/// The entry of `KINDS` that describes this value, used as the starting type when editing it.
pub fn type_name_kind(b: &Bson) -> &'static str {
    match b {
        Bson::String(_) => "string",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::Double(_) => "double",
        Bson::Decimal128(_) => "decimal",
        Bson::Boolean(_) => "bool",
        Bson::DateTime(_) => "date",
        Bson::ObjectId(_) => "objectId",
        Bson::Null | Bson::Undefined => "null",
        Bson::Array(_) => "array",
        Bson::RegularExpression(_) => "regex",
        b if uuid_of(b).is_some() => "uuid",
        _ => "object",
    }
}

pub fn value_text(b: &Bson) -> String {
    match b {
        Bson::String(s) => s.clone(),
        Bson::ObjectId(o) => o.to_hex(),
        Bson::DateTime(t) => t.try_to_rfc3339_string().unwrap_or_default(),
        Bson::Null | Bson::Undefined => String::new(),
        Bson::Document(_) | Bson::Array(_) => compact(b),
        Bson::Int64(n) => n.to_string(),
        Bson::Decimal128(d) => d.to_string(),
        Bson::RegularExpression(r) => format!("/{}/{}", r.pattern, r.options),
        b if uuid_of(b).is_some() => uuid_of(b).unwrap().to_string(),
        other => compact(other),
    }
}

pub fn make_value(kind: &str, text: &str) -> Res<Bson> {
    let t = text.trim();
    let bad = |what: &str| format!("\"{t}\" is not a valid {what}");
    Ok(match kind {
        "string" => Bson::String(text.to_string()),
        "int" => Bson::Int32(t.parse().map_err(|_| bad("int (32-bit whole number)"))?),
        "long" => Bson::Int64(t.parse().map_err(|_| bad("long (64-bit whole number)"))?),
        "double" => Bson::Double(t.parse().map_err(|_| bad("double"))?),
        "decimal" => Bson::Decimal128(t.parse().map_err(|_| bad("decimal"))?),
        "bool" => Bson::Boolean(match t {
            "true" | "1" => true,
            "false" | "0" | "" => false,
            _ => return Err(bad("bool (true or false)")),
        }),
        "date" => Bson::DateTime(mongodb::bson::DateTime::parse_rfc3339_str(t).map_err(|_| bad("date (like 2024-05-01T12:00:00Z)"))?),
        "objectId" => Bson::ObjectId(mongodb::bson::oid::ObjectId::parse_str(t).map_err(|_| bad("ObjectId (24 hex characters)"))?),
        "null" => Bson::Null,
        "object" => Bson::Document(parse_doc(if t.is_empty() { "{}" } else { t })?),
        "array" => match parse_value(if t.is_empty() { "[]" } else { t })? {
            a @ Bson::Array(_) => a,
            _ => return Err(bad("array")),
        },
        "regex" => {
            // Accepts /pattern/flags or a bare pattern, like the original's new RegExp(text).
            let (pattern, flags) = match t.strip_prefix('/').and_then(|r| r.rfind('/').map(|i| (&r[..i], &r[i + 1..]))) {
                Some((p, f)) => (p, f),
                None => (t, ""),
            };
            let mut opts: Vec<char> = flags.chars().collect();
            opts.sort_unstable();
            Bson::RegularExpression(mongodb::bson::Regex { pattern: pattern.into(), options: opts.into_iter().collect() })
        }
        "uuid" => Bson::Binary(mongodb::bson::Binary::from_uuid(mongodb::bson::Uuid::parse_str(t).map_err(|_| bad("UUID"))?)),
        _ => return Err(format!("Unknown type {kind}")),
    })
}

/// Value at a dotted path such as `customer.name` or `items.0.sku`.
pub fn get_path<'a>(d: &'a Document, path: &str) -> Option<&'a Bson> {
    let mut cur: Option<&Bson> = None;
    for (i, seg) in path.split('.').enumerate() {
        cur = match (i, cur) {
            (0, _) => d.get(seg),
            (_, Some(Bson::Document(x))) => x.get(seg),
            (_, Some(Bson::Array(a))) => seg.parse::<usize>().ok().and_then(|n| a.get(n)),
            _ => None,
        };
        cur?;
    }
    cur
}

fn child_mut<'a>(b: &'a mut Bson, seg: &str) -> Option<&'a mut Bson> {
    match b {
        Bson::Document(d) => d.get_mut(seg),
        Bson::Array(a) => seg.parse::<usize>().ok().and_then(|n| a.get_mut(n)),
        _ => None,
    }
}

/// Sets (or with `None` removes) the value at `path` inside a copy of the document.
pub fn edit_path(d: &Document, path: &[String], value: Option<Bson>) -> Res<Document> {
    let mut root = Bson::Document(d.clone());
    let (last, parents) = path.split_last().ok_or("Empty path")?;
    let mut cur = &mut root;
    for seg in parents {
        cur = child_mut(cur, seg).ok_or_else(|| format!("No field {seg}"))?;
    }
    match (cur, value) {
        (Bson::Document(p), Some(v)) => {
            p.insert(last.clone(), v);
        }
        (Bson::Document(p), None) => {
            p.remove(last);
        }
        (Bson::Array(a), v) => {
            let i: usize = last.parse().map_err(|_| "Bad array index")?;
            match v {
                Some(v) if i < a.len() => a[i] = v,
                Some(v) => a.push(v),
                None if i < a.len() => {
                    a.remove(i);
                }
                None => {}
            }
        }
        _ => return Err("That value cannot hold fields".into()),
    }
    match root {
        Bson::Document(d) => Ok(d),
        _ => unreachable!(),
    }
}

pub struct IndexStat {
    pub name: String,
    pub ops: i64,
    /// When the counter started, in ms since the epoch.
    pub since: Option<i64>,
    pub hidden: bool,
    pub size: Option<usize>,
}

pub struct IndexInfo {
    pub stats: Vec<IndexStat>,
    /// Profiler level: 0 off, 1 slow operations, 2 all.
    pub level: i32,
    pub slowms: i64,
    pub shapes: Vec<Shape>,
}

/// Profiled queries that share filter fields and sort.
pub struct Shape {
    pub filter: Document,
    pub sort: Document,
    pub count: usize,
    pub avg_ms: f64,
    pub suggestion: Document,
}

/// Unused for a week; a younger counter proves nothing because it resets on restart.
pub fn is_unused(ops: i64, since_ms: Option<i64>, now_ms: i64) -> bool {
    ops == 0 && since_ms.is_some_and(|s| now_ms - s > 7 * 24 * 3600 * 1000)
}

/// Compound index for a query in the usual order: equality fields, then sort fields, then ranges.
pub fn suggest_index(filter: &Document, sort: &Document) -> Document {
    let mut eq = Vec::new();
    let mut range = Vec::new();
    for (k, v) in filter.iter().filter(|(k, _)| !k.starts_with('$')) {
        let is_eq = match v {
            Bson::Document(d) if d.keys().any(|k| k.starts_with('$')) => d.keys().any(|k| matches!(k.as_str(), "$eq" | "$in" | "$elemMatch")),
            _ => true,
        };
        if is_eq { eq.push(k) } else { range.push(k) }
    }
    let mut out = Document::new();
    for k in eq {
        out.insert(k, 1);
    }
    for (k, v) in sort {
        let dir = match v {
            Bson::Int32(n) if *n < 0 => -1,
            Bson::Int64(n) if *n < 0 => -1,
            Bson::Double(n) if *n < 0.0 => -1,
            _ => 1,
        };
        if !out.contains_key(k) {
            out.insert(k, dir);
        }
    }
    for k in range {
        if !out.contains_key(k) {
            out.insert(k, 1);
        }
    }
    out
}

/// Groups profile entries (`filter`, `sort`, `millis`) by query shape, busiest first.
pub fn group_profile(entries: &[Document]) -> Vec<Shape> {
    let mut groups: Vec<(String, Shape, f64)> = Vec::new();
    for en in entries {
        let filter = en.get_document("filter").cloned().unwrap_or_default();
        let sort = en.get_document("sort").cloned().unwrap_or_default();
        let suggestion = suggest_index(&filter, &sort);
        // A bare full scan has no field to index.
        if suggestion.is_empty() {
            continue;
        }
        let ms = match en.get("millis") {
            Some(Bson::Int32(n)) => *n as f64,
            Some(Bson::Int64(n)) => *n as f64,
            _ => 0.0,
        };
        let mut fields: Vec<&String> = filter.keys().collect();
        fields.sort();
        let key = format!("{fields:?}{}", compact(&Bson::Document(sort.clone())));
        match groups.iter_mut().find(|g| g.0 == key) {
            Some(g) => {
                g.1.count += 1;
                g.2 += ms;
            }
            None => groups.push((key, Shape { filter, sort, count: 1, avg_ms: 0.0, suggestion }, ms)),
        }
    }
    let mut out: Vec<Shape> = groups
        .into_iter()
        .map(|(_, mut s, total)| {
            s.avg_ms = total / s.count as f64;
            s
        })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then(b.avg_ms.total_cmp(&a.avg_ms)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(d: &Document) -> Vec<(String, i32)> {
        d.iter().map(|(k, v)| (k.clone(), v.as_i32().unwrap())).collect()
    }
    fn kv(v: &[(&str, i32)]) -> Vec<(String, i32)> {
        v.iter().map(|(k, d)| (k.to_string(), *d)).collect()
    }

    #[test]
    fn suggests_equality_only() {
        assert_eq!(keys(&suggest_index(&doc! { "a": 1, "b": "x" }, &doc! {})), kv(&[("a", 1), ("b", 1)]));
    }

    #[test]
    fn suggests_equality_then_sort() {
        assert_eq!(keys(&suggest_index(&doc! { "status": "a" }, &doc! { "createdAt": -1 })), kv(&[("status", 1), ("createdAt", -1)]));
    }

    #[test]
    fn suggests_equality_sort_range() {
        let f = doc! { "total": { "$lt": 50 }, "customer.tier": "gold" };
        assert_eq!(keys(&suggest_index(&f, &doc! { "createdAt": 1 })), kv(&[("customer.tier", 1), ("createdAt", 1), ("total", 1)]));
    }

    #[test]
    fn in_and_elem_match_count_as_equality() {
        let f = doc! { "n": { "$gt": 1 }, "t": { "$in": ["a", "b"] }, "l": { "$elemMatch": { "x": 1 } }, "$or": [{ "z": 1 }] };
        assert_eq!(keys(&suggest_index(&f, &doc! {})), kv(&[("t", 1), ("l", 1), ("n", 1)]));
    }

    #[test]
    fn sort_on_an_equality_field_keeps_its_place() {
        assert_eq!(keys(&suggest_index(&doc! { "a": 1 }, &doc! { "a": -1, "b": -1 })), kv(&[("a", 1), ("b", -1)]));
    }

    #[test]
    fn groups_profile_by_shape() {
        let e = |f: Document, s: Option<Document>, ms: i32| {
            let mut d = doc! { "filter": f, "millis": ms };
            if let Some(s) = s {
                d.insert("sort", s);
            }
            d
        };
        let entries = vec![e(doc! { "a": 1 }, None, 10), e(doc! { "a": 2 }, None, 30), e(doc! { "a": 1 }, Some(doc! { "b": 1 }), 5), e(doc! {}, None, 99)];
        let g = group_profile(&entries);
        assert_eq!(g.len(), 2);
        assert_eq!((g[0].count, g[0].avg_ms), (2, 20.0));
        assert_eq!(keys(&g[1].suggestion), kv(&[("a", 1), ("b", 1)]));
    }

    #[test]
    fn unused_needs_an_old_zero_counter() {
        let week = 7 * 24 * 3600 * 1000;
        assert!(is_unused(0, Some(0), week + 1));
        assert!(!is_unused(0, Some(0), week));
        assert!(!is_unused(3, Some(0), week * 2));
        assert!(!is_unused(0, None, week * 2));
    }

    #[test]
    fn explain_summary_find_with_index() {
        let d = parse_doc(r#"{queryPlanner: {winningPlan: {stage: "LIMIT"}}, executionStats: {nReturned: 50, executionTimeMillis: 0, totalKeysExamined: 50, totalDocsExamined: 50,
            executionStages: {stage: "LIMIT", nReturned: 50, executionTimeMillisEstimate: 0, inputStage: {stage: "FETCH", nReturned: 50, executionTimeMillisEstimate: 0, docsExamined: 50,
            inputStage: {stage: "IXSCAN", nReturned: 50, executionTimeMillisEstimate: 0, indexName: "status_1_createdAt_-1", keysExamined: 50}}}}}"#).unwrap();
        let s = explain_summary(&d);
        let stages: Vec<_> = s.rows.iter().map(|r| (r.depth, r.stage.as_str())).collect();
        assert_eq!(stages, [(0, "LIMIT"), (1, "FETCH"), (2, "IXSCAN")]);
        assert_eq!(s.rows[2].index, "status_1_createdAt_-1");
        assert_eq!((s.rows[2].keys, s.rows[1].docs, s.keys, s.docs, s.returned), (Some(50), Some(50), 50, 50, 50));
        assert!(s.warnings.is_empty());
    }

    #[test]
    fn explain_summary_collscan_and_ratio() {
        let d = parse_doc(r#"{executionStats: {nReturned: 3, executionTimeMillis: 41, totalKeysExamined: 0, totalDocsExamined: 20000,
            executionStages: {stage: "COLLSCAN", nReturned: 3, executionTimeMillisEstimate: 40, docsExamined: 20000}}}"#).unwrap();
        let s = explain_summary(&d);
        assert_eq!((s.rows.len(), s.ms), (1, 41));
        assert_eq!(s.warnings.len(), 2);
        assert!(s.warnings[0].starts_with("COLLSCAN"));
    }

    #[test]
    fn explain_summary_slot_based_aggregate() {
        // MongoDB 8 reports lowercase slot-based stages for aggregates, with reads on fetch.
        let d = parse_doc(r#"{explainVersion: "2", queryPlanner: {winningPlan: {queryPlan: {stage: "GROUP"}}},
            executionStats: {nReturned: 125, executionTimeMillis: 8, totalKeysExamined: 5000, totalDocsExamined: 5000,
            executionStages: {stage: "project", nReturned: 125, executionTimeMillisEstimate: 6, inputStage: {stage: "group", nReturned: 125, executionTimeMillisEstimate: 6,
            inputStage: {stage: "fetch", nReturned: 5000, executionTimeMillisEstimate: 6, numReads: 5000,
            inputStage: {stage: "ixseek", nReturned: 5000, executionTimeMillisEstimate: 0, indexName: "status_1_createdAt_-1", keysExamined: 5000}}}}}}"#).unwrap();
        let s = explain_summary(&d);
        let stages: Vec<_> = s.rows.iter().map(|r| r.stage.as_str()).collect();
        assert_eq!(stages, ["project", "group", "fetch", "ixseek"]);
        assert_eq!((s.rows[2].docs, s.rows[3].keys, s.rows[3].index.as_str()), (Some(5000), Some(5000), "status_1_createdAt_-1"));
        // A $group returns few results by design, so no selectivity warning.
        assert!(s.warnings.is_empty());
    }

    #[test]
    fn explain_summary_cursor_wrapper_and_shards() {
        let cursor = r#"{stages: [{$cursor: {executionStats: {nReturned: 2, executionTimeMillis: 1, totalKeysExamined: 2, totalDocsExamined: 2,
            executionStages: {stage: "FETCH", nReturned: 2, docsExamined: 2, inputStage: {stage: "IXSCAN", nReturned: 2, indexName: "a_1", keysExamined: 2}}}}}, {$group: {}}]}"#;
        let one = explain_summary(&parse_doc(cursor).unwrap());
        assert_eq!(one.rows.len(), 2);
        let c = parse_doc(cursor).unwrap();
        let sharded = doc! { "shards": { "a": c.clone(), "b": c } };
        let s = explain_summary(&sharded);
        assert_eq!((s.rows.len(), s.keys, s.docs, s.returned, s.ms), (4, 4, 4, 4, 2));
    }

    #[test]
    fn explain_summary_sharded_find_and_sort() {
        let d = parse_doc(r#"{executionStats: {nReturned: 4, totalDocsExamined: 4, executionStages: {stage: "SHARD_MERGE", nReturned: 4, shards: [
            {shardName: "s1", executionStages: {stage: "SORT", nReturned: 2, inputStage: {stage: "COLLSCAN", nReturned: 2, docsExamined: 2}}},
            {shardName: "s2", executionStages: {stage: "FETCH", nReturned: 2, inputStage: {stage: "SORT_KEY_GENERATOR", nReturned: 2}}}]}}}"#).unwrap();
        let s = explain_summary(&d);
        assert_eq!(s.rows.len(), 5);
        assert_eq!(s.warnings.len(), 2);
        assert!(explain_summary(&doc! { "ok": 1 }).rows.is_empty());
    }

    #[test]
    fn stage_preview_cuts_limits_and_skips_writes() {
        let p = vec![doc! { "$match": {} }, doc! { "$out": "x" }, doc! { "$group": { "_id": 1 } }];
        assert_eq!(stage_preview(&p, 2).unwrap(), vec![doc! { "$match": {} }, doc! { "$group": { "_id": 1 } }, doc! { "$limit": 20 }]);
        assert_eq!(stage_preview(&p, 0).unwrap().len(), 2);
        assert!(stage_preview(&p, 3).is_none());
    }

    fn stat(path: &str, docs: usize, types: &[&'static str]) -> FieldStat {
        FieldStat { path: path.into(), depth: path.matches('.').count(), docs, types: types.iter().map(|t| (*t, 1)).collect(), examples: Vec::new() }
    }

    #[test]
    fn driver_errors_drop_labels_and_raw_reply() {
        let raw = "Kind: Command failed: Error code 59 (CommandNotFound): no such command: 'nope', labels: {}, source: None, server response: Some(RawDocumentBuf { data: \"5d00\" })";
        assert_eq!(e(raw), "Command failed: Error code 59 (CommandNotFound): no such command: 'nope'");
        assert_eq!(e("plain"), "plain");
    }

    #[test]
    fn schema_from_stats_maps_types_and_required() {
        let s = vec![stat("_id", 10, &["objectId"]), stat("age", 10, &["int", "string"]), stat("nick", 4, &["string"]), stat("tags", 10, &["array"])];
        let js = schema_from_stats(&s, 10);
        let js = js.get_document("$jsonSchema").unwrap();
        let names = |a: &Vec<Bson>| a.iter().map(|b| b.as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(names(js.get_array("required").unwrap()), ["_id", "age", "tags"]);
        let p = js.get_document("properties").unwrap();
        assert_eq!(names(p.get_document("age").unwrap().get_array("bsonType").unwrap()), ["int", "string"]);
        assert_eq!(p.get_document("tags").unwrap().get_str("bsonType").unwrap(), "array");
        assert_eq!(p.get_document("nick").unwrap().get_str("bsonType").unwrap(), "string");
    }

    #[test]
    fn schema_from_stats_nests_objects_but_not_arrays() {
        let s = vec![stat("addr", 5, &["object"]), stat("addr.city", 5, &["string"]), stat("addr.zip", 2, &["string"]), stat("list", 5, &["array"]), stat("list.x", 5, &["int"])];
        let js = schema_from_stats(&s, 10);
        let top = js.get_document("$jsonSchema").unwrap();
        assert!(top.get("required").is_none());
        let p = top.get_document("properties").unwrap();
        let addr = p.get_document("addr").unwrap();
        assert_eq!(addr.get_array("required").unwrap(), &vec![Bson::String("city".into())]);
        assert!(addr.get_document("properties").unwrap().get_document("zip").is_ok());
        assert!(p.get_document("list").unwrap().get("properties").is_none());
    }

    #[test]
    fn parses_shell_syntax() {
        let d = parse_doc(r#"{_id: ObjectId("64b7f0c2a1b2c3d4e5f60718"), n: NumberLong(5), 'a': {$gt: 2}, s: "ObjectId(x)",}"#).unwrap();
        assert!(matches!(d.get("_id"), Some(Bson::ObjectId(_))));
        assert_eq!(d.get("n"), Some(&Bson::Int64(5)));
        assert_eq!(d.get_str("s").unwrap(), "ObjectId(x)");
        assert!(parse_doc("{a: new Date('2024-01-02T03:04:05Z')}").unwrap().get_datetime("a").is_ok());
        assert!(parse_doc("[1]").is_err());
        assert_eq!(parse_pipeline("[{$match: {}}, {$out: 'x'}]").map(|p| has_write_stage(&p)), Ok(true));
    }

    #[test]
    fn edit_json_round_trips_types() {
        let d = doc! { "i": 1i32, "l": 2i64, "f": 3.0f64, "o": { "x": 1i64 } };
        assert_eq!(parse_doc(&to_edit_json(&d)).unwrap(), d);
    }

    #[test]
    fn diff_and_table() {
        let a = doc! { "x": 1, "o": { "y": 1, "z": 2 } };
        let b = doc! { "x": 1, "o": { "y": 2 }, "w": true };
        let paths: Vec<String> = diff(&a, &b).into_iter().map(|c| c.0).collect();
        assert_eq!(paths, ["o.y", "o.z", "w"]);
        let t = to_table(&[doc! { "a": "x,\"y\"" }, doc! { "b": false }], ',');
        assert_eq!(t, "a,b\n\"x,\"\"y\"\"\",\n,false");
        assert_eq!(mask_uri("mongodb://u:secret@h:27017/db"), "mongodb://u:****@h:27017/db");
    }

    #[test]
    fn update_stage_building() {
        let st = update_stages(doc! { "a.b": 1 }, vec!["x".into(), "y.z".into()]);
        assert_eq!(st, vec![doc! { "$set": { "a.b": 1 } }, doc! { "$unset": ["x", "y.z"] }]);
        assert_eq!(update_stages(Document::new(), vec!["x".into()]), vec![doc! { "$unset": ["x"] }]);
        assert!(update_stages(Document::new(), vec![]).is_empty());
    }

    #[test]
    fn shell_queries() {
        let s = parse_shell(
            r#"db.approvals.aggregate([
  { $match: { travelID: { $gt: "" } } },
  { $group: { _id: "$travelID", n: { $sum: 1 }, ids: { $push: "$_id" }, statuses: { $push: "$status" } } },
  { $match: { n: { $gt: 1 } } }
])"#,
        )
        .unwrap();
        assert_eq!(s.coll, "approvals");
        assert!(matches!(s.kind, ShellKind::Aggregate(ref p) if p.len() == 3));
        let s = parse_shell("db.getCollection('a.b').find({x: 'y)'}, {x: 1}).sort({_id: -1}).skip(5).limit(10);").unwrap();
        assert_eq!(s.coll, "a.b");
        let ShellKind::Find(q) = s.kind else { panic!() };
        assert_eq!((q.skip, q.limit, q.filter.get_str("x").unwrap()), (5, Some(10), "y)"));
        assert!(q.sort.is_some() && q.projection.is_some());
        assert!(parse_shell("db.x.drop()").is_err());
    }

    #[test]
    fn field_actions() {
        assert_eq!(strip_indices("items.2.sku"), "items.sku");
        assert_eq!(field_size(&Bson::String("abc".into())), 5);
        let d = doc! { "a b": 1, "_id": ObjectIdLike(), "n": 5i64 };
        assert_eq!(parse_doc(&shell_text(&Bson::Document(d.clone()))).unwrap(), d);
        let p = group_count_pipeline("items.0.sku", &Bson::Null, doc! {}).unwrap();
        assert_eq!(p[1].get_str("$unwind").unwrap(), "$items");
        assert!(group_count_pipeline("a.0.b.1.c", &Bson::Null, doc! {}).is_err());
        assert_eq!(group_count_pipeline("tags", &Bson::Array(vec![]), doc! {}).unwrap().len(), 4);
    }

    #[allow(non_snake_case)]
    fn ObjectIdLike() -> Bson {
        Bson::ObjectId(mongodb::bson::oid::ObjectId::parse_str("6650f1c2a1b2c3d4e5f60700").unwrap())
    }

    /// The driver must go through the SOCKS5 port and ask for the server by name (so replica
    /// sets and TLS keep working); a stand-in proxy records the request and refuses it.
    #[test]
    fn connects_through_socks5_by_name() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let seen = tokio::spawn(async move {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut head = [0u8; 2];
                s.read_exact(&mut head).await.unwrap();
                let mut m = vec![0u8; head[1] as usize];
                s.read_exact(&mut m).await.unwrap();
                s.write_all(&[5, 0]).await.unwrap();
                let mut req = [0u8; 5];
                s.read_exact(&mut req).await.unwrap();
                assert_eq!(req[3], 3, "the host should be sent as a name");
                let mut name = vec![0u8; req[4] as usize];
                s.read_exact(&mut name).await.unwrap();
                let mut p = [0u8; 2];
                s.read_exact(&mut p).await.unwrap();
                let _ = s.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).await;
                (String::from_utf8(name).unwrap(), u16::from_be_bytes(p))
            });
            let net = Net { ssh: None, proxy: Some(Proxy { host: "127.0.0.1".into(), port, user: String::new(), password: None }) };
            let r = open_client(0, "mongodb://db.internal.example:27018/", Duration::from_millis(1500), &net).await;
            assert!(r.is_err());
            assert_eq!(seen.await.unwrap(), ("db.internal.example".to_string(), 27018));
        });
    }

    #[test]
    fn random_ids() {
        let a = random_id(17);
        assert_eq!(a.len(), 17);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(a, random_id(17));
    }

    #[test]
    fn import_and_schema() {
        let csv = CsvOptions { separator: None, header: true, camel_case: true };
        let d = parse_import("First Name;age;ok;when;note\n\"Ann; B\";42;TRUE;2024-01-02T03:04:05Z;\n\nBob;3.5;false;x;\"say \"\"hi\"\"\"\n", ImportKind::Csv, &csv).unwrap();
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].get_str("firstName").unwrap(), "Ann; B");
        assert_eq!(d[0].get("age"), Some(&Bson::Int32(42)));
        assert_eq!(d[0].get("ok"), Some(&Bson::Boolean(true)));
        assert!(d[0].get_datetime("when").is_ok());
        assert_eq!(d[0].get("note"), Some(&Bson::Null));
        assert_eq!(d[1].get("age"), Some(&Bson::Double(3.5)));
        assert_eq!(d[1].get_str("note").unwrap(), "say \"hi\"");
        assert_eq!(csv_value("007"), Bson::String("007".into()));
        let j = parse_import("{a: 1}\n\n{a: 2, b: ObjectId('6650f1c2a1b2c3d4e5f60700')}", ImportKind::JsonLines, &csv).unwrap();
        assert_eq!(j.len(), 2);
        assert!(parse_import("[1]", ImportKind::Json, &csv).is_err());
        let s = analyze_schema(&[doc! { "a": 1, "o": { "x": "s" }, "l": [{ "y": 1 }, { "y": 2 }] }, doc! { "a": "two" }]);
        let a = s.iter().find(|f| f.path == "a").unwrap();
        assert_eq!((a.docs, a.types.len()), (2, 2));
        let y = s.iter().find(|f| f.path == "l.y").unwrap();
        assert_eq!((y.docs, y.depth), (1, 1));
    }

    #[test]
    fn typed_edits() {
        let d = doc! { "a": { "b": [1, 2] }, "c": "x" };
        let e = edit_path(&d, &["a".into(), "b".into(), "1".into()], Some(make_value("long", "7").unwrap())).unwrap();
        assert_eq!(get_path(&e, "a.b.1"), Some(&Bson::Int64(7)));
        let e = edit_path(&e, &["c".into()], None).unwrap();
        assert!(!e.contains_key("c"));
        assert!(make_value("int", "x").is_err());
        assert!(make_value("objectId", "6650f1c2a1b2c3d4e5f60700").is_ok());
        let r = make_value("regex", "/ab+c/mi").unwrap();
        assert_eq!((type_name_kind(&r), value_text(&r)), ("regex", "/ab+c/im".to_string()));
        let u = make_value("uuid", "67e55044-10b1-426f-9247-bb680e5fe0c8").unwrap();
        assert_eq!(type_name_kind(&u), "uuid");
        assert!(possible_timestamp(&Bson::Int64(1_700_000_000)).is_some() && possible_timestamp(&Bson::Int32(42)).is_none());
    }

    #[test]
    fn compare_relations_and_tools() {
        let a = vec![doc! { "_id": 1, "x": 1, "y": 2 }, doc! { "_id": 2, "x": 1 }, doc! { "_id": 3 }];
        let b = vec![doc! { "_id": 1, "y": 2, "x": 1 }, doc! { "_id": 2, "x": 2 }, doc! { "_id": 4 }];
        let r = compare_docs(a, b);
        assert_eq!((r.same, r.changed.len(), r.only_src.len(), r.only_dst.len()), (1, 1, 1, 1));
        assert_eq!(preview_field(&doc! { "_id": 1, "owner": "6650f1c2a1b2c3d4e5f60700", "name": "Ada" }), "name");
        assert_eq!(values_at(&[doc! { "t": [1, 2] }, doc! { "t": 2 }], "t"), vec![Bson::Int32(1), Bson::Int32(2)]);
        assert_eq!(values_at(&[doc! { "items": [{ "p": 1 }, { "p": 3 }] }], "items.p"), vec![Bson::Int32(1), Bson::Int32(3)]);
        let oid = Bson::ObjectId(mongodb::bson::oid::ObjectId::new());
        let found = proto_relations(&[doc! { "_id": oid.clone(), "owner": oid.clone(), "code": "abc123", "items": [{ "product": oid.clone() }], "tags": [oid.clone()] }], 6);
        let paths: Vec<&str> = found.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, ["owner", "code", "items.product", "tags"]);
        assert_eq!(uri_without_db("mongodb://u:p@h:1/shop?tls=true"), "mongodb://u:p@h:1/?tls=true");
        assert_eq!(uri_without_db("mongodb+srv://h/shop"), "mongodb+srv://h/");
        assert_eq!(uri_without_db("mongodb://h:1"), "mongodb://h:1");
        assert_eq!(dump_args("mongodb://h", "shop", Some("orders"), "/tmp/a", true), ["--uri=mongodb://h", "--archive=/tmp/a", "--db=shop", "--collection=orders", "--gzip", "--forceTableScan"]);
        assert_eq!(restore_args("mongodb://h/shop", "/tmp/a", "shop", None, "copy", true, false), ["--uri=mongodb://h/", "--archive=/tmp/a", "--nsInclude=shop.*", "--nsFrom=shop.*", "--nsTo=copy.*", "--drop"]);
    }

    fn code_query() -> Query {
        let id = mongodb::bson::oid::ObjectId::parse_str("64b7f0c2a1b2c3d4e5f60718").unwrap();
        let at = mongodb::bson::DateTime::parse_rfc3339_str("2024-01-02T03:04:05Z").unwrap();
        Query { filter: doc! { "_id": id, "at": { "$gt": at }, "n": 5i64 }, sort: Some(doc! { "at": -1 }), projection: Some(doc! { "n": 1 }), skip: 10, limit: Some(20) }
    }

    #[test]
    fn driver_code_embeds_extended_json() {
        let what = CodeOf::Find(code_query());
        for (lang, parser) in [(Lang::Node, "EJSON.parse("), (Lang::Python, "json_util.loads("), (Lang::Java, "Document.parse("), (Lang::Go, "bson.UnmarshalExtJSON(js, false, v)"), (Lang::Rust, "serde_json::from_str(")] {
            let code = driver_code(lang, "shop", "orders", &what);
            assert!(code.contains(parser), "{}", lang.name());
            assert!(code.contains("\"$oid\": \"64b7f0c2a1b2c3d4e5f60718\""), "{}", lang.name());
            assert!(code.contains("\"$date\": \"2024-01-02T03:04:05Z\""), "{}", lang.name());
            assert!(code.contains("20") && code.contains("10"), "{}", lang.name());
        }
        let sh = driver_code(Lang::Mongosh, "shop", "orders", &what);
        assert!(sh.starts_with("db.orders.find({") && sh.ends_with(".sort({ at: -1 }).skip(10).limit(20)"), "{sh}");
        assert!(driver_code(Lang::Mongosh, "shop", "my-coll", &what).starts_with("db.getCollection(\"my-coll\").find("));
    }

    #[test]
    fn driver_code_pipeline_and_rust_json_reads_back() {
        let p = vec![doc! { "$match": { "_id": mongodb::bson::oid::ObjectId::parse_str("64b7f0c2a1b2c3d4e5f60718").unwrap() } }, doc! { "$limit": 3 }];
        let what = CodeOf::Aggregate(p.clone());
        assert!(driver_code(Lang::Node, "shop", "orders", &what).contains("coll.aggregate(pipeline)"));
        assert!(driver_code(Lang::Python, "shop", "orders", &what).contains("json_util.loads(r\"\"\"["));
        assert!(driver_code(Lang::Java, "shop", "orders", &what).contains("{ \"pipeline\": ["));
        assert!(driver_code(Lang::Go, "shop", "orders", &what).contains("coll.Aggregate(ctx, wrapped.Pipeline)"));
        assert!(driver_code(Lang::Mongosh, "shop", "orders", &what).starts_with("db.orders.aggregate([{ $match:"));
        // The literal the Rust snippet embeds must parse with the Rust driver's own types.
        let rust = driver_code(Lang::Rust, "shop", "orders", &what);
        let json = rust.split("r#\"").nth(1).unwrap().split("\"#").next().unwrap();
        assert_eq!(serde_json::from_str::<Vec<Document>>(json).unwrap(), p);
        let q = code_query();
        let json = to_edit_json(&q.filter);
        assert_eq!(serde_json::from_str::<Document>(&json).unwrap(), q.filter);
    }
}


