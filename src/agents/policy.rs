//! What happens to a call: allowed, asked, or refused with a reason the agent and the activity list show. Pure,
//! so the whole matrix is tested; the App re-runs it when the user clicks Allow, since state may have moved.

use super::config::{Access, ConnPolicy, Mode, OpClass, Scope, ScopeMode};
use super::scrub::{Rules, wild};
use super::tools::{Call, Target, calls_procedure, changes_structure, each_stage, locks_rows, never_for_agents, touches_users};
use crate::db::sql::Dialect;
use crate::db::sql::lex::{Tok, read_only_check, split_statements, tokens, use_target};
use std::collections::HashSet;

/// What the App knows about the connection right now.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LiveState {
    pub connected: bool,
    /// The session unlock (`Conn.writable`).
    pub writable: bool,
    /// `prefs.always_writable`, which unlocks every connection.
    pub always_writable: bool,
    /// The SQL engine, None for MongoDB. System namespaces differ per engine.
    pub sql: Option<Dialect>,
}

/// Classes the user allowed "for this session" on one connection of one link session. Structure and the shell are
/// never granted, so asking for them does nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Grants {
    classes: HashSet<OpClass>,
}

impl Grants {
    pub fn grant(&mut self, c: OpClass) {
        if matches!(c, OpClass::ReadData | OpClass::ChangeData | OpClass::RunScripts) {
            self.classes.insert(c);
        }
    }

    pub fn has(&self, c: OpClass) -> bool {
        self.classes.contains(&c)
    }
}

/// Why a call needs the user. Several can hold at once; the approval shows each.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AskWhy {
    /// The class is set to Ask.
    pub setting: bool,
    /// Structure changes and the shell always ask, whatever the setting.
    pub always: bool,
    /// The connection is locked: Allow also unlocks it.
    pub unlock: bool,
    /// The connection is not open: Allow also opens it.
    pub connect: bool,
    /// Documents or rows matched, over the large-change threshold.
    pub large: Option<u64>,
    /// Whether "Allow for this session" may be offered.
    pub session_ok: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    Allow,
    Ask(AskWhy),
    Refuse(String),
}

pub fn class_name(c: OpClass) -> &'static str {
    match c {
        OpClass::ReadData => "Read data",
        OpClass::ChangeData => "Change data",
        OpClass::ChangeStructure => "Change structure",
        OpClass::RunScripts => "Run scripts",
        OpClass::ServerUsers => "Server and users",
    }
}

/// `hides_data` comes from `hides_data(rules, call, ..)` below: whether rules hide fields where this call reaches.
/// `matched` is the count a write would touch, when the App counted it first.
pub fn decide(p: Option<&ConnPolicy>, live: LiveState, call: &Call, class: OpClass, grants: &Grants, matched: Option<u64>, hides_data: bool) -> Decision {
    use Decision::Refuse;
    if *call == Call::ListConnections {
        return Decision::Allow;
    }
    let Some(p) = p.filter(|p| p.access != Access::NotShared) else { return Refuse("This connection is not shared with agents. The user can share it in Settings > AI agents.".into()) };
    if class == OpClass::ServerUsers {
        return Refuse("Agents can't manage users, roles or the server.".into());
    }
    if p.access == Access::Read && class != OpClass::ReadData {
        return Refuse(format!("This connection is shared read only, and this call needs {}.", class_name(class)));
    }
    if matches!(call, Call::QuerySql { .. }) && class != OpClass::ReadData {
        return Refuse("query_sql runs read-only statements only. Use run_sql to change data.".into());
    }
    if let Err(e) = reach(p, live, call) {
        return Refuse(e);
    }
    if hides_data && call.is_free_text() {
        return Refuse("Rules hide fields this could reach, so free SQL, commands and the shell are off on this connection. Use the structured tools (find, aggregate, select_rows), which Quarry checks field by field.".into());
    }
    let mode = p.mode(class);
    if mode == Mode::Never {
        return Refuse(format!("{} is set to Never for agents on this connection.", class_name(class)));
    }
    let shell = matches!(call, Call::RunShell { .. });
    let always = shell || class == OpClass::ChangeStructure;
    // Free SQL runs read-only on a locked connection, as the user's own does, because the database holds it there (BEGIN
    // READ ONLY on PostgreSQL, a read-only session on MySQL), so it never needs the unlock. A SQL script is held the same
    // way: its runner has no driver, and every statement comes back through Quarry's relay. On MongoDB the shell's
    // read-only guard patches the driver inside the process the script controls, so a script can undo it: the shell
    // unlocks like a write there.
    let held_read_only = matches!(call, Call::RunSql { .. }) || (shell && live.sql.is_some());
    let writes = matches!(class, OpClass::ChangeData | OpClass::ChangeStructure | OpClass::RunScripts) && !held_read_only;
    let why = AskWhy {
        setting: mode == Mode::Ask,
        always,
        unlock: writes && !(live.writable || live.always_writable),
        connect: !live.connected,
        large: matched.filter(|m| class == OpClass::ChangeData && *m > p.large_change),
        session_ok: !always,
    };
    let only_setting = !why.always && !why.unlock && !why.connect && why.large.is_none();
    if !why.setting && only_setting {
        return Decision::Allow;
    }
    // A session grant stands in for the setting only; locks, closed connections and large changes still ask.
    if only_setting && grants.has(class) {
        return Decision::Allow;
    }
    Decision::Ask(why)
}

/// What an agent's script may send on a SQL connection. Its SQL is only known as it runs, so Quarry's relay asks this
/// about each statement once its values are filled in, before the read-only check and the read-only session hold it.
/// The approval covered the script, so the settings in force then decide: users and roles are never the agent's, the
/// credential and activity namespaces that free SQL may not name are refused here too, and a class set to Never refuses
/// its statements.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShellSql {
    pub change: Mode,
    pub structure: Mode,
}

impl ShellSql {
    pub fn new(p: &ConnPolicy) -> ShellSql {
        ShellSql { change: p.change, structure: p.structure }
    }

    /// `sql` may hold several statements (run_sql); each is classed on its own.
    pub fn check(&self, sql: &str, d: Dialect, read_only: bool) -> Result<(), String> {
        if let Some(s) = system_in_sql(sql, d) {
            return Err(format!("{s} holds server credentials or internals and is not allowed for agents."));
        }
        if touches_users(sql, d) {
            return Err("Agents can't manage users, roles or the server.".into());
        }
        // Classed by its first words, a DO block or a prepared statement would pass a DROP or a CREATE ROLE as a data
        // change, so what cannot be classed from its text is refused instead.
        if let Some(why) = never_for_agents(sql, d) {
            return Err(why);
        }
        if read_only {
            // MySQL's read-only check passes a SELECT whole, and a READ ONLY session still takes the locks it asks for.
            if locks_rows(sql, d) {
                return Err("A locking read (FOR UPDATE, FOR SHARE, LOCK IN SHARE MODE) is not allowed while the connection is locked for writes.".into());
            }
            return Ok(());
        }
        for (_, stmt) in split_statements(sql, d) {
            if read_only_check(stmt, d).is_ok() {
                continue;
            }
            if calls_procedure(stmt, d) && (self.change == Mode::Never || self.structure == Mode::Never) {
                return Err("CALL runs a procedure Quarry cannot read, which may change data or structure, so it needs neither Change data nor Change structure set to Never on this connection.".into());
            }
            let class = if changes_structure(stmt, d) { OpClass::ChangeStructure } else { OpClass::ChangeData };
            let mode = if class == OpClass::ChangeStructure { self.structure } else { self.change };
            if mode == Mode::Never {
                return Err(format!("{} is set to Never for agents on this connection, so this statement was not run.", class_name(class)));
            }
        }
        Ok(())
    }
}

/// Whether free-form text tools are unsafe for this call: query_sql and run_sql on SQL engines, run_command and
/// run_shell on MongoDB, where Hide-mode rules match a field in reach. Structured tools are checked field by field
/// by the guard instead, so this is false for them. `known` is the field list per namespace in reach: the catalog's
/// columns on SQL engines, the keys of a schema sample on MongoDB (a sample, so the shell stays Ask). None, or a
/// list that did not load, counts every Hide-mode rule.
pub fn hides_data(rules: &Rules, call: &Call, known: Option<&[((String, String), Vec<String>)]>) -> bool {
    call.is_free_text() && rules.hides_any(known)
}

/// Scope and system namespaces for every namespace the call names.
fn reach(p: &ConnPolicy, live: LiveState, call: &Call) -> Result<(), String> {
    if call.is_free_text() && p.scope.mode != ScopeMode::All {
        return Err("This connection shares only some databases with agents, and free SQL, commands and the shell could reach the others.".into());
    }
    for t in call.targets() {
        if !visible(&p.scope, &t.db, t.coll.as_deref())? {
            return Err(format!("{} is not shared with agents on this connection.", show(&t)));
        }
        system(live.sql, &t)?;
    }
    match call {
        Call::Aggregate { pipeline, .. } | Call::AggregateInto { pipeline, .. } | Call::Explain { pipeline: Some(pipeline), .. } => {
            let mut bad = None;
            each_stage(pipeline, &mut |name, _| {
                if matches!(name, "$currentOp" | "$listSessions" | "$listLocalSessions") {
                    bad = Some(name.to_string());
                }
            });
            if let Some(b) = bad {
                return Err(format!("{b} reads server internals and is not allowed for agents."));
            }
        }
        Call::RunCommand { command, .. } => {
            let name = command.keys().next().map(String::as_str).unwrap_or("");
            if ["getLog", "currentOp", "getCmdLineOpts", "getParameter", "hostInfo"].iter().any(|c| c.eq_ignore_ascii_case(name)) {
                return Err(format!("The {name} command reads server internals and is not allowed for agents."));
            }
        }
        Call::QuerySql { sql, .. } | Call::RunSql { sql, .. } => {
            if let Some(d) = live.sql {
                if let Some(s) = system_in_sql(sql, d) {
                    return Err(format!("{s} holds server credentials or internals and is not allowed for agents."));
                }
                if let Some(why) = never_for_agents(sql, d) {
                    return Err(why);
                }
                // Free SQL is classed statement by statement like a script's, so Never on Change data or Change
                // structure holds for it too.
                if matches!(call, Call::RunSql { .. }) {
                    ShellSql::new(p).check(sql, d, false)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn show(t: &Target) -> String {
    match &t.coll {
        Some(c) => format!("{}.{c}", t.db),
        None => t.db.clone(),
    }
}

/// Whether the scope covers a namespace, or with `coll` None a database (some of it, for Only; not all of it
/// excluded, for Except). A glob that does not parse is an error rather than a silent miss.
pub fn visible(scope: &Scope, db: &str, coll: Option<&str>) -> Result<bool, String> {
    if scope.mode == ScopeMode::All {
        return Ok(true);
    }
    let mut hit = false;
    for g in &scope.globs {
        let g = g.trim();
        let (gdb, gcoll) = match g.split_once('.') {
            Some((d, c)) if !d.is_empty() && !c.is_empty() => (d, Some(c)),
            None if !g.is_empty() => (g, None),
            _ => return Err(format!("The scope pattern `{g}` is not valid, so nothing is shared until it is fixed.")),
        };
        hit |= match (coll, gcoll) {
            (Some(c), Some(gc)) => wild(gdb, db) && wild(gc, c),
            (Some(_), None) => wild(gdb, db),
            (None, Some(_)) => scope.mode == ScopeMode::Only && wild(gdb, db),
            (None, None) => wild(gdb, db),
        };
    }
    Ok(if scope.mode == ScopeMode::Only { hit } else { !hit })
}

const PG_SYSTEM: [&str; 10] = [
    "pg_authid",
    "pg_shadow",
    "pg_user_mappings",
    "pg_user_mapping",
    "pg_subscription",
    "pg_stat_activity",
    "pg_stat_statements",
    "pg_stat_get_activity",
    "pg_stat_get_backend_activity",
    "pg_stat_statements_info",
];
const MY_SYSTEM: [&str; 3] = ["mysql", "performance_schema", "sys"];

/// Namespaces that hold credentials or other sessions' queries, refused outside Server and users.
fn system(sql: Option<Dialect>, t: &Target) -> Result<(), String> {
    let coll = t.coll.as_deref().unwrap_or("");
    let bad = match sql {
        None => matches!(t.db.as_str(), "local" | "config") || (coll.starts_with("system.") && coll != "system.views"),
        Some(Dialect::Postgres) => PG_SYSTEM.contains(&coll.to_ascii_lowercase().as_str()),
        Some(_) => MY_SYSTEM.contains(&t.db.to_ascii_lowercase().as_str()),
    };
    if bad { Err(format!("{} holds server credentials or internals and is not allowed for agents.", show(t))) } else { Ok(()) }
}

/// Best effort over free SQL text: a PostgreSQL credential or activity view named anywhere, or a MySQL system
/// schema used as a qualifier or by USE. The real boundary is the database role.
fn system_in_sql(sql: &str, d: Dialect) -> Option<String> {
    for (_, stmt) in split_statements(sql, d) {
        if let Some(db) = use_target(stmt, d)
            && MY_SYSTEM.contains(&db.to_ascii_lowercase().as_str())
            && d != Dialect::Postgres
        {
            return Some(db);
        }
        let toks: Vec<(Tok, &str)> = tokens(stmt, d).into_iter().filter(|(t, _)| matches!(t, Tok::Word | Tok::Quoted | Tok::Str | Tok::Punct)).map(|(t, r)| (t, &stmt[r])).collect();
        for (i, (t, w)) in toks.iter().enumerate() {
            if !matches!(t, Tok::Word | Tok::Quoted) && !(*t == Tok::Str && w.starts_with('"')) {
                continue;
            }
            let name = w.trim_matches('`').trim_matches('"').to_ascii_lowercase();
            if d == Dialect::Postgres {
                if PG_SYSTEM.contains(&name.as_str()) {
                    return Some(name);
                }
            } else if MY_SYSTEM.contains(&name.as_str()) && toks.get(i + 1).is_some_and(|(t, p)| *t == Tok::Punct && *p == ".") {
                return Some(name);
            } else if name == "processlist" {
                // SHOW PROCESSLIST and information_schema.PROCESSLIST list other sessions' statements.
                return Some(name);
            } else if i == 0 && name == "show" && toks.get(1).zip(toks.get(2)).is_some_and(|((_, a), (_, b))| a.eq_ignore_ascii_case("create") && b.eq_ignore_ascii_case("user")) {
                // SHOW CREATE USER prints the account's password hash.
                return Some("show create user".into());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::config::{Action, Matcher, PresetMode, RuleSpec};
    use crate::agents::tools::{ConnRef, Ns};
    use mongodb::bson::doc;

    fn ns(db: &str, coll: &str) -> Ns {
        Ns { conn: ConnRef::Id(1), db: db.into(), coll: coll.into() }
    }

    fn find(db: &str, coll: &str) -> Call {
        Call::Find { ns: ns(db, coll), filter: doc! {}, projection: None, sort: None, skip: 0, limit: None }
    }

    fn update() -> Call {
        Call::Update { ns: ns("shop", "orders"), filter: doc! {}, update: crate::agents::tools::Update::Doc(doc! { "$set": { "a": 1 } }), many: true }
    }

    fn sql(s: &str) -> Call {
        Call::QuerySql { conn: ConnRef::Id(1), db: None, sql: s.into(), limit: None }
    }

    fn command(c: mongodb::bson::Document) -> Call {
        Call::RunCommand { conn: ConnRef::Id(1), db: "shop".into(), command: c }
    }

    /// run_command can drop or make collections and indexes, so those commands get the structure tools' approval: the
    /// Change structure setting, always asked, never granted for the session, even where scripts are allowed.
    #[test]
    fn structure_commands_ask_like_structure_tools() {
        let p = ConnPolicy { structure: Mode::Ask, ..pol(Access::ReadWrite, Mode::Allow) };
        let mut g = Grants::default();
        g.grant(OpClass::RunScripts);
        g.grant(OpClass::ChangeStructure);
        for c in [doc! { "drop": "orders" }, doc! { "createIndexes": "orders", "indexes": [] }, doc! { "collMod": "orders" }, doc! { "renameCollection": "shop.orders", "to": "shop.old" }, doc! { "aggregate": "orders", "pipeline": [{ "$merge": { "into": "copy" } }], "cursor": {} }] {
            let call = command(c);
            let class = crate::agents::tools::classify(&call, None);
            assert_eq!(class, OpClass::ChangeStructure, "{call:?}");
            let d = decide(Some(&p), OPEN, &call, class, &g, None, false);
            assert!(matches!(&d, Decision::Ask(w) if w.always && w.setting && !w.session_ok), "{call:?} => {d:?}");
        }
        let never = ConnPolicy { structure: Mode::Never, ..pol(Access::ReadWrite, Mode::Allow) };
        let call = command(doc! { "drop": "orders" });
        assert!(is_refused(&decide(Some(&never), OPEN, &call, crate::agents::tools::classify(&call, None), &g, None, false)), "Change structure set to Never refuses it");
        let ping = command(doc! { "ping": 1 });
        assert_eq!(decide(Some(&p), OPEN, &ping, crate::agents::tools::classify(&ping, None), &g, None, false), Decision::Allow);
    }

    fn pol(access: Access, mode: Mode) -> ConnPolicy {
        ConnPolicy { access, read: mode, change: mode, structure: mode, scripts: mode, ..ConnPolicy::default() }
    }

    const OPEN: LiveState = LiveState { connected: true, writable: true, always_writable: false, sql: None };

    fn is_refused(d: &Decision) -> bool {
        matches!(d, Decision::Refuse(_))
    }

    /// Every combination of access, mode, unlock, grant, threshold and hiding, checked against the rules the
    /// policy promises rather than against a copy of its code.
    #[test]
    fn matrix() {
        let calls: Vec<(Call, OpClass)> = vec![
            (find("shop", "orders"), OpClass::ReadData),
            (Call::Aggregate { ns: ns("shop", "orders"), pipeline: vec![doc! { "$addFields": { "x": 1 } }], limit: None }, OpClass::ReadData),
            (Call::Aggregate { ns: ns("shop", "orders"), pipeline: vec![doc! { "$match": { "x": 1 } }], limit: None }, OpClass::ReadData),
            (sql("SELECT 1"), OpClass::ReadData),
            (update(), OpClass::ChangeData),
            (Call::DropCollection { ns: ns("shop", "orders") }, OpClass::ChangeStructure),
            (Call::RunSql { conn: ConnRef::Id(1), db: None, sql: "UPDATE t SET a = 1".into() }, OpClass::RunScripts),
            (Call::RunShell { conn: ConnRef::Id(1), db: None, script: "db.x.find()".into() }, OpClass::RunScripts),
            (command(doc! { "ping": 1 }), OpClass::RunScripts),
            (command(doc! { "drop": "orders" }), OpClass::ChangeStructure),
            (command(doc! { "aggregate": "orders", "pipeline": [{ "$out": "copy" }], "cursor": {} }), OpClass::ChangeStructure),
        ];
        for (call, class) in &calls {
            if matches!(call, Call::RunCommand { .. }) {
                assert_eq!(crate::agents::tools::classify(call, None), *class, "{call:?}");
            }
        }
        let mut n = 0;
        for access in [Access::NotShared, Access::Read, Access::ReadWrite] {
            for mode in [Mode::Never, Mode::Ask, Mode::Allow] {
                for writable in [false, true] {
                    for granted in [false, true] {
                        for matched in [None, Some(5), Some(500)] {
                            for hides in [false, true] {
                                for (call, class) in &calls {
                                    let p = pol(access, mode);
                                    let mut g = Grants::default();
                                    if granted {
                                        g.grant(*class);
                                    }
                                    let live = LiveState { writable, sql: Some(Dialect::MySql), ..OPEN };
                                    let d = decide(Some(&p), live, call, *class, &g, matched, hides);
                                    n += 1;
                                    let ctx = format!("{access:?} {mode:?} writable={writable} granted={granted} {matched:?} hides={hides} {call:?} => {d:?}");
                                    let shell = matches!(call, Call::RunShell { .. });
                                    let free = call.is_free_text();
                                    let write = *class != OpClass::ReadData;
                                    // Every call here is on MySQL, where free SQL and scripts both run read-only when locked.
                                    let unlocks = write && !matches!(call, Call::RunSql { .. }) && !shell;
                                    if access == Access::NotShared || mode == Mode::Never || (access == Access::Read && write) || (hides && free) {
                                        assert!(is_refused(&d), "{ctx}");
                                        continue;
                                    }
                                    assert!(!is_refused(&d), "{ctx}");
                                    if shell || *class == OpClass::ChangeStructure {
                                        assert!(matches!(&d, Decision::Ask(w) if w.always && !w.session_ok), "{ctx}");
                                    }
                                    if unlocks && !writable {
                                        assert!(matches!(&d, Decision::Ask(w) if w.unlock), "{ctx}");
                                    }
                                    if !unlocks {
                                        assert!(!matches!(&d, Decision::Ask(w) if w.unlock), "{ctx}: runs read-only instead");
                                    }
                                    if *class == OpClass::ChangeData && matched == Some(500) {
                                        assert!(matches!(&d, Decision::Ask(w) if w.large == Some(500)), "{ctx}");
                                    }
                                    let plain = !shell && *class != OpClass::ChangeStructure && (!unlocks || writable) && !(*class == OpClass::ChangeData && matched == Some(500));
                                    if plain {
                                        let expect_allow = mode == Mode::Allow || granted;
                                        assert_eq!(d == Decision::Allow, expect_allow, "{ctx}");
                                    } else {
                                        assert!(matches!(d, Decision::Ask(_)), "{ctx}");
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(n, 3 * 3 * 2 * 2 * 3 * 2 * calls.len());
    }

    #[test]
    fn not_shared_and_missing_refuse() {
        assert!(is_refused(&decide(None, OPEN, &find("a", "b"), OpClass::ReadData, &Grants::default(), None, false)));
        assert_eq!(decide(None, OPEN, &Call::ListConnections, OpClass::ReadData, &Grants::default(), None, false), Decision::Allow);
    }

    #[test]
    fn closed_connection_asks_to_open() {
        let p = pol(Access::ReadWrite, Mode::Allow);
        let d = decide(Some(&p), LiveState { connected: false, ..OPEN }, &find("a", "b"), OpClass::ReadData, &Grants::default(), None, false);
        assert!(matches!(d, Decision::Ask(w) if w.connect && !w.setting));
    }

    #[test]
    fn always_writable_unlocks() {
        let p = pol(Access::ReadWrite, Mode::Allow);
        let live = LiveState { writable: false, always_writable: true, ..OPEN };
        assert_eq!(decide(Some(&p), live, &update(), OpClass::ChangeData, &Grants::default(), Some(1), false), Decision::Allow);
    }

    #[test]
    fn grants_never_cover_structure_or_shell() {
        let mut g = Grants::default();
        g.grant(OpClass::ChangeStructure);
        g.grant(OpClass::ServerUsers);
        assert_eq!(g, Grants::default());
        g.grant(OpClass::RunScripts);
        let p = pol(Access::ReadWrite, Mode::Ask);
        let shell = Call::RunShell { conn: ConnRef::Id(1), db: None, script: "1".into() };
        assert!(matches!(decide(Some(&p), OPEN, &shell, OpClass::RunScripts, &g, None, false), Decision::Ask(_)));
        let run = Call::RunSql { conn: ConnRef::Id(1), db: None, sql: "UPDATE t SET a = 1".into() };
        assert_eq!(decide(Some(&p), OPEN, &run, OpClass::RunScripts, &g, None, false), Decision::Allow);
    }

    /// The shell's read-only guard runs inside the script's own process, so a locked connection asks to unlock; free
    /// SQL is held read-only by the database and runs locked.
    #[test]
    fn the_shell_unlocks_and_free_sql_runs_locked() {
        let p = pol(Access::ReadWrite, Mode::Allow);
        let locked = LiveState { writable: false, ..OPEN };
        let shell = Call::RunShell { conn: ConnRef::Id(1), db: None, script: "async function run() { return 1 }".into() };
        let d = decide(Some(&p), locked, &shell, OpClass::RunScripts, &Grants::default(), None, false);
        assert!(matches!(&d, Decision::Ask(w) if w.unlock && w.always && !w.session_ok), "{d:?}");
        let run = Call::RunSql { conn: ConnRef::Id(1), db: None, sql: "UPDATE t SET a = 1".into() };
        assert_eq!(decide(Some(&p), LiveState { sql: Some(Dialect::Postgres), ..locked }, &run, OpClass::RunScripts, &Grants::default(), None, false), Decision::Allow);
        // A SQL script reaches the server only through Quarry's relay, so a locked connection holds it read-only: it still
        // always asks, but never to unlock.
        for d in [Dialect::MySql, Dialect::Postgres] {
            let d = decide(Some(&p), LiveState { sql: Some(d), ..locked }, &shell, OpClass::RunScripts, &Grants::default(), None, false);
            assert!(matches!(&d, Decision::Ask(w) if !w.unlock && w.always && !w.session_ok), "{d:?}");
        }
    }

    /// The same access level and settings hold for SQL scripts: shared read only refuses them, Never refuses them, rules
    /// that hide data refuse them, and scope must be the whole connection.
    #[test]
    fn sql_scripts_follow_the_connection_settings() {
        let shell = Call::RunShell { conn: ConnRef::Id(1), db: Some("shop".into()), script: "async function run() { return await sql`SELECT 1` }".into() };
        let live = LiveState { sql: Some(Dialect::MySql), ..OPEN };
        let g = Grants::default();
        assert!(is_refused(&decide(Some(&pol(Access::Read, Mode::Allow)), live, &shell, OpClass::RunScripts, &g, None, false)));
        assert!(is_refused(&decide(Some(&pol(Access::ReadWrite, Mode::Never)), live, &shell, OpClass::RunScripts, &g, None, false)));
        let Decision::Refuse(why) = decide(Some(&pol(Access::ReadWrite, Mode::Allow)), live, &shell, OpClass::RunScripts, &g, None, true) else { panic!("hidden data refuses the shell") };
        assert!(why.contains("the shell are off"), "{why}");
        let only = ConnPolicy { scope: Scope { mode: ScopeMode::Only, globs: vec!["shop".into()] }, ..pol(Access::ReadWrite, Mode::Allow) };
        assert!(is_refused(&decide(Some(&only), live, &shell, OpClass::RunScripts, &g, None, false)));
        let mysql = Call::RunShell { conn: ConnRef::Id(1), db: Some("mysql".into()), script: "async function run() { return 1 }".into() };
        assert!(is_refused(&decide(Some(&pol(Access::ReadWrite, Mode::Allow)), live, &mysql, OpClass::RunScripts, &g, None, false)), "a system database as the script's own");
        // The hide rule check sees a SQL script as free text, against the catalog's columns.
        let rule = RuleSpec { on: true, matcher: Matcher::FieldName("^password$".into()), scope: Vec::new(), action: Action::Remove, mode: PresetMode::Hide };
        let rules = Rules::compile(&crate::agents::scrub::tests_support::no_presets(), &Default::default(), &[rule], &[], &[0; 32]).unwrap();
        let users = (("shop".to_string(), "users".to_string()), vec!["id".to_string(), "password".to_string()]);
        let orders = (("shop".to_string(), "orders".to_string()), vec!["id".to_string()]);
        assert!(hides_data(&rules, &shell, Some(&[users, orders.clone()])));
        assert!(!hides_data(&rules, &shell, Some(&[orders])));
        assert!(hides_data(&rules, &shell, None), "a catalog not loaded yet counts as hiding");
    }

    /// Each statement of a SQL script is classed as it reaches the relay: users and roles and server internals never run,
    /// a class set to Never refuses its statements, reads always pass, and a locked connection refuses locking reads
    /// on top of what its read-only check and session refuse.
    #[test]
    fn sql_script_statements_are_classed_as_they_run() {
        let open = ShellSql { change: Mode::Ask, structure: Mode::Ask };
        for d in [Dialect::MySql, Dialect::Postgres] {
            for ro in [false, true] {
                assert!(open.check("SELECT id FROM orders", d, ro).is_ok(), "{d:?}");
                let e = open.check("GRANT SELECT ON orders TO app", d, ro).unwrap_err();
                assert!(e.contains("users, roles"), "{d:?}: {e}");
                assert!(open.check("CREATE USER x", d, ro).is_err(), "{d:?}");
            }
            assert!(open.check("UPDATE orders SET total = 0", d, false).is_ok(), "{d:?}");
            assert!(open.check("DROP TABLE orders", d, false).is_ok(), "{d:?}");
            assert!(open.check("SELECT id FROM orders FOR UPDATE", d, false).is_ok(), "{d:?}: a writer may lock rows");
            let e = open.check("SELECT id FROM orders FOR UPDATE", d, true).unwrap_err();
            assert!(e.contains("locking read"), "{d:?}: {e}");
        }
        assert!(open.check("SELECT * FROM mysql.user", Dialect::MySql, true).unwrap_err().contains("not allowed for agents"));
        assert!(open.check("USE mysql", Dialect::MySql, false).is_err());
        assert!(open.check("SELECT rolpassword FROM pg_authid", Dialect::Postgres, true).unwrap_err().contains("not allowed for agents"));
        assert!(open.check("SELECT * FROM t LOCK IN SHARE MODE", Dialect::MySql, true).is_err());
        let no_structure = ShellSql { structure: Mode::Never, ..open };
        for s in ["DROP TABLE orders", "TRUNCATE orders", "ALTER TABLE orders ADD c int", "CREATE TABLE x (a int)"] {
            let e = no_structure.check(s, Dialect::MySql, false).unwrap_err();
            assert!(e.contains("Change structure is set to Never"), "{s}: {e}");
        }
        assert!(no_structure.check("DELETE FROM orders", Dialect::MySql, false).is_ok());
        let no_change = ShellSql { change: Mode::Never, ..open };
        for s in ["DELETE FROM orders", "INSERT INTO orders VALUES (1)"] {
            let e = no_change.check(s, Dialect::Postgres, false).unwrap_err();
            assert!(e.contains("Change data is set to Never"), "{s}: {e}");
        }
        // A procedure's body is not read, so CALL needs both classes.
        for p in [no_change, no_structure] {
            let e = p.check("CALL refresh()", Dialect::Postgres, false).unwrap_err();
            assert!(e.contains("CALL runs a procedure"), "{e}");
        }
        assert!(open.check("CALL refresh()", Dialect::MySql, false).is_ok());
        assert!(no_change.check("SELECT 1", Dialect::Postgres, false).is_ok() && no_change.check("DROP TABLE orders", Dialect::Postgres, false).is_ok());
        let p = ConnPolicy { change: Mode::Never, structure: Mode::Allow, ..ConnPolicy::default() };
        assert_eq!(ShellSql::new(&p), ShellSql { change: Mode::Never, structure: Mode::Allow });
    }

    /// With Change structure set to Never and Change data allowed, nothing that wraps a DROP or a role change gets through
    /// as a data change, in a script or in run_sql, locked or not.
    #[test]
    fn wrapped_structure_and_role_changes_are_refused() {
        let s = ShellSql { change: Mode::Allow, structure: Mode::Never };
        let my = ["PREPARE s FROM 'DROP TABLE orders'", "EXECUTE s", "SET ROLE admin", "SET DEFAULT ROLE ALL TO app", "SET GLOBAL read_only = 0", "SET PERSIST max_connections = 1", "KILL 1", "SHUTDOWN", "LOAD DATA INFILE '/tmp/x' INTO TABLE orders", "SELECT LOAD_FILE('/etc/passwd')"];
        let pg = ["DO $$ BEGIN EXECUTE 'DROP TABLE orders'; EXECUTE 'CREATE ROLE x SUPERUSER'; END $$", "SET SESSION ROLE x", "SET LOCAL ROLE x", "SET SESSION AUTHORIZATION x", "RESET ROLE", "ALTER SYSTEM SET fsync = off", "SELECT set_config('role', 'x', false)", "COPY orders TO PROGRAM 'id'", "SELECT pg_terminate_backend(1)"];
        let live = |d| LiveState { sql: Some(d), ..OPEN };
        let p = ConnPolicy { change: Mode::Allow, structure: Mode::Never, scripts: Mode::Allow, ..pol(Access::ReadWrite, Mode::Allow) };
        for (d, list) in [(Dialect::MySql, &my[..]), (Dialect::Postgres, &pg[..])] {
            for q in list {
                for ro in [false, true] {
                    assert!(s.check(q, d, ro).is_err(), "{d:?} script ro={ro}: {q}");
                }
                let run = Call::RunSql { conn: ConnRef::Id(1), db: None, sql: q.to_string() };
                let class = crate::agents::tools::classify(&run, Some(d));
                for writable in [false, true] {
                    let d2 = decide(Some(&p), LiveState { writable, ..live(d) }, &run, class, &Grants::default(), None, false);
                    assert!(is_refused(&d2), "{d:?} run_sql writable={writable}: {q} => {d2:?}");
                }
            }
        }
        // run_sql now keeps Change structure = Never too, and still runs data changes.
        let drop = Call::RunSql { conn: ConnRef::Id(1), db: None, sql: "UPDATE orders SET a = 1; DROP TABLE orders".into() };
        let Decision::Refuse(why) = decide(Some(&p), live(Dialect::MySql), &drop, OpClass::RunScripts, &Grants::default(), None, false) else { panic!("DROP under Never") };
        assert!(why.contains("Change structure is set to Never"), "{why}");
        let update = Call::RunSql { conn: ConnRef::Id(1), db: None, sql: "UPDATE orders SET a = 1".into() };
        assert_eq!(decide(Some(&p), live(Dialect::MySql), &update, OpClass::RunScripts, &Grants::default(), None, false), Decision::Allow);
        // A read through query_sql may not read server files either.
        let file = Call::QuerySql { conn: ConnRef::Id(1), db: None, sql: "SELECT LOAD_FILE('/etc/passwd')".into(), limit: None };
        assert!(is_refused(&decide(Some(&p), live(Dialect::MySql), &file, OpClass::ReadData, &Grants::default(), None, false)));
    }

    #[test]
    fn never_names_the_setting() {
        let p = pol(Access::ReadWrite, Mode::Never);
        let Decision::Refuse(why) = decide(Some(&p), OPEN, &update(), OpClass::ChangeData, &Grants::default(), None, false) else { panic!() };
        assert!(why.contains("Change data is set to Never"), "{why}");
        let Decision::Refuse(why) = decide(Some(&pol(Access::ReadWrite, Mode::Allow)), OPEN, &update(), OpClass::ServerUsers, &Grants::default(), None, false) else { panic!() };
        assert!(why.contains("users, roles or the server"), "{why}");
    }

    #[test]
    fn query_sql_must_be_read_only() {
        let p = pol(Access::ReadWrite, Mode::Allow);
        assert!(is_refused(&decide(Some(&p), OPEN, &sql("DELETE FROM t"), OpClass::RunScripts, &Grants::default(), None, false)));
    }

    #[test]
    fn scope() {
        let only = Scope { mode: ScopeMode::Only, globs: vec!["shop.orders".into(), "logs".into(), "bi.*_daily".into()] };
        let except = Scope { mode: ScopeMode::Except, globs: vec!["shop.secrets".into(), "hr".into()] };
        let v = |s: &Scope, db: &str, c: Option<&str>| visible(s, db, c).unwrap();
        assert!(v(&only, "shop", Some("orders")) && v(&only, "logs", Some("x")) && v(&only, "bi", Some("sales_daily")) && v(&only, "shop", None));
        assert!(!v(&only, "shop", Some("users")) && !v(&only, "bi", Some("sales")) && !v(&only, "hr", None));
        assert!(v(&except, "shop", Some("orders")) && v(&except, "shop", None));
        assert!(!v(&except, "shop", Some("secrets")) && !v(&except, "hr", Some("x")) && !v(&except, "hr", None));
        assert!(visible(&Scope { mode: ScopeMode::Except, globs: vec![".x".into()] }, "a", Some("b")).is_err());
        let p = ConnPolicy { scope: except.clone(), ..pol(Access::ReadWrite, Mode::Allow) };
        assert!(is_refused(&decide(Some(&p), OPEN, &find("shop", "secrets"), OpClass::ReadData, &Grants::default(), None, false)));
        let lookup = Call::Aggregate { ns: ns("shop", "orders"), pipeline: vec![doc! { "$lookup": { "from": "secrets", "pipeline": [], "as": "s" } }], limit: None };
        assert!(is_refused(&decide(Some(&p), OPEN, &lookup, OpClass::ReadData, &Grants::default(), None, false)));
        let into = Call::AggregateInto { ns: ns("shop", "orders"), pipeline: vec![doc! { "$out": { "db": "hr", "coll": "x" } }] };
        assert!(is_refused(&decide(Some(&p), OPEN, &into, OpClass::ChangeData, &Grants::default(), None, false)));
        assert!(is_refused(&decide(Some(&p), LiveState { sql: Some(Dialect::MySql), ..OPEN }, &sql("SELECT 1"), OpClass::ReadData, &Grants::default(), None, false)), "free SQL cannot be held to a scope");
    }

    #[test]
    fn system_namespaces() {
        let p = pol(Access::ReadWrite, Mode::Allow);
        let g = Grants::default();
        let mongo = |c: &Call| decide(Some(&p), OPEN, c, OpClass::ReadData, &g, None, false);
        for (db, coll) in [("admin", "system.users"), ("admin", "system.keys"), ("shop", "system.profile"), ("local", "oplog.rs"), ("config", "chunks")] {
            assert!(is_refused(&mongo(&find(db, coll))), "{db}.{coll}");
        }
        assert_eq!(mongo(&find("shop", "system.views")), Decision::Allow);
        assert_eq!(mongo(&find("shop", "orders")), Decision::Allow);
        let cur = Call::Aggregate { ns: ns("admin", "x"), pipeline: vec![doc! { "$currentOp": {} }], limit: None };
        assert!(is_refused(&mongo(&cur)));
        let cur = Call::Aggregate { ns: ns("admin", "x"), pipeline: vec![doc! { "$facet": { "a": [{ "$listSessions": {} }] } }], limit: None };
        assert!(is_refused(&mongo(&cur)));
        let cmd = |c: mongodb::bson::Document| decide(Some(&p), OPEN, &Call::RunCommand { conn: ConnRef::Id(1), db: "admin".into(), command: c }, OpClass::RunScripts, &g, None, false);
        assert!(is_refused(&cmd(doc! { "getLog": "global" })));
        assert!(is_refused(&cmd(doc! { "currentOp": 1 })));
        assert!(is_refused(&cmd(doc! { "find": "system.users" })));
        assert!(!is_refused(&cmd(doc! { "ping": 1 })));

        let my = LiveState { sql: Some(Dialect::MySql), ..OPEN };
        let my_d = |c: &Call| decide(Some(&p), my, c, OpClass::ReadData, &g, None, false);
        for db in ["mysql", "performance_schema", "sys"] {
            assert!(is_refused(&my_d(&find(db, "user"))), "{db}");
        }
        assert!(is_refused(&my_d(&sql("SELECT user, authentication_string FROM mysql.user"))));
        assert!(is_refused(&my_d(&sql("SELECT * FROM `performance_schema`.`threads`"))));
        assert!(is_refused(&my_d(&sql("USE mysql; SELECT * FROM user"))));
        assert!(is_refused(&my_d(&sql("SHOW FULL PROCESSLIST"))));
        assert!(is_refused(&my_d(&sql("SELECT info FROM information_schema.PROCESSLIST"))));
        assert!(is_refused(&my_d(&sql("show create user 'app'@'%'"))));
        assert_eq!(my_d(&sql("SHOW CREATE TABLE shop.t")), Decision::Allow);
        assert_eq!(my_d(&sql("SELECT 'mysql.user', sys_name FROM shop.t")), Decision::Allow);
        assert_eq!(my_d(&find("config", "x")), Decision::Allow, "a MySQL database may be called config");

        let pg = LiveState { sql: Some(Dialect::Postgres), ..OPEN };
        let pg_d = |c: &Call| decide(Some(&p), pg, c, OpClass::ReadData, &g, None, false);
        for t in PG_SYSTEM {
            assert!(is_refused(&pg_d(&find("pg_catalog", t))), "{t}");
            assert!(is_refused(&pg_d(&sql(&format!("SELECT * FROM {t}")))), "{t}");
        }
        assert!(is_refused(&pg_d(&sql(r#"SELECT rolpassword FROM pg_catalog."pg_authid""#))));
        assert!(is_refused(&pg_d(&sql("SELECT query FROM pg_stat_get_activity(NULL)"))));
        assert_eq!(pg_d(&sql("SELECT 'pg_authid'")), Decision::Allow);
        assert_eq!(pg_d(&find("public", "orders")), Decision::Allow);
    }

    #[test]
    fn hides_data_per_call() {
        let rule = |m: Matcher, scope: &[&str]| RuleSpec { on: true, matcher: m, scope: scope.iter().map(|s| s.to_string()).collect(), action: Action::Remove, mode: PresetMode::Hide };
        let rules = Rules::compile(&crate::agents::scrub::tests_support::no_presets(), &Default::default(), &[rule(Matcher::FieldName("^password$".into()), &["shop.users"])], &[], &[0; 32]).unwrap();
        assert!(!hides_data(&rules, &find("shop", "users"), None), "structured tools are the guard's");
        assert!(hides_data(&rules, &sql("SELECT 1"), None), "no field list: strict");
        let users = (("shop".to_string(), "users".to_string()), vec!["id".to_string(), "password".to_string()]);
        let orders = (("shop".to_string(), "orders".to_string()), vec!["id".to_string()]);
        assert!(hides_data(&rules, &sql("SELECT 1"), Some(&[users.clone(), orders.clone()])));
        assert!(!hides_data(&rules, &sql("SELECT 1"), Some(&[orders.clone()])));
        let shell = Call::RunShell { conn: ConnRef::Id(1), db: Some("shop".into()), script: "db.users.find()".into() };
        let sampled = |keys: &[&str]| vec![(("shop".to_string(), "users".to_string()), keys.iter().map(|k| k.to_string()).collect::<Vec<_>>())];
        assert!(hides_data(&rules, &shell, Some(&sampled(&["_id", "profile.password"]))), "a sampled MongoDB key matches");
        assert!(!hides_data(&rules, &shell, Some(&sampled(&["_id", "profile.email"]))));
        let mask = Rules::compile(&crate::agents::scrub::tests_support::no_presets(), &Default::default(), &[RuleSpec { mode: PresetMode::MaskOutput, ..rule(Matcher::FieldName("^email$".into()), &[]) }], &[], &[0; 32]).unwrap();
        assert!(!hides_data(&mask, &shell, None), "MaskOutput rules never turn free text off");
    }
}
