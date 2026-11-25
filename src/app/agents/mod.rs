//! AI agents in the App: the saved settings, the link listener's state and connected sessions, and the answers to
//! agent calls. Decisions are made here, on the UI thread, from state the user sees.

use super::*;
use crate::agents::config::{Access, AgentsConfig};
use crate::agents::{link, store};
use serde_json::{Value, json};
pub(crate) use quarry_core::state::agents::*;

pub(super) mod exec;
mod activity;
mod approval;
mod conn_sheet;
mod demo;
mod open_shell;
mod rules;
mod settings;
pub(super) use rules::RulesWin;
#[cfg(test)]
pub(super) use rules::tester_lines;

impl App {
    /// Loads agents.json and starts the listener. The listener runs while Quarry does, so a bridge can tell
    /// "agents are off" from "Quarry is closed"; while off it answers nothing else.
    pub(super) fn agents_start(&mut self) {
        if self.core.ephemeral {
            return;
        }
        let dir = link::agents_dir();
        let key = store::key();
        let (cfg, trust) = store::load(&dir, key);
        if let Some(k) = key {
            self.core.agents.exec.hash_key = exec::hash_key_from(k);
        }
        self.core.agents.cfg = cfg;
        self.core.agents.rev += 1;
        self.core.agents.distrust = (trust != store::Trust::Signed).then_some(trust);
        // On its own thread, which sends back the last calls of earlier sessions for the Activity window.
        if self.core.agents.cfg.audit.on {
            self.core.agents.exec.audit = Some(self.core.worker.agents_audit(dir.clone(), self.core.agents.cfg.audit.max_bytes, exec::ACTIVITY_MAX));
        }
        let token = if self.core.agents.cfg.enabled { self.agents_token() } else { None };
        self.core.agents.handle = Some(self.core.worker.agents_listen(dir, token));
        self.core.agents.link = LinkState::Starting;
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
        self.core.agents.cfg.enabled = on;
        let token = if on && !self.core.ephemeral { self.agents_token() } else { None };
        if let Some(h) = &self.core.agents.handle {
            h.set_token(token);
        }
        if !on {
            self.core.agents.sessions.clear();
        }
        self.save_agents();
    }

    pub(super) fn save_agents(&mut self) {
        self.core.agents.rev += 1;
        if self.core.ephemeral {
            return;
        }
        match store::save(&link::agents_dir(), &self.core.agents.cfg, store::key()) {
            Ok(()) => self.core.agents.distrust = None,
            Err(e) => self.err(e),
        }
    }

    /// Reverts the change call `id` made, from the Activity window. Like the user's own undo it is a write, so the
    /// connection must be unlocked; each document or row goes back only while it is as the agent left it.
    pub(super) fn agent_undo(&mut self, id: u64) {
        let Some(conn) = self.core.agents.activity.iter().find(|a| a.id == id && a.undo.is_some()).and_then(|a| a.conn) else { return };
        let Some(c) = self.conn(conn) else { return self.warn("That connection was removed from Quarry.") };
        if !(c.writable || self.core.prefs.always_writable) {
            return self.warn("Undo is a write. Unlock writes on that connection first.");
        }
        let target = match (c.sql.as_ref(), c.client.as_ref()) {
            (Some(s), _) => db::AgentTarget::Sql(s.clone()),
            (None, Some(cl)) => db::AgentTarget::Mongo(cl.clone()),
            _ => return self.warn_offline(),
        };
        let Some(u) = self.core.agents.undo.take(id) else { return };
        // A PostgreSQL write ran in one database, and its undo runs on that one's pool.
        let target = match (target, &u) {
            (db::AgentTarget::Sql(s), db::AgentUndo::Sql(db::UndoAction { kind: db::UndoKind::Sql { database, .. }, .. })) => db::AgentTarget::Sql(s.at(database)),
            (t, _) => t,
        };
        if let Some(a) = self.core.agents.activity.iter_mut().find(|a| a.id == id) {
            a.undo = None;
            a.summary = format!("{} · undoing", a.summary);
        }
        self.core.worker.agent_undo(id, target, u);
    }

    /// Keeps what undoes call `id`. Older entries go once the total is over the cap; their calls say so.
    pub(super) fn agent_keep_undo(&mut self, id: u64, u: db::AgentUndo) {
        self.core.agent_keep_undo(id, u)
    }

    /// An Undo call finished. What it could not reach comes back so it can be tried again.
    pub(super) fn agent_undone(&mut self, id: u64, res: db::Res<String>, back: Option<db::AgentUndo>) {
        let (conn, database, ns) = match self.core.agents.activity.iter().find(|a| a.id == id) {
            Some(a) => (a.conn, a.database.clone(), back.as_ref().map(|u| u.ns())),
            None => (None, String::new(), None),
        };
        let done = res.is_ok();
        if let Some(a) = self.core.agents.activity.iter_mut().find(|a| a.id == id) {
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
                let ns = self.core.agents.activity.iter().find(|a| a.id == id).map(|a| a.ns.clone()).unwrap_or_default();
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
    pub(super) fn mark_agent_changed(&mut self, conn: u64, database: &str, db: &str, coll: Option<&str>) {
        for t in self.core.tabs.iter_mut().filter(|t| t.sel.conn == conn && (database.is_empty() || t.sel.db == database) && t.sel.space() == db && coll.is_none_or(|c| t.sel.coll == c)) {
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
            .core.conns
            .iter()
            .filter_map(|c| {
                let p = self.core.agents.cfg.conn(c.meta.id)?;
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
        self.core.agents.cfg.enabled = true;
        for (i, access) in [Access::ReadWrite, Access::Read].into_iter().enumerate() {
            if let Some(c) = self.core.conns.get(i) {
                let p = crate::agents::config::ConnPolicy { access, ..self.core.agents.cfg.defaults.clone() };
                self.core.agents.cfg.conns.insert(c.meta.id, p);
            }
        }
        self.core.agents.link = LinkState::Listening;
        self.core.agents.sessions.push(AgentSession { id: 1, client: "claude-code".into(), calls: 14 });
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
            app.core.conns.push(Conn { masked_uri: "mongodb://u:***@db.example.com".into(), ..Conn::new(ConnMeta { id, name: name.into(), tag: "Production".into(), ..Default::default() }, Status::Idle) });
        }
        app.core.agents.cfg.enabled = true;
        app.core.agents.cfg.conns.insert(11, ConnPolicy { access: Access::ReadWrite, ..Default::default() });
        app.core.agents.cfg.conns.insert(12, ConnPolicy::default());
        app.core.agents.cfg.conns.insert(13, ConnPolicy { access: Access::Read, ..Default::default() });
        let v = app.agent_connections();
        let list = v["connections"].as_array().unwrap();
        assert_eq!(list.iter().map(|c| c["name"].as_str().unwrap()).collect::<Vec<_>>(), ["shared", "read"]);
        assert_eq!(list[0]["access"], "read_write");
        assert_eq!(list[0]["id"], "11");
        assert!(!v.to_string().contains("example.com"));
    }
}
