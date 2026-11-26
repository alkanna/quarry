//! Tabs, the Documents view, drafts, delete and undo run headlessly, with made-up answers.

use super::*;
use crate::db::{self, UndoAction, UndoKind};
use crate::state::{Conn, ConnMeta, Draft, Open, Sel, Status, View};
use mongodb::bson::{doc, Bson, Document};

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

fn docs(core: &mut Core, a: DocsA) -> Result<Reply, Refusal> {
    core.execute(Action::Docs(a))
}

fn rows(ds: &[Document]) -> Vec<db::Row> {
    ds.iter().map(|d| db::Row::from_doc(d).unwrap()).collect()
}

/// What the app's autocomplete would build; the tests only look at the rows.
fn no_info(_: &[db::Row], _: Option<&std::collections::HashMap<String, Vec<Bson>>>) -> (Vec<String>, crate::state::complete::Fields) {
    (Vec::new(), Default::default())
}

#[test]
fn tabs_open_replace_close_reopen_and_go_back() {
    let mut core = offline();
    assert_eq!(core.tabs.len(), 1);
    let first = uid(&core);
    // A plain click takes the current tab when nothing would be lost there; the old one can come back.
    core.execute(Action::Tab(TabA::Open { sel: sel("items"), how: Open::Here })).unwrap();
    assert_eq!((core.tabs.len(), core.t().unwrap().sel.coll.as_str(), core.closed.len()), (1, "items", 1));
    // A filter is work, so the next click opens a tab beside it.
    core.tabs[0].filter_txt = "{ a: 1 }".into();
    core.execute(Action::Tab(TabA::Open { sel: sel("users"), how: Open::Here })).unwrap();
    assert_eq!((core.tabs.len(), core.cur), (2, 1));
    let users = uid(&core);
    core.execute(Action::Tab(TabA::Nav { back: true })).unwrap();
    assert_eq!(core.t().unwrap().sel.coll, "items");
    core.execute(Action::Tab(TabA::Close { tab: users })).unwrap();
    assert_eq!(core.tabs.len(), 1);
    core.execute(Action::Tab(TabA::Reopen)).unwrap();
    assert_eq!((core.tabs.len(), core.t().unwrap().sel.coll.as_str()), (2, "users"));
    // A tab that closed in between is refused, not taken for another one.
    let r = core.execute(Action::Tab(TabA::Pick { tab: first })).unwrap_err();
    assert_eq!((r.kind, r.msg.as_str()), (RefusalKind::Invalid, super::tabs::TAB_GONE));
}

#[test]
fn tabs_move_close_many_and_switch_view() {
    let mut core = offline();
    for c in ["b", "c"] {
        core.execute(Action::Tab(TabA::Open { sel: sel(c), how: Open::NewTab })).unwrap();
    }
    let (a, c) = (core.tabs[0].uid, core.tabs[2].uid);
    core.execute(Action::Tab(TabA::Move { tab: c, to: 0 })).unwrap();
    assert_eq!((core.tabs[0].uid, core.cur), (c, 0), "the current tab stays current where it moved");
    core.execute(Action::Tab(TabA::View { tab: c, view: View::Indexes })).unwrap();
    assert_eq!(core.t().unwrap().view, View::Indexes);
    assert_eq!(core.execute(Action::Tab(TabA::View { tab: a, view: View::Schema })).unwrap_err().kind, RefusalKind::Invalid, "only the current tab");
    core.execute(Action::Tab(TabA::CloseMany { tab: a, what: crate::state::TabClose::Others })).unwrap();
    assert_eq!((core.tabs.len(), core.tabs[0].uid, core.closed.len()), (1, a, 2));
}

#[test]
fn a_page_answer_for_an_older_read_is_dropped() {
    let mut core = offline();
    core.tabs[0].req = 5;
    core.docs_loaded(4, Ok((rows(&[doc! { "_id": 1 }]), 1, 2)), false, &no_info);
    assert!(core.tabs[0].docs.is_empty(), "a slower older answer never overwrites a newer one");
    core.docs_loaded(5, Ok((rows(&[doc! { "_id": 1 }, doc! { "_id": 2 }]), 2, 2)), false, &no_info);
    assert_eq!((core.tabs[0].docs.len(), core.tabs[0].count), (2, 2));
    // Checks follow their documents by _id across a reload.
    let tab = uid(&core);
    docs(&mut core, DocsA::Click { tab, agg: false, row: 1, cmd: true, shift: false }).unwrap();
    core.docs_loaded(5, Ok((rows(&[doc! { "_id": 2 }]), 1, 2)), false, &no_info);
    assert_eq!(core.tabs[0].checked.iter().copied().collect::<Vec<_>>(), [0]);
    // A failed read clears the rows and shows its panel.
    core.docs_loaded(5, Err("boom".into()), false, &no_info);
    assert!(core.tabs[0].docs.is_empty() && core.tabs[0].failed.iter().any(|f| f.view == View::Documents));
}

#[test]
fn a_field_edit_is_staged_and_saving_asks_to_unlock_without_a_toast() {
    let mut core = offline();
    let tab = uid(&core);
    let d = doc! { "_id": 1, "a": 1 };
    // Locked: the inline editor does not open, the app opens Unlock and runs it again after.
    let r = docs(&mut core, DocsA::Edit { tab, doc: d.clone(), act: EditA::Start { path: vec!["a".into()], value: Bson::Int32(1) } }).unwrap_err();
    assert_eq!(r.kind, RefusalKind::ReadOnly { conn: CONN });
    assert!(core.toasts.is_empty() && core.inline.is_none());
    core.conns[0].writable = true;
    docs(&mut core, DocsA::Edit { tab, doc: d.clone(), act: EditA::Start { path: vec!["a".into()], value: Bson::Int32(1) } }).unwrap();
    core.inline.as_mut().unwrap().text = "2".into();
    docs(&mut core, DocsA::Edit { tab, doc: d.clone(), act: EditA::Commit }).unwrap();
    let key = core.draft_key(&d);
    assert_eq!(core.drafts[&key].current, doc! { "_id": 1, "a": 2 });
    assert!(core.inline.is_none());
    // Saving on a connection locked again asks to unlock, quietly.
    core.conns[0].writable = false;
    let r = docs(&mut core, DocsA::Save { key: key.clone() }).unwrap_err();
    assert_eq!(r.kind, RefusalKind::ReadOnly { conn: CONN });
    assert!(core.toasts.is_empty());
    // Unlocked with Ask before saving on: the confirmation opens with the change.
    core.conns[0].writable = true;
    core.prefs.confirm_save = true;
    docs(&mut core, DocsA::Save { key: key.clone() }).unwrap();
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::Save { tag, changes })] if *tag == key && changes.len() == 1));
    // Offline, writing keeps the draft and says so.
    docs(&mut core, DocsA::Write { keys: vec![key.clone()] }).unwrap();
    assert!(core.drafts.contains_key(&key) && core.saving.is_empty());
    assert_eq!(core.toasts.last().unwrap().title, "Not connected");
}

#[test]
fn a_failed_save_keeps_the_edits_and_a_saved_one_can_be_undone() {
    let mut core = offline();
    let d = doc! { "_id": 1, "a": 1 };
    let key = core.draft_key(&d);
    let draft = Draft { conn: CONN, db: "shop".into(), schema: String::new(), coll: "orders".into(), original: d.clone(), current: doc! { "_id": 1, "a": 2 } };
    core.saving.insert(key.clone(), draft.clone());
    core.inject(Msg::Saved { tag: key.clone(), res: Err("changed on the server".into()), undo: None });
    assert_eq!(core.drafts[&key].current, draft.current, "nothing typed is lost");
    core.drafts.clear();
    core.saving.insert(key.clone(), draft);
    let undo = UndoAction { agent: None, at: std::time::Instant::now(), conn: CONN, db: "shop".into(), coll: "orders".into(), kind: UndoKind::Update { old: d.clone(), new: doc! { "_id": 1, "a": 2 } } };
    core.inject(Msg::Saved { tag: key, res: Ok("Saved".into()), undo: Some(undo) });
    assert_eq!((core.undo.len(), core.status.as_str()), (1, "Saved"));
    assert!(core.toasts.last().unwrap().acts.iter().any(|(l, _)| l == "Undo"));
    // Undo is a write: refused while locked, with the wording it always had.
    let at = core.undo_id(core.undo[0].at);
    let r = core.execute(Action::Undo(UndoA::Apply { at, confirmed: false })).unwrap_err();
    assert_eq!((r.kind, r.msg.as_str()), (RefusalKind::ReadOnly { conn: CONN }, super::undo::NOTE_UNDO));
    // A write that lands meanwhile moves the list; the retry still finds its own entry.
    core.push_undo(UndoAction { agent: None, at: std::time::Instant::now() + std::time::Duration::from_secs(1), conn: CONN, db: "shop".into(), coll: "orders".into(), kind: UndoKind::Insert { id: Bson::Int32(2) } });
    core.conns[0].writable = true;
    assert_eq!(core.execute(Action::Undo(UndoA::Apply { at, confirmed: false })).unwrap_err().msg, "That connection is closed.");
    assert_eq!(core.undo.len(), 2, "a refused undo stays in the list");
    assert_eq!(core.execute(Action::Undo(UndoA::Apply { at: at + 1, confirmed: false })).unwrap_err().msg, "That change is no longer in Recent changes.");
}

#[test]
fn insert_delete_and_duplicate_ask_to_unlock_and_a_refused_insert_reopens() {
    let mut core = offline();
    let tab = uid(&core);
    assert_eq!(docs(&mut core, DocsA::Insert { tab }).unwrap_err().kind, RefusalKind::ReadOnly { conn: CONN });
    assert!(core.insert.is_none() && core.toasts.is_empty());
    let two = vec![doc! { "_id": 1 }, doc! { "_id": 2 }];
    assert_eq!(docs(&mut core, DocsA::Delete { tab, docs: two.clone() }).unwrap_err().kind, RefusalKind::ReadOnly { conn: CONN });
    core.conns[0].writable = true;
    docs(&mut core, DocsA::Delete { tab, docs: two.clone() }).unwrap();
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::Delete(s, d))] if *s == sel("orders") && d.len() == 2));
    // One copy opens the insert editor without its _id.
    docs(&mut core, DocsA::Duplicate { tab, docs: vec![doc! { "_id": 1, "a": 1 }] }).unwrap();
    assert!(core.insert.as_deref().is_some_and(|t| t.contains("\"a\"") && !t.contains("_id")));
    docs(&mut core, DocsA::InsertClose).unwrap();
    // The confirmation of a tab that closed meanwhile runs nothing.
    let r = docs(&mut core, DocsA::DeleteRun { at: sel("gone"), docs: two }).unwrap_err();
    assert_eq!(r.msg, "Nothing ran: gone is no longer open in a tab.");
    // A refused insert over this collection comes back to the editor.
    core.inject(Msg::Written { at: ("shop".into(), "orders".into()), res: Err("duplicate key".into()), undo: None, reload: false, retry: Some(("shop".into(), "orders".into(), "{ a: 1 }".into())) });
    assert_eq!(core.insert.as_deref(), Some("{ a: 1 }"));
}

#[test]
fn a_projection_refuses_edits_and_the_query_bar_runs() {
    let mut core = offline();
    let tab = uid(&core);
    docs(&mut core, DocsA::History { tab, field: BarField::Project, text: "{ a: 1 }".into() }).unwrap();
    docs(&mut core, DocsA::Run { tab }).unwrap();
    let t = core.t().unwrap();
    assert_eq!(t.query.projection, Some(doc! { "a": 1 }));
    let key = core.col_key(false);
    assert_eq!(core.prefs.history[&key].project.first().map(String::as_str), Some("{ a: 1 }"));
    core.conns[0].writable = true;
    assert_eq!(docs(&mut core, DocsA::Duplicate { tab, docs: vec![doc! { "_id": 1 }] }).unwrap_err().kind, RefusalKind::Invalid);
    assert!(core.toasts.last().unwrap().title.starts_with("These rows hold only the projected fields"));
    // A filter that does not parse stays under the bar.
    docs(&mut core, DocsA::History { tab, field: BarField::Filter, text: "{ a: ".into() }).unwrap();
    docs(&mut core, DocsA::Run { tab }).unwrap();
    assert!(core.t().unwrap().bar_err.as_deref().is_some_and(|e| e.starts_with("Filter")));
}

#[test]
fn every_tree_edit_survives_a_serde_round_trip() {
    let all = [
        EditA::Start { path: vec!["a".into()], value: Bson::Int32(1) },
        EditA::Commit,
        EditA::Expand,
        EditA::Cancel,
        EditA::Remove { path: vec!["a".into()] },
        EditA::AddField { parent: Vec::new() },
        EditA::CommitNew,
        EditA::EditDoc,
    ];
    for act in all {
        let a = Action::Docs(DocsA::Edit { tab: 1, doc: doc! { "_id": 1 }, act });
        let json = serde_json::to_value(&a).unwrap();
        let back: Action = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), json);
    }
}

#[test]
fn pipeline_stages_follow_their_cards_and_an_older_run_is_dropped() {
    let mut core = offline();
    let tab = uid(&core);
    core.sync_stages();
    let n = core.t().unwrap().stages.len();
    core.execute(Action::Aggregate(AggA::Stages { tab, acts: vec![StageA::Add("$sort".into())] })).unwrap();
    let t = core.t().unwrap();
    assert_eq!(t.stages.len(), n + 1);
    assert!(t.agg_txt.contains("$sort"), "the text follows the stages: {}", t.agg_txt);
    // A pipeline that does not parse says why and runs nothing.
    core.tabs[0].agg_txt = "[ { $match: ".into();
    core.execute(Action::Aggregate(AggA::Run { tab })).unwrap();
    assert_eq!(core.tabs[0].agg_req, 0);
    assert!(!core.toasts.is_empty());
    // Only the run the tab waits for fills it; an older one goes to the error list quietly.
    core.tabs[0].agg_req = 9;
    let errors = core.errors.len();
    core.inject(Msg::Aggregated { req: 8, res: Err("old".into()) });
    assert_eq!((core.tabs[0].agg_req, core.errors.len()), (9, errors + 1));
    core.inject(Msg::Aggregated { req: 9, res: Ok((rows(&[doc! { "_id": 1, "n": 2 }]), 4)) });
    let t = &core.tabs[0];
    assert_eq!((t.agg_req, t.agg_docs.len(), t.agg_ms), (0, 1, Some(4)));
    assert_eq!(core.status, "Pipeline returned 1 documents");
}

#[test]
fn indexes_answer_only_their_read_and_a_locked_create_keeps_the_dialog() {
    let mut core = offline();
    let tab = uid(&core);
    core.tabs[0].idx_req = 3;
    core.inject(Msg::Indexes { req: 2, res: Ok(vec![doc! { "name": "_id_", "key": { "_id": 1 } }]) });
    assert!(core.tabs[0].indexes.is_empty(), "an older list is dropped");
    core.inject(Msg::Indexes { req: 3, res: Ok(vec![doc! { "name": "_id_", "key": { "_id": 1 } }]) });
    assert_eq!(core.tabs[0].idx_rows.len(), 1);
    // Locked: the read-only toast as before; the New index dialog calls Core itself, so it stays open with no Unlock.
    let r = core.execute(Action::Indexes(IdxA::Create { tab, spec: doc! { "key": { "a": 1 }, "name": "a_1" } })).unwrap_err();
    assert_eq!(r.kind, RefusalKind::ReadOnly { conn: CONN });
    assert_eq!(core.toasts.last().unwrap().title, "shop-prod is read-only");
}
