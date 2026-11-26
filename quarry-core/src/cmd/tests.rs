//! The registry pins what the menu bar, the palette and the keys did before it: titles, order, chords and guards.

use super::*;
use crate::state::{Conn, ConnMeta, Open, Sel};

/// Every command, through a match with no wildcard, so a new one cannot miss this list.
fn all_cmds() -> Vec<Cmd> {
    let all = [
        Cmd::Reload, Cmd::Run, Cmd::RunAll, Cmd::Explain, Cmd::NewQuery, Cmd::CloseTab, Cmd::NextTab, Cmd::PrevTab, Cmd::Back, Cmd::Forward, Cmd::FindQuery, Cmd::FindColl,
        Cmd::Connect, Cmd::LockWrites, Cmd::NewDb, Cmd::Server, Cmd::Users, Cmd::Disconnect, Cmd::EditConn, Cmd::AllowWrites, Cmd::SaveToLibrary, Cmd::SaveAs, Cmd::OpenPicker,
        Cmd::ShowLibrary, Cmd::LinkFolder, Cmd::NewGroup, Cmd::NewConn, Cmd::NewTab, Cmd::Palette, Cmd::CloudSql, Cmd::Reopen, Cmd::Undo, Cmd::Log, Cmd::Settings, Cmd::CheckUpdates,
        Cmd::Shortcuts, Cmd::Insert, Cmd::Export, Cmd::ZoomIn, Cmd::ZoomOut, Cmd::ZoomReset, Cmd::Quit, Cmd::Github, Cmd::ReleaseNotes, Cmd::ReportIssue, Cmd::TextUndo, Cmd::TextRedo,
        Cmd::Cut, Cmd::Copy, Cmd::Paste, Cmd::SelectAll,
    ];
    let mut v: Vec<Cmd> = (0..8).map(Cmd::ViewAt).chain((0..9).map(Cmd::GoTab)).collect();
    for c in all {
        match c {
            Cmd::Reload | Cmd::Run | Cmd::RunAll | Cmd::Explain | Cmd::ViewAt(_) | Cmd::NewQuery | Cmd::CloseTab | Cmd::GoTab(_) | Cmd::NextTab | Cmd::PrevTab | Cmd::Back | Cmd::Forward | Cmd::FindQuery | Cmd::FindColl
            | Cmd::Connect | Cmd::LockWrites | Cmd::NewDb | Cmd::Server | Cmd::Users | Cmd::Disconnect | Cmd::EditConn | Cmd::AllowWrites | Cmd::SaveToLibrary | Cmd::SaveAs | Cmd::OpenPicker
            | Cmd::ShowLibrary | Cmd::LinkFolder | Cmd::NewGroup | Cmd::NewConn | Cmd::NewTab | Cmd::Palette | Cmd::CloudSql | Cmd::Reopen | Cmd::Undo | Cmd::Log | Cmd::Settings | Cmd::CheckUpdates
            | Cmd::Shortcuts | Cmd::Insert | Cmd::Export | Cmd::ZoomIn | Cmd::ZoomOut | Cmd::ZoomReset | Cmd::Quit | Cmd::Github | Cmd::ReleaseNotes | Cmd::ReportIssue | Cmd::TextUndo | Cmd::TextRedo
            | Cmd::Cut | Cmd::Copy | Cmd::Paste | Cmd::SelectAll => v.push(c),
        }
    }
    v
}

#[test]
fn every_command_has_one_row_and_a_label() {
    let all = all_cmds();
    assert_eq!(all.len(), CMDS.len(), "CMDS and the command list differ");
    for c in all {
        assert_eq!(CMDS.iter().filter(|s| s.cmd == c).count(), 1, "{c:?} must have exactly one row");
        assert!(!c.spec().label.is_empty(), "{c:?} has no label");
    }
}

#[test]
fn each_chord_is_unique_per_scope() {
    let mut seen = std::collections::HashSet::new();
    for (s, c) in KEY_ORDER.iter() {
        // Page and View share App::keys, App runs beside them, so a chord is unique across all of them.
        let class = if s.scope == Scope::Native { 1 } else { 0 };
        assert!(seen.insert((class, *c)), "{} is taken twice", c.text());
    }
    // Native keys never reach App::keys, but none of them may shadow a page key either.
    let page: Vec<Chord> = KEY_ORDER.iter().filter(|(s, _)| s.scope != Scope::Native).map(|x| x.1).collect();
    assert!(KEY_ORDER.iter().filter(|(s, _)| s.scope == Scope::Native).all(|(_, c)| !page.contains(c)));
}

#[test]
fn longer_chords_are_tried_first() {
    for (i, (_, a)) in KEY_ORDER.iter().enumerate() {
        for (_, b) in &KEY_ORDER[i + 1..] {
            assert!(!(a.key == b.key && a.mods() < b.mods()), "{} is tried before {}", a.text(), b.text());
        }
    }
}

fn menu(m: Menu) -> Vec<String> {
    menu_items(m, 0..=u8::MAX).into_iter().map(|i| i.map_or("-".to_string(), |s| format!("{} {}", s.menu.unwrap().title, s.keys.first().map(Chord::text).unwrap_or_default()).trim_end().to_string())).collect()
}

#[test]
fn the_menu_bar_keeps_its_items_keys_and_order() {
    assert_eq!(menu(Menu::App), ["Check for Updates\u{2026}", "-", "Settings\u{2026} Cmd+,", "-", "Quit Quarry Cmd+Q"]);
    assert_eq!(menu_items(Menu::App, 0..=1).len(), 3, "the app menu's predefined items go between Settings and Quit");
    assert_eq!(menu(Menu::File), ["New Connection\u{2026} Cmd+N", "New Tab\u{2026} Cmd+T", "Open Collection\u{2026} Cmd+K", "Reopen Closed Tab Cmd+Shift+T", "-", "Save to library Cmd+S", "-", "Insert Document\u{2026}", "Export\u{2026}", "-", "Close Tab Cmd+W"]);
    assert_eq!(menu(Menu::Edit), ["Undo Cmd+Z", "Redo Cmd+Shift+Z", "-", "Cut Cmd+X", "Copy Cmd+C", "Paste Cmd+V", "Select All Cmd+A", "-", "Find in Query Cmd+F", "Find Collection Cmd+Shift+F"]);
    assert_eq!(menu(Menu::View), [
        "Documents or Content Cmd+Alt+1", "Aggregate or Structure Cmd+Alt+2", "Indexes or Relations Cmd+Alt+3", "Schema or Triggers Cmd+Alt+4", "Import or Info Cmd+Alt+5", "Compare or Query Cmd+Alt+6", "Shell Cmd+Alt+7", "Watch Cmd+Alt+8",
        "-", "Query Log Cmd+L", "-", "Bigger Interface Cmd+=", "Smaller Interface Cmd+-", "Actual Size Cmd+0",
    ]);
    assert_eq!(menu(Menu::Query), ["Run Pipeline or Script Cmd+Enter", "Explain Cmd+E", "-", "Reload Tab Cmd+R", "Refresh Connection Cmd+Shift+R"]);
    assert_eq!(menu(Menu::Window), ["Show Next Tab Cmd+Shift+]", "Show Previous Tab Cmd+Shift+[", "Back Cmd+[", "Forward Cmd+]"]);
    assert_eq!(menu(Menu::Help), ["Keyboard Shortcuts Cmd+/", "-", "Quarry on GitHub", "Release Notes", "Report an Issue"]);
}

#[test]
fn the_menu_replays_only_keys_a_widget_reads() {
    let replayed: Vec<Cmd> = CMDS.iter().filter(|s| s.menu.is_some() && matches!(s.scope, Scope::View | Scope::Native)).map(|s| s.cmd).collect();
    assert_eq!(replayed, [Cmd::Run, Cmd::Explain, Cmd::FindQuery, Cmd::FindColl, Cmd::SaveToLibrary, Cmd::Quit, Cmd::TextUndo, Cmd::TextRedo, Cmd::Cut, Cmd::Copy, Cmd::Paste, Cmd::SelectAll]);
}

#[test]
fn the_sheet_lists_commands_then_widget_keys() {
    let s = sheet();
    for line in ["Cmd+1 to Cmd+8", "Cmd+9", "Cmd+[, Cmd+]", "Cmd+Alt+1 to Cmd+Alt+8", "Cmd+Shift+F, /", "Cmd+Shift+], Cmd+Shift+}, Ctrl+Tab", "Cmd+/"] {
        assert!(s.lines().any(|l| l.starts_with(&format!("{line:<30}"))), "the sheet lost {line}:\n{s}");
    }
    assert!(s.lines().any(|l| l.starts_with("Alt+Up/Down") && l.contains("Move the focused row")));
    assert!(s.lines().any(|l| l.starts_with("Cmd+Shift+[, Cmd+Shift+{, Ctrl+Shift+Tab  Previous tab")), "long keys run into their help:\n{s}");
    assert!(s.lines().any(|l| l == format!("{:30}On a SQL tab: Content, Structure, Relations, Triggers, Info, Query, Shell", "")));
    assert!(!s.contains("Cmd+Z") && !s.contains("Cmd+Q"), "keys egui handles stay off the sheet as before");
}

const CONN: u64 = 1;

fn with_tab(engine: Engine, status: Status) -> Core {
    let mut core = Core::for_test();
    core.conns.push(Conn::new(ConnMeta { id: CONN, name: "shop".into(), engine, ..Default::default() }, status));
    core.active = Some(CONN);
    core.execute(Action::Tab(TabA::Open { sel: Sel { conn: CONN, db: "shop".into(), coll: "orders".into(), ..Default::default() }, how: Open::Here })).unwrap();
    core
}

fn rows(core: &Core) -> Vec<String> {
    palette(core).map(|s| format!("{} {}", s.cmd.title(core), s.keys.first().map(Chord::text).unwrap_or_default()).trim_end().to_string()).collect()
}

#[test]
fn the_palette_lists_what_applies_in_its_order() {
    let mut mongo = with_tab(Engine::Mongo, Status::Ready);
    // The demo, so Check for updates stays off whatever the build.
    mongo.demo = true;
    assert_eq!(rows(&mongo), [
        "Reload tab Cmd+R", "Explain Cmd+E", "Show Aggregate Cmd+Alt+2", "Show Indexes Cmd+Alt+3", "Show Schema Cmd+Alt+4", "Show Import Cmd+Alt+5", "Show Compare Cmd+Alt+6", "Show Shell Cmd+Alt+7", "Show Watch Cmd+Alt+8", "Close tab Cmd+W",
        "Refresh connection Cmd+Shift+R", "New database\u{2026}", "Server status", "Users", "Disconnect", "Edit connection\u{2026}", "Read-only: allow writes\u{2026}",
        "Save to library\u{2026} Cmd+S", "Show the library Cmd+Shift+L", "Link a folder\u{2026}",
        "New group",
        "New connection\u{2026} Cmd+N", "Query log Cmd+L", "Settings Cmd+,", "Keyboard shortcuts Cmd+/",
    ]);
    let mut pg = with_tab(Engine::Postgres, Status::Ready);
    pg.demo = true;
    pg.conns[0].writable = true;
    pg.closed.push(crate::state::Tab::new(Sel::default()));
    assert_eq!(rows(&pg), [
        "Reload tab Cmd+R", "Explain Cmd+E", "Show Structure Cmd+Alt+2", "Show Relations Cmd+Alt+3", "Show Triggers Cmd+Alt+4", "Show Info Cmd+Alt+5", "Show Query Cmd+Alt+6", "Show Shell Cmd+Alt+7", "New query in shop", "Close tab Cmd+W",
        "Refresh connection Cmd+Shift+R", "Lock writes", "Server status", "Roles", "Disconnect", "Edit connection\u{2026}",
        "Save to library\u{2026} Cmd+S", "Show the library Cmd+Shift+L", "Link a folder\u{2026}",
        "New group",
        "New connection\u{2026} Cmd+N", "Add from Cloud SQL\u{2026}", "Reopen closed tab Cmd+Shift+T", "Query log Cmd+L", "Settings Cmd+,", "Keyboard shortcuts Cmd+/",
    ]);
    // An idle connection offers Connect in Refresh's place, and nothing that needs a session.
    let idle = with_tab(Engine::MySql, Status::Idle);
    let r = rows(&idle);
    assert!(r.contains(&"Connect Cmd+Shift+R".to_string()) && !r.iter().any(|x| x == "Server status" || x == "Disconnect"), "{r:?}");
}

fn act(c: Cmd, core: &Core) -> String {
    format!("{:?}", c.action(core))
}

#[test]
fn commands_resolve_the_current_tab_and_connection() {
    let mut core = with_tab(Engine::Mongo, Status::Ready);
    let first = core.t().unwrap().uid;
    core.execute(Action::Tab(TabA::Open { sel: Sel { conn: CONN, db: "shop".into(), coll: "items".into(), ..Default::default() }, how: Open::NewTab })).unwrap();
    let second = core.t().unwrap().uid;
    assert_eq!(act(Cmd::GoTab(8), &core), format!("{:?}", Some(Action::Tab(TabA::Pick { tab: second }))));
    assert_eq!(act(Cmd::GoTab(4), &core), act(Cmd::GoTab(8), &core), "a place past the last tab takes the last one");
    assert_eq!(act(Cmd::NextTab, &core), format!("{:?}", Some(Action::Tab(TabA::Pick { tab: first }))));
    assert_eq!(act(Cmd::ViewAt(1), &core), format!("{:?}", Some(Action::Tab(TabA::View { tab: second, view: View::Aggregate }))));
    assert_eq!(act(Cmd::Connect, &core), format!("{:?}", Some(Action::Conn(ConnA::Connect { conn: CONN }))));
    assert_eq!(act(Cmd::Users, &core), format!("{:?}", Some(Action::Users(UsersA::Mongo(MongoUsersA::Open { conn: CONN })))));
    // Explain and Run outside a Query view are the app's; Reload is too.
    assert_eq!(act(Cmd::Explain, &core), "None");
    assert_eq!(act(Cmd::Reload, &core), "None");
    // The demo never reconnects.
    core.demo = true;
    assert_eq!(act(Cmd::Connect, &core), "None");
    // Run, Run all and Explain on a SQL Query view are its Run.
    let mut sql = with_tab(Engine::MySql, Status::Ready);
    sql.tabs[0].view = View::Sql;
    let tab = sql.t().unwrap().uid;
    assert_eq!(act(Cmd::Explain, &sql), format!("{:?}", Some(Action::Sql(SqlA::Run { tab, all: false, explain: true }))));
    assert_eq!(act(Cmd::RunAll, &sql), format!("{:?}", Some(Action::Sql(SqlA::Run { tab, all: true, explain: false }))));
    assert_eq!(act(Cmd::NewDb, &sql), format!("{:?}", Some(Action::Ddl(DdlA::NewDb { conn: CONN }))));
}

#[test]
fn key_guards_leave_the_key_alone() {
    let mut sql = with_tab(Engine::MySql, Status::Ready);
    // Seven SQL views: Cmd+Alt+8 is left for Cmd+8.
    assert!(Cmd::ViewAt(6).keys_on(&sql, false) && !Cmd::ViewAt(7).keys_on(&sql, false));
    // Content holds a script; Cmd+Enter belongs to the Query view only.
    assert!(Cmd::SaveToLibrary.keys_on(&sql, false) && !Cmd::Run.keys_on(&sql, false));
    sql.tabs[0].view = View::Structure;
    assert!(!Cmd::SaveToLibrary.keys_on(&sql, false) && !Cmd::OpenPicker.keys_on(&sql, false));
    // The Reload and Explain keys are taken even when they do nothing.
    sql.tabs.clear();
    assert!(Cmd::Reload.keys_on(&sql, false) && !Cmd::GoTab(0).keys_on(&sql, false) && !Cmd::NextTab.keys_on(&sql, false));
}

#[test]
fn a_window_takes_the_page_layer() {
    let mut core = Core::for_test();
    assert_eq!(core.layer(), Layer::Page);
    core.insert = Some("{}".into());
    assert_eq!(core.layer(), Layer::Window);
    core.insert = None;
    core.agents.activity_open = true;
    assert_eq!(core.layer(), Layer::Window);
}
