//! State behind src/app/server.rs, which draws it.

use std::time::{Duration, Instant};
use crate::db;
use mongodb::bson::{Document, doc};

#[derive(Default)]
pub struct ServerWin {
    pub conn: u64,
    /// Tags this window's commands, so answers to a closed window or another connection's are dropped.
    pub id: u64,
    /// Commands sent and not answered yet; nothing new is sent until they are, so a slow server
    /// never gets a pile of them.
    pub waiting: usize,
    pub ops_tab: bool,
    /// When to ask again; None asks on the next frame.
    pub next: Option<Instant>,
    /// Ask for the section on screen even when it would not poll by itself.
    pub refresh: bool,
    /// The latest serverStatus and the one before it, for per-second rates.
    pub status: Option<db::Res<(Instant, Document)>>,
    pub prev: Option<(Instant, Document)>,
    pub hello: Option<db::Res<Document>>,
    pub rs: Option<db::Res<Document>>,
    pub ops: Option<db::Res<Vec<Document>>>,
    pub auto_ops: bool,
    /// The role cannot list other users' operations, so only its own are asked for.
    pub own_only: bool,
    /// The operation whose Kill waits for its second click, and when it was armed (`arm_time`).
    pub confirm_kill: Option<(String, f64)>,
}

impl ServerWin {
    pub fn new(conn: u64, id: u64) -> Self {
        Self { conn, id, ..Default::default() }
    }

    /// Canned answers for the offline demo; `ops` opens the Operations section.
    pub fn demo(conn: u64, ops: bool) -> Self {
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

pub const POLL: Duration = Duration::from_secs(2);
