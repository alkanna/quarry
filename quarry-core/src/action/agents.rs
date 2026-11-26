//! AI agents on Core: their tool calls enter through `execute` as `AgentsA::ToolCall`, which runs the executor in `exec.rs` (parse,
//! resolve, the rules, the guard and the policy, the approval queue, the run and the answer). The app draws the
//! approval, Activity and settings windows (src/app/agents/) and turns their clicks into `AgentsA`.

use super::{Refusal, RefusalKind, Reply};
use crate::agents::config::{Access, AgentsConfig, Matcher, PresetMode, RuleSpec};
use crate::state::agents::activity::Filter;
use crate::state::agents::rules::{RulesTab, RulesWin};
use crate::state::agents::settings::ClientKind;
use crate::agents::link;
use crate::agents::store;
use crate::core::Core;
use crate::db;
use crate::state::agents::{Answer, LinkState, Outcome};
use crate::state::Status;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub mod exec;
pub mod open_shell;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum AgentsA {
    /// The approval dialog's answer for call `id`. Allowing runs the policy again first.
    Answer { id: u64, answer: Answer },
    /// The Activity window's Undo call.
    Undo { id: u64 },
    /// The Activity window's Clear: drops every call that is not waiting or running.
    Clear,
    /// Settings > AI agents' main switch.
    SetEnabled(bool),
    /// The access picker of a connection.
    SetAccess { conn: u64, access: Access },
    /// Signs and saves the settings after an edit the window made in place.
    Save,
    /// The settings as a window edited them on a copy: replaces them, then signs and saves.
    Config(Box<AgentsConfig>),
    /// The Data protection window as it was drawn this frame; None closes it, dropping the rules still empty.
    Rules(Option<Box<RulesWin>>),
    /// Opens Data protection on a preset's checks or the custom rules, as `scope` sees them.
    OpenRules { scope: Option<u64>, tab: RulesTab },
    /// Opens Data protection on a custom rule of `scope`, or on a new one when `rule` is None.
    RulesFor { scope: Option<u64>, rule: Option<usize> },
    /// Adds the rules of an exported file to `scope`'s list. A file that does not parse or compile changes nothing.
    ImportRules { scope: Option<u64>, text: String },
    /// Opens the Activity window with its filter set, or closes it.
    Activity { open: bool, filter: Option<Filter> },
    /// The Activity window's filter.
    ActivityFilter(Filter),
    /// The Activity row whose arguments show below the list.
    Pick(Option<u64>),
    /// The connection page of Settings > AI agents, or the list again when None.
    Page(Option<u64>),
    /// The scope glob field of that page, as typed.
    Globs(String),
    /// The client whose setup snippet Settings shows.
    Client(ClientKind),
    /// Settings > AI agents' Edit rules: the window on its defaults.
    EditRules,
    /// A tool call from an agent over the link; its answer goes back on the reply registered for (session, call).
    ToolCall { session: u64, call: u64, tool: String, args: ToolArgs, client: String },
}

/// An agent's tool arguments. They can hold passwords (a CREATE USER, a createUser command), so Debug never shows them.
#[derive(Serialize, Deserialize, Clone)]
#[serde(transparent)]
pub struct ToolArgs(pub Value);

impl std::fmt::Debug for ToolArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ToolArgs(..)")
    }
}

/// Two-letter mark and product name. MariaDB shares the MySQL engine, so only the server
/// reply, once connected, tells it apart.
pub fn engine_mark(engine: db::sql::Engine, product: &str) -> (&'static str, &'static str) {
    match (engine, product) {
        (db::sql::Engine::Mongo, _) => ("MO", "MongoDB"),
        (db::sql::Engine::Postgres, _) => ("PG", "PostgreSQL"),
        (_, "MariaDB") => ("MA", "MariaDB"),
        _ => ("MY", "MySQL"),
    }
}

impl Core {
    /// A tool call that came over the link: its reply waits under (session, call) while the call runs as an action.
    pub(crate) fn agent_tool_call(&mut self, session: u64, call: u64, tool: String, args: Value, client: String, reply: exec::Reply) {
        self.agents.exec.replies.insert((session, call), reply);
        let _ = self.execute(super::Action::Agents(AgentsA::ToolCall { session, call, tool, args: ToolArgs(args), client }));
    }

    pub(crate) fn agents_act(&mut self, a: AgentsA) -> Result<Reply, Refusal> {
        match a {
            AgentsA::Answer { id, answer } => self.agent_answer(id, answer),
            AgentsA::Undo { id } => return self.agent_undo(id),
            AgentsA::Clear => {
                self.agents.activity.retain(|a| a.outcome == Outcome::Waiting || a.outcome == Outcome::Running);
                self.agents.picked = None;
            }
            AgentsA::SetEnabled(on) => self.set_agents_enabled(on),
            AgentsA::SetAccess { conn, access } => self.set_agent_access(conn, access),
            AgentsA::Save => self.save_agents(),
            AgentsA::Config(c) => {
                self.agents.cfg = *c;
                self.save_agents();
            }
            AgentsA::Rules(w) => match w {
                Some(w) => self.agents.rules = Some(*w),
                None => {
                    self.agents.rules = None;
                    self.drop_empty_rules();
                }
            },
            AgentsA::OpenRules { scope, tab } => self.agents.rules = Some(RulesWin { tab, scope, ..Default::default() }),
            AgentsA::RulesFor { scope, rule } => self.rules_for(scope, rule),
            AgentsA::ImportRules { scope, text } => {
                if let Err(msg) = self.import_rules(scope, &text) {
                    return Err(Refusal { kind: RefusalKind::Invalid, msg });
                }
            }
            AgentsA::Activity { open, filter } => {
                if let Some(f) = filter {
                    self.agents.filter = f;
                }
                self.agents.activity_open = open;
            }
            AgentsA::ActivityFilter(f) => self.agents.filter = f,
            AgentsA::Pick(id) => self.agents.picked = id,
            AgentsA::Page(id) => {
                self.agents.page = id;
                if let Some(id) = id {
                    self.agents.globs = self.agents.cfg.conns.get(&id).map(|p| p.scope.globs.join(", ")).unwrap_or_default();
                }
            }
            AgentsA::Globs(g) => self.agents.globs = g,
            AgentsA::Client(c) => self.agents.client = c,
            AgentsA::EditRules => self.agents.rules = Some(RulesWin::default()),
            AgentsA::ToolCall { session, call, tool, args, client } => {
                // Only a call the link handed over has a reply waiting; any other runs nothing.
                let Some(reply) = self.agents.exec.replies.remove(&(session, call)) else {
                    return Err(Refusal { kind: RefusalKind::Invalid, msg: "No agent call is waiting for that answer.".into() });
                };
                self.agent_call(session, call, tool, args.0, client, reply);
            }
        }
        Ok(Reply::Done)
    }

    /// The custom rules of `scope`: every connection's, or one connection's own.
    pub fn rule_list(&mut self, scope: Option<u64>) -> &mut Vec<RuleSpec> {
        match scope {
            None => &mut self.agents.cfg.rules,
            Some(id) => {
                let cfg = &mut self.agents.cfg;
                &mut cfg.conns.entry(id).or_insert_with(|| cfg.defaults.clone()).rules
            }
        }
    }

    /// A rule that does nothing until its pattern is filled in: one that does not compile stops every agent call.
    fn rules_for(&mut self, scope: Option<u64>, rule: Option<usize>) {
        let i = rule.unwrap_or_else(|| {
            let r = RuleSpec { on: false, matcher: Matcher::FieldPath(String::new()), scope: Vec::new(), action: crate::agents::config::Action::Replace("hidden".into()), mode: PresetMode::Hide };
            let list = self.rule_list(scope);
            list.push(r);
            list.len() - 1
        });
        self.agents.rules = Some(RulesWin { tab: RulesTab::Custom, scope, sel: Some(i), ..Default::default() });
        if rule.is_none() {
            self.save_agents();
        }
    }

    /// Add rule starts a rule with no pattern; one still empty when the window closes was never filled in.
    fn drop_empty_rules(&mut self) {
        let empty = |r: &RuleSpec| matches!(&r.matcher, Matcher::FieldPath(s) | Matcher::FieldName(s) | Matcher::ValueRegex(s) if s.trim().is_empty());
        let before = self.agents.cfg.clone();
        self.agents.cfg.rules.retain(|r| !empty(r));
        for p in self.agents.cfg.conns.values_mut() {
            p.rules.retain(|r| !empty(r));
        }
        if before != self.agents.cfg {
            self.save_agents();
        }
    }

    /// Adds the rules of an exported file to the global list, or to connection `scope`'s. A file that does not parse,
    /// or holds a rule that does not compile, changes nothing: such a rule would stop every agent call.
    pub fn import_rules(&mut self, scope: Option<u64>, text: &str) -> Result<usize, String> {
        let rules: Vec<RuleSpec> = serde_json::from_str(text).map_err(|e| format!("not a Quarry rules file: {e}"))?;
        // Tested alone before anything changes.
        let none = crate::agents::config::Presets { secrets: crate::agents::config::Preset { on: false, ..self.agents.cfg.presets.secrets.clone() }, pii: crate::agents::config::Preset { on: false, ..self.agents.cfg.presets.pii.clone() } };
        crate::agents::scrub::Rules::compile(&none, &Default::default(), &rules, &[], &crate::state::agents::rules::TESTER_KEY).map_err(|e| format!("Nothing was imported. {e}"))?;
        let n = rules.len();
        self.rule_list(scope).extend(rules);
        self.save_agents();
        Ok(n)
    }

    /// Loads agents.json and starts the listener. The listener runs while Quarry does, so a bridge can tell
    /// "agents are off" from "Quarry is closed"; while off it answers nothing else.
    pub fn agents_start(&mut self) {
        if self.ephemeral {
            return;
        }
        let dir = link::agents_dir();
        let key = store::key();
        let (cfg, trust) = store::load(&dir, key);
        if let Some(k) = key {
            self.agents.exec.hash_key = exec::hash_key_from(k);
        }
        self.agents.cfg = cfg;
        self.agents.rev += 1;
        self.agents.distrust = (trust != store::Trust::Signed).then_some(trust);
        // On its own thread, which sends back the last calls of earlier sessions for the Activity window.
        if self.agents.cfg.audit.on {
            self.agents.exec.audit = Some(self.worker.agents_audit(dir.clone(), self.agents.cfg.audit.max_bytes, exec::ACTIVITY_MAX));
        }
        let token = if self.agents.cfg.enabled { self.agents_token() } else { None };
        self.agents.handle = Some(self.worker.agents_listen(dir, token));
        self.agents.link = LinkState::Starting;
    }

    fn agents_token(&mut self) -> Option<link::Token> {
        match link::ensure_token(&link::agents_dir()) {
            Ok(t) => Some(t),
            Err(e) => {
                self.err(format!("Could not create the AI agents key file: {e}"));
                None
            }
        }
    }

    pub fn set_agents_enabled(&mut self, on: bool) {
        self.agents.cfg.enabled = on;
        let token = if on && !self.ephemeral { self.agents_token() } else { None };
        if let Some(h) = &self.agents.handle {
            h.set_token(token);
        }
        if !on {
            self.agents.sessions.clear();
        }
        self.save_agents();
    }

    /// Shares a connection with the defaults the first time, then only changes its access.
    pub fn set_agent_access(&mut self, id: u64, a: Access) {
        let defaults = self.agents.cfg.defaults.clone();
        self.agents.cfg.conns.entry(id).or_insert(defaults).access = a;
        self.save_agents();
    }

    pub fn save_agents(&mut self) {
        self.agents.rev += 1;
        if self.ephemeral {
            return;
        }
        match store::save(&link::agents_dir(), &self.agents.cfg, store::key()) {
            Ok(()) => self.agents.distrust = None,
            Err(e) => self.err(e),
        }
    }

    /// Reverts the change call `id` made, from the Activity window. Like the user's own undo it is a write, so the
    /// connection must be unlocked; each document or row goes back only while it is as the agent left it.
    fn agent_undo(&mut self, id: u64) -> Result<Reply, Refusal> {
        let Some(conn) = self.agents.activity.iter().find(|a| a.id == id && a.undo.is_some()).and_then(|a| a.conn) else { return Ok(Reply::Done) };
        let Some(c) = self.conn(conn) else { return Err(self.refuse("That connection was removed from Quarry.")) };
        // A warning, not the Unlock dialog: the user unlocks from the connection first, as before.
        if !(c.writable || self.prefs.always_writable) {
            return Err(self.refuse("Undo is a write. Unlock writes on that connection first."));
        }
        let target = match (c.sql.as_ref(), c.client.as_ref()) {
            (Some(s), _) => db::AgentTarget::Sql(s.clone()),
            (None, Some(cl)) => db::AgentTarget::Mongo(cl.clone()),
            _ => {
                self.warn_offline();
                return Err(Refusal { kind: RefusalKind::Offline, msg: self.last_warning() });
            }
        };
        let Some(u) = self.agents.undo.take(id) else { return Ok(Reply::Done) };
        // A PostgreSQL write ran in one database, and its undo runs on that one's pool.
        let target = match (target, &u) {
            (db::AgentTarget::Sql(s), db::AgentUndo::Sql(db::UndoAction { kind: db::UndoKind::Sql { database, .. }, .. })) => db::AgentTarget::Sql(s.at(database)),
            (t, _) => t,
        };
        if let Some(a) = self.agents.activity.iter_mut().find(|a| a.id == id) {
            a.undo = None;
            a.summary = format!("{} · undoing", a.summary);
        }
        self.worker.agent_undo(id, target, u);
        Ok(Reply::Done)
    }

    /// An Undo call finished. What it could not reach comes back so it can be tried again.
    pub(crate) fn agent_undone(&mut self, id: u64, res: db::Res<String>, back: Option<db::AgentUndo>) {
        let (conn, database, ns) = match self.agents.activity.iter().find(|a| a.id == id) {
            Some(a) => (a.conn, a.database.clone(), back.as_ref().map(|u| u.ns())),
            None => (None, String::new(), None),
        };
        let done = res.is_ok();
        if let Some(a) = self.agents.activity.iter_mut().find(|a| a.id == id) {
            let base = a.summary.trim_end_matches(" · undoing").to_string();
            a.summary = if done { format!("{base} · undone") } else { base };
        }
        match res {
            Ok(m) => self.ok(m),
            Err(e) => self.err(e),
        }
        if let Some(u) = back {
            self.agent_keep_undo(id, u);
        }
        if let Some(conn) = conn {
            let (db, coll) = ns.unwrap_or_else(|| {
                let ns = self.agents.activity.iter().find(|a| a.id == id).map(|a| a.ns.clone()).unwrap_or_default();
                let ns = ns.strip_prefix(&format!("{database}.")).filter(|_| !database.is_empty()).map(String::from).unwrap_or(ns);
                let (d, c) = ns.split_once('.').unwrap_or((&ns, ""));
                (d.to_string(), c.to_string())
            });
            self.mark_agent_changed(conn, &database, &db, (!coll.is_empty()).then_some(coll.as_str()));
        }
    }

    /// Marks the open tabs on what an agent changed, so they say "Changed by an agent" until reloaded. `coll` None marks
    /// every tab of the database.
    /// `database` narrows it to one PostgreSQL database; empty matches the schema in any.
    pub fn mark_agent_changed(&mut self, conn: u64, database: &str, db: &str, coll: Option<&str>) {
        for t in self.tabs.iter_mut().filter(|t| t.sel.conn == conn && (database.is_empty() || t.sel.db == database) && t.sel.space() == db && coll.is_none_or(|c| t.sel.coll == c)) {
            t.agent_changed = true;
        }
    }

    /// The shared connections, without anything an agent could reach the server with: no address, user or secret.
    pub fn agent_connections(&self) -> Value {
        let list: Vec<Value> = self
            .conns
            .iter()
            .filter_map(|c| {
                let p = self.agents.cfg.conn(c.meta.id)?;
                let (_, engine) = engine_mark(c.meta.engine, &c.server.product);
                Some(json!({
                    // A string: ids are 64-bit and JavaScript clients would round them.
                    "id": c.meta.id.to_string(),
                    "name": c.meta.name,
                    "engine": engine,
                    "tag": c.meta.tag,
                    "access": if p.access == Access::ReadWrite { "read_write" } else { "read" },
                    "connected": c.status == Status::Ready,
                }))
            })
            .collect();
        json!({ "connections": list })
    }
}
