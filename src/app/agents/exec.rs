//! The executor of agent calls. Everything here runs on the UI thread and only decides: parse, find the connection,
//! check the rules and the guard, ask the policy, then answer at once, queue an approval, or start the operation on
//! the Worker runtime. Operations answer the agent through the oneshot themselves and report back with
//! `AgentEvent::Done`, so the Activity list, the pill and the audit log stay here.

use super::*;
use crate::agents::audit::{self, Entry, Summary};
use crate::agents::config::OpClass;
use crate::agents::guard;
use crate::agents::link::ToolErr;
use crate::agents::policy::{self, AskWhy, Decision, Grants, LiveState};
use crate::agents::scrub::Rules;
use crate::agents::tools::{self, Call, ConnRef};
use db::sql::Dialect;
use std::time::{Duration, Instant};

type Reply = tokio::sync::oneshot::Sender<Result<Value, ToolErr>>;

/// Calls kept in the Activity list; the audit log on disk keeps the rest.
pub(in crate::app) const ACTIVITY_MAX: usize = 500;
/// The TypeScript MCP SDK gives up on a request after 60 s, so Quarry answers first.
const ASK_WAIT: Duration = Duration::from_secs(50);
/// How long sampled MongoDB keys stand in for a collection's fields.
const KEYS_FRESH: Duration = Duration::from_secs(300);
/// When the sidebar looks again at a structure change still running on the server after the time limit.
const RECHECK: Duration = Duration::from_secs(30);
/// Arguments kept for the detail view and the audit log.
const ARGS_SHOWN: usize = audit::ARGS_MAX;

pub(in crate::app) const OFF: &str = "AI agents are off in Quarry Settings.";
pub(in crate::app) const DENIED: &str = "The user denied this in Quarry.";
pub(in crate::app) const NO_ANSWER: &str = "No answer in Quarry within 50 s.";
const CLOSED: &str = "The connection closed before the call could run. Open it in Quarry, then try again.";

/// The SQL dialect of a connection, from its engine while it has not connected yet; None on MongoDB.
fn dialect_of(c: &Conn) -> Option<Dialect> {
    match (&c.sql, c.meta.engine) {
        (Some(s), _) => Some(s.dialect),
        (None, db::sql::Engine::Mongo) => None,
        (None, db::sql::Engine::MySql) => Some(Dialect::MySql),
        (None, db::sql::Engine::Postgres) => Some(Dialect::Postgres),
    }
}

/// A call in the approval queue: what runs once the user allows it.
struct Pending {
    call: Call,
    class: OpClass,
    conn: u64,
    session: u64,
    why: AskWhy,
    reply: Reply,
    /// The agent's stated reason, for the dialog.
    reason: Option<String>,
    /// For a write: what it touches, read before it was decided.
    snap: Option<db::Snapshot>,
    /// The connection was unlocked when the call was queued, which is what the dialog showed; set by `agent_queue`.
    writable: bool,
}

/// A call whose operation runs on the runtime.
struct Running {
    run: db::AgentRun,
    conn: u64,
    approved: bool,
    /// It changes the server: a cancel lets it run to its end, so what it changed keeps its undo, or for changes no undo
    /// covers (structure, commands, free SQL that writes, the shell) so it never stops half way unrecorded.
    write: bool,
    /// Namespaces it changes, whose open tabs say so once it is done.
    targets: Vec<tools::Target>,
    /// It may make or drop a collection or table, so the sidebar lists them again once it is done.
    structure: bool,
}

/// The executor's own state, beside what the windows read in `AgentsState`.
pub(in crate::app) struct Exec {
    pending: HashMap<u64, Pending>,
    /// Writes whose snapshot is being read, before the policy decides on them.
    preparing: HashMap<u64, Pending>,
    running: HashMap<u64, Running>,
    /// (link session, link call id) to Activity id, so a cancel finds its call.
    calls: HashMap<(u64, u64), u64>,
    /// What the user allowed "for this session", per (link session, connection).
    grants: HashMap<(u64, u64), Grants>,
    /// Compiled rules per connection with the settings they came from, compiled again once those change. An error is
    /// kept too: a rule that does not compile refuses every call there.
    rules: HashMap<u64, (u64, Result<Arc<Rules>, String>)>,
    /// Keys a MongoDB describe sampled, per (connection, db, collection).
    keys: HashMap<(u64, String, String), (Instant, Vec<String>)>,
    pub(in crate::app) audit: Option<std::sync::mpsc::Sender<Entry>>,
    next: u64,
    /// How long an approval waits; tests shorten it.
    pub(in crate::app) ask_wait: Duration,
    /// The key of the Hash scrub action. Derived from the settings key when the keychain gives one, so hashes match
    /// across launches; random otherwise, and in the demo and the tests.
    pub(in crate::app) hash_key: [u8; 32],
}

impl Default for Exec {
    fn default() -> Self {
        Exec { pending: HashMap::new(), preparing: HashMap::new(), running: HashMap::new(), calls: HashMap::new(), grants: HashMap::new(), rules: HashMap::new(), keys: HashMap::new(), audit: None, next: 1, ask_wait: ASK_WAIT, hash_key: crate::agents::link::random() }
    }
}

/// The scrub hash key, derived from the settings key so neither can be learned from the other.
pub(in crate::app) fn hash_key_from(k: &[u8; 32]) -> [u8; 32] {
    use hmac::{Hmac, KeyInit, Mac};
    let mut m = <Hmac<sha2::Sha256> as KeyInit>::new_from_slice(k).expect("HMAC takes any key length");
    m.update(b"quarry-agents-scrub-hash");
    let mut out = [0u8; 32];
    out.copy_from_slice(&m.finalize().into_bytes());
    out
}

/// Local wall-clock time of a Unix time, "22:41:07".
fn clock(secs: u64) -> String {
    let s = secs as i64;
    let off = db::sql::advanced::local_offset_at(s).unwrap_or(0) as i64;
    crate::db::dates::hms((s + off).rem_euclid(86_400))
}

/// "db.coll", "schema.table", a database, or empty.
fn ns_label(call: &Call) -> String {
    match (call.ns(), call) {
        (Some(ns), _) => format!("{}.{}", ns.db, ns.coll),
        (None, Call::ListCollections { db, .. } | Call::RunCommand { db, .. }) => db.clone(),
        (None, Call::QuerySql { db: Some(db), .. } | Call::RunSql { db: Some(db), .. } | Call::RunShell { db: Some(db), .. }) => db.clone(),
        _ => String::new(),
    }
}

/// Tools that only make sense on one kind of server: Some(true) for MongoDB, Some(false) for SQL, None for both.
fn for_mongo(call: &Call) -> Option<bool> {
    use Call as C;
    match call {
        C::Find { .. } | C::Aggregate { .. } | C::Count { .. } | C::Explain { .. } | C::Insert { .. } | C::Update { .. } | C::Delete { .. } | C::AggregateInto { .. } | C::CreateCollection { .. } | C::DropCollection { .. } | C::RunCommand { .. } => Some(true),
        C::SelectRows { .. } | C::QuerySql { .. } | C::SqlInsertRows { .. } | C::SqlUpdateRows { .. } | C::SqlDeleteRows { .. } | C::CreateTable { .. } | C::DropTable { .. } | C::RunSql { .. } => Some(false),
        _ => None,
    }
}

/// System schemas left out when collecting the columns free SQL could reach: the policy refuses them by name, and
/// information_schema holds names only.
const MY_SYSTEM: [&str; 4] = ["mysql", "performance_schema", "sys", "information_schema"];

impl App {
    fn agent_id(&mut self) -> u64 {
        let id = self.agents.exec.next;
        self.agents.exec.next += 1;
        id
    }

    fn agent_push(&mut self, a: Activity) {
        self.agents.activity.push_back(a);
        while self.agents.activity.len() > ACTIVITY_MAX {
            self.agents.activity.pop_front();
        }
    }

    fn agent_activity(&mut self, id: u64) -> Option<&mut Activity> {
        self.agents.activity.iter_mut().rev().find(|a| a.id == id)
    }

    /// A call refused before it ran: the agent gets the reason as a tool error, the Activity list and the log a line.
    fn agent_refuse(&mut self, mut act: Activity, reply: Reply, code: &str, why: String) {
        let _ = reply.send(Err(ToolErr::new(code, why.clone())));
        act.outcome = Outcome::Refused(why);
        self.agent_audit(&act, "refused", Summary::default());
        self.agent_push(act);
    }

    /// The shared connection a call names, by id or by name.
    fn agent_conn(&self, r: &ConnRef) -> Result<u64, String> {
        let shared = |id: u64| self.agents.cfg.conn(id).is_some() && self.conn(id).is_some();
        match r {
            ConnRef::Id(id) if shared(*id) => Ok(*id),
            ConnRef::Id(id) => Err(format!("No connection with id {id} is shared with agents. Call list_connections.")),
            ConnRef::Name(n) => {
                if let Ok(id) = n.parse::<u64>()
                    && shared(id)
                {
                    return Ok(id);
                }
                let exact: Vec<u64> = self.conns.iter().filter(|c| c.meta.name == *n && shared(c.meta.id)).map(|c| c.meta.id).collect();
                let found = if exact.is_empty() { self.conns.iter().filter(|c| c.meta.name.eq_ignore_ascii_case(n) && shared(c.meta.id)).map(|c| c.meta.id).collect() } else { exact };
                match found.as_slice() {
                    [id] => Ok(*id),
                    [] => Err(format!("No connection named {n} is shared with agents. Call list_connections.")),
                    _ => Err(format!("Several shared connections are named {n}. Pass its id from list_connections.")),
                }
            }
        }
    }

    /// The connection's compiled rules, from the cache while the settings they came from are unchanged.
    fn agent_rules(&mut self, conn: u64) -> Result<Arc<Rules>, String> {
        let rev = self.agents.rev;
        if let Some((v, r)) = self.agents.exec.rules.get(&conn)
            && *v == rev
        {
            return r.clone();
        }
        let cfg = &self.agents.cfg;
        let p = cfg.conn(conn);
        let r = Rules::from_config(cfg, p, &self.agents.exec.hash_key).map(Arc::new).map_err(|e| format!("The data rules for this connection do not compile, so Quarry refuses every agent call on it until they are fixed in Settings > AI agents. {e}"));
        self.agents.exec.rules.insert(conn, (rev, r.clone()));
        r
    }

    /// The arguments for the detail view and the audit log: text fields read as documents where they parse, every
    /// document scrubbed by the namespace's rules, then the value rules over the whole text, cut to 4 KB.
    fn agent_args(&self, rules: Option<&Rules>, args: &Value, call: Option<&Call>) -> String {
        let fallback;
        let rules = match rules {
            Some(r) => r,
            None => match Rules::from_config(&self.agents.cfg, None, &self.agents.exec.hash_key) {
                Ok(r) => {
                    fallback = r;
                    &fallback
                }
                // Global rules that do not compile scrub nothing, so nothing is shown.
                Err(_) => return "[hidden: the data rules do not compile]".into(),
            },
        };
        let nsr = match call.and_then(Call::ns) {
            Some(ns) => rules.for_ns(&ns.db, &ns.coll),
            None => rules.all(),
        };
        let mut v = args.clone();
        if let Value::Object(m) = &mut v {
            m.remove("reason");
            for (k, x) in m.iter_mut() {
                let Value::String(s) = x else { continue };
                let parsed = if k == "pipeline" { db::parse_pipeline(s).ok().map(|p| Bson::Array(p.into_iter().map(Bson::Document).collect())) } else if matches!(k.as_str(), "filter" | "projection" | "sort" | "keys" | "command" | "update") { db::parse_doc(s).ok().map(Bson::Document) } else { None };
                if let Some(b) = parsed {
                    *x = b.into_relaxed_extjson();
                }
            }
        }
        let text = match Bson::try_from(v.clone()) {
            Ok(Bson::Document(mut d)) => {
                nsr.scrub_doc(&mut d);
                Bson::Document(d).into_relaxed_extjson().to_string()
            }
            _ => v.to_string(),
        };
        let s = nsr.scrub_text(&text);
        if s.len() <= ARGS_SHOWN {
            return s;
        }
        let mut end = ARGS_SHOWN - 3;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }

    /// The fields free-form text could reach, per namespace, or None when any of them is not known yet, which the
    /// policy then reads strictly. Missing SQL catalogs start loading so a later call can pass.
    fn agent_known(&mut self, conn: u64, call: &Call) -> Option<Vec<((String, String), Vec<String>)>> {
        let c = self.conn(conn)?;
        if let Some(sql) = c.sql.clone() {
            let pg = sql.dialect == Dialect::Postgres;
            let database = sql.pg_database();
            let spaces: Vec<String> = if pg {
                c.dbs.iter().find(|d| d.0 == database)?.1.clone()?
            } else {
                if c.dbs.is_empty() {
                    return None;
                }
                c.dbs.iter().map(|d| d.0.clone()).filter(|d| !MY_SYSTEM.contains(&d.to_ascii_lowercase().as_str())).collect()
            };
            let mut out = Vec::new();
            let mut missing = Vec::new();
            for s in spaces {
                let key = if pg { ns_key(&database, &s) } else { s.clone() };
                match self.sql_schema.get(&(conn, key.clone())) {
                    Some(sch) => out.extend(sch.tables.iter().map(|t| ((s.clone(), t.name.clone()), t.cols.iter().map(|c| c.name.clone()).collect()))),
                    None => missing.push((key, s)),
                }
            }
            if missing.is_empty() {
                return Some(out);
            }
            for (key, s) in missing {
                if self.sql_schema_asked.insert((conn, key)) {
                    self.worker.sql_schema(conn, sql.at(&database), s);
                }
            }
            return None;
        }
        // MongoDB: every collection of the database must have a fresh sample, which describe takes.
        let db = match call {
            Call::RunCommand { db, .. } => db.clone(),
            Call::RunShell { db: Some(db), .. } => db.clone(),
            _ => return None,
        };
        let colls = c.dbs.iter().find(|d| d.0 == db)?.1.clone()?;
        let mut out = Vec::new();
        for coll in colls {
            let (at, keys) = self.agents.exec.keys.get(&(conn, db.clone(), coll.clone()))?;
            if at.elapsed() > KEYS_FRESH {
                return None;
            }
            out.push(((db.clone(), coll), keys.clone()));
        }
        Some(out)
    }

    /// Everything that decides a call, run when it arrives and again when the user allows it, since the connection
    /// may have been locked, closed or unshared meanwhile.
    fn agent_check(&mut self, call: &Call, conn: u64, session: u64, matched: Option<u64>) -> Result<(Arc<Rules>, OpClass, Decision), (String, &'static str)> {
        let Some(c) = self.conn(conn) else { return Err(("This connection was removed from Quarry.".into(), "refused")) };
        if self.agents.cfg.conn(conn).is_none() {
            return Err(("This connection is not shared with agents. The user can share it in Settings > AI agents.".into(), "refused"));
        }
        let mongo = c.meta.engine == db::sql::Engine::Mongo;
        let dialect = dialect_of(c);
        let live = LiveState { connected: c.status == Status::Ready, writable: c.writable, always_writable: self.prefs.always_writable, sql: dialect };
        match for_mongo(call) {
            Some(true) if !mongo => return Err(("This tool works on MongoDB connections. Use select_rows, query_sql or run_shell on SQL connections.".into(), "wrong_engine")),
            Some(false) if mongo => return Err(("This tool works on SQL connections. Use find, aggregate or count on MongoDB.".into(), "wrong_engine")),
            _ => {}
        }
        let class = tools::classify(call, dialect);
        let rules = self.agent_rules(conn).map_err(|e| (e, "rules"))?;
        guard::check(call, &rules).map_err(|e| (format!("{e}."), "guard"))?;
        let hides = call.is_free_text() && {
            let known = self.agent_known(conn, call);
            policy::hides_data(&rules, call, known.as_deref())
        };
        let grants = self.agents.exec.grants.get(&(session, conn)).cloned().unwrap_or_default();
        let d = policy::decide(self.agents.cfg.conn(conn), live, call, class, &grants, matched, hides);
        Ok((rules, class, d))
    }

    /// A call from an agent, straight from the link. Answered now, or queued for approval, or started.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn agent_call(&mut self, ctx: &egui::Context, session: u64, link: u64, tool: String, args: Value, client: String, reply: Reply) {
        if !self.agents.cfg.enabled {
            let _ = reply.send(Err(ToolErr::new("off", OFF)));
            return;
        }
        let id = self.agent_id();
        let mut act = Activity { id, time: clock(audit::now()), session, client, tool: tool.clone(), conn: None, ns: String::new(), outcome: Outcome::Running, summary: String::new(), args: String::new(), answer: String::new(), undo: None };
        let call = match tools::parse(&tool, &args) {
            Ok(c) => c,
            Err(e) => {
                act.args = self.agent_args(None, &args, None);
                return self.agent_refuse(act, reply, "bad_args", e.0);
            }
        };
        if call == Call::ListConnections {
            let v = self.agent_connections();
            act.summary = format!("{} connections", v["connections"].as_array().map_or(0, Vec::len));
            act.outcome = Outcome::Allowed;
            let _ = reply.send(Ok(v));
            self.agent_audit(&act, "allowed", Summary::default());
            return self.agent_push(act);
        }
        act.ns = ns_label(&call);
        let conn = match call.conn().map(|r| self.agent_conn(r)) {
            Some(Ok(c)) => c,
            Some(Err(why)) => {
                act.args = self.agent_args(None, &args, None);
                return self.agent_refuse(act, reply, "not_shared", why);
            }
            None => unreachable!("every tool but list_connections names a connection"),
        };
        act.conn = Some(conn);
        let (rules, class, decision) = match self.agent_check(&call, conn, session, None) {
            Ok(x) => x,
            Err((why, code)) => {
                let rules = self.agent_rules(conn).ok();
                act.args = self.agent_args(rules.as_deref(), &args, Some(&call));
                return self.agent_refuse(act, reply, code, why);
            }
        };
        act.args = self.agent_args(Some(&rules), &args, Some(&call));
        self.agents.exec.calls.insert((session, link), id);
        let reason = tools::reason(&args);
        match decision {
            Decision::Refuse(why) => self.agent_refuse(act, reply, "refused", why),
            Decision::Ask(w) if w.connect => {
                let name = self.conn(conn).map(|c| c.meta.name.clone()).unwrap_or_default();
                self.agent_refuse(act, reply, "not_open", format!("Open {name} in Quarry first: agents never open connections themselves."))
            }
            // A write is decided once Quarry has read what it touches: the count meets the large-change threshold, and
            // what it would take to undo meets the undo caps.
            Decision::Allow | Decision::Ask(_) if class == OpClass::ChangeData => {
                act.outcome = Outcome::Running;
                self.agent_push(act);
                let why = match decision {
                    Decision::Ask(w) => w,
                    _ => AskWhy::default(),
                };
                self.agent_prepare(ctx, id, Pending { call, class, conn, session, why, reply, reason, snap: None, writable: false });
            }
            Decision::Allow => {
                act.outcome = Outcome::Running;
                self.agent_push(act);
                let writable = self.agent_unlocked(conn);
                self.agent_start(id, call, conn, rules, reply, false, None, writable);
            }
            Decision::Ask(why) => {
                act.outcome = Outcome::Waiting;
                self.agent_push(act);
                let facts = self.agent_facts(&call, conn);
                self.agent_queue(ctx, id, Pending { call, class, conn, session, why, reply, reason, snap: None, writable: false }, facts, None);
            }
        }
    }

    /// Puts a call in the approval queue; it is denied after `ask_wait`. `no_undo` says why Allow can only run it
    /// without undo.
    fn agent_queue(&mut self, ctx: &egui::Context, id: u64, mut p: Pending, facts: Vec<String>, no_undo: Option<String>) {
        p.writable = self.agent_unlocked(p.conn);
        let Some(act) = self.agent_activity(id) else { return };
        act.outcome = Outcome::Waiting;
        let (client, tool, ns, code) = (act.client.clone(), act.tool.clone(), act.ns.clone(), act.args.clone());
        let ask = Ask { id, client, op: tool.replace('_', " ").to_uppercase(), tool, conn: p.conn, ns, code, facts, reason: p.reason.clone(), why: p.why.clone(), no_undo, at: Instant::now(), shown: None, gone: false };
        self.agents.asks.push_back(ask);
        self.agents.exec.pending.insert(id, p);
        self.worker.agent_after(self.agents.exec.ask_wait, db::AgentEvent::Expired { activity: id });
        // A minimised or hidden Quarry would otherwise let the approval time out unseen.
        if !ctx.input(|i| i.viewport().focused.unwrap_or(true)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
        }
    }

    /// The client or pool a call runs on, taken now so the operation never looks back at App state.
    fn agent_target(&self, conn: u64) -> Option<db::AgentTarget> {
        match self.conn(conn) {
            Some(Conn { sql: Some(s), .. }) => Some(db::AgentTarget::Sql(s.clone())),
            Some(Conn { client: Some(c), .. }) => Some(db::AgentTarget::Mongo(c.clone())),
            _ => None,
        }
    }

    /// Answers a call already in the Activity list with an error and ends it there.
    fn agent_end(&mut self, id: u64, reply: Reply, code: &str, why: String, outcome: Outcome) {
        let _ = reply.send(Err(ToolErr::new(code, why.clone())));
        let decision = if matches!(outcome, Outcome::Refused(_)) { "refused" } else { "allowed" };
        let err = matches!(outcome, Outcome::Failed(_)).then(|| why.clone());
        self.agent_finish(id, outcome, decision, Summary { error: err, ..Default::default() });
    }

    /// A call that was allowed but could not start: the agent gets the error and the Activity list a failed call.
    fn agent_fail(&mut self, id: u64, reply: Reply, code: &str, why: String, approved: bool) {
        let _ = reply.send(Err(ToolErr::new(code, why.clone())));
        self.agent_finish(id, Outcome::Failed(why.clone()), if approved { "approved" } else { "allowed" }, Summary { error: Some(why), ..Default::default() });
    }

    /// Reads what a write touches before it is decided. Inserts and $out pipelines need no read.
    fn agent_prepare(&mut self, ctx: &egui::Context, id: u64, p: Pending) {
        if let Some(snap) = db::agent_snapshot_now(&p.call, &self.agents.cfg.limits) {
            self.agents.exec.preparing.insert(id, p);
            return self.agent_prepared(ctx, id, Ok(snap));
        }
        let Some(target) = self.agent_target(p.conn) else {
            return self.agent_end(id, p.reply, "not_open", CLOSED.into(), Outcome::Failed(CLOSED.into()));
        };
        self.worker.agent_prepare(id, target, p.call.clone(), self.agents.cfg.limits.clone());
        self.agents.exec.preparing.insert(id, p);
    }

    /// The snapshot of a write arrived: the policy runs again with the count, then the write starts, asks, or is refused.
    /// A change too large to undo always asks, whatever the settings, and Allow then says it runs without undo.
    fn agent_prepared(&mut self, ctx: &egui::Context, id: u64, res: db::Res<db::Snapshot>) {
        let Some(mut p) = self.agents.exec.preparing.remove(&id) else { return };
        let snap = match res {
            Ok(s) => s,
            Err(e) => {
                let ns = p.call.ns().map(|n| (n.db.clone(), n.coll.clone())).unwrap_or_default();
                let e = match self.agent_rules(p.conn) {
                    Ok(r) => crate::agents::scrub::scrub_error(&e, &r.for_ns(&ns.0, &ns.1)),
                    Err(r) => r,
                };
                return self.agent_end(id, p.reply, "failed", e.clone(), Outcome::Failed(e));
            }
        };
        let (rules, _, decision) = match self.agent_check(&p.call, p.conn, p.session, Some(snap.matched)) {
            Ok(x) => x,
            Err((why, code)) => return self.agent_end(id, p.reply, code, why.clone(), Outcome::Refused(why)),
        };
        let facts = vec![write_fact(&p.call, &snap)];
        let no_undo = snap.no_undo.clone();
        match decision {
            Decision::Refuse(why) => self.agent_end(id, p.reply, "refused", why.clone(), Outcome::Refused(why)),
            Decision::Ask(w) if w.connect => {
                let why = "The connection closed. Open it in Quarry, then try again.".to_string();
                self.agent_end(id, p.reply, "not_open", why.clone(), Outcome::Refused(why))
            }
            Decision::Allow if no_undo.is_none() => {
                let writable = self.agent_unlocked(p.conn);
                self.agent_start(id, p.call, p.conn, rules, p.reply, false, Some(snap), writable)
            }
            Decision::Allow => {
                // Allowed by the settings, but nothing could take it back: the user decides, once.
                p.why = AskWhy::default();
                p.snap = Some(snap);
                self.agent_queue(ctx, id, p, facts, no_undo);
            }
            Decision::Ask(w) => {
                p.why = AskWhy { session_ok: w.session_ok && no_undo.is_none(), ..w };
                p.snap = Some(snap);
                self.agent_queue(ctx, id, p, facts, no_undo);
            }
        }
    }

    /// Starts an allowed call on the runtime. The connection's client or pool is taken now; the operation never
    /// looks back at App state.
    /// `snap` is for writes: what the write was decided on. It keeps its undo unless the snapshot says it could not
    /// (a change over the undo caps was offered only as Allow without undo). `writable` is whether it may write,
    /// decided by the caller: for a call that waited for approval, as it was when it was queued.
    fn agent_start(&mut self, id: u64, call: Call, conn: u64, rules: Arc<Rules>, reply: Reply, approved: bool, snap: Option<db::Snapshot>, writable: bool) {
        let keep_undo = snap.as_ref().is_some_and(|s| s.no_undo.is_none());
        let Some(target) = self.agent_target(conn) else {
            return self.agent_fail(id, reply, "not_open", CLOSED.into(), approved);
        };
        let scope = self.agents.cfg.conn(conn).map(|p| p.scope.clone()).unwrap_or_default();
        let shell = match &call {
            Call::RunShell { db, script, .. } => match self.agent_shell_job(conn, db.as_deref().unwrap_or(""), script, writable) {
                Ok(j) => Some(j),
                Err(why) => return self.agent_fail(id, reply, "failed", why, approved),
            },
            _ => None,
        };
        let dialect = self.conn(conn).and_then(dialect_of);
        let class = tools::classify(&call, dialect);
        let write = changes_server(&call, class, writable, dialect.is_some());
        let structure = write && class != OpClass::ChangeData;
        let targets = if write { call.targets() } else { Vec::new() };
        let job = db::AgentJob { activity: id, conn, target, call, rules, limits: self.agents.cfg.limits.clone(), scope, snap, keep_undo, writable, shell };
        let run = self.worker.agent_run(job, reply);
        self.agents.exec.running.insert(id, Running { run, conn, approved, write, targets, structure });
    }

    /// The Bun job of a run_shell call, built like the Shell tab's, with the agent time limit for each operation.
    /// MongoDB: the connection's address comes from the keychain here and goes only to the runner. The policy runs the
    /// shell only once the connection is unlocked; `readOnly` still follows the lock in case that ever changes. The
    /// secrets in that address come back beside the job, so the output loses them before the agent sees it; they are
    /// never logged or audited.
    /// SQL: the runner gets no address and no password. Its one connection is a relay on the connection's own pool, read
    /// only unless `writable`, with `database` as the MySQL database or PostgreSQL schema, as run_sql takes it. The
    /// settings of Change data and Change structure go with it, for the relay to check each statement against.
    fn agent_shell_job(&self, conn: u64, db: &str, script: &str, writable: bool) -> Result<db::AgentShell, String> {
        // The demo and the tests never read the keychain, nor run Bun.
        if self.ephemeral {
            return Err("The shell is not available in this Quarry window.".into());
        }
        let runtime = super::super::tools::find_bun(&self.prefs.shell_runtime).ok_or("Bun is needed for the shell, and the user has not installed it.")?;
        let c = self.conn(conn).ok_or("This connection was removed from Quarry.")?;
        let name = c.meta.name.clone();
        let packages: Vec<String> = self.prefs.shell_packages.iter().map(|p| db::package_spec(p).0).collect();
        let timeout = self.agents.cfg.limits.secs.max(1) as u64 * 1000;
        let job = |input: Value, sql: Vec<(String, db::SqlRelay)>| db::ShellJob { runtime: runtime.clone(), dir: config_dir().join("shell"), input: input.to_string(), packages: self.prefs.shell_packages.clone(), sql };
        if let Some(sql) = c.sql.clone() {
            let p = self.agents.cfg.conn(conn).ok_or("This connection is not shared with agents.")?;
            let pg = sql.dialect == Dialect::Postgres;
            // The agent names a schema of the connected database on PostgreSQL, the one whose catalog the policy read.
            let schema = if pg && db.is_empty() { "public".to_string() } else { db.to_string() };
            let key = if pg { ns_key(&sql.pg_database(), &schema) } else { schema.clone() };
            let tables: Vec<String> = self.sql_schema.get(&(conn, key)).map(|s| s.tables.iter().map(|t| t.name.clone()).collect()).unwrap_or_default();
            let entry = json!({ "name": name, "db": if pg { sql.pg_database() } else { schema.clone() }, "tables": tables, "dialect": if pg { "postgres" } else { "mysql" } });
            let input = json!({ "mongo": [], "sql": [entry], "code": script, "packages": packages, "timeoutMs": timeout, "main": name });
            let relay = db::SqlRelay { db: sql, schema, read_only: !writable, conn_id: Default::default() };
            return Ok(db::AgentShell { job: job(input, vec![(name, relay)]), redact: db::Redact::default(), sql: Some(policy::ShellSql::new(p)) });
        }
        let uri = stored_uri(conn).map_err(uri_err)?;
        let colls: Vec<String> = c.dbs.iter().find(|d| d.0 == db).and_then(|d| d.1.clone()).unwrap_or_default();
        let proxy = self.shell_proxy(conn);
        let redact = db::Redact::new(&uri, &proxy["password"].as_str().map(String::from).into_iter().collect::<Vec<_>>());
        let input = json!({ "mongo": [{ "name": name, "uri": uri, "db": db, "proxy": proxy, "collections": colls, "readOnly": !writable }], "sql": [], "code": script, "packages": packages, "timeoutMs": timeout, "main": name });
        Ok(db::AgentShell { job: job(input, Vec::new()), redact, sql: None })
    }

    /// Whether the connection's writes are unlocked right now.
    fn agent_unlocked(&self, conn: u64) -> bool {
        self.conn(conn).is_some_and(|c| c.writable) || self.prefs.always_writable
    }

    /// Whether an approved call may write. Free SQL runs read-only when the connection was locked as the call was queued,
    /// as the dialog said, even if the user unlocked it while the dialog was open: allowing it never makes it a write. A
    /// lock taken meanwhile does make it read-only. Calls whose approval unlocks are writable once it did.
    fn agent_run_writable(&self, p: &Pending) -> bool {
        self.agent_unlocked(p.conn) && (p.writable || p.why.unlock)
    }

    /// Quarry's own facts for an approval, above the agent's reason: what can't be undone, and that free SQL runs read-only.
    fn agent_facts(&self, call: &Call, conn: u64) -> Vec<String> {
        let locked = !self.agent_unlocked(conn);
        let name = self.conn(conn).map(|c| c.meta.name.clone()).unwrap_or_default();
        let mut f = Vec::new();
        match call {
            Call::CreateCollection { .. } | Call::DropCollection { .. } | Call::CreateIndex { .. } | Call::DropIndex { .. } | Call::CreateTable { .. } | Call::DropTable { .. } => f.push("Structure changes can't be undone from Agent activity.".to_string()),
            Call::RunCommand { .. } => f.push("Whatever this command changes can't be undone from Agent activity.".to_string()),
            Call::RunSql { .. } => {
                f.push("What this changes can't be undone from Agent activity.".to_string());
                if locked {
                    f.push(format!("Runs read-only: {name} is locked for writes.\nUnlock it in Quarry first to let it write."));
                }
            }
            Call::RunShell { .. } => {
                f.push("What this changes can't be undone from Agent activity.".to_string());
                // Only on SQL: the MongoDB shell's guard lives in the script's own process, so the policy asks to unlock instead.
                if let Some(p) = self.conn(conn).filter(|c| c.meta.engine.is_sql()).and_then(|_| self.agents.cfg.conn(conn)) {
                    if locked {
                        f.push(format!("Runs read-only: {name} is locked for writes.\nUnlock it in Quarry first to let it write."));
                    }
                    f.push(sql_script_fact(p));
                }
            }
            _ => {}
        }
        f
    }

    /// Sets how a call ended and writes its audit line.
    fn agent_finish(&mut self, id: u64, outcome: Outcome, decision: &str, result: Summary) {
        self.agents.exec.calls.retain(|_, a| *a != id);
        let Some(a) = self.agent_activity(id) else { return };
        a.outcome = outcome;
        let a = a.clone();
        self.agent_audit(&a, decision, result);
    }

    /// One line of the audit log for a finished call, written off the UI thread. Never in the demo or the tests.
    fn agent_audit(&mut self, a: &Activity, decision: &str, result: Summary) {
        if self.ephemeral || !self.agents.cfg.audit.on {
            return;
        }
        let reason = match &a.outcome {
            Outcome::Refused(r) | Outcome::Failed(r) => Some(r.clone()),
            Outcome::Denied => Some(DENIED.into()),
            Outcome::TimedOut => Some(NO_ANSWER.into()),
            _ => None,
        };
        let conn_name = a.conn.and_then(|id| self.conns.iter().find(|c| c.meta.id == id)).map(|c| c.meta.name.clone());
        let e = Entry { time: audit::now(), session: a.session, client: a.client.clone(), tool: a.tool.clone(), conn_id: a.conn, conn_name, ns: (!a.ns.is_empty()).then(|| a.ns.clone()), args: a.args.clone(), decision: decision.into(), reason, result, undo: a.undo };
        if self.agents.exec.audit.is_none() {
            self.agents.exec.audit = Some(self.worker.agents_audit(crate::agents::link::agents_dir(), self.agents.cfg.audit.max_bytes, 0));
        }
        if self.agents.exec.audit.as_ref().is_some_and(|tx| tx.send(e).is_err()) {
            // The writer thread ended; the next call opens it again.
            self.agents.exec.audit = None;
        }
    }

    /// The user answered the approval dialog for call `id`. The policy runs again first: the connection may have been
    /// locked, closed or unshared while the dialog was open.
    pub(in crate::app) fn agent_answer(&mut self, id: u64, a: Answer) {
        self.agents.asks.retain(|k| k.id != id);
        let Some(p) = (a != Answer::Close).then(|| self.agents.exec.pending.remove(&id)).flatten() else { return };
        if a == Answer::Deny {
            let _ = p.reply.send(Err(ToolErr::new("denied", DENIED)));
            return self.agent_finish(id, Outcome::Denied, "denied", Summary::default());
        }
        // Allow without undo says the connection unlocks too when the dialog said it was locked.
        if (a == Answer::UnlockOnce || (a == Answer::WithoutUndo && p.why.unlock))
            && let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == p.conn)
        {
            c.writable = true;
        }
        if a == Answer::Session && p.why.session_ok {
            self.agents.exec.grants.entry((p.session, p.conn)).or_default().grant(p.class);
        }
        let matched = p.snap.as_ref().map(|s| s.matched);
        let why = match self.agent_check(&p.call, p.conn, p.session, matched) {
            Err((why, _)) | Ok((_, _, Decision::Refuse(why))) => why,
            Ok((_, _, Decision::Ask(w))) if w.connect => "The connection closed while the approval was open. Open it in Quarry, then try again.".into(),
            Ok((_, _, Decision::Ask(w))) if w.unlock => "The connection is locked. Allow it again with Unlock writes and allow.".into(),
            Ok((rules, _, _)) => {
                if let Some(act) = self.agent_activity(id) {
                    act.outcome = Outcome::Running;
                }
                let writable = self.agent_run_writable(&p);
                return self.agent_start(id, p.call, p.conn, rules, p.reply, true, p.snap, writable);
            }
        };
        let _ = p.reply.send(Err(ToolErr::new("refused", why.clone())));
        self.agent_finish(id, Outcome::Refused(why), "refused", Summary::default());
    }

    /// The approval waited 50 s: the agent is told no, and the dialog keeps the call as gone until closed.
    fn agent_expired(&mut self, id: u64) {
        let Some(p) = self.agents.exec.pending.remove(&id) else { return };
        let _ = p.reply.send(Err(ToolErr::new("timeout", NO_ANSWER)));
        if let Some(k) = self.agents.asks.iter_mut().find(|k| k.id == id) {
            k.gone = true;
        }
        self.agent_finish(id, Outcome::TimedOut, "timed out", Summary::default());
    }

    /// The agent cancelled call `link` of `session`, or went away: a waiting call is never started, a running one stops.
    fn agent_cancel(&mut self, session: u64, link: u64) {
        let Some(id) = self.agents.exec.calls.remove(&(session, link)) else { return };
        self.agent_drop(id);
    }

    fn agent_drop(&mut self, id: u64) {
        if self.agents.exec.preparing.remove(&id).is_some() {
            return self.agent_finish(id, Outcome::Cancelled, "cancelled", Summary::default());
        }
        // Stopping a change half way would leave what it did with no undo and no record, so it finishes and is logged.
        if self.agents.exec.running.get(&id).is_some_and(|r| r.write) {
            return;
        }
        if self.agents.exec.pending.remove(&id).is_some() {
            if let Some(k) = self.agents.asks.iter_mut().find(|k| k.id == id) {
                k.gone = true;
            }
            return self.agent_finish(id, Outcome::Cancelled, "cancelled", Summary::default());
        }
        if let Some(r) = self.agents.exec.running.remove(&id) {
            r.run.cancel(&self.worker);
            self.agent_finish(id, Outcome::Cancelled, "cancelled", Summary::default());
        }
    }

    /// A link session closed: its grants end, and its calls still waiting for approval are gone. Running calls finish,
    /// so the log says what they did.
    fn agent_session_closed(&mut self, session: u64) {
        self.agents.exec.grants.retain(|k, _| k.0 != session);
        let waiting: Vec<u64> = self.agents.exec.pending.iter().chain(self.agents.exec.preparing.iter()).filter(|(_, p)| p.session == session).map(|(id, _)| *id).collect();
        for id in waiting {
            self.agent_drop(id);
        }
    }

    fn agent_done(&mut self, f: db::Finished) {
        let Some(r) = self.agents.exec.running.remove(&f.activity) else { return };
        if let Some((db, coll, keys)) = f.keys {
            self.agents.exec.keys.insert((r.conn, db, coll), (Instant::now(), keys));
        }
        let decision = if r.approved { "approved" } else { "allowed" };
        let outcome = match &f.error {
            Some(e) => Outcome::Failed(e.clone()),
            None if r.approved => Outcome::Approved,
            None => Outcome::Allowed,
        };
        if let Some(a) = self.agent_activity(f.activity) {
            a.summary = f.summary;
            a.answer = f.answer;
        }
        if let Some(u) = f.undo {
            self.agent_keep_undo(f.activity, u);
        }
        for t in &r.targets {
            self.mark_agent_changed(r.conn, &t.db, t.coll.as_deref());
        }
        // Also after an error: free SQL or a script may have changed something before it failed.
        if r.structure && let Some(t) = r.targets.first() {
            self.agent_refresh_list(r.conn, &t.db);
            // Still running on the server: look again once it has had time to finish.
            if f.running {
                self.worker.agent_after(RECHECK, db::AgentEvent::Refresh { conn: r.conn, db: t.db.clone() });
            }
        }
        self.agent_finish(f.activity, outcome, decision, Summary { count: f.count, bytes: f.bytes, ms: Some(f.ms), error: f.error });
    }

    /// The audit log's last entries, shown under this session's calls at start.
    fn agent_tail(&mut self, entries: Vec<Entry>) {
        let mut old = Vec::with_capacity(entries.len());
        for e in entries {
            let id = self.agent_id();
            let outcome = match (e.decision.as_str(), &e.result.error) {
                (_, Some(err)) if !matches!(e.decision.as_str(), "refused" | "denied" | "timed out" | "cancelled") => Outcome::Failed(err.clone()),
                ("approved", _) => Outcome::Approved,
                ("denied", _) => Outcome::Denied,
                ("timed out", _) => Outcome::TimedOut,
                ("cancelled", _) => Outcome::Cancelled,
                ("refused", _) => Outcome::Refused(e.reason.clone().unwrap_or_default()),
                _ => Outcome::Allowed,
            };
            let summary = e.result.count.map(|n| format!("{n} returned")).unwrap_or_default();
            old.push(Activity { id, time: clock(e.time), session: e.session, client: e.client, tool: e.tool, conn: e.conn_id, ns: e.ns.unwrap_or_default(), outcome, summary, args: e.args, answer: String::new(), undo: None });
        }
        for a in old.into_iter().rev() {
            self.agents.activity.push_front(a);
        }
        while self.agents.activity.len() > ACTIVITY_MAX {
            self.agents.activity.pop_front();
        }
    }

    /// Everything the link and the runtime send about agents.
    pub(in crate::app) fn on_agent(&mut self, ctx: &egui::Context, ev: db::AgentEvent) {
        use db::AgentEvent as E;
        match ev {
            E::Listening => self.agents.link = LinkState::Listening,
            E::Busy => self.agents.link = LinkState::Busy,
            E::Failed(e) => self.agents.link = LinkState::Failed(e),
            E::Opened { session } => self.agents.sessions.push(AgentSession { id: session, client: String::new(), calls: 0 }),
            E::Closed { session } => {
                self.agents.sessions.retain(|s| s.id != session);
                self.agent_session_closed(session);
            }
            E::Call { session, id, tool, args, client, reply } => {
                if let Some(s) = self.agents.sessions.iter_mut().find(|s| s.id == session) {
                    s.calls += 1;
                    s.client = client.name.clone();
                }
                self.agent_call(ctx, session, id, tool, args, client.name, reply);
            }
            E::Cancel { session, id } => self.agent_cancel(session, id),
            E::Done(f) => self.agent_done(f),
            E::Expired { activity } => self.agent_expired(activity),
            E::Prepared { activity, res } => self.agent_prepared(ctx, activity, res),
            E::Undone { activity, res, back } => self.agent_undone(activity, res, back),
            E::Tail(entries) => self.agent_tail(entries),
            E::Refresh { conn, db } => self.agent_refresh_list(conn, &db),
            E::AuditFailed(e) => self.err(e),
        }
    }
}

impl App {
    /// Refreshes `db` as the sidebar's Refresh does, after an agent may have made or dropped a collection or table. On
    /// PostgreSQL an agent names a schema, so its whole database is refreshed.
    fn agent_refresh_list(&mut self, conn: u64, db: &str) {
        let db = match self.conn(conn).and_then(|c| c.sql.as_ref()) {
            Some(s) if s.dialect == Dialect::Postgres => s.pg_database(),
            _ => db.to_string(),
        };
        self.refresh_db(conn, &db);
    }
}

/// What Quarry checks on each statement of a SQL script, for its approval: the settings it was allowed under.
fn sql_script_fact(p: &crate::agents::config::ConnPolicy) -> String {
    use crate::agents::config::Mode;
    let mut refused = vec!["user and role statements", "server credentials and activity", "server commands", "SQL it cannot read (DO blocks, prepared statements)"];
    if p.change == Mode::Never {
        refused.push("data changes");
    }
    if p.structure == Mode::Never {
        refused.push("structure changes");
    }
    format!("Quarry checks each statement the script sends and refuses {}. Rows are scrubbed by the data rules before the script sees them.", refused.join(", "))
}

/// Whether a started call can change the server: a cancel then lets it finish so its end is recorded, and open tabs on
/// what it reaches are told. Free SQL and SQL scripts on a locked connection run read-only and change nothing. The
/// MongoDB shell always counts, since its read-only guard is not a boundary.
fn changes_server(call: &Call, class: OpClass, writable: bool, sql: bool) -> bool {
    match call {
        Call::RunSql { .. } => writable,
        Call::RunShell { .. } => writable || !sql,
        _ => matches!(class, OpClass::ChangeData | OpClass::ChangeStructure),
    }
}

/// Quarry's own fact about a write, first in the approval: what it touches, counted just now.
fn write_fact(call: &Call, snap: &db::Snapshot) -> String {
    let n = group(&snap.matched.to_string());
    match call {
        Call::Insert { .. } => format!("Inserts {n} documents."),
        Call::SqlInsertRows { .. } => format!("Inserts {n} rows."),
        Call::SqlUpdateRows { .. } | Call::SqlDeleteRows { .. } => format!("Matches {n} rows, counted just now."),
        Call::AggregateInto { pipeline, .. } => {
            let last = pipeline.last().and_then(|s| s.keys().next().cloned()).unwrap_or_default();
            format!("Writes its results with {last}.")
        }
        _ => format!("Matches {n} documents, counted just now."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::config::{Access, ConnPolicy, Mode};
    use tokio::sync::oneshot;

    const CONN: u64 = 21;

    fn app() -> egui_kittest::Harness<'static, App> {
        egui_kittest::Harness::builder().with_accessibility_check(false).build_eframe(|cc| App::for_test(cc))
    }

    /// A MongoDB connection marked open without a client, shared with `read` as the read mode.
    fn shared(app: &mut App, read: Mode) {
        app.conns.push(Conn::new(ConnMeta { id: CONN, name: "prod".into(), ..Default::default() }, Status::Ready));
        app.agents.cfg.enabled = true;
        app.agents.cfg.conns.insert(CONN, ConnPolicy { access: Access::ReadWrite, read, change: Mode::Allow, structure: Mode::Ask, ..Default::default() });
    }

    fn call(app: &mut App, ctx: &egui::Context, link: u64, tool: &str, args: Value) -> oneshot::Receiver<Result<Value, ToolErr>> {
        let (reply, rx) = oneshot::channel();
        app.agent_call(ctx, 1, link, tool.into(), args, "claude-code".into(), reply);
        rx
    }

    fn refused(rx: &mut oneshot::Receiver<Result<Value, ToolErr>>) -> ToolErr {
        rx.try_recv().expect("answered at once").expect_err("refused")
    }

    fn find(filter: Value) -> Value {
        json!({ "connection": "prod", "database": "shop", "collection": "users", "filter": filter })
    }

    #[test]
    fn refusals_are_answered_at_once_and_logged() {
        let mut h = app();
        let ctx = h.ctx.clone();
        let app = h.state_mut();
        shared(app, Mode::Allow);
        let e = refused(&mut call(app, &ctx, 1, "find", json!({ "connection": "prod", "database": "shop" })));
        assert_eq!(e.code, "bad_args");
        let e = refused(&mut call(app, &ctx, 2, "find", json!({ "connection": "nope", "database": "shop", "collection": "users" })));
        assert!(e.message.contains("No connection named nope"), "{}", e.message);
        let e = refused(&mut call(app, &ctx, 3, "select_rows", json!({ "connection": CONN, "database": "shop", "table": "users" })));
        assert_eq!(e.code, "wrong_engine");
        // The Secrets preset hides password: filtering on it would probe the value.
        let e = refused(&mut call(app, &ctx, 4, "find", find(json!({ "password": { "$regex": "^a" } }))));
        assert_eq!(e.code, "guard");
        let e = refused(&mut call(app, &ctx, 5, "run_command", json!({ "connection": "prod", "database": "shop", "command": { "shutdown": 1 } })));
        assert!(e.message.contains("not available to agents"), "{}", e.message);
        let e = refused(&mut call(app, &ctx, 6, "find", json!({ "connection": "prod", "database": "local", "collection": "oplog.rs" })));
        assert!(e.message.contains("internals"), "{}", e.message);
        app.agents.cfg.conns.get_mut(&CONN).unwrap().read = Mode::Never;
        let e = refused(&mut call(app, &ctx, 7, "count", find(json!({}))));
        assert!(e.message.contains("Never"), "{}", e.message);
        app.conns[0].status = Status::Idle;
        app.agents.cfg.conns.get_mut(&CONN).unwrap().read = Mode::Allow;
        let e = refused(&mut call(app, &ctx, 8, "count", find(json!({}))));
        assert_eq!(e.code, "not_open");
        assert_eq!(app.agents.activity.len(), 8);
        assert!(app.agents.activity.iter().all(|a| matches!(a.outcome, Outcome::Refused(_))));
        // Arguments are scrubbed before they are shown or logged.
        let a = &app.agents.activity[3];
        assert!(a.args.contains("password") && !a.args.contains("^a"), "{}", a.args);
        app.agents.cfg.enabled = false;
        assert_eq!(refused(&mut call(app, &ctx, 9, "count", find(json!({})))).code, "off");
    }

    #[test]
    fn list_connections_needs_no_connection() {
        let mut h = app();
        let ctx = h.ctx.clone();
        let app = h.state_mut();
        shared(app, Mode::Ask);
        let v = call(app, &ctx, 1, "list_connections", json!({})).try_recv().unwrap().unwrap();
        assert_eq!(v["connections"][0]["name"], "prod");
        assert_eq!(app.agents.activity[0].outcome, Outcome::Allowed);
    }

    #[test]
    fn ask_then_deny_then_cancel() {
        let mut h = app();
        let ctx = h.ctx.clone();
        let app = h.state_mut();
        shared(app, Mode::Ask);
        let mut rx = call(app, &ctx, 1, "count", json!({ "connection": "prod", "database": "shop", "collection": "users", "reason": "count\nusers" }));
        assert!(rx.try_recv().is_err(), "waits for the user");
        let ask = app.agents.asks[0].clone();
        assert_eq!((ask.op.as_str(), ask.reason.as_deref(), ask.why.setting, ask.why.session_ok), ("COUNT", Some("count users"), true, true));
        assert_eq!(app.agents.activity[0].outcome, Outcome::Waiting);
        app.agent_answer(ask.id, Answer::Deny);
        assert_eq!(rx.try_recv().unwrap().unwrap_err().message, DENIED);
        assert!(app.agents.asks.is_empty());
        assert_eq!(app.agents.activity[0].outcome, Outcome::Denied);
        // A cancel closes the approval as gone and nothing runs.
        let mut rx = call(app, &ctx, 2, "count", find(json!({})));
        let id = app.agents.asks[0].id;
        app.on_agent(&ctx, db::AgentEvent::Cancel { session: 1, id: 2 });
        assert!(app.agents.asks[0].gone);
        assert_eq!(app.agent_activity(id).unwrap().outcome, Outcome::Cancelled);
        app.agent_answer(id, Answer::Once);
        assert!(rx.try_recv().is_err(), "the reply was dropped, never answered");
        assert!(app.agents.exec.running.is_empty());
        assert!(app.agents.asks.is_empty(), "Once on a gone call only closes it");
    }

    #[test]
    fn allowing_rechecks_the_policy_and_session_grants() {
        let mut h = app();
        let ctx = h.ctx.clone();
        let app = h.state_mut();
        shared(app, Mode::Ask);
        let mut rx = call(app, &ctx, 1, "count", find(json!({})));
        let id = app.agents.asks[0].id;
        // Unshared while the dialog was open.
        app.agents.cfg.conns.get_mut(&CONN).unwrap().access = Access::NotShared;
        app.agent_answer(id, Answer::Once);
        assert!(rx.try_recv().unwrap().unwrap_err().message.contains("not shared"));
        app.agents.cfg.conns.get_mut(&CONN).unwrap().access = Access::Read;
        let mut rx = call(app, &ctx, 2, "count", find(json!({})));
        let id = app.agents.asks[0].id;
        app.agent_answer(id, Answer::Session);
        // Allowed, then it fails to run since the test connection has no client; the grant stays.
        assert_eq!(rx.try_recv().unwrap().unwrap_err().code, "not_open");
        let mut rx = call(app, &ctx, 3, "count", find(json!({})));
        assert!(app.agents.asks.is_empty(), "the session grant covers the next read");
        assert_eq!(rx.try_recv().unwrap().unwrap_err().code, "not_open");
        app.on_agent(&ctx, db::AgentEvent::Closed { session: 1 });
        let _rx = call(app, &ctx, 4, "count", find(json!({})));
        assert_eq!(app.agents.asks.len(), 1, "grants end with their link session");
    }

    #[test]
    fn rules_that_do_not_compile_refuse_and_recompile_on_change() {
        let mut h = app();
        let ctx = h.ctx.clone();
        let app = h.state_mut();
        shared(app, Mode::Allow);
        app.agents.cfg.rules.push(crate::agents::config::RuleSpec { on: true, matcher: crate::agents::config::Matcher::FieldName("(".into()), scope: Vec::new(), action: crate::agents::config::Action::Remove, mode: Default::default() });
        app.agents.rev += 1;
        assert_eq!(refused(&mut call(app, &ctx, 1, "count", find(json!({})))).code, "rules");
        app.agents.cfg.rules.clear();
        app.agents.rev += 1;
        let mut rx = call(app, &ctx, 2, "count", find(json!({})));
        assert_eq!(rx.try_recv().unwrap().unwrap_err().code, "not_open", "allowed this time, then no client");
    }

    #[test]
    fn activity_is_capped() {
        let mut h = app();
        let ctx = h.ctx.clone();
        let app = h.state_mut();
        shared(app, Mode::Allow);
        for i in 0..(ACTIVITY_MAX as u64 + 20) {
            let _ = call(app, &ctx, i, "find", json!({}));
        }
        assert_eq!(app.agents.activity.len(), ACTIVITY_MAX);
        assert_eq!(app.agents.activity.back().unwrap().id, ACTIVITY_MAX as u64 + 20);
    }

    #[test]
    fn the_audit_tail_comes_first() {
        let mut h = app();
        let ctx = h.ctx.clone();
        let app = h.state_mut();
        shared(app, Mode::Allow);
        let _ = call(app, &ctx, 1, "list_connections", json!({}));
        let e = |d: &str, err: Option<&str>| Entry { time: 1_700_000_000, session: 9, client: "cursor".into(), tool: "find".into(), decision: d.into(), result: Summary { error: err.map(String::from), count: Some(3), ..Default::default() }, ..Default::default() };
        app.on_agent(&ctx, db::AgentEvent::Tail(vec![e("allowed", None), e("refused", Some("x")), e("approved", Some("boom"))]));
        let o: Vec<Outcome> = app.agents.activity.iter().map(|a| a.outcome.clone()).collect();
        assert_eq!(o, [Outcome::Allowed, Outcome::Refused(String::new()), Outcome::Failed("boom".into()), Outcome::Allowed]);
        assert_eq!(app.agents.activity[0].summary, "3 returned");
    }

    fn insert(n: usize) -> Value {
        json!({ "connection": "prod", "database": "shop", "collection": "users", "documents": (0..n).map(|i| json!({ "a": i })).collect::<Vec<_>>() })
    }

    #[test]
    fn a_write_too_large_to_undo_always_asks_and_runs_only_without_undo() {
        let mut h = app();
        let ctx = h.ctx.clone();
        let app = h.state_mut();
        shared(app, Mode::Allow);
        app.conns[0].writable = true;
        app.agents.cfg.limits.undo_docs = 2;
        // Change data is set to Allow, but nothing could take this back.
        let mut rx = call(app, &ctx, 1, "insert", insert(3));
        assert!(rx.try_recv().is_err(), "waits for the user");
        let ask = app.agents.asks[0].clone();
        assert!(ask.no_undo.as_deref().is_some_and(|w| w.contains("more than the 2")), "{:?}", ask.no_undo);
        assert_eq!(ask.facts, ["Inserts 3 documents."]);
        assert!(!ask.why.session_ok);
        app.agent_answer(ask.id, Answer::WithoutUndo);
        // Allowed, then it fails to run since the test connection has no client.
        assert_eq!(rx.try_recv().unwrap().unwrap_err().code, "not_open");
        // Within the cap it runs at once.
        let mut rx = call(app, &ctx, 2, "insert", insert(2));
        assert!(app.agents.asks.is_empty());
        assert_eq!(rx.try_recv().unwrap().unwrap_err().code, "not_open");
    }

    #[test]
    fn a_write_on_a_locked_connection_asks_to_unlock() {
        let mut h = app();
        let ctx = h.ctx.clone();
        let app = h.state_mut();
        shared(app, Mode::Allow);
        let mut rx = call(app, &ctx, 1, "insert", insert(1));
        let ask = app.agents.asks[0].clone();
        assert!(ask.why.unlock && ask.no_undo.is_none());
        app.agent_answer(ask.id, Answer::UnlockOnce);
        assert!(app.conns[0].writable, "Unlock writes and allow unlocks the connection");
        assert_eq!(rx.try_recv().unwrap().unwrap_err().code, "not_open");
    }

    /// A script can undo the shell's read-only guard, so a locked connection is never offered as "runs read-only": the
    /// approval asks to unlock, and allowing unlocks it.
    #[test]
    fn run_shell_on_a_locked_connection_asks_to_unlock() {
        let mut h = app();
        let ctx = h.ctx.clone();
        let app = h.state_mut();
        shared(app, Mode::Allow);
        app.agents.cfg.presets = crate::agents::scrub::tests_support::no_presets();
        app.agents.cfg.conns.get_mut(&CONN).unwrap().scripts = Mode::Allow;
        let mut rx = call(app, &ctx, 1, "run_shell", json!({ "connection": "prod", "database": "shop", "script": "async function run() { return 1 }" }));
        let ask = app.agents.asks[0].clone();
        assert!(ask.why.unlock && ask.why.always && !ask.why.session_ok, "{:?}", ask.why);
        assert!(ask.facts.iter().all(|f| !f.contains("read-only")), "{:?}", ask.facts);
        app.agent_answer(ask.id, Answer::UnlockOnce);
        assert!(app.conns[0].writable);
        assert_eq!(rx.try_recv().unwrap().unwrap_err().code, "not_open", "allowed, then no client in the test");
    }

    /// A MySQL connection marked open without a pool, shared for reading and writing, with no rule presets so nothing hides
    /// data.
    fn shared_sql(app: &mut App, engine: db::sql::Engine) {
        app.conns.push(Conn::new(ConnMeta { id: CONN, name: "shop".into(), engine, ..Default::default() }, Status::Ready));
        app.agents.cfg.enabled = true;
        app.agents.cfg.presets = crate::agents::scrub::tests_support::no_presets();
        app.agents.cfg.conns.insert(CONN, ConnPolicy { access: Access::ReadWrite, scripts: Mode::Allow, structure: Mode::Never, ..Default::default() });
    }

    fn sql_script() -> Value {
        json!({ "connection": "shop", "database": "shop", "script": "async function run() { return await sql`SELECT id FROM users` }" })
    }

    /// run_shell reaches SQL connections. A locked one never asks to unlock: the script's statements come back through
    /// Quarry's relay, which runs them read-only, and allowing it leaves the connection locked. The approval says so and
    /// names what each statement is checked for.
    #[test]
    fn run_shell_on_sql_runs_read_only_when_locked() {
        for engine in [db::sql::Engine::MySql, db::sql::Engine::Postgres] {
            let mut h = app();
            let ctx = h.ctx.clone();
            let app = h.state_mut();
            shared_sql(app, engine);
            let mut rx = call(app, &ctx, 1, "run_shell", sql_script());
            assert!(rx.try_recv().is_err(), "{engine:?}: every script asks");
            let ask = app.agents.asks[0].clone();
            assert!(!ask.why.unlock && ask.why.always && !ask.why.session_ok, "{engine:?}: {:?}", ask.why);
            assert!(ask.facts.iter().any(|f| f.starts_with("Runs read-only")), "{engine:?}: {:?}", ask.facts);
            let checks = ask.facts.iter().find(|f| f.starts_with("Quarry checks each statement")).expect("the statement checks are listed");
            assert!(checks.contains("user and role") && checks.contains("structure changes") && !checks.contains("data changes"), "{checks}");
            assert!(!app.agent_run_writable(&app.agents.exec.pending[&ask.id]), "{engine:?}: queued locked, runs read-only");
            app.agent_answer(ask.id, Answer::Once);
            assert!(!app.conns[0].writable, "{engine:?}: allowing a SQL script never unlocks");
            assert_eq!(rx.try_recv().unwrap().unwrap_err().code, "not_open", "{engine:?}: allowed, then no pool in the test");
            // Unlocked when it was queued, it may write.
            app.conns[0].writable = true;
            let _rx = call(app, &ctx, 2, "run_shell", sql_script());
            let ask = app.agents.asks[0].clone();
            assert!(app.agent_run_writable(&app.agents.exec.pending[&ask.id]));
            assert!(!ask.facts.iter().any(|f| f.starts_with("Runs read-only")), "{:?}", ask.facts);
        }
    }

    /// The same settings hold for SQL scripts as for free SQL: rules that hide data turn them off, shared read only and
    /// Never refuse them.
    #[test]
    fn run_shell_on_sql_follows_the_connection_settings() {
        let mut h = app();
        let ctx = h.ctx.clone();
        let app = h.state_mut();
        shared_sql(app, db::sql::Engine::MySql);
        // The Secrets preset hides password, and with no catalog loaded every column could be one.
        app.agents.cfg.presets = Default::default();
        app.agents.rev += 1;
        let e = refused(&mut call(app, &ctx, 1, "run_shell", sql_script()));
        assert!(e.message.contains("the shell are off"), "{}", e.message);
        app.agents.cfg.presets = crate::agents::scrub::tests_support::no_presets();
        app.agents.rev += 1;
        app.agents.cfg.conns.get_mut(&CONN).unwrap().access = Access::Read;
        let e = refused(&mut call(app, &ctx, 2, "run_shell", sql_script()));
        assert!(e.message.contains("shared read only"), "{}", e.message);
        app.agents.cfg.conns.get_mut(&CONN).unwrap().access = Access::ReadWrite;
        app.agents.cfg.conns.get_mut(&CONN).unwrap().scripts = Mode::Never;
        let e = refused(&mut call(app, &ctx, 3, "run_shell", sql_script()));
        assert!(e.message.contains("Run scripts is set to Never"), "{}", e.message);
        // MongoDB tools still refuse a SQL connection, and say the shell works there.
        let e = refused(&mut call(app, &ctx, 4, "find", json!({ "connection": "shop", "database": "shop", "collection": "users" })));
        assert!(e.code == "wrong_engine" && e.message.contains("run_shell"), "{}", e.message);
    }

    /// A structure change has no undo, so a cancel never stops it half way: it finishes and the Activity says how it
    /// ended, here failing on a server nobody answers at.
    #[test]
    fn a_cancel_lets_a_change_without_undo_finish() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = rt.block_on(mongodb::Client::with_uri_str("mongodb://127.0.0.1:9/?serverSelectionTimeoutMS=300&connectTimeoutMS=300")).unwrap();
        let mut h = app();
        let ctx = h.ctx.clone();
        let id = {
            let app = h.state_mut();
            shared(app, Mode::Allow);
            app.conns[0].writable = true;
            app.conns[0].client = Some(client);
            let _rx = call(app, &ctx, 1, "drop_collection", json!({ "connection": "prod", "database": "shop", "collection": "old" }));
            let id = app.agents.asks[0].id;
            app.agent_answer(id, Answer::Once);
            assert!(app.agents.exec.running.get(&id).is_some_and(|r| r.write && r.structure));
            app.on_agent(&ctx, db::AgentEvent::Cancel { session: 1, id: 1 });
            assert!(app.agents.exec.running.contains_key(&id), "still running after the cancel");
            assert_eq!(app.agent_activity(id).unwrap().outcome, Outcome::Running);
            id
        };
        let t = Instant::now();
        let outcome = loop {
            h.step();
            let o = h.state().agents.activity.iter().find(|a| a.id == id).unwrap().outcome.clone();
            if o != Outcome::Running {
                break o;
            }
            assert!(t.elapsed() < Duration::from_secs(30), "the drop never ended");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(matches!(outcome, Outcome::Failed(_)), "{outcome:?}");
        assert!(h.state().agents.exec.running.is_empty());
    }

    /// An index build Quarry stops waiting for at the time limit goes on on the server, so the Activity says it is still
    /// running rather than failed.
    #[test]
    fn a_structure_change_past_the_time_limit_is_still_running() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        // Nobody answers there, and the driver keeps looking for a server well past the 1 s limit.
        let client = rt.block_on(mongodb::Client::with_uri_str("mongodb://127.0.0.1:9/?serverSelectionTimeoutMS=60000")).unwrap();
        let mut h = app();
        let ctx = h.ctx.clone();
        let id = {
            let app = h.state_mut();
            shared(app, Mode::Allow);
            app.agents.cfg.limits.secs = 1;
            app.conns[0].writable = true;
            app.conns[0].client = Some(client);
            let _rx = call(app, &ctx, 1, "create_index", json!({ "connection": "prod", "database": "shop", "collection": "orders", "keys": { "at": -1 } }));
            let id = app.agents.asks[0].id;
            app.agent_answer(id, Answer::Once);
            id
        };
        let t = Instant::now();
        let a = loop {
            h.step();
            let a = h.state().agents.activity.iter().find(|a| a.id == id).unwrap().clone();
            if a.outcome != Outcome::Running {
                break a;
            }
            assert!(t.elapsed() < Duration::from_secs(30), "the call never ended");
            std::thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(a.outcome, Outcome::Approved);
        assert!(a.summary.starts_with("Still running on the server"), "{}", a.summary);
    }

    /// run_sql runs as the dialog said when it was queued: unlocking while the dialog is open never turns an approved
    /// read-only run into a write, and locking meanwhile makes it read-only.
    #[test]
    fn run_sql_keeps_the_lock_it_was_asked_under() {
        let mut h = app();
        let ctx = h.ctx.clone();
        let app = h.state_mut();
        app.conns.push(Conn::new(ConnMeta { id: CONN, name: "shop".into(), engine: db::sql::Engine::MySql, ..Default::default() }, Status::Ready));
        app.agents.cfg.enabled = true;
        app.agents.cfg.presets = crate::agents::scrub::tests_support::no_presets();
        app.agents.cfg.conns.insert(CONN, ConnPolicy { access: Access::ReadWrite, scripts: Mode::Ask, structure: Mode::Ask, ..Default::default() });
        let sql = json!({ "connection": "shop", "sql": "UPDATE t SET a = 1" });
        let _rx = call(app, &ctx, 1, "run_sql", sql.clone());
        let locked = app.agents.asks[0].clone();
        assert!(!locked.why.unlock && locked.facts.iter().any(|f| f.starts_with("Runs read-only")), "{:?}", locked.facts);
        app.conns[0].writable = true;
        assert!(!app.agent_run_writable(&app.agents.exec.pending[&locked.id]), "unlocked while the dialog was open: still read-only");
        let _rx = call(app, &ctx, 2, "run_sql", sql);
        let open = app.agents.asks[1].clone();
        assert!(app.agent_run_writable(&app.agents.exec.pending[&open.id]));
        app.conns[0].writable = false;
        assert!(!app.agent_run_writable(&app.agents.exec.pending[&open.id]), "locked meanwhile: read-only");
        // A call whose approval unlocks the connection writes once it did.
        let _rx = call(app, &ctx, 3, "create_table", json!({ "connection": "shop", "database": "shop", "table": "t2", "columns": [{ "name": "a", "type": "int" }] }));
        let unlock = app.agents.asks[2].clone();
        assert!(unlock.why.unlock);
        assert!(!app.agent_run_writable(&app.agents.exec.pending[&unlock.id]));
        app.conns[0].writable = true;
        assert!(app.agent_run_writable(&app.agents.exec.pending[&unlock.id]));
    }

    /// What a cancel lets finish and what marks open tabs as changed by an agent.
    #[test]
    fn what_counts_as_a_change() {
        let c = |call: Call, writable: bool| {
            let class = tools::classify(&call, Some(Dialect::MySql));
            changes_server(&call, class, writable, true)
        };
        let run = || Call::RunSql { conn: ConnRef::Id(1), db: None, sql: "UPDATE t SET a = 1".into() };
        assert!(!c(run(), false), "free SQL on a locked connection runs read-only and marks no tab");
        assert!(c(run(), true));
        let shell = || Call::RunShell { conn: ConnRef::Id(1), db: None, script: "async function run() { return 1 }".into() };
        assert!(c(shell(), true));
        assert!(!c(shell(), false), "a SQL script on a locked connection runs read-only");
        assert!(changes_server(&shell(), OpClass::RunScripts, false, false), "the MongoDB shell's guard is not a boundary");
        let cmd = |d: mongodb::bson::Document| Call::RunCommand { conn: ConnRef::Id(1), db: "shop".into(), command: d };
        assert!(c(cmd(mongodb::bson::doc! { "drop": "x" }), true));
        assert!(!c(cmd(mongodb::bson::doc! { "ping": 1 }), true));
        let ns = || crate::agents::tools::Ns { conn: ConnRef::Id(1), db: "shop".into(), coll: "t".into() };
        assert!(c(Call::DropTable { ns: ns() }, true));
        assert!(c(Call::SqlDeleteRows { ns: ns(), wher: Vec::new() }, true));
        assert!(!c(Call::SelectRows { ns: ns(), columns: Vec::new(), wher: Vec::new(), order: Vec::new(), limit: None, offset: 0 }, true));
    }

    #[test]
    fn undo_over_the_total_cap_expires_the_oldest() {
        let mut h = app();
        let app = h.state_mut();
        for id in 1..=3 {
            app.agent_push(Activity { id, time: String::new(), session: 1, client: String::new(), tool: "insert".into(), conn: Some(CONN), ns: "shop.users".into(), outcome: Outcome::Allowed, summary: "1 inserted".into(), args: String::new(), answer: String::new(), undo: None });
        }
        let u = || db::AgentUndo::Inserted { db: "shop".into(), coll: "users".into(), docs: vec![mongodb::bson::doc! { "_id": 1, "pad": "x".repeat(100) }] };
        let one = u().bytes();
        app.agents.cfg.limits.undo_total_bytes = one * 2;
        app.agent_keep_undo(1, u());
        app.agent_keep_undo(2, u());
        assert_eq!(app.agent_activity(1).unwrap().undo, Some(1));
        app.agent_keep_undo(3, u());
        let first = app.agent_activity(1).unwrap().clone();
        assert_eq!(first.undo, None);
        assert!(first.summary.ends_with("undo expired"), "{}", first.summary);
        assert_eq!((app.agent_activity(2).unwrap().undo, app.agent_activity(3).unwrap().undo), (Some(2), Some(3)));
        assert_eq!(app.agents.undo.total(), one * 2);
    }

    #[test]
    fn hash_keys_are_derived_not_copied() {
        let k = [9u8; 32];
        assert_ne!(hash_key_from(&k), k);
        assert_eq!(hash_key_from(&k), hash_key_from(&k));
    }
}
