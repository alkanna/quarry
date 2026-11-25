//! App methods over the state types, which live in quarry_core::state.

use super::*;
pub(crate) use quarry_core::state::*;

/// What to run once writes are allowed.
pub(super) type Retry = Box<dyn FnOnce(&mut App)>;

/// The core's dialogs, carrying the app closure an unlock reruns.
pub(crate) type Modal = quarry_core::state::Modal<Retry>;

impl App {
    pub(super) fn ok(&mut self, s: impl Into<String>) {
        self.core.status = s.into();
    }

    pub(super) fn conn(&self, id: u64) -> Option<&Conn> {
        self.core.conns.iter().find(|c| c.meta.id == id)
    }

    pub(super) fn t(&self) -> Option<&Tab> {
        self.core.tabs.get(self.core.cur)
    }

    pub(super) fn tm(&mut self) -> Option<&mut Tab> {
        self.core.tabs.get_mut(self.core.cur)
    }

    pub(super) fn sel_is_view(&self) -> bool {
        self.t().is_some_and(|t| self.core.views.has(t.sel.conn, &t.sel.nskey(), &t.sel.coll))
    }

    pub(super) fn sel_writable(&self) -> bool {
        if self.sel_is_view() {
            return false;
        }
        self.t().and_then(|t| self.conn(t.sel.conn)).is_some_and(|c| c.writable)
    }

    pub(super) fn timeout(&self) -> Duration {
        Duration::from_millis(self.core.prefs.timeout_ms)
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
        if self.core.ephemeral {
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
        self.stop_conn_work(id);
        self.core.req += 1;
        let attempt = self.core.req;
        let Some(c) = self.core.conns.iter_mut().find(|c| c.meta.id == id) else { return };
        c.status = Status::Connecting;
        c.attempt = attempt;
        c.client = None;
        c.sql = None;
        c.tunnel = None;
        // A new session may see other indexes and statistics, so its plans start fresh.
        self.core.explain_prev.forget(id);
        // Every connection starts read-only, a reconnect included; Always allow writes sets it back each frame.
        c.writable = false;
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
            self.core.worker.sql_connect_with(id, attempt, uri, self.timeout(), net, adv);
        } else {
            self.core.worker.connect(id, attempt, uri, self.timeout(), net);
        }
    }

    /// Something a restart would lose: unsaved edits, writes in flight, running scripts or tools.
    /// Work a restart would lose. Settings does not count, since Restart sits in it; every other dialog may hold typed input.
    pub(super) fn busy_work(&self) -> bool {
        self.modal.as_ref().is_some_and(|m| !matches!(m, Modal::Settings))
            || !self.core.drafts.is_empty()
            || !self.core.saving.is_empty()
            || self.core.json_edit.is_some()
            || self.core.insert.is_some()
            || self.core.inline.is_some()
            || self.core.new_field.is_some()
            || self.core.tool.as_ref().is_some_and(|t| t.done.is_none())
            || self.core.tabs.iter().any(|t| t.shell.cancel.is_some() || t.compare.running || t.loading)
    }
}
