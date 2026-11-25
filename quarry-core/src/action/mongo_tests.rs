//! Compare, Schema, Import, the field menu and the grid's columns run headlessly, with made-up answers.

use super::*;
use crate::db::{self, ColOp, ColOut};
use crate::state::{CmpList, ColAct, Conn, ConnMeta, Danger, Open, Sel, Status, View};
use mongodb::bson::{doc, Bson};

const CONN: u64 = 1;

fn sel(coll: &str) -> Sel {
    Sel { conn: CONN, db: "shop".into(), coll: coll.into(), ..Default::default() }
}

/// A Mongo connection without a session, writes locked, and a tab on `orders`.
fn offline() -> Core {
    let mut core = Core::for_test();
    core.conns.push(Conn::new(ConnMeta { id: CONN, name: "shop-prod".into(), ..Default::default() }, Status::Ready));
    core.execute(Action::Tab(TabA::Open { sel: sel("orders"), how: Open::Here })).unwrap();
    core
}

fn uid(core: &Core) -> u64 {
    core.t().unwrap().uid
}

fn mongo(core: &mut Core, a: MongoA) -> Result<Reply, Refusal> {
    core.execute(Action::Mongo(a))
}

/// Every Mongo and grid variant, for `all_actions`.
pub(super) fn all_mongo() -> Vec<Action> {
    let c = [
        CmpA::Conn { tab: 1, conn: 2 },
        CmpA::Db { tab: 1, db: "shop".into() },
        CmpA::Coll { tab: 1, coll: "orders".into() },
        CmpA::List { tab: 1, list: CmpList::OnlyDst },
        CmpA::Page { tab: 1, page: 2 },
        CmpA::Opts { tab: 1, insert: true, replace: false, delete: true },
        CmpA::Run { tab: 1 },
        CmpA::Sync { tab: 1 },
    ];
    let s = [
        SchemaA::Analyze { tab: 1 },
        SchemaA::Limit { tab: 1, limit: 1000, last: true },
        SchemaA::Rules { tab: 1, level: 1, warn: true },
        SchemaA::Generate { tab: 1 },
        SchemaA::Reload { tab: 1 },
        SchemaA::Apply { tab: 1 },
        SchemaA::Fold { tab: 1, path: "a".into() },
        SchemaA::FoldAll { tab: 1, closed: true },
        SchemaA::AddColumn { tab: 1, path: "a".into() },
        SchemaA::FilterHas { tab: 1, path: "a".into(), exists: false },
        SchemaA::Map { tab: 1, path: "a".into() },
    ];
    let i = [
        ImportA::Opts { tab: 1, kind: db::ImportKind::Csv, separator: Some(';'), header: true, camel: false, upsert: true },
        ImportA::File { tab: 1, path: "a.csv".into() },
        ImportA::Start { tab: 1 },
        ImportA::Insert { tab: 1 },
        ImportA::Export { tab: 1, path: "a.json".into() },
    ];
    let f = [
        FieldA::PinColumn { tab: 1, path: "a".into() },
        FieldA::Highlight { tab: 1, path: "a".into() },
        FieldA::Group { tab: 1, path: "a".into(), value: Bson::Int32(1), filtered: true },
        FieldA::Distinct { tab: 1, path: "a".into() },
        FieldA::Index { tab: 1, path: "a.0.b".into() },
        FieldA::GoRef { tab: 1, path: "userId".into(), value: Bson::String("u1".into()) },
        FieldA::Rename { tab: 1, doc: doc! { "_id": 1, "a": 2 }, path: vec!["a".into()] },
    ];
    let g = [
        GridA::Cols { key: "k".into(), agg: false, acts: vec![ColAct::Pin("a".into()), ColAct::MoveTo("b".into(), 0), ColAct::Customize { col: "a".into(), label: "A".into(), path: "a.b".into() }, ColAct::Reset] },
        GridA::Widths { key: "k".into(), set: vec![("a".into(), Some(120.0)), ("b".into(), None)], save: true },
        GridA::Sort { tab: 1, text: "{ a: -1 }".into(), sort: Some(doc! { "a": -1 }) },
        GridA::ScopeQuery(true),
        GridA::Values { tab: 1, col: "a".into(), op: ColOp::CountDistinct },
    ];
    let mut all: Vec<Action> = c.into_iter().map(|a| Action::Mongo(MongoA::Compare(a))).collect();
    all.extend(s.into_iter().map(|a| Action::Mongo(MongoA::Schema(a))));
    all.extend(i.into_iter().map(|a| Action::Mongo(MongoA::Import(a))));
    all.extend(f.into_iter().map(|a| Action::Mongo(MongoA::Field(a))));
    all.extend(g.into_iter().map(Action::Grid));
    all
}

/// No wildcard, so a variant missing from `all_mongo` fails to compile here first.
pub(super) fn mongo_variant(a: &MongoA) {
    match a {
        MongoA::Compare(c) => match c {
            CmpA::Conn { .. } | CmpA::Db { .. } | CmpA::Coll { .. } | CmpA::List { .. } | CmpA::Page { .. } | CmpA::Opts { .. } | CmpA::Run { .. } | CmpA::Sync { .. } => {}
        },
        MongoA::Schema(s) => match s {
            SchemaA::Analyze { .. } | SchemaA::Limit { .. } | SchemaA::Rules { .. } | SchemaA::Generate { .. } | SchemaA::Reload { .. } | SchemaA::Apply { .. } | SchemaA::Fold { .. } | SchemaA::FoldAll { .. } | SchemaA::AddColumn { .. } | SchemaA::FilterHas { .. } | SchemaA::Map { .. } => {}
        },
        MongoA::Import(i) => match i {
            ImportA::Opts { .. } | ImportA::File { .. } | ImportA::Start { .. } | ImportA::Insert { .. } | ImportA::Export { .. } => {}
        },
        MongoA::Field(f) => match f {
            FieldA::PinColumn { .. } | FieldA::Highlight { .. } | FieldA::Group { .. } | FieldA::Distinct { .. } | FieldA::Index { .. } | FieldA::GoRef { .. } | FieldA::Rename { .. } => {}
        },
    }
}

pub(super) fn grid_variant(a: &GridA) {
    match a {
        GridA::Cols { .. } | GridA::Widths { .. } | GridA::Sort { .. } | GridA::ScopeQuery(_) | GridA::Values { .. } => {}
    }
}

#[test]
fn a_comparison_answer_for_an_older_run_is_dropped() {
    let mut core = offline();
    let t = &mut core.tabs[0];
    t.view = View::Compare;
    (t.compare.req, t.compare.running) = (7, true);
    core.inject(Msg::Compared { req: 6, res: Ok(Default::default()) });
    assert!(core.tabs[0].compare.running && core.tabs[0].compare.result.is_none(), "an older answer changes nothing");
    let r = db::Compared { only_src: vec![doc! { "_id": 1 }], ..Default::default() };
    core.inject(Msg::Compared { req: 7, res: Ok(r) });
    let c = &core.tabs[0].compare;
    assert!(!c.running && c.result.is_some());
    assert!(c.list == CmpList::OnlySrc, "opens on the first list with rows");
}

#[test]
fn compare_picks_drop_a_diff_for_another_target_and_sync_asks_to_unlock_it() {
    let mut core = offline();
    let tab = uid(&core);
    // Without a session on either side, Compare says why.
    mongo(&mut core, MongoA::Compare(CmpA::Conn { tab, conn: CONN })).unwrap();
    mongo(&mut core, MongoA::Compare(CmpA::Coll { tab, coll: "orders_copy".into() })).unwrap();
    let r = mongo(&mut core, MongoA::Compare(CmpA::Run { tab })).unwrap_err();
    assert_eq!((r.kind, r.msg.as_str()), (RefusalKind::Invalid, "Connect both sides first."));
    // A result for the picked target, as if it had run.
    let c = &mut core.tabs[0].compare;
    c.ran = (c.conn, c.db.clone(), c.coll.clone());
    c.set_result(Some(db::Compared { only_dst: vec![doc! { "_id": 1 }], ..Default::default() }));
    // Writes are locked on the target: Unlock opens with nothing to run after it.
    assert!(matches!(mongo(&mut core, MongoA::Compare(CmpA::Sync { tab })), Ok(Reply::Done)));
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::Unlock(CONN))]));
    core.conns[0].writable = true;
    mongo(&mut core, MongoA::Compare(CmpA::Sync { tab })).unwrap();
    let fx = core.take_ui_fx();
    let [UiFx::Open(Dialog::Typed { expect, action: Danger::At(at, d), .. })] = fx.as_slice() else { panic!("the typed confirmation") };
    assert_eq!((expect.as_str(), at.coll.as_str()), ("orders_copy", "orders"));
    assert!(matches!(**d, Danger::Sync { conn: CONN }));
    // Another collection picked: the diff was for the old one, so it goes.
    mongo(&mut core, MongoA::Compare(CmpA::Coll { tab, coll: "items".into() })).unwrap();
    assert!(core.tabs[0].compare.result.is_none());
    assert_eq!(mongo(&mut core, MongoA::Compare(CmpA::Sync { tab })).unwrap_err().kind, RefusalKind::Invalid, "nothing to sync");
}

#[test]
fn a_schema_answer_for_an_older_analysis_is_dropped_and_map_fields_are_saved() {
    let mut core = offline();
    let tab = uid(&core);
    let t = &mut core.tabs[0];
    (t.schema.req, t.schema.running) = (3, true);
    let stat = |path: &str, depth| db::FieldStat { path: path.into(), depth, docs: 2, types: vec![("object", 2)], examples: Vec::new(), dynamic: false };
    core.inject(Msg::Schema { req: 2, res: Ok((vec![stat("a", 0)], 2, 5)) });
    assert!(core.tabs[0].schema.stats.is_empty() && core.tabs[0].schema.running);
    core.inject(Msg::Schema { req: 3, res: Ok((vec![stat("a", 0), stat("a.b", 1)], 2, 5)) });
    let s = &core.tabs[0].schema;
    assert_eq!((s.stats.len(), s.ms, s.running), (2, Some(5), false));
    mongo(&mut core, MongoA::Schema(SchemaA::FoldAll { tab, closed: true })).unwrap();
    assert!(core.tabs[0].schema.closed.contains("a"));
    mongo(&mut core, MongoA::Schema(SchemaA::Fold { tab, path: "a".into() })).unwrap();
    assert!(core.tabs[0].schema.closed.is_empty());
    let key = core.tabs[0].coll_key();
    mongo(&mut core, MongoA::Schema(SchemaA::Map { tab, path: "a".into() })).unwrap();
    assert_eq!(core.prefs.schema_maps.get(&key).map(Vec::as_slice), Some(&["a".to_string()][..]));
    mongo(&mut core, MongoA::Schema(SchemaA::Map { tab, path: "a".into() })).unwrap();
    assert!(!core.prefs.schema_maps.contains_key(&key), "an empty list is not kept");
    // Apply asks for the collection's name, tied to this tab.
    core.tabs[0].schema.vtext = "{ $jsonSchema: { required: ['a'] } }".into();
    mongo(&mut core, MongoA::Schema(SchemaA::Apply { tab })).unwrap();
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::Typed { action: Danger::At(..), .. })]));
}

#[test]
fn import_parses_the_input_and_asks_before_writing() {
    let mut core = offline();
    let tab = uid(&core);
    core.tabs[0].import.text = "[{ \"a\": 1 }, { \"a\": 2 }]".into();
    // Locked writes: Unlock, with nothing to run after it, as the view always asked.
    mongo(&mut core, MongoA::Import(ImportA::Start { tab })).unwrap();
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::Unlock(CONN))]));
    core.conns[0].writable = true;
    mongo(&mut core, MongoA::Import(ImportA::Start { tab })).unwrap();
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::ImportInsert(2))]));
    assert_eq!(core.tabs[0].import.fields, ["_id", "a"]);
    let kind = core.tabs[0].import.kind;
    mongo(&mut core, MongoA::Import(ImportA::Opts { tab, kind, separator: None, header: true, camel: false, upsert: true })).unwrap();
    mongo(&mut core, MongoA::Import(ImportA::Start { tab })).unwrap();
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::Typed { action: Danger::At(_, d), .. })] if matches!(**d, Danger::Upsert { ref key } if key == "_id")));
    // CSV reads the same text as nothing to import.
    mongo(&mut core, MongoA::Import(ImportA::Opts { tab, kind: db::ImportKind::Csv, separator: None, header: true, camel: false, upsert: false })).unwrap();
    core.tabs[0].import.text.clear();
    assert_eq!(mongo(&mut core, MongoA::Import(ImportA::Start { tab })).unwrap_err().kind, RefusalKind::Invalid);
}

#[test]
fn rename_is_refused_while_writes_are_locked() {
    let mut core = offline();
    let tab = uid(&core);
    let a = MongoA::Field(FieldA::Rename { tab, doc: doc! { "_id": 1, "name": "x" }, path: vec!["name".into()] });
    let r = mongo(&mut core, a.clone()).unwrap_err();
    assert_eq!(r.kind, RefusalKind::ReadOnly { conn: CONN });
    assert!(core.take_ui_fx().is_empty());
    core.conns[0].writable = true;
    mongo(&mut core, a).unwrap();
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::Rename { name, .. })] if name == "name"));
}

#[test]
fn field_menu_columns_and_index() {
    let mut core = offline();
    let tab = uid(&core);
    core.tabs[0].fields = vec!["_id".into(), "a".into()];
    let key = core.col_key(false);
    mongo(&mut core, MongoA::Field(FieldA::PinColumn { tab, path: "b.c".into() })).unwrap();
    let p = &core.prefs.cols[&key];
    assert_eq!((p.pinned.as_slice(), p.shown.as_ref().unwrap().last().unwrap().as_str(), core.prefs.table), (&["b.c".to_string()][..], "b.c", true));
    mongo(&mut core, MongoA::Field(FieldA::Highlight { tab, path: "items.0.sku".into() })).unwrap();
    assert_eq!(core.prefs.cols[&key].highlight, ["items.sku"]);
    mongo(&mut core, MongoA::Field(FieldA::Index { tab, path: "items.0.sku".into() })).unwrap();
    assert_eq!(core.t().unwrap().view, View::Indexes);
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::CreateIndex(f))] if f.keys.contains("items.sku")));
}

#[test]
fn answers_of_the_field_menu() {
    let mut core = offline();
    core.inject(Msg::Grouped { conn: CONN, db: "shop".into(), coll: "orders".into(), path: "status".into(), pipeline: Vec::new(), res: Ok(vec![doc! { "_id": "paid", "count": 3 }]) });
    assert_eq!(core.status, "1 distinct values of status");
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Late(Dialog::Grouped { path, .. })] if path == "status"));
    core.inject(Msg::ColumnValues { conn: CONN, op: ColOp::Copy, coll: "orders".into(), path: "a".into(), res: Ok((ColOut::Copy("1\n2".into(), 2), false)) });
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::CopyText(t)] if t == "1\n2"));
    core.inject(Msg::ColumnValues { conn: CONN, op: ColOp::Sum, coll: "orders".into(), path: "a".into(), res: Ok((ColOut::Notice("3".into()), true)) });
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::Text { body, .. })] if body.contains("From the first")));
    // A found reference becomes a relation of the database and opens the document.
    core.inject(Msg::RefFound { conn: CONN, db: "shop".into(), coll: "orders".into(), path: "userId".into(), value: Bson::String("u1".into()), res: Ok(Some(("users".into(), "name".into()))) });
    assert_eq!(core.prefs.relations["1/shop"][0].target, "users");
    let t = core.t().unwrap();
    assert!(t.sel.coll == "users" && t.filter_txt.contains("u1"), "{}", t.filter_txt);
    core.take_ui_fx();
    core.inject(Msg::Analyzed { conn: CONN, db: "shop".into(), res: Ok(vec![("orders".into(), "userId".into(), "users".into(), "name".into()), ("orders".into(), "itemId".into(), "items".into(), String::new())]) });
    assert_eq!(core.prefs.relations["1/shop"].len(), 2, "a relation already saved is not added again");
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::IfFree(Dialog::Relations { conn: CONN, .. })]));
}

#[test]
fn grid_columns_widths_and_page_size() {
    let mut core = offline();
    core.tabs[0].fields = (0..12).map(|i| format!("f{i}")).collect();
    let key = core.col_key(false);
    core.execute(Action::Grid(GridA::Cols { key: key.clone(), agg: false, acts: vec![ColAct::Hide("f0".into()), ColAct::Pin("f3".into())] })).unwrap();
    let shown = super::grid::visible_cols(core.prefs.cols.get(&key), &core.tabs[0].fields, core.default_cols());
    assert_eq!((shown[0].as_str(), shown.contains(&"f0".to_string())), ("f3", false));
    core.execute(Action::Grid(GridA::Widths { key: key.clone(), set: vec![("f1".into(), Some(140.0))], save: false })).unwrap();
    core.execute(Action::Grid(GridA::Widths { key: key.clone(), set: vec![("f2".into(), Some(90.0)), ("f1".into(), None)], save: true })).unwrap();
    assert_eq!(core.prefs.cols[&key].widths.len(), 1);
    // A page read with another page size goes back to the first page.
    let size = core.page_size();
    let t = &mut core.tabs[0];
    (t.size, t.page, t.loading) = (size + 1, 3, false);
    core.follow_page_size();
    assert_eq!(core.tabs[0].page, 0);
}
