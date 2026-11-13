//! AI agents in the App: the saved settings, the link listener's state and connected sessions, and the answers to
//! agent calls. Decisions are made here, on the UI thread, from state the user sees.

use super::*;
use crate::agents::config::{Access, AgentsConfig};
use crate::agents::{link, store};
use serde_json::{Value, json};

pub(super) mod exec;
mod activity;
mod approval;
mod conn_sheet;
mod demo;
mod rules;
mod settings;
pub(super) use activity::Filter;
pub(super) use rules::RulesWin;
#[cfg(test)]
pub(super) use rules::tester_lines;
pub(super) use settings::ClientKind;

#[derive(Default, PartialEq)]
pub(super) enum LinkState {
    /// No listener: the demo and the tests.
    #[default]
    Idle,
    Starting,
    Listening,
    Busy,
    Failed(String),
}

pub(super) struct AgentSession {
    pub(super) id: u64,
    /// What the agent says it is; shown, never trusted.
    pub(super) client: String,
    pub(super) calls: u32,
}

/// How a call ended, for the Activity window, the pill and the audit log.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Outcome {
    Running,
    /// In the approval queue.
    Waiting,
    /// Allowed by the settings.
    Allowed,
    /// Allowed by the user in the approval dialog.
    Approved,
    Denied,
    /// No answer before the agent's own timeout.
    TimedOut,
    /// The agent cancelled it or went away.
    Cancelled,
    /// Refused by a setting, a rule or the guard; the text says which.
    Refused(String),
    /// Allowed, then the database or Quarry failed; scrubbed error text.
    Failed(String),
}

/// One agent call as the Activity window shows it. Everything here is already scrubbed.
#[derive(Clone, Debug)]
pub(super) struct Activity {
    pub(super) id: u64,
    /// Local wall-clock time, "22:41:07".
    pub(super) time: String,
    pub(super) session: u64,
    /// As the agent reported it; shown, never trusted.
    pub(super) client: String,
    pub(super) tool: String,
    pub(super) conn: Option<u64>,
    /// "db.coll", "schema.table" or empty.
    pub(super) ns: String,
    pub(super) outcome: Outcome,
    /// One line: "412 changed", "200 of 3,120 rows".
    pub(super) summary: String,
    /// Arguments as sent, scrubbed and cut, for the detail view.
    pub(super) args: String,
    /// The start of the answer, scrubbed and cut, for the detail view.
    pub(super) answer: String,
    /// Set while Undo call can still revert this change.
    pub(super) undo: Option<u64>,
}

/// A call waiting in the approval queue. The parsed call and its reply stay with the executor; this is what the
/// dialog draws.
#[derive(Clone, Debug)]
pub(super) struct Ask {
    /// The Activity id of the call.
    pub(super) id: u64,
    pub(super) client: String,
    pub(super) tool: String,
    pub(super) conn: u64,
    pub(super) ns: String,
    /// "UPDATE MANY", "SCRIPT", "DROP COLLECTION".
    pub(super) op: String,
    /// The operation as Quarry will run it, pretty-printed and scrubbed.
    pub(super) code: String,
    /// Quarry's own facts, first: "Matches 412 documents, counted just now."
    pub(super) facts: Vec<String>,
    /// The agent's stated reason, untrusted, at most 300 characters.
    pub(super) reason: Option<String>,
    pub(super) why: crate::agents::policy::AskWhy,
    /// Set when Allow without undo is the only allow, with why it can't be undone; empty draws the undo cap line.
    pub(super) no_undo: Option<String>,
    /// When the call was queued; it is denied 50 s later.
    pub(super) at: std::time::Instant,
    /// When the dialog first showed, for the one-second delay before Allow works.
    pub(super) shown: Option<std::time::Instant>,
    /// The agent cancelled or timed out while it was open: only Close remains.
    pub(super) gone: bool,
}

/// What the user chose in the approval dialog.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Answer {
    Deny,
    Once,
    /// Covers the same class on the same connection for this link session.
    Session,
    /// Unlocks the connection's writes for the session, then allows once.
    UnlockOnce,
    /// Over the undo cap: runs without keeping what it needs to undo.
    WithoutUndo,
    /// Dismisses a call whose agent stopped waiting.
    Close,
}

#[derive(Default)]
pub(super) struct AgentsState {
    /// Newest last, capped; the audit log on disk keeps the full history.
    pub(super) activity: std::collections::VecDeque<Activity>,
    /// Oldest first; the dialog shows the front.
    pub(super) asks: std::collections::VecDeque<Ask>,
    /// The Activity window is open.
    pub(super) activity_open: bool,
    pub(super) cfg: AgentsConfig,
    /// Bumped whenever `cfg` changes, so compiled rules and the tester's lines know to recompute.
    pub(super) rev: u64,
    /// Why agents.json could not be trusted at start, until the user reviews and saves: it came back with agents
    /// off and everything but the custom rules at its default.
    pub(super) distrust: Option<store::Trust>,
    pub(super) handle: Option<db::AgentsHandle>,
    pub(super) link: LinkState,
    pub(super) sessions: Vec<AgentSession>,
    /// The client whose setup snippet Settings shows.
    pub(super) client: ClientKind,
    /// Calls waiting or running, grants, compiled rules and the audit writer; see `exec.rs`.
    pub(super) exec: exec::Exec,
    /// The connection whose own page Settings > AI agents shows instead of the list.
    pub(super) page: Option<u64>,
    /// The scope glob field of that page, as typed.
    pub(super) globs: String,
    /// The Data protection window.
    pub(super) rules: Option<RulesWin>,
    /// What undoes agent changes, by Activity id. In memory only and capped in total by the limits.
    pub(super) undo: crate::agents::undo::UndoStore<db::AgentUndo>,
    pub(super) filter: Filter,
    /// The Activity row whose arguments and answer show below the list.
    pub(super) picked: Option<u64>,
    /// What the approval dialog answered, for the UI tests.
    #[cfg(test)]
    pub(super) answered: Vec<(u64, Answer)>,
}

impl App {
    /// Loads agents.json and starts the listener. The listener runs while Quarry does, so a bridge can tell
    /// "agents are off" from "Quarry is closed"; while off it answers nothing else.
    pub(super) fn agents_start(&mut self) {
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

    pub(super) fn set_agents_enabled(&mut self, on: bool) {
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

    pub(super) fn save_agents(&mut self) {
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
    pub(super) fn agent_undo(&mut self, id: u64) {
        let Some(conn) = self.agents.activity.iter().find(|a| a.id == id && a.undo.is_some()).and_then(|a| a.conn) else { return };
        let Some(c) = self.conn(conn) else { return self.warn("That connection was removed from Quarry.") };
        if !(c.writable || self.prefs.always_writable) {
            return self.warn("Undo is a write. Unlock writes on that connection first.");
        }
        let target = match (c.sql.as_ref(), c.client.as_ref()) {
            (Some(s), _) => db::AgentTarget::Sql(s.clone()),
            (None, Some(cl)) => db::AgentTarget::Mongo(cl.clone()),
            _ => return self.warn_offline(),
        };
        let Some(u) = self.agents.undo.take(id) else { return };
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
    }

    /// Keeps what undoes call `id`. Older entries go once the total is over the cap; their calls say so.
    pub(super) fn agent_keep_undo(&mut self, id: u64, u: db::AgentUndo) {
        let bytes = u.bytes();
        let gone = self.agents.undo.push(id, bytes, u, self.agents.cfg.limits.undo_total_bytes);
        if let Some(a) = self.agents.activity.iter_mut().find(|a| a.id == id) {
            a.undo = Some(id);
        }
        for g in gone {
            if let Some(a) = self.agents.activity.iter_mut().find(|a| a.id == g) {
                a.undo = None;
                a.summary = format!("{} · undo expired", a.summary);
            }
        }
    }

    /// An Undo call finished. What it could not reach comes back so it can be tried again.
    pub(super) fn agent_undone(&mut self, id: u64, res: db::Res<String>, back: Option<db::AgentUndo>) {
        let (conn, ns) = match self.agents.activity.iter().find(|a| a.id == id) {
            Some(a) => (a.conn, back.as_ref().map(|u| u.ns())),
            None => (None, None),
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
                let (d, c) = ns.split_once('.').unwrap_or((&ns, ""));
                (d.to_string(), c.to_string())
            });
            self.mark_agent_changed(conn, &db, (!coll.is_empty()).then_some(coll.as_str()));
        }
    }

    /// Marks the open tabs on what an agent changed, so they say "Changed by an agent" until reloaded. `coll` None marks
    /// every tab of the database.
    pub(super) fn mark_agent_changed(&mut self, conn: u64, db: &str, coll: Option<&str>) {
        for t in self.tabs.iter_mut().filter(|t| t.sel.conn == conn && t.sel.space() == db && coll.is_none_or(|c| t.sel.coll == c)) {
            t.agent_changed = true;
        }
    }

    /// "Changed by an agent" with Reload, over a tab whose collection or table an agent changed since it loaded.
    pub(super) fn agent_notice(&mut self, ui: &mut egui::Ui) {
        if !self.t().is_some_and(|t| t.agent_changed) {
            return;
        }
        ui.add_space(6.0);
        let mut reload = false;
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), Sense::hover());
            ui.painter().circle_filled(r.center(), STATUS_DOT_R, WARNING());
            ui.label(RichText::new("Changed by an agent since this loaded").size(TXT_SMALL).color(TEXT()));
            reload = ui.button(RichText::new("Reload").size(TXT_SMALL)).clicked();
        });
        if reload {
            self.reload();
        }
    }

    /// The shared connections, without anything an agent could reach the server with: no address, user or secret.
    pub(super) fn agent_connections(&self) -> Value {
        let list: Vec<Value> = self
            .conns
            .iter()
            .filter_map(|c| {
                let p = self.agents.cfg.conn(c.meta.id)?;
                let (_, engine) = super::rail::engine_mark(c.meta.engine, &c.server.product);
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

    /// QUARRY_TEST_SETTINGS=agents: agents on with two shared demo connections and one client, never touching disk.
    pub(super) fn agents_demo(&mut self) {
        self.agents.cfg.enabled = true;
        for (i, access) in [Access::ReadWrite, Access::Read].into_iter().enumerate() {
            if let Some(c) = self.conns.get(i) {
                let p = crate::agents::config::ConnPolicy { access, ..self.agents.cfg.defaults.clone() };
                self.agents.cfg.conns.insert(c.meta.id, p);
            }
        }
        self.agents.link = LinkState::Listening;
        self.agents.sessions.push(AgentSession { id: 1, client: "claude-code".into(), calls: 14 });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::config::ConnPolicy;

    #[test]
    fn agents_see_only_shared_connections_without_secrets() {
        let mut h = egui_kittest::Harness::builder().with_accessibility_check(false).build_eframe(|cc| App::for_test(cc));
        let app = h.state_mut();
        for (id, name) in [(11, "shared"), (12, "hidden"), (13, "read")] {
            app.conns.push(Conn { masked_uri: "mongodb://u:***@db.example.com".into(), ..Conn::new(ConnMeta { id, name: name.into(), tag: "Production".into(), ..Default::default() }, Status::Idle) });
        }
        app.agents.cfg.enabled = true;
        app.agents.cfg.conns.insert(11, ConnPolicy { access: Access::ReadWrite, ..Default::default() });
        app.agents.cfg.conns.insert(12, ConnPolicy::default());
        app.agents.cfg.conns.insert(13, ConnPolicy { access: Access::Read, ..Default::default() });
        let v = app.agent_connections();
        let list = v["connections"].as_array().unwrap();
        assert_eq!(list.iter().map(|c| c["name"].as_str().unwrap()).collect::<Vec<_>>(), ["shared", "read"]);
        assert_eq!(list[0]["access"], "read_write");
        assert_eq!(list[0]["id"], "11");
        assert!(!v.to_string().contains("example.com"));
    }
}
