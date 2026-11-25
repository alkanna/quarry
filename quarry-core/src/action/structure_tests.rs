//! The Structure view's dialogs, triggers and maintenance run headlessly on a made-up structure.

use super::sql_tests::structure_core;
use super::*;
use crate::db::sql::ddl::{Change, ColDef, Pos, TableOpt};
use crate::db::sql::structure::{CheckConstraint, ForeignKey, Trigger};
use crate::db::sql::Maint;
use crate::state::View;

fn run(core: &mut Core, a: StructA) -> Result<Reply, Refusal> {
    core.execute(Action::Structure(a))
}

/// `structure_core` with a CHECK, a foreign key and a trigger as well.
fn full_core() -> (Core, u64) {
    let (mut core, uid) = structure_core();
    let d = core.tabs[0].sql.as_mut().unwrap().st.data.as_mut().unwrap();
    d.checks = vec![CheckConstraint { name: "chk_note".into(), expr: "note <> ''".into(), ..Default::default() }];
    d.foreign_keys = vec![ForeignKey { name: "fk_note".into(), cols: vec!["note".into()], ref_table: "notes".into(), ref_cols: vec!["id".into()], ..Default::default() }];
    d.triggers = vec![Trigger { name: "trg".into(), timing: "BEFORE".into(), event: "INSERT".into(), body: "SET NEW.note = 'x'".into(), ..Default::default() }];
    (core, uid)
}

#[test]
fn a_dialog_opens_on_its_column_and_stages_onto_the_list_as_it_is_now() {
    let (mut core, uid) = full_core();
    run(&mut core, StructA::EditColumn { tab: uid, col: Some("note".into()) }).unwrap();
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::ColEdit(f))] if f.from.as_deref() == Some("note")), "the column dialog did not open on note");
    run(&mut core, StructA::EditColumn { tab: uid, col: Some("gone".into()) }).unwrap();
    assert!(core.take_ui_fx().is_empty(), "an unknown column opens nothing");
    run(&mut core, StructA::DuplicateColumn { tab: uid, col: "note".into() }).unwrap();
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::ColEdit(f))] if f.from.is_none() && f.def.name != "note"), "a copy gets a free name");

    // Stage takes the dialog's own changes, and its edit of a column replaces the one staged before.
    let modify = |ty: &str| Change::Modify { from: "note".into(), def: ColDef::new("note", ty), pos: Pos::Keep };
    run(&mut core, StructA::Stage { tab: uid, changes: vec![modify("TEXT")], replaces: Some("note".into()) }).unwrap();
    run(&mut core, StructA::DropCheck { tab: uid, check: "chk_note".into() }).unwrap();
    run(&mut core, StructA::DropCheck { tab: uid, check: "chk_note".into() }).unwrap();
    run(&mut core, StructA::Stage { tab: uid, changes: vec![modify("VARCHAR")], replaces: Some("note".into()) }).unwrap();
    assert_eq!(core.staged(), [Change::DropCheck("chk_note".into()), modify("VARCHAR")]);

    // A check with a staged drop refuses its dialog.
    run(&mut core, StructA::EditCheck { tab: uid, check: Some("chk_note".into()) }).unwrap();
    assert!(core.take_ui_fx().is_empty() && core.toasts.last().is_some_and(|t| t.title.contains("already has a staged change")));

    run(&mut core, StructA::DropFk { tab: uid, fk: "fk_note".into() }).unwrap();
    assert!(core.staged().contains(&Change::DropFk("fk_note".into())));

    // The table options replace every staged option and keep the rest.
    let opt = |c: &str| Change::Table { opt: TableOpt::Comment(c.into()), was: TableOpt::Comment(String::new()) };
    run(&mut core, StructA::StageTable { tab: uid, changes: vec![opt("a")] }).unwrap();
    run(&mut core, StructA::StageTable { tab: uid, changes: vec![opt("b")] }).unwrap();
    assert_eq!(core.staged().iter().filter(|c| matches!(c, Change::Table { .. })).collect::<Vec<_>>(), [&opt("b")]);
    assert_eq!(core.staged().len(), 4);
    run(&mut core, StructA::EditTable { tab: uid }).unwrap();
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::TableOpts(f))] if f.comment == "b" && f.had_staged));
}

#[test]
fn a_trigger_saves_only_on_an_unlocked_connection() {
    let (mut core, uid) = full_core();
    run(&mut core, StructA::TrigNew { tab: uid }).unwrap();
    let s = |c: &Core| c.tabs[0].sql.as_ref().unwrap().st.trig_new;
    assert!(s(&core) && core.tabs[0].sql.as_ref().unwrap().st.trig.contains_key(""), "a new draft");
    let e = run(&mut core, StructA::TrigSave { tab: uid, key: String::new() }).unwrap_err();
    assert_eq!(e.kind, RefusalKind::ReadOnly { conn: 1 });
    let e = run(&mut core, StructA::TrigAskDrop { tab: uid, name: "trg".into() }).unwrap_err();
    assert_eq!(e.kind, RefusalKind::ReadOnly { conn: 1 });
    core.conns[0].writable = true;
    // No session to send it on.
    let e = run(&mut core, StructA::TrigSave { tab: uid, key: String::new() }).unwrap_err();
    assert_eq!(e.kind, RefusalKind::Offline);
    run(&mut core, StructA::TrigAskDrop { tab: uid, name: "trg".into() }).unwrap();
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::Typed { expect, .. })] if expect == "trg"));
    run(&mut core, StructA::TrigRevert { tab: uid, key: String::new() }).unwrap();
    assert!(!s(&core) && core.tabs[0].sql.as_ref().unwrap().st.trig.is_empty(), "Revert drops the draft");
    core.tabs[0].sql.as_mut().unwrap().st.applying = true;
    run(&mut core, StructA::TrigNew { tab: uid }).unwrap();
    let e = run(&mut core, StructA::TrigSave { tab: uid, key: String::new() }).unwrap_err();
    assert_eq!(e.msg, crate::state::sql::alter::BUSY_NOTE, "one structure change at a time");
}

#[test]
fn maintenance_asks_to_unlock_and_the_heavy_ones_ask_first() {
    let (mut core, uid) = full_core();
    let e = run(&mut core, StructA::Maintain { tab: uid, what: Maint::Analyze }).unwrap_err();
    assert_eq!(e.kind, RefusalKind::ReadOnly { conn: 1 });
    core.conns[0].writable = true;
    run(&mut core, StructA::Maintain { tab: uid, what: Maint::Optimize }).unwrap();
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::SqlMaintain { what: Maint::Optimize, stmt })] if stmt.starts_with("OPTIMIZE TABLE")));
    assert_ne!(core.tabs[0].view, View::Sql, "nothing ran before the yes");
    run(&mut core, StructA::RunMaint { tab: uid, what: Maint::Optimize }).unwrap();
    assert_eq!(core.tabs[0].view, View::Sql);
    assert!(core.tabs[0].sql.as_ref().unwrap().query.starts_with("OPTIMIZE TABLE"));
}

/// One of each RoutineA variant, for `all_actions`.
pub(super) fn routine_actions() -> Vec<RoutineA> {
    use crate::db::sql::routines::Kind;
    use crate::state::sql::routines::Pane;
    let (conn, db, schema) = (1, "shop".to_string(), String::new());
    vec![
        RoutineA::Load { conn, db: db.clone(), schema: schema.clone() },
        RoutineA::Reload { conn, db: db.clone(), schema: schema.clone() },
        RoutineA::Fold { conn, ns: db.clone() },
        RoutineA::Open { conn, db: db.clone(), schema: schema.clone(), routine: routine("f"), pane: Pane::Run },
        RoutineA::New { conn, db, schema, kind: Kind::Procedure },
        RoutineA::Pane(Pane::Drop),
        RoutineA::Save { restore: true },
        RoutineA::Drop,
        RoutineA::Run,
    ]
}

fn routine(name: &str) -> crate::db::sql::routines::Routine {
    crate::db::sql::routines::Routine { kind: crate::db::sql::routines::Kind::Function, name: name.into(), args: String::new(), returns: "int".into(), lang: "SQL".into() }
}

#[test]
fn routine_answers_land_only_where_they_were_asked_for() {
    use crate::db::sql::routines::{Answer, Detail};
    use crate::state::sql::routines::{Loaded, Pane};
    let (mut core, _) = full_core();
    let act = |core: &mut Core, a: RoutineA| core.execute(Action::Routines(a));
    let list = |core: &Core| match core.routines.lists.get(&(1, "shop".to_string())) {
        Some(Loaded::Ok(l)) => Some(l.len()),
        _ => None,
    };
    // No session: the list is empty at once, and a list nobody waits for is not put back.
    act(&mut core, RoutineA::Load { conn: 1, db: "shop".into(), schema: String::new() }).unwrap();
    assert_eq!(list(&core), Some(0));
    let answer = || Msg::SqlRoutine { conn: 1, req: 0, ans: Answer::List { database: "shop".into(), schema: String::new(), res: Ok(vec![routine("f")]) } };
    core.inject(answer());
    assert_eq!(list(&core), Some(0), "a stale list replaced the one shown");
    core.routines.lists.insert((1, "shop".into()), Loaded::Wait);
    core.inject(answer());
    assert_eq!(list(&core), Some(1));

    act(&mut core, RoutineA::Open { conn: 1, db: "shop".into(), schema: String::new(), routine: routine("f"), pane: Pane::View }).unwrap();
    core.routines.win.as_mut().unwrap().req = 5;
    let detail = |s: &str| Answer::Detail { res: Ok(Box::new(Detail { source: s.into(), ..Default::default() })) };
    core.inject(Msg::SqlRoutine { conn: 1, req: 4, ans: detail("old") });
    assert!(core.routines.win.as_ref().unwrap().detail.is_none(), "an older read filled the window");
    core.inject(Msg::SqlRoutine { conn: 1, req: 5, ans: detail("CREATE FUNCTION f()") });
    let w = core.routines.win.as_ref().unwrap();
    assert!(w.text == "CREATE FUNCTION f()" && w.req == 0);
    act(&mut core, RoutineA::Pane(Pane::Drop)).unwrap();
    assert!(core.routines.win.as_ref().unwrap().pane == Pane::Drop);

    // Save and Drop write, so a locked connection asks to unlock first.
    let e = act(&mut core, RoutineA::Save { restore: false }).unwrap_err();
    assert_eq!(e.kind, RefusalKind::ReadOnly { conn: 1 });
    let e = act(&mut core, RoutineA::Drop).unwrap_err();
    assert_eq!(e.kind, RefusalKind::ReadOnly { conn: 1 });
    assert!(!core.routines.win.as_ref().unwrap().busy);
}
