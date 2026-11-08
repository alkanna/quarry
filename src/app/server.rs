//! The Server window: health of the connected server and its running operations. It asks the
//! server only while it is open, Quarry is focused, and for the section on screen.

use super::*;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_secs(2);

/// What a role without the needed privilege sees in place of a section.
const NEEDS_MONITOR: &str = "Not available for this user: it needs the clusterMonitor role (on Atlas, atlasAdmin or a custom role that includes clusterMonitor).";

#[derive(Default)]
pub(super) struct ServerWin {
    pub(super) conn: u64,
    /// Tags this window's commands, so answers to a closed window or another connection's are dropped.
    id: u64,
    /// Commands sent and not answered yet; nothing new is sent until they are, so a slow server
    /// never gets a pile of them.
    waiting: usize,
    ops_tab: bool,
    /// When to ask again; None asks on the next frame.
    next: Option<Instant>,
    /// Ask for the section on screen even when it would not poll by itself.
    refresh: bool,
    /// The latest serverStatus and the one before it, for per-second rates.
    status: Option<db::Res<(Instant, Document)>>,
    prev: Option<(Instant, Document)>,
    hello: Option<db::Res<Document>>,
    rs: Option<db::Res<Document>>,
    ops: Option<db::Res<Vec<Document>>>,
    auto_ops: bool,
    /// The role cannot list other users' operations, so only its own are asked for.
    own_only: bool,
    confirm_kill: Option<String>,
}

impl ServerWin {
    pub(super) fn new(conn: u64, id: u64) -> Self {
        Self { conn, id, ..Default::default() }
    }

    /// Canned answers for the offline demo; `ops` opens the Operations section.
    pub(super) fn demo(conn: u64, ops: bool) -> Self {
        let at = |ins: i64, q: i64, cmd: i64, bytes: i64| {
            doc! {
                "version": "8.0.4", "host": "shop-db-1:27017", "uptime": 302_400.0,
                "connections": { "current": 34, "available": 838 },
                "opcounters": { "insert": ins, "query": q, "update": 120, "delete": 4, "getmore": 0, "command": cmd },
                "network": { "bytesIn": bytes, "bytesOut": bytes * 3 },
                "wiredTiger": { "cache": { "bytes currently in the cache": 1_288_490_188_i64, "maximum bytes configured": 4_294_967_296_i64 } },
            }
        };
        let now = Instant::now();
        let t = |ms: i64| mongodb::bson::DateTime::from_millis(1_700_000_000_000 + ms);
        let op = |id: i32, secs: f64, op: &str, ns: &str, app: &str, cmd: Document| doc! { "opid": id, "microsecs_running": (secs * 1e6) as i64, "op": op, "ns": ns, "appName": app, "client": "10.0.4.17:51522", "command": cmd };
        Self {
            conn,
            ops_tab: ops,
            status: Some(Ok((now, at(1_500, 9_000, 20_000, 9_000_000)))),
            prev: Some((now - POLL, at(1_440, 8_760, 19_900, 8_600_000))),
            hello: Some(Ok(doc! { "setName": "rs0", "primary": "shop-db-1:27017", "hosts": ["shop-db-1:27017", "shop-db-2:27017", "shop-db-3:27017"] })),
            rs: Some(Ok(doc! { "members": [
                { "name": "shop-db-1:27017", "stateStr": "PRIMARY", "optimeDate": t(10_000) },
                { "name": "shop-db-2:27017", "stateStr": "SECONDARY", "optimeDate": t(10_000) },
                { "name": "shop-db-3:27017", "stateStr": "SECONDARY", "optimeDate": t(4_000) },
            ] })),
            ops: Some(Ok(vec![
                op(9121, 184.2, "command", "shop.orders", "reports-cron", doc! { "aggregate": "orders", "pipeline": [{ "$match": { "createdAt": { "$gte": t(0) } } }, { "$group": { "_id": "$status", "n": { "$sum": 1 } } }] }),
                op(9188, 12.7, "command", "shop.orders", "", doc! { "createIndexes": "orders", "indexes": [{ "key": { "createdAt": -1, "total": 1 }, "name": "createdAt_-1_total_1" }] }),
                op(9204, 0.4, "query", "shop.customers", "shop-api", doc! { "find": "customers", "filter": { "email": "ada@example.com" } }),
            ])),
            ..Default::default()
        }
    }
}

/// Whether a server error means the user lacks a privilege, including Atlas's wording.
pub(super) fn is_unauthorized(e: &str) -> bool {
    e.contains("(Unauthorized)") || e.contains("not authorized") || e.contains("is not allowed to do action")
}

fn num(d: &Document, path: &str) -> Option<f64> {
    match db::get_path(d, path)? {
        Bson::Int32(n) => Some(*n as f64),
        Bson::Int64(n) => Some(*n as f64),
        Bson::Double(n) => Some(*n),
        _ => None,
    }
}

fn uptime(s: f64) -> String {
    let s = s as i64;
    match s {
        s if s >= 86_400 => format!("{}d {}h", s / 86_400, s % 86_400 / 3600),
        s if s >= 3600 => format!("{}h {}m", s / 3600, s % 3600 / 60),
        s => format!("{}m {}s", s / 60, s % 60),
    }
}

/// Label and value rows from a serverStatus answer; rates need the one before it and the seconds between.
pub(super) fn overview_rows(cur: &Document, prev: Option<(f64, &Document)>) -> Vec<(&'static str, String)> {
    let mut rows = vec![("Version", cur.get_str("version").unwrap_or_default().to_string()), ("Host", cur.get_str("host").unwrap_or_default().to_string())];
    if let Some(s) = num(cur, "uptime") {
        rows.push(("Uptime", uptime(s)));
    }
    if let (Some(c), Some(a)) = (num(cur, "connections.current"), num(cur, "connections.available")) {
        rows.push(("Connections", format!("{} open, {} available", group(&(c as i64).to_string()), group(&(a as i64).to_string()))));
    }
    const OPS: [&str; 6] = ["insert", "query", "update", "delete", "getmore", "command"];
    match prev.filter(|(secs, _)| *secs > 0.0) {
        Some((secs, p)) => {
            let rate = |path: &str| (num(cur, path).unwrap_or(0.0) - num(p, path).unwrap_or(0.0)).max(0.0) / secs;
            let per: Vec<f64> = OPS.iter().map(|k| rate(&format!("opcounters.{k}"))).collect();
            let parts: Vec<String> = OPS.iter().zip(&per).filter(|(_, r)| **r >= 0.5).map(|(k, r)| format!("{k} {r:.0}")).collect();
            rows.push(("Operations", format!("{:.0}/s{}", per.iter().sum::<f64>(), if parts.is_empty() { String::new() } else { format!("  ({})", parts.join(", ")) })));
            rows.push(("Network", format!("{}/s in, {}/s out", human_bytes(rate("network.bytesIn") as usize), human_bytes(rate("network.bytesOut") as usize))));
        }
        None => rows.push(("Operations", "measuring…".into())),
    }
    if let (Some(used), Some(max)) = (num(cur, "wiredTiger.cache.bytes currently in the cache"), num(cur, "wiredTiger.cache.maximum bytes configured")) {
        rows.push(("Cache", format!("{} of {} ({:.0}%)", human_bytes(used as usize), human_bytes(max as usize), used / max.max(1.0) * 100.0)));
    }
    rows
}

/// Replica set members as (name, state, lag) rows, or a note when there is no set to show.
/// replSetGetStatus gives states and lag; without it, hello still names the members.
pub(super) fn member_rows(rs: Option<&db::Res<Document>>, hello: Option<&db::Res<Document>>) -> (Vec<[String; 3]>, Option<String>) {
    if let Some(Ok(d)) = rs {
        let members: Vec<&Document> = d.get_array("members").map(|a| a.iter().filter_map(Bson::as_document).collect()).unwrap_or_default();
        let optime = |m: &Document| m.get_datetime("optimeDate").ok().map(|t| t.timestamp_millis());
        let primary = members.iter().find(|m| m.get_str("stateStr") == Ok("PRIMARY")).and_then(|m| optime(m));
        let rows = members
            .iter()
            .map(|m| {
                let state = m.get_str("stateStr").unwrap_or_default();
                let lag = match (state, primary, optime(m)) {
                    ("SECONDARY", Some(p), Some(o)) => format!("{}s behind", ((p - o) / 1000).max(0)),
                    _ => String::new(),
                };
                [m.get_str("name").unwrap_or_default().to_string(), state.to_string(), lag]
            })
            .collect();
        return (rows, None);
    }
    let note = match rs {
        Some(Err(e)) if e.contains("NoReplicationEnabled") || e.contains("not running with --replSet") => return (Vec::new(), Some("Standalone server: no replica set.".into())),
        Some(Err(e)) if is_unauthorized(e) => Some("States and lag need the clusterMonitor role.".to_string()),
        Some(Err(e)) => Some(format!("Replica set status unavailable: {e}")),
        _ => None,
    };
    match hello {
        Some(Ok(h)) if h.get_str("msg") == Ok("isdbgrid") => (Vec::new(), Some("Sharded cluster: connected through mongos.".into())),
        Some(Ok(h)) if h.contains_key("setName") => {
            let primary = h.get_str("primary").unwrap_or_default();
            let hosts = h.get_array("hosts").map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>()).unwrap_or_default();
            (hosts.iter().map(|x| [x.to_string(), if *x == primary { "PRIMARY".into() } else { "member".into() }, String::new()]).collect(), note)
        }
        Some(Ok(_)) => (Vec::new(), Some("Standalone server: no replica set.".into())),
        _ => (Vec::new(), note),
    }
}

/// Running operations, longest first, without the currentOp that listed them.
pub(super) fn ops_of(d: &Document) -> Vec<Document> {
    let mut ops: Vec<Document> = d.get_array("inprog").map(|a| a.iter().filter_map(Bson::as_document).filter(|o| !o.get_document("command").is_ok_and(|c| c.contains_key("currentOp"))).cloned().collect()).unwrap_or_default();
    ops.sort_by(|a, b| num(b, "microsecs_running").unwrap_or(0.0).total_cmp(&num(a, "microsecs_running").unwrap_or(0.0)));
    ops
}

impl App {
    pub(super) fn server_window(&mut self, ctx: &egui::Context) {
        let Some(mut s) = self.server.take() else { return };
        let conn = s.conn;
        let client = self.conn(conn).and_then(|c| c.client.clone());
        let writable = self.conn(conn).is_some_and(|c| c.writable);
        let name = self.conn(conn).map(|c| c.meta.name.clone()).unwrap_or_default();
        let polls = !s.ops_tab || s.auto_ops;
        if let (Some(client), false) = (&client, self.demo) {
            let now = Instant::now();
            if s.waiting == 0 && s.next.is_none_or(|t| now >= t) && ctx.input(|i| i.focused) {
                s.next = Some(now + POLL);
                let (id, mut sent) = (s.id, 0);
                let mut ask = |cmd: Document, what: &str| {
                    sent += 1;
                    self.worker.command(client.clone(), "admin".into(), cmd, &format!("server:{id}:{what}"));
                };
                if s.ops_tab {
                    if s.auto_ops || s.ops.is_none() || s.refresh {
                        let mut cmd = doc! { "currentOp": 1, "active": true };
                        if s.own_only {
                            cmd.insert("$ownOps", true);
                        }
                        ask(cmd, "ops");
                    }
                } else {
                    // A section that failed is not asked again until Refresh, so a missing privilege costs one call.
                    if !matches!(s.status, Some(Err(_))) {
                        ask(doc! { "serverStatus": 1, "repl": 0, "metrics": 0, "locks": 0 }, "status");
                    }
                    if !matches!(s.rs, Some(Err(_))) {
                        ask(doc! { "replSetGetStatus": 1 }, "rs");
                    }
                    if s.hello.is_none() {
                        ask(doc! { "hello": 1 }, "hello");
                    }
                }
                s.waiting = sent;
                s.refresh = false;
            }
            if polls {
                ctx.request_repaint_after(POLL);
            }
        }
        let mut open = true;
        let (mut unlock, mut kill) = (false, None);
        let screen = ctx.content_rect();
        let r = dialog(egui::Id::unique(("quarry-layer", "server-window"))).show(ctx, |ui| {
            ui.set_width((screen.width() * 0.7).clamp(620.0, 1100.0));
            ui.horizontal(|ui| {
                dialog_title(ui, format!("Server: {name}"));
                ui.add_space(12.0);
                for (ops, label) in [(false, "Overview"), (true, "Operations")] {
                    if ui.selectable_label(s.ops_tab == ops, label).clicked() && s.ops_tab != ops {
                        s.ops_tab = ops;
                        s.next = None;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⟳ Refresh").clicked() {
                        s.status = s.status.take().filter(|r| r.is_ok());
                        s.rs = s.rs.take().filter(|r| r.is_ok());
                        s.hello = None;
                        s.refresh = true;
                        s.next = None;
                    }
                    if s.ops_tab {
                        ui.checkbox(&mut s.auto_ops, "Every 2 s");
                    }
                });
            });
            ui.add_space(4.0);
            egui::ScrollArea::both().max_height(screen.height() - 220.0).auto_shrink([false, true]).show(ui, |ui| {
                if s.ops_tab {
                    self.ops_section(ui, &mut s, writable, &mut unlock, &mut kill);
                } else {
                    overview_section(ui, &s);
                }
            });
            open = !done(ui);
        });
        if unlock {
            self.modal = Some(Modal::Unlock(conn, None));
        }
        if let (Some(op), Some(client)) = (kill, client) {
            self.worker.command(client, "admin".into(), doc! { "killOp": 1, "op": op }, &format!("server:{}:kill", s.id));
        }
        if open && !(r.should_close() && self.modal.is_none()) {
            self.server = Some(s);
        }
    }

    fn ops_section(&self, ui: &mut egui::Ui, s: &mut ServerWin, writable: bool, unlock: &mut bool, kill: &mut Option<Bson>) {
        if s.own_only {
            ui.label(RichText::new("Showing only your operations: listing everyone's needs the inprog privilege (clusterMonitor role).").size(TXT_SMALL).color(WARNING()));
        }
        if !writable {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Read-only: unlock writes to kill operations.").color(SUCCESS()));
                *unlock = ui.button("Unlock…").clicked();
            });
        }
        let ops = match &s.ops {
            None => return spin(ui),
            Some(Err(e)) if is_unauthorized(e) => return void(ui.label(RichText::new(NEEDS_MONITOR).color(MUTED()))),
            Some(Err(e)) => return void(ui.label(RichText::new(format!("Could not list operations: {e}")).color(ALERT()))),
            Some(Ok(ops)) if ops.is_empty() => return void(ui.label(RichText::new("Nothing is running.").color(MUTED()))),
            Some(Ok(ops)) => ops,
        };
        egui::Grid::new("server-ops").striped(true).num_columns(7).spacing([16.0, 6.0]).show(ui, |ui| {
            for h in ["OPID", "SECS", "OP", "NAMESPACE", "CLIENT", "COMMAND", ""] {
                caption(ui, h);
            }
            ui.end_row();
            for o in ops {
                let id = o.get("opid").cloned().unwrap_or(Bson::Null);
                let key = db::compact(&id);
                let mono = |t: String, c: Color32| RichText::new(t).monospace().color(c);
                ui.label(mono(key.clone(), FAINT()));
                ui.label(mono(format!("{:.1}", num(o, "microsecs_running").unwrap_or(0.0) / 1e6), BRIGHT()));
                ui.label(mono(o.get_str("op").unwrap_or_default().into(), TEXT()));
                ui.label(mono(o.get_str("ns").unwrap_or_default().into(), TEXT()));
                let client = [o.get_str("appName"), o.get_str("client"), o.get_str("client_s")].into_iter().flatten().filter(|x| !x.is_empty()).collect::<Vec<_>>().join("  ");
                ui.label(mono(client, MUTED()));
                let cmd = o.get("command").map(db::shell_text).unwrap_or_default();
                let short: String = cmd.chars().take(80).collect();
                ui.label(mono(if short.len() < cmd.len() { format!("{short}…") } else { short }, C_STRING())).on_hover_text(&cmd);
                ui.horizontal(|ui| {
                    if !writable {
                        return;
                    }
                    if s.confirm_kill.as_deref() == Some(key.as_str()) {
                        if danger(ui, "Kill it").clicked() {
                            *kill = Some(id.clone());
                            s.confirm_kill = None;
                        }
                        if ui.button("Keep").clicked() {
                            s.confirm_kill = None;
                        }
                    } else if ui.button("Kill…").clicked() {
                        s.confirm_kill = Some(key);
                    }
                });
                ui.end_row();
            }
        });
    }

    /// Applies an answer to one of the Server window's commands. Errors stay in the window
    /// instead of the status bar, so a role without privileges is not told so every 2 seconds.
    pub(super) fn server_reply(&mut self, tag: &str, r: db::Res<Document>) {
        let mut parts = tag.splitn(3, ':').skip(1);
        let (id, what) = (parts.next().and_then(|i| i.parse::<u64>().ok()), parts.next().unwrap_or_default());
        if what == "kill" {
            match r {
                Ok(_) => {
                    self.ok("Asked the server to kill the operation");
                    if let Some(s) = self.server.as_mut().filter(|s| Some(s.id) == id) {
                        s.refresh = true;
                        s.next = None;
                    }
                }
                Err(e) if is_unauthorized(&e) => self.err("Your role cannot kill this operation: killing other users' operations needs the killop privilege."),
                Err(e) => self.err(e),
            }
            return;
        }
        let Some(s) = self.server.as_mut().filter(|s| Some(s.id) == id) else { return };
        s.waiting = s.waiting.saturating_sub(1);
        match what {
            "status" => {
                if let Some(Ok(cur)) = s.status.take() {
                    s.prev = Some(cur);
                }
                s.status = Some(r.map(|d| (Instant::now(), d)));
            }
            "hello" => s.hello = Some(r),
            "rs" => s.rs = Some(r),
            "ops" => match r {
                Err(e) if is_unauthorized(&e) && !s.own_only => {
                    s.own_only = true;
                    s.refresh = true;
                    s.next = None;
                }
                r => s.ops = Some(r.map(|d| ops_of(&d))),
            },
            _ => {}
        }
    }
}

fn void(_: egui::Response) {}

fn overview_section(ui: &mut egui::Ui, s: &ServerWin) {
    settings_section(ui, "SERVER", |ui| match &s.status {
        None => spin(ui),
        Some(Err(e)) if is_unauthorized(e) => void(ui.label(RichText::new(NEEDS_MONITOR).color(MUTED()))),
        Some(Err(e)) => void(ui.label(RichText::new(format!("Could not read the server status: {e}")).color(ALERT()))),
        Some(Ok((at, cur))) => {
            let prev = s.prev.as_ref().map(|(t, d)| (at.duration_since(*t).as_secs_f64(), d));
            egui::Grid::new("server-overview").num_columns(2).spacing([24.0, 6.0]).show(ui, |ui| {
                for (k, v) in overview_rows(cur, prev) {
                    ui.label(RichText::new(k).color(MUTED()));
                    ui.label(RichText::new(v).monospace().color(BRIGHT()));
                    ui.end_row();
                }
            });
        }
    });
    ui.add_space(8.0);
    settings_section(ui, "REPLICA SET", |ui| {
        if s.rs.is_none() && s.hello.is_none() {
            return spin(ui);
        }
        let (rows, note) = member_rows(s.rs.as_ref(), s.hello.as_ref());
        if !rows.is_empty() {
            egui::Grid::new("server-members").num_columns(3).spacing([24.0, 6.0]).show(ui, |ui| {
                for [name, state, lag] in rows {
                    ui.label(RichText::new(name).monospace().color(BRIGHT()));
                    ui.label(RichText::new(&state).color(if state == "PRIMARY" { SUCCESS() } else { TEXT() }));
                    ui.label(RichText::new(lag).monospace().color(MUTED()));
                    ui.end_row();
                }
            });
        }
        if let Some(n) = note {
            ui.label(RichText::new(n).size(TXT_SMALL).color(MUTED()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privilege_errors_are_recognized() {
        assert!(is_unauthorized("Command failed: Error code 13 (Unauthorized): not authorized on admin to execute command { serverStatus: 1 }"));
        assert!(is_unauthorized("Command failed: Error code 8000 (AtlasError): user is not allowed to do action [serverStatus] on [admin.]"));
        assert!(!is_unauthorized("Command failed: Error code 76 (NoReplicationEnabled): not running with --replSet"));
    }

    #[test]
    fn overview_rates_come_from_two_samples() {
        let at = |ins: i64, bytes: i64| doc! { "version": "8.0.4", "host": "db1:27017", "uptime": 90_061.0, "connections": { "current": 34, "available": 838 }, "opcounters": { "insert": ins, "query": 0, "update": 0, "delete": 0, "getmore": 0, "command": 10 }, "network": { "bytesIn": bytes, "bytesOut": 0 } };
        let rows = overview_rows(&at(100, 0), None);
        assert!(rows.contains(&("Uptime", "1d 1h".into())));
        assert!(rows.contains(&("Connections", "34 open, 838 available".into())));
        assert!(rows.contains(&("Operations", "measuring…".into())));
        let rows = overview_rows(&at(140, 4096), Some((2.0, &at(100, 0))));
        assert!(rows.contains(&("Operations", "20/s  (insert 20)".into())));
        assert!(rows.contains(&("Network", "2 KB/s in, 0 bytes/s out".into())));
    }

    #[test]
    fn members_fall_back_to_hello() {
        let p = mongodb::bson::DateTime::from_millis(10_000);
        let s = mongodb::bson::DateTime::from_millis(7_000);
        let rs = Ok(doc! { "members": [{ "name": "a:1", "stateStr": "PRIMARY", "optimeDate": p }, { "name": "b:1", "stateStr": "SECONDARY", "optimeDate": s }] });
        let (rows, note) = member_rows(Some(&rs), None);
        assert_eq!(rows[1], ["b:1".to_string(), "SECONDARY".into(), "3s behind".into()]);
        assert!(note.is_none());
        let denied = Err("Command failed: Error code 13 (Unauthorized): not authorized".to_string());
        let hello = Ok(doc! { "setName": "rs0", "primary": "a:1", "hosts": ["a:1", "b:1"] });
        let (rows, note) = member_rows(Some(&denied), Some(&hello));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][1], "PRIMARY");
        assert_eq!(note.unwrap(), "States and lag need the clusterMonitor role.");
        let standalone = Err("Command failed: Error code 76 (NoReplicationEnabled): not running with --replSet".to_string());
        assert_eq!(member_rows(Some(&standalone), Some(&Ok(doc! {}))).1.unwrap(), "Standalone server: no replica set.");
    }

    #[test]
    fn ops_are_longest_first_without_currentop() {
        let d = doc! { "inprog": [{ "opid": 1, "microsecs_running": 5_i64 }, { "opid": 2, "microsecs_running": 900_i64 }, { "opid": 3, "command": { "currentOp": 1 } }] };
        let ops = ops_of(&d);
        assert_eq!(ops.iter().map(|o| o.get_i32("opid").unwrap()).collect::<Vec<_>>(), [2, 1]);
    }
}
