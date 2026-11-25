//! Connections run headlessly: connect answers made up and injected, stale ones dropped, the writes lock, and a real
//! connect when QUARRY_TEST_MONGO is set.

use super::*;
use crate::db::sql::Engine;
use crate::state::errors::Tone;
use crate::state::groups::Place;
use crate::state::{Conn, ConnMeta, Status};
use mongodb::bson::doc;

const CONN: u64 = 4;

pub(super) fn conn_actions() -> Vec<Action> {
    let mut all: Vec<Action> = [ConnA::Connect { conn: 1 }, ConnA::Disconnect { conn: 1 }, ConnA::Activate { conn: 1 }, ConnA::Unlock { conn: 1 }, ConnA::Lock { conn: 1 }].into_iter().map(Action::Conn).collect();
    let (db, coll) = (|| "shop".to_string(), || "orders".to_string());
    let side = [
        SideA::ToggleDb { conn: 1, db: db() },
        SideA::OpenDb { conn: 1, db: db() },
        SideA::Fold { conn: 1, db: db(), views: true },
        SideA::Revealed,
        SideA::Asked { conn: 1, db: db(), retry: true },
        SideA::Schema { conn: 1, db: db(), schema: "public".into() },
        SideA::MongoNewDb { conn: 1 },
        SideA::MongoNewColl { conn: 1, db: db() },
        SideA::MongoCreate { conn: 1, db: db(), name: coll(), form: Default::default(), ver: Some((8, 0)) },
        SideA::MongoCreateView { conn: 1, db: db(), name: "paid".into(), view: Some((coll(), vec![doc! { "$match": { "paid": true } }])) },
        SideA::Rename { conn: 1, db: db(), coll: coll(), to: "orders_2024".into() },
        SideA::Duplicate { conn: 1, db: db(), coll: coll(), to: "orders_copy".into() },
        SideA::DropDb { conn: 1, db: db() },
        SideA::DropColl { conn: 1, db: db(), coll: coll() },
        SideA::Stats { conn: 1, db: db(), coll: coll() },
        SideA::ViewDef { conn: 1, db: db(), coll: coll() },
    ];
    all.extend(side.into_iter().map(Action::Side));
    let groups = [
        GroupA::Toggle { group: 1 },
        GroupA::FoldAll,
        GroupA::Move { conn: 1, to: Place::Into(2, Some(0)) },
        GroupA::New { name: "Prod".into() },
        GroupA::Rename { group: 1, name: "Prod".into() },
        GroupA::Color { group: 1, color: "red".into() },
        GroupA::ConnectAll { group: 1 },
        GroupA::DisconnectAll { group: 1 },
        GroupA::RailOpen { group: 1 },
        GroupA::Unfold { group: 1 },
        GroupA::Save { group: 1, name: "Prod".into(), color: "red".into(), fold_rail: true, members: vec![1, 2] },
        GroupA::Delete { group: 1 },
        GroupA::Drop(RailDrop::Pair { conn: 1, target: 2 }),
        GroupA::RailOrder { order: vec![2, 1] },
    ];
    all.push(Action::Conn(ConnA::Switch { conn: 1 }));
    all.push(Action::Conn(ConnA::Save { meta: ConnMeta { id: 1, name: "local".into(), ..Default::default() }, uri: Secret::new("mongodb://ada:pw@localhost".into()), secrets: vec![("ssh-password".into(), Secret::new("pw".into()))], group: Some(2) }));
    all.extend(groups.into_iter().map(|g| Action::Conn(ConnA::Group(g))));
    all
}

fn side(c: &mut Core, a: SideA) -> Result<Reply, Refusal> {
    c.execute(Action::Side(a))
}

fn conn(c: &mut Core, a: ConnA) -> Result<Reply, Refusal> {
    c.execute(Action::Conn(a))
}

/// A saved connection waiting for the answer to connect attempt 7.
fn connecting(engine: Engine) -> Core {
    let mut core = Core::for_test();
    let mut c = Conn::new(ConnMeta { id: CONN, name: "shop-prod".into(), engine, ..Default::default() }, Status::Connecting);
    c.attempt = 7;
    core.conns.push(c);
    core
}

#[test]
fn connect_in_an_ephemeral_core_never_reads_the_keychain() {
    let mut core = connecting(Engine::Mongo);
    core.conns[0].status = Status::Idle;
    assert_eq!(conn(&mut core, ConnA::Connect { conn: CONN }), Ok(Reply::Done));
    assert_eq!(core.status, "Offline demo: no database is connected.");
    assert!(core.conns[0].status == Status::Idle);
}

#[test]
fn a_stale_connect_answer_is_dropped_and_a_failure_is_explained() {
    for engine in [Engine::Mongo, Engine::MySql] {
        let mut core = connecting(engine);
        let answer = |attempt: u64| match engine {
            Engine::Mongo => Msg::Connected { conn: CONN, attempt, res: Err("connection refused".into()) },
            _ => Msg::SqlConnected { conn: CONN, attempt, res: Err("connection refused".into()) },
        };
        // An answer to an earlier attempt, replaced by attempt 7, changes nothing.
        core.inject(answer(6));
        assert!(core.conns[0].status == Status::Connecting);
        assert!(core.errors.is_empty() && core.toasts.is_empty());
        core.inject(answer(7));
        assert!(matches!(core.conns[0].status, Status::Failed(_)));
        assert_eq!(core.errors.len(), 1);
        assert!(core.errors[0].connect && core.errors[0].place == "Connect · shop-prod");
        // No card is on screen without an active connection, so a toast explains it.
        assert!(core.toasts.iter().any(|t| t.tone == Tone::Err));
        // A late duplicate of the same attempt is dropped: the connection no longer waits for it.
        core.inject(answer(7));
        assert_eq!(core.errors.len(), 1);
    }
}

#[test]
fn unlock_lock_and_disconnect() {
    let mut core = connecting(Engine::Mongo);
    core.conns[0].status = Status::Ready;
    core.conns[0].dbs = vec![("shop".into(), Some(vec!["orders".into()]))];
    assert!(!core.conn_writable(CONN), "every connection starts read-only");
    conn(&mut core, ConnA::Unlock { conn: CONN }).unwrap();
    assert!(core.conn_writable(CONN));
    assert_eq!(core.status, "Writes allowed on shop-prod for this session");
    conn(&mut core, ConnA::Lock { conn: CONN }).unwrap();
    assert!(!core.conn_writable(CONN));
    assert_eq!(core.status, "Writes locked");
    conn(&mut core, ConnA::Unlock { conn: CONN }).unwrap();
    conn(&mut core, ConnA::Disconnect { conn: CONN }).unwrap();
    let c = &core.conns[0];
    assert!(c.status == Status::Idle && !c.writable && c.dbs.is_empty());
    assert_eq!(core.status, "Disconnected from shop-prod");
    // Activating an idle connection connects it, which an ephemeral core answers with the demo note.
    conn(&mut core, ConnA::Activate { conn: CONN }).unwrap();
    assert_eq!((core.active, core.status.as_str()), (Some(CONN), "Offline demo: no database is connected."));
}

#[test]
fn server_info_collections_and_test_answers() {
    let mut core = connecting(Engine::Mongo);
    core.conns[0].status = Status::Ready;
    core.conns[0].dbs = vec![("shop".into(), None)];
    core.inject(Msg::Command { tag: format!("srv-build:{CONN}"), res: Ok(doc! { "version": "8.0.4" }) });
    core.inject(Msg::Command { tag: format!("srv-hello:{CONN}"), res: Ok(doc! { "setName": "rs0", "me": "db1:27017" }) });
    let s = &core.conns[0].server;
    assert_eq!((s.version.as_str(), s.set_name.as_str(), s.host.as_str()), ("8.0.4", "rs0", "db1:27017"));
    // A server that refuses them just shows less, without telling.
    core.inject(Msg::Command { tag: format!("srv-build:{CONN}"), res: Err("not authorized".into()) });
    assert!(core.toasts.is_empty() && core.errors.is_empty());

    core.inject(Msg::Collections { conn: CONN, db: "shop".into(), res: Err("not authorized on shop".into()) });
    assert!(core.side_err.contains_key(&(CONN, "shop".into())));
    assert!(core.toasts.is_empty(), "a list nobody asked for fails quietly");
    assert_eq!(core.errors.len(), 1);
    core.inject(Msg::Collections { conn: CONN, db: "shop".into(), res: Ok((vec!["orders".into(), "paid".into()], vec!["paid".into()])) });
    assert!(core.side_err.is_empty());
    assert_eq!(core.conns[0].dbs[0].1.as_deref(), Some(&["orders".to_string(), "paid".to_string()][..]));
    assert!(core.views.has(CONN, "shop", "paid"));

    core.test_started = Some(std::time::Instant::now());
    core.inject(Msg::Tested { req: 9, res: Ok("Connected.".into()), info: None });
    let (req, res) = core.conn_test.as_ref().unwrap();
    assert_eq!(*req, 9);
    assert!(res.as_ref().unwrap().starts_with("Connected  ·  ") && res.as_ref().unwrap().ends_with(" ms"));
}

/// Connects to the live MongoDB, then leaves the reload of its tabs to the app.
#[test]
fn live_mongo_connect() {
    let Ok(uri) = std::env::var("QUARRY_TEST_MONGO") else { return };
    let mut core = Core::for_test();
    core.conns.push(Conn::new(ConnMeta { id: CONN, name: "test".into(), ..Default::default() }, Status::Idle));
    core.connect_uri(CONN, uri);
    assert!(core.conns[0].status == Status::Connecting);
    core.step_until("the connect", |c| c.conns[0].status != Status::Connecting);
    let c = &core.conns[0];
    assert!(c.status == Status::Ready, "{}", core.status);
    assert!(c.client.is_some() && !c.writable && !c.dbs.is_empty());
    assert_eq!(core.status, "Connected to test (read-only)");
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Connected { conn: CONN, sql: false }]));
    core.step_until("the server version", |c| !c.conns[0].server.version.is_empty());
}

#[test]
fn the_tree_opens_folds_and_retries() {
    let mut core = connecting(Engine::Postgres);
    core.conns[0].status = Status::Ready;
    let db = || "shop".to_string();
    side(&mut core, SideA::ToggleDb { conn: CONN, db: db() }).unwrap();
    assert!(core.open_dbs.contains(&(CONN, db())));
    side(&mut core, SideA::ToggleDb { conn: CONN, db: db() }).unwrap();
    assert!(core.open_dbs.is_empty());
    side(&mut core, SideA::OpenDb { conn: CONN, db: db() }).unwrap();
    side(&mut core, SideA::OpenDb { conn: CONN, db: db() }).unwrap();
    assert!(core.open_dbs.contains(&(CONN, db())), "a recent database never closes one that is open");
    assert_eq!(core.reveal, Some((CONN, db(), None)));
    side(&mut core, SideA::Revealed).unwrap();
    assert!(core.reveal.is_none());
    side(&mut core, SideA::Fold { conn: CONN, db: db(), views: true }).unwrap();
    assert!(core.side_closed.contains(&(CONN, db(), true)));
    side(&mut core, SideA::Fold { conn: CONN, db: db(), views: true }).unwrap();
    assert!(core.side_closed.is_empty());

    // A failed schema list shows in the tree; Retry clears it and the failures of the database's schemas.
    core.inject(Msg::SqlSchemas { conn: CONN, database: db(), res: Err("permission denied for database shop".into()) });
    core.inject(Msg::SqlTables { conn: CONN, database: db(), schema: "sales".into(), res: Err("permission denied for schema sales".into()) });
    assert_eq!(core.side_err.len(), 2);
    assert!(core.toasts.is_empty(), "lists fail quietly");
    core.side_err.insert((CONN, "shopping".into()), "other".into());
    side(&mut core, SideA::Asked { conn: CONN, db: db(), retry: true }).unwrap();
    assert_eq!(core.side_err.len(), 1, "only the retried database's errors go");
    assert!(core.sql_asked.contains(&(CONN, db())));

    // The schema list arrives: no pick yet, so public is picked.
    let schema = |name: &str, system: bool| crate::db::sql::PgSchema { name: name.into(), tables: 0, system };
    core.inject(Msg::SqlSchemas { conn: CONN, database: db(), res: Ok(vec![schema("pg_catalog", true), schema("public", false), schema("sales", false)]) });
    assert_eq!(core.conns[0].pg_picked("shop"), "public");
    core.inject(Msg::SqlTables { conn: CONN, database: db(), schema: "sales".into(), res: Ok((vec!["orders".into(), "paid".into()], vec!["paid".into()])) });
    assert_eq!(core.conns[0].tables_in("shop", "sales").map(Vec::len), Some(2));
    side(&mut core, SideA::Schema { conn: CONN, db: db(), schema: "sales".into() }).unwrap();
    assert_eq!(core.conns[0].pg_picked("shop"), "sales");
}

#[test]
fn mongo_writes_ask_to_unlock_first() {
    let mut core = connecting(Engine::Mongo);
    core.conns[0].status = Status::Ready;
    // Locked: New collection refuses without a toast, so the app's Unlock runs it again after.
    let r = side(&mut core, SideA::MongoNewColl { conn: CONN, db: "shop".into() }).unwrap_err();
    assert_eq!(r.kind, RefusalKind::ReadOnly { conn: CONN });
    assert!(core.toasts.is_empty() && core.take_ui_fx().is_empty());
    let create = || SideA::MongoCreate { conn: CONN, db: "shop".into(), name: "orders".into(), form: Default::default(), ver: None };
    assert_eq!(side(&mut core, create()).unwrap_err().kind, RefusalKind::ReadOnly { conn: CONN });
    // A confirmed drop on a locked connection opens Unlock with nothing after it, as it always did.
    side(&mut core, SideA::DropColl { conn: CONN, db: "shop".into(), coll: "orders".into() }).unwrap();
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::Unlock(CONN))]));

    conn(&mut core, ConnA::Unlock { conn: CONN }).unwrap();
    side(&mut core, SideA::MongoNewDb { conn: CONN }).unwrap();
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::MongoNewColl(f))] if f.new_db && f.conn == CONN));
    // Unlocked but without a session: not connected, and nothing is waiting to open.
    assert_eq!(side(&mut core, create()).unwrap_err().kind, RefusalKind::Offline);
    assert!(core.open_after.is_none());
    assert_eq!(side(&mut core, SideA::Duplicate { conn: CONN, db: "shop".into(), coll: "orders".into(), to: "orders_copy".into() }).unwrap_err().kind, RefusalKind::Offline);
    assert!(core.copying.is_empty());
    // A form the server would refuse is refused here with the reason.
    let bad = SideA::MongoCreate { conn: CONN, db: "shop".into(), name: "orders".into(), form: crate::db::collopts::CollForm { kind: crate::db::collopts::Kind::Capped, ..Default::default() }, ver: None };
    assert_eq!(side(&mut core, bad).unwrap_err().kind, RefusalKind::Invalid);
}

#[test]
fn groups_take_their_members_in_order_and_disconnect_together() {
    let mut core = Core::for_test();
    for id in 1..=3 {
        core.conns.push(Conn::new(ConnMeta { id, name: format!("c{id}"), ..Default::default() }, Status::Ready));
    }
    let group = |c: &mut Core, a: GroupA| c.execute(Action::Conn(ConnA::Group(a))).unwrap();
    group(&mut core, GroupA::New { name: "  ".into() });
    assert!(core.groups.groups.is_empty(), "a blank name makes no group");
    group(&mut core, GroupA::New { name: " Prod ".into() });
    let gid = core.groups.groups[0].id;
    assert_eq!(core.groups.groups[0].name, "Prod");
    group(&mut core, GroupA::Move { conn: 2, to: Place::Into(gid, None) });
    group(&mut core, GroupA::Save { group: gid, name: "Production".into(), color: "red".into(), fold_rail: true, members: vec![3, 2] });
    let g = core.groups.group(gid).unwrap();
    assert_eq!((g.name.as_str(), g.color.as_str(), g.fold_rail, g.members.clone()), ("Production", "red", true, vec![3, 2]));
    let open = core.groups.group(gid).unwrap().open;
    group(&mut core, GroupA::Toggle { group: gid });
    assert_eq!(core.groups.group(gid).unwrap().open, !open);
    group(&mut core, GroupA::Unfold { group: gid });
    assert!(!core.groups.group(gid).unwrap().fold_rail);
    assert!(core.status.starts_with("Production shows its connections one by one"));

    core.conns[2].writable = true;
    group(&mut core, GroupA::DisconnectAll { group: gid });
    assert_eq!(core.status, "Disconnected 2 connections in Production");
    assert!(core.conns[1].status == Status::Idle && core.conns[2].status == Status::Idle && !core.conns[2].writable);
    assert!(core.conns[0].status == Status::Ready, "a connection outside the group stays open");
    group(&mut core, GroupA::Move { conn: 2, to: Place::Loose(None) });
    assert_eq!(core.groups.group(gid).unwrap().members, vec![3]);
}

#[test]
fn a_rail_drop_pairs_two_tiles_and_its_undo_takes_it_back() {
    let mut core = Core::for_test();
    for id in 1..=3 {
        core.conns.push(Conn::new(ConnMeta { id, name: format!("c{id}"), ..Default::default() }, Status::Ready));
    }
    core.rail_order = vec![1, 2, 3];
    let group = |c: &mut Core, a: GroupA| c.execute(Action::Conn(ConnA::Group(a))).unwrap();
    group(&mut core, GroupA::Drop(RailDrop::Pair { conn: 3, target: 1 }));
    let gid = core.groups.groups[0].id;
    assert_eq!(core.groups.group(gid).unwrap().members, vec![1, 3]);
    assert_eq!(core.rail_order, [1, 3, 2]);
    assert_eq!(core.toasts.last().unwrap().title, "Grouped c1 and c3.");
    let (_, r) = core.restores.pop().unwrap();
    let crate::state::restore::Restore::Placed { moved, created, tile, prev, at } = r else { panic!("not a drop's undo") };
    core.restore_placed(moved, created, tile, prev, at);
    assert!(core.groups.groups.is_empty() && core.rail_order == [1, 2, 3]);
    assert_eq!(core.status, "Grouping undone.");

    group(&mut core, GroupA::New { name: "Prod".into() });
    let gid = core.groups.groups[0].id;
    group(&mut core, GroupA::Move { conn: 2, to: Place::Into(gid, None) });
    group(&mut core, GroupA::Delete { group: gid });
    assert!(core.groups.groups.is_empty());
    assert_eq!(core.toasts.last().unwrap().title, "Group Prod deleted. Its connection is in no group now.");
    let (_, r) = core.restores.pop().unwrap();
    let crate::state::restore::Restore::Group { g, at } = r else { panic!("not a group's undo") };
    core.restore_group(g, at);
    assert_eq!(core.groups.groups[0].members, vec![2]);
    group(&mut core, GroupA::RailOrder { order: vec![3, 1, 2] });
    assert_eq!(core.rail_order, [3, 1, 2]);
}

#[test]
fn save_adds_the_connection_to_its_group_and_connects() {
    let mut core = Core::for_test();
    core.execute(Action::Conn(ConnA::Group(GroupA::New { name: "Prod".into() }))).unwrap();
    let gid = core.groups.groups[0].id;
    let save = |name: &str| ConnA::Save { meta: ConnMeta { id: CONN, name: name.into(), ..Default::default() }, uri: Secret::new("mongodb://ada:Zq9secret@db1:27017".into()), secrets: vec![("ssh-password".into(), Secret::new(String::new()))], group: Some(gid) };
    conn(&mut core, save("shop-prod")).unwrap();
    let c = &core.conns[0];
    assert_eq!((c.meta.name.as_str(), core.active), ("shop-prod", Some(CONN)));
    assert!(!c.masked_uri.contains("Zq9secret"));
    assert_eq!(core.groups.group(gid).unwrap().members, vec![CONN]);
    // An ephemeral core connects nothing.
    assert_eq!(core.status, "Offline demo: no database is connected.");
    // Saving again edits it in place and locks it.
    core.conns[0].writable = true;
    conn(&mut core, save("shop")).unwrap();
    assert!(core.conns.len() == 1 && core.conns[0].meta.name == "shop" && !core.conns[0].writable);

    assert!(!format!("{:?}", Action::Conn(save("other"))).contains("Zq9secret"), "the address never shows in Debug");
    // Read back from JSON the address is gone, so nothing is saved.
    let back: Action = serde_json::from_str(&serde_json::to_string(&Action::Conn(save("other"))).unwrap()).unwrap();
    assert!(!serde_json::to_string(&back).unwrap().contains("Zq9secret"));
    assert_eq!(core.execute(back).unwrap_err().kind, RefusalKind::Invalid);
    assert_eq!(core.conns[0].meta.name, "shop");
}
