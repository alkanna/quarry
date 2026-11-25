//! Connections: connect, disconnect, the writes lock, and the answers that fill a connection once it opens (its databases,
//! collections and server info). The rail, the sidebar card, the Connections panel and the Unlock dialog draw them.

use super::{Refusal, RefusalKind, Reply, Secret, UiFx};
use crate::core::Core;
use crate::db;
use crate::state::config::{secret, set_secret, store_uri, stored_uri, uri_err};
use crate::state::groups::{Group, Place};
use crate::state::restore::Restore;
use crate::state::*;
use mongodb::bson::{doc, Document};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ConnA {
    /// Connect with the address in the Keychain, or reconnect; Retry and Refresh databases too.
    Connect { conn: u64 },
    Disconnect { conn: u64 },
    /// Makes `conn` the active connection, connecting it if it is idle.
    Activate { conn: u64 },
    /// Allow writes on `conn` for this session, what the Unlock dialog's Allow writes does.
    Unlock { conn: u64 },
    Lock { conn: u64 },
    /// Makes `conn` the active connection and nothing else, for a pick of a connection that is open already.
    Switch { conn: u64 },
    Group(GroupA),
    /// The connection form's Save and connect: the address and the form's secrets go to the Keychain, `meta` to
    /// connections.json, and a new connection joins `group`. Each secret is empty for one the form does not use.
    Save { meta: ConnMeta, uri: Secret, secrets: Vec<(String, Secret)>, group: Option<u64> },
}

/// Connection groups, as the Connections panel, the rail's folders and the Edit group dialog change them.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum GroupA {
    /// A group's row in the panel folded or unfolded.
    Toggle { group: u64 },
    /// Folds every group, or opens every one when all are folded.
    FoldAll,
    /// A connection dragged into a group or out of every group.
    Move { conn: u64, to: Place },
    New { name: String },
    Rename { group: u64, name: String },
    Color { group: u64, color: String },
    ConnectAll { group: u64 },
    DisconnectAll { group: u64 },
    /// A folder of the rail opened or closed.
    RailOpen { group: u64 },
    /// Don't fold in the rail.
    Unfold { group: u64 },
    /// The Edit group dialog's Save: the fields, then the members in their order.
    Save { group: u64, name: String, color: String, fold_rail: bool, members: Vec<u64> },
    /// Deletes a group, with an Undo; its members are in no group afterwards.
    Delete { group: u64 },
    /// A tile let go on the rail.
    Drop(RailDrop),
    /// The rail's tiles reordered, open connection ids in their new order.
    RailOrder { order: Vec<u64> },
}

/// What a tile let go on the rail does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RailDrop {
    Reorder { conn: u64, anchor: u64, after: bool },
    Into { conn: u64, gid: u64, at: Option<usize> },
    Pair { conn: u64, target: u64 },
}

/// Moves `conn` to just before or after `anchor` in the rail order. An anchor that is gone, or is `conn` itself, puts it last.
pub fn move_next_to(order: &mut Vec<u64>, conn: u64, anchor: u64, after: bool) {
    // A connection dragged in from the sidebar may be closed, and a closed one has no tile to place.
    if !order.contains(&conn) {
        return;
    }
    order.retain(|c| *c != conn);
    let at = order.iter().position(|c| *c == anchor).map_or(order.len(), |i| i + after as usize);
    order.insert(at, conn);
}

impl Core {
    pub(super) fn conn_act(&mut self, a: ConnA) -> Result<Reply, Refusal> {
        match a {
            ConnA::Connect { conn } => {
                self.connect(conn);
                return Ok(self.conn(conn).filter(|c| c.status == Status::Connecting).map_or(Reply::Done, |c| Reply::Pending(c.attempt)));
            }
            ConnA::Disconnect { conn } => self.disconnect(conn),
            ConnA::Activate { conn } => {
                self.active = Some(conn);
                if self.conn(conn).is_some_and(|c| c.status == Status::Idle) && !self.demo {
                    self.connect(conn);
                }
            }
            ConnA::Unlock { conn } => {
                let name = self.conn_name(conn);
                if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
                    c.writable = true;
                }
                self.ok(format!("Writes allowed on {name} for this session"));
            }
            ConnA::Lock { conn } => {
                if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
                    c.writable = false;
                    self.ok("Writes locked");
                }
            }
            ConnA::Switch { conn } => self.active = Some(conn),
            ConnA::Group(a) => self.group_act(a),
            ConnA::Save { meta, uri, secrets, group } => return self.save_conn(meta, uri, secrets, group),
        }
        Ok(Reply::Done)
    }

    fn group_act(&mut self, a: GroupA) {
        match a {
            GroupA::Toggle { group } => {
                if let Some(g) = self.groups.group_mut(group) {
                    g.open = !g.open;
                    self.save_groups();
                }
            }
            GroupA::FoldAll => {
                // One press folds everything; with all folded already, the next one opens everything.
                let any_open = self.groups.groups.iter().any(|g| g.open);
                for g in &mut self.groups.groups {
                    g.open = !any_open;
                }
                self.save_groups();
            }
            GroupA::Move { conn, to } => {
                let all = self.conn_ids();
                if self.groups.place(conn, to, &all) {
                    self.save_groups();
                }
            }
            GroupA::New { name } => {
                let name = name.trim();
                if !name.is_empty() {
                    self.groups.new_group(name);
                    self.save_groups();
                }
            }
            GroupA::Rename { group, name } => {
                let name = name.trim();
                if let Some(g) = self.groups.group_mut(group).filter(|_| !name.is_empty()) {
                    g.name = name.into();
                    self.save_groups();
                }
            }
            GroupA::Color { group, color } => {
                if let Some(g) = self.groups.group_mut(group) {
                    g.color = color;
                    self.save_groups();
                }
            }
            GroupA::ConnectAll { group } => {
                let ids: Vec<u64> = self.groups.group(group).map(|g| g.members.clone()).unwrap_or_default();
                for id in ids {
                    if self.conn(id).is_some_and(|c| c.status.can_connect()) && !self.demo {
                        self.connect(id);
                    }
                }
            }
            GroupA::DisconnectAll { group } => {
                let Some(g) = self.groups.group(group) else { return };
                let (ids, name) = (g.members.clone(), g.name.clone());
                let mut n = 0;
                for id in ids {
                    if self.conn(id).is_some_and(|c| c.status.is_open()) && self.drop_session(id).is_some() {
                        n += 1;
                    }
                }
                // One message for the whole group, where `disconnect` would raise one per connection.
                self.ok(if n == 1 { format!("Disconnected 1 connection in {name}") } else { format!("Disconnected {n} connections in {name}") });
            }
            GroupA::RailOpen { group } => {
                let open = !self.groups.rail_open(group);
                self.groups.set_rail_open(group, open);
                self.save_groups();
            }
            GroupA::Unfold { group } => {
                if let Some(g) = self.groups.group_mut(group) {
                    g.fold_rail = false;
                    let name = g.name.clone();
                    self.save_groups();
                    self.ok(format!("{name} shows its connections one by one in the rail. Edit group turns folding back on."));
                }
            }
            GroupA::Save { group, name, color, fold_rail, members } => {
                // Placing the members one by one in their order gives the group exactly that order and takes new ones out
                // of their old groups; those that left go first.
                let Some(old) = self.groups.group(group).map(|g| g.members.clone()) else { return };
                let all = self.conn_ids();
                for c in old.iter().filter(|c| !members.contains(c)) {
                    self.groups.place(*c, Place::Loose(None), &all);
                }
                for (i, c) in members.iter().enumerate() {
                    self.groups.place(*c, Place::Into(group, Some(i)), &all);
                }
                if let Some(g) = self.groups.group_mut(group) {
                    if !name.trim().is_empty() {
                        g.name = name.trim().into();
                    }
                    g.color = color;
                    g.fold_rail = fold_rail;
                }
                self.save_groups();
            }
            GroupA::Delete { group } => self.delete_group(group),
            GroupA::Drop(d) => self.rail_drop(d),
            GroupA::RailOrder { order } => self.rail_order = order,
        }
    }

    fn delete_group(&mut self, id: u64) {
        let Some(at) = self.groups.groups.iter().position(|g| g.id == id) else { return };
        let g = self.groups.groups.remove(at);
        let all = self.conn_ids();
        // The members follow the connections that were already loose, in saved order.
        self.groups.loose = self.groups.loose_order(&all);
        self.save_groups();
        let (name, n) = (g.name.clone(), g.members.len());
        let msg = match n {
            0 => format!("Group {name} deleted."),
            1 => format!("Group {name} deleted. Its connection is in no group now."),
            _ => format!("Group {name} deleted. Its {n} connections are in no group now."),
        };
        self.toast_restore(msg, Restore::Group { g, at });
    }

    /// Applies one drop on the rail: the groups are saved, and the toast offers Undo.
    fn rail_drop(&mut self, d: RailDrop) {
        let all = self.conn_ids();
        // Undo puts back only what this drop moves: each connection's spot in the groups, the tile's place in the rail and
        // the group it makes. A whole snapshot would also throw away group edits made while the toast is up.
        let tile = match d {
            RailDrop::Reorder { conn, .. } | RailDrop::Into { conn, .. } | RailDrop::Pair { conn, .. } => conn,
        };
        let at = self.rail_order.iter().position(|c| *c == tile).unwrap_or(0);
        let prev = at.checked_sub(1).and_then(|i| self.rail_order.get(i).copied());
        let spot = |a: &Core, c: u64| {
            let (g, i) = a.groups.spot(c, &all);
            (c, g, i)
        };
        let undo = |moved: Vec<(u64, Option<u64>, usize)>, created: Option<u64>| Restore::Placed { moved, created, tile, prev, at };
        match d {
            RailDrop::Reorder { conn, anchor, after } => {
                if !self.rail_order.contains(&conn) {
                    return;
                }
                // A member of a folded group dropped between tiles leaves its group. An unfolded one is loose in the rail already.
                let left = self.groups.group_of(conn).filter(|g| g.fold_rail).map(|g| g.name.clone());
                if let Some(g) = left {
                    let was = vec![spot(self, conn)];
                    self.groups.place(conn, Place::Loose(None), &all);
                    self.toast_restore(format!("{} left {g}.", self.conn_name(conn)), undo(was, None));
                }
                move_next_to(&mut self.rail_order, conn, anchor, after);
            }
            RailDrop::Into { conn, gid, at } => {
                let joined = self.groups.group_of(conn).map(|g| g.id) != Some(gid);
                let was = vec![spot(self, conn)];
                if self.groups.place(conn, Place::Into(gid, at), &all) && joined {
                    let g = self.groups.group(gid).map(|g| g.name.clone()).unwrap_or_default();
                    // The folder sits at its first open member, so a tile from above it must follow the group or the folder jumps.
                    let members = self.groups.group(gid).map(|g| g.members.clone()).unwrap_or_default();
                    if let Some(last) = self.rail_order.iter().copied().filter(|c| *c != conn && members.contains(c)).last() {
                        move_next_to(&mut self.rail_order, conn, last, true);
                    }
                    self.toast_restore(format!("{} added to {g}.", self.conn_name(conn)), undo(was, None));
                }
            }
            RailDrop::Pair { conn, target } => {
                let was = vec![spot(self, conn), spot(self, target)];
                let gid = self.groups.new_group("New group");
                self.groups.place(target, Place::Into(gid, None), &all);
                self.groups.place(conn, Place::Into(gid, None), &all);
                self.groups.set_rail_open(gid, true);
                move_next_to(&mut self.rail_order, conn, target, true);
                self.toast_restore_edit(format!("Grouped {} and {}.", self.conn_name(target), self.conn_name(conn)), undo(was, Some(gid)), Some(gid));
            }
        }
        self.save_groups();
        self.save_prefs();
    }

    /// The Undo of a deleted group.
    pub fn restore_group(&mut self, g: Group, at: usize) {
        let (name, alive) = (g.name.clone(), self.conn_ids());
        self.groups.restore(g, at, &alive);
        self.save_groups();
        self.ok(format!("Group {name} is back."));
    }

    /// The Undo of a drop on the rail.
    pub fn restore_placed(&mut self, moved: Vec<(u64, Option<u64>, usize)>, created: Option<u64>, tile: u64, prev: Option<u64>, at: usize) {
        let alive = self.conn_ids();
        self.groups.undo_drop(&moved, created, &alive);
        // The tile goes back after the tile it followed, or to its old index when that one has closed since.
        if self.rail_order.contains(&tile) {
            self.rail_order.retain(|c| *c != tile);
            let i = match prev {
                None => 0,
                Some(p) => self.rail_order.iter().position(|c| *c == p).map_or(at.min(self.rail_order.len()), |i| i + 1),
            };
            self.rail_order.insert(i, tile);
        }
        self.save_groups();
        self.ok("Grouping undone.");
    }

    pub fn save_groups(&mut self) {
        self.groups.v = 1;
        let b = serde_json::to_vec_pretty(&self.groups).unwrap_or_default();
        self.write_config("groups.json", b);
    }

    pub fn conn_ids(&self) -> Vec<u64> {
        self.conns.iter().map(|c| c.meta.id).collect()
    }

    fn save_conn(&mut self, meta: ConnMeta, uri: Secret, secrets: Vec<(String, Secret)>, group: Option<u64>) -> Result<Reply, Refusal> {
        // An action read back from JSON has no secrets, and saving it would erase the stored ones.
        let (Some(uri), true) = (uri.expose(), secrets.iter().all(|s| s.1.expose().is_some())) else {
            return Err(self.refuse("Nothing saved: the connection's address or a password did not come through."));
        };
        let id = meta.id;
        // Demo ids overlap real ones, so the demo keeps its edits in memory only.
        let saved = if self.ephemeral { Ok(()) } else { store_uri(id, uri) };
        if let Err(e) = saved {
            let msg = format!("Could not save the address to the Keychain: {e}");
            self.err(msg.clone());
            return Err(Refusal { kind: RefusalKind::Invalid, msg });
        }
        let stored: Vec<Result<(), String>> = if self.ephemeral { Vec::new() } else { secrets.iter().map(|(what, v)| set_secret(id, what, v.expose().unwrap_or_default())).collect() };
        let masked = db::mask_uri(uri);
        match self.conns.iter_mut().find(|c| c.meta.id == id) {
            Some(c) => {
                c.meta = meta;
                c.masked_uri = masked;
                c.writable = false;
                c.dbs.clear();
                c.server = Default::default();
            }
            None => self.conns.push(Conn { masked_uri: masked, ..Conn::new(meta, Status::Idle) }),
        }
        self.active = Some(id);
        self.save_conns();
        if let Some(g) = group {
            let all = self.conn_ids();
            if self.groups.place(id, Place::Into(g, None), &all) {
                self.save_groups();
            }
        }
        self.connect(id);
        if let Some(Err(e)) = stored.into_iter().find(|r| r.is_err()) {
            self.err(e);
        }
        Ok(Reply::Done)
    }

    pub fn connect(&mut self, id: u64) {
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

    /// Connects with a known address, so tests need no keychain. Not an action: the address may hold a password.
    pub fn connect_uri(&mut self, id: u64, uri: String) {
        let tools = self.cloud_tools();
        self.stop_conn_work(id);
        self.req += 1;
        let attempt = self.req;
        let timeout = self.timeout();
        let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == id) else { return };
        c.status = Status::Connecting;
        c.attempt = attempt;
        c.client = None;
        c.sql = None;
        c.tunnel = None;
        // A new session may see other indexes and statistics, so its plans start fresh.
        self.explain_prev.forget(id);
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
            self.worker.sql_connect_with(id, attempt, uri, timeout, net, adv);
        } else {
            self.worker.connect(id, attempt, uri, timeout, net);
        }
    }

    /// Where the proxy and gcloud are: Settings first, else where the browser found them, else the worker looks.
    pub fn cloud_tools(&self) -> crate::cloudsql::Tools {
        let set = |s: &str| Some(s.trim()).filter(|s| !s.is_empty()).map(std::path::PathBuf::from);
        let found = self.cloud.start.as_ref();
        crate::cloudsql::Tools { proxy: set(&self.prefs.cloud_sql_proxy).or_else(|| found.and_then(|s| s.proxy.clone())), gcloud: set(&self.prefs.gcloud).or_else(|| found.and_then(|s| s.gcloud.clone())) }
    }

    /// Drops the session but keeps the saved connection, which then leaves the rail.
    pub fn disconnect(&mut self, id: u64) {
        if let Some(name) = self.drop_session(id) {
            self.ok(format!("Disconnected from {name}"));
        }
    }

    /// Closes the connection's session and forgets what was listed on it. Returns its name, or None when it is gone.
    pub fn drop_session(&mut self, id: u64) -> Option<String> {
        let c = self.conns.iter_mut().find(|c| c.meta.id == id)?;
        (c.client, c.sql, c.tunnel, c.status, c.writable) = (None, None, None, Status::Idle, false);
        c.forget_lists();
        let name = c.meta.name.clone();
        self.stop_conn_work(id);
        self.pg_tables_asked.forget(id, None, None);
        self.explain_prev.forget(id);
        Some(name)
    }

    /// A watch holds its own client and a Shell run its own tunnel or relay, any of which would keep an old session alive.
    pub fn stop_conn_work(&mut self, id: u64) {
        for t in &mut self.tabs {
            if t.sel.conn == id {
                t.stop_work();
            } else if t.shell.attached.iter().any(|a| a.conn == id) {
                t.shell.stop();
            }
        }
    }

    pub fn save_conns(&mut self) {
        let metas: Vec<&ConnMeta> = self.conns.iter().map(|c| &c.meta).collect();
        let b = serde_json::to_vec_pretty(&metas).unwrap_or_default();
        self.write_config("connections.json", b);
    }

    /// A connection that could not open. Its card explains it when on screen, else a toast does.
    pub fn err_conn(&mut self, conn: u64, raw: String) {
        let name = self.conn_name(conn);
        let e = self.log_err(raw, Some(conn), None, format!("Connect · {name}"), "connect");
        if !self.conn_card_shown(conn) {
            self.err_toast(e);
        }
    }

    pub(super) fn on_connected(&mut self, conn: u64, attempt: u64, res: db::Res<(mongodb::Client, Vec<String>, Option<crate::ssh::Tunnel>)>) {
        // A connect the user stopped (Disconnect, removal) or replaced is dropped, and its tunnel with it.
        let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn && c.waits_for(attempt)) else { return };
        match res {
            Ok((client, dbs, tunnel)) => {
                // Trust the SSH host key the first time; later a different key is refused.
                let mut trusted = None;
                if let (Some(t), Some(sc)) = (&tunnel, c.meta.ssh.as_mut()) && sc.known_host.is_none() && !t.fingerprint.is_empty() {
                    sc.known_host = Some(t.fingerprint.clone());
                    trusted = Some(t.fingerprint.clone());
                }
                c.tunnel = tunnel.map(db::Way::Ssh);
                // Load every database's collections up front so the list can show counts.
                for d in &dbs {
                    self.worker.collections(conn, client.clone(), d.clone());
                }
                // Version, replica set and host for the status bar.
                self.worker.command(client.clone(), "admin".into(), doc! { "buildInfo": 1 }, &format!("srv-build:{conn}"));
                self.worker.command(client.clone(), "admin".into(), doc! { "hello": 1 }, &format!("srv-hello:{conn}"));
                c.client = Some(client);
                c.status = Status::Ready;
                c.forget_lists();
                c.dbs = dbs.into_iter().map(|d| (d, None)).collect();
                self.pg_tables_asked.forget(conn, None, None);
                let n = c.meta.name.clone();
                self.ok(format!("Connected to {n} (read-only)"));
                if let Some(fp) = trusted {
                    self.save_conns();
                    self.ok(format!("Connected to {n} through SSH (read-only). Host key remembered: {fp}"));
                }
                self.ui_fx.push(UiFx::Connected { conn, sql: false });
            }
            Err(e) => {
                c.status = Status::Failed(e.clone());
                self.err_conn(conn, e);
            }
        }
    }

    pub(super) fn on_sql_connected(&mut self, conn: u64, attempt: u64, res: db::Res<(db::sql::Db, Vec<String>, Option<db::Way>)>) {
        // Dropping a stopped or replaced connect's answer also stops its Cloud SQL proxy or SSH tunnel.
        let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn && c.waits_for(attempt)) else { return };
        match res {
            Ok((sql, dbs, tunnel)) => {
                let mut trusted = None;
                if let (Some(db::Way::Ssh(t)), Some(sc)) = (&tunnel, c.meta.ssh.as_mut()) && sc.known_host.is_none() && !t.fingerprint.is_empty() {
                    sc.known_host = Some(t.fingerprint.clone());
                    trusted = Some(t.fingerprint.clone());
                }
                c.tunnel = tunnel;
                // A server can hold hundreds of schemas, so only those on screen load now.
                self.sql_asked.retain(|k| k.0 != conn);
                let wanted: Vec<String> = dbs.iter().filter(|d| self.open_dbs.contains(&(conn, (*d).clone())) || self.tabs.iter().any(|t| t.sel.conn == conn && t.sel.db == **d)).cloned().collect();
                let pg = sql.dialect == db::sql::Dialect::Postgres;
                for d in wanted {
                    self.sql_asked.insert((conn, d.clone()));
                    // A PostgreSQL database lists its schemas first; the tables of the picked one follow.
                    if pg {
                        self.worker.sql_schemas(conn, sql.clone(), d);
                    } else {
                        self.worker.sql_tables(conn, sql.clone(), d);
                    }
                }
                c.server = ServerInfo { version: sql.label().split_once(' ').map(|(_, v)| v.to_string()).unwrap_or_default(), product: match sql.dialect { db::sql::Dialect::MySql => "MySQL", db::sql::Dialect::MariaDb => "MariaDB", db::sql::Dialect::Postgres => "PostgreSQL" }.into(), host: host_of(&c.masked_uri), tls: sql.tls, ..Default::default() };
                c.sql = Some(sql);
                c.status = Status::Ready;
                c.forget_lists();
                c.dbs = dbs.into_iter().map(|d| (d, None)).collect();
                self.pg_tables_asked.forget(conn, None, None);
                let n = c.meta.name.clone();
                self.ok(format!("Connected to {n} (read-only)"));
                if let Some(fp) = trusted {
                    self.save_conns();
                    self.ok(format!("Connected to {n} through SSH (read-only). Host key remembered: {fp}"));
                }
                self.ui_fx.push(UiFx::Connected { conn, sql: true });
            }
            Err(e) => {
                c.status = Status::Failed(e.clone());
                self.err_conn(conn, e);
            }
        }
    }

    /// The version, replica set and host a Mongo connection was asked for when it opened.
    pub(super) fn srv_reply(&mut self, tag: &str, res: db::Res<Document>) {
        // Nice to have: a server that refuses these just shows less.
        let Ok(d) = res else { return };
        let (kind, id) = tag.split_once(':').unwrap_or_default();
        if let Some(c) = self.conns.iter_mut().find(|c| id.parse() == Ok(c.meta.id)) {
            if kind == "srv-build" {
                c.server.version = d.get_str("version").unwrap_or_default().to_string();
            } else {
                c.server.set_name = d.get_str("setName").unwrap_or_default().to_string();
                // The member answering, else the host from the connection string.
                c.server.host = d.get_str("me").map(str::to_string).unwrap_or_else(|_| host_of(&c.masked_uri));
            }
        }
    }

    pub(super) fn collections_done(&mut self, conn: u64, db: String, res: db::Res<(Vec<String>, Vec<String>)>) {
        match res {
            Ok((v, views)) => {
                self.side_err.remove(&(conn, db.clone()));
                self.views.set(conn, &db, views);
                if let Some(d) = self.conns.iter_mut().find(|c| c.meta.id == conn).and_then(|c| c.dbs.iter_mut().find(|d| d.0 == db)) {
                    d.1 = Some(v);
                }
            }
            Err(e) => {
                self.side_err.insert((conn, db.clone()), e.clone());
                self.err_quiet(e, Some(conn), Some(Sel { conn, db: db.clone(), ..Default::default() }), format!("List collections · {db}"))
            }
        }
    }

    /// The connection form's Test answered.
    pub(super) fn tested(&mut self, req: u64, res: db::Res<String>, info: Option<db::sql::TestInfo>) {
        // A SQL test already says how long it took; the others get the time measured here.
        let started = self.test_started.take();
        let res = res.map(|m| match started {
            Some(t) if !m.contains(" ms") => format!("{}  ·  {} ms", m.trim_end_matches('.'), t.elapsed().as_millis()),
            _ => m,
        });
        self.conn_test = Some((req, res));
        self.test_info = info.map(|i| (req, i));
    }
}
