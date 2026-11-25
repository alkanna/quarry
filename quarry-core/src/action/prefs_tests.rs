//! Settings, updates, toasts and the script library run headlessly, with made-up worker answers.

use super::*;
use crate::db::Msg;
use crate::library::disk::FileOut;
use crate::library::ops::DropAt;
use crate::library::tree::Node;
use crate::library::{Folder, Script, ScriptRef};
use crate::state::errors::Act;
use crate::state::library::open::{How, Pending};
use crate::state::settings::SettingsCat;
use crate::state::{Conn, ConnMeta, Status, Upd};

fn run(c: &mut Core, a: Action) {
    c.execute(a).expect("the action runs");
}

fn opened(c: &mut Core) -> Vec<String> {
    c.take_ui_fx()
        .into_iter()
        .map(|f| match f {
            UiFx::Open(Dialog::Settings) => "settings".into(),
            UiFx::Open(Dialog::WhatsNew(w)) => format!("whats new {}", w.releases.len()),
            UiFx::Open(Dialog::ErrorDetails(e)) => format!("details {}", e.id),
            UiFx::Open(Dialog::Text { title, .. }) => title,
            UiFx::Open(_) => "other dialog".into(),
            UiFx::Restart => "restart".into(),
            UiFx::OpenScript { conn, .. } => format!("open on {conn}"),
            UiFx::OpenResolved { script, .. } => format!("open {}", script.name),
            // Other areas add effects these tests never see.
            #[allow(unreachable_patterns)]
            _ => "other".into(),
        })
        .collect()
}

#[test]
fn a_setting_is_saved_once_per_frame_and_only_when_it_changed() {
    let mut c = Core::for_test();
    run(&mut c, Action::Prefs(PrefsA::Set(Pref::KeepToasts(true))));
    run(&mut c, Action::Prefs(PrefsA::Set(Pref::TimeoutMs(60_000))));
    assert!(c.prefs.keep_toasts && c.prefs.timeout_ms == 60_000 && c.prefs_dirty);
    c.flush_prefs();
    assert!(!c.prefs_dirty);
    run(&mut c, Action::Prefs(PrefsA::Set(Pref::KeepToasts(true))));
    assert!(!c.prefs_dirty, "setting the value it already has writes nothing");
}

#[test]
fn settings_drawn_on_a_copy_become_actions() {
    let mut c = Core::for_test();
    let before = PrefVals::of(&c.prefs);
    let mut v = before.clone();
    (v.vim, v.data_font) = (true, "Geist Mono".into());
    let changes = before.diff(&v);
    assert_eq!(changes, vec![Pref::DataFont("Geist Mono".into()), Pref::Vim(true)]);
    c.width_cache.insert((1, 0, 0, "a".into(), None, false), 40.0);
    for p in changes {
        run(&mut c, Action::Prefs(PrefsA::Set(p)));
    }
    assert!(c.prefs.vim && c.prefs.data_font == "Geist Mono");
    assert!(c.width_cache.is_empty(), "widths measured in the old font are dropped");
}

#[test]
fn turning_always_writable_off_locks_every_connection_again() {
    let mut c = Core::for_test();
    c.conns.push(Conn { writable: true, ..Conn::new(ConnMeta { id: 1, name: "local".into(), ..Default::default() }, Status::Ready) });
    c.prefs.always_writable = true;
    run(&mut c, Action::Prefs(PrefsA::Set(Pref::AlwaysWritable(false))));
    assert!(!c.conns[0].writable);
}

#[test]
fn settings_open_on_a_category_and_the_list_picks_another() {
    let mut c = Core::for_test();
    run(&mut c, Action::Prefs(PrefsA::Open(SettingsCat::Tools)));
    assert_eq!((c.settings_cat, opened(&mut c)), (SettingsCat::Tools, vec!["settings".to_string()]));
    run(&mut c, Action::Prefs(PrefsA::Cat(SettingsCat::Reading)));
    run(&mut c, Action::Prefs(PrefsA::Zoom(1.25)));
    assert_eq!((c.settings_cat, c.prefs.ui_scale), (SettingsCat::Reading, 1.25));
}

fn release(v: &str) -> crate::update::Available {
    crate::update::Available { version: v.into(), name: String::new(), url: String::new(), size: 0, sum_url: String::new(), notes: Vec::new() }
}

#[test]
fn a_check_asked_for_says_up_to_date_and_a_quiet_one_keeps_its_failure_for_settings() {
    let mut c = Core::for_test();
    c.upd = Upd::Checking { manual: true };
    c.inject(Msg::UpdateChecked(Ok(None)));
    assert!(matches!(c.upd, Upd::UpToDate(_)));
    c.upd = Upd::Checking { manual: false };
    c.inject(Msg::UpdateChecked(Err("rate limited".into())));
    assert!(matches!(c.upd, Upd::Idle));
    assert_eq!(c.update_note, "Last check failed: rate limited");
    // With automatic updates off, a release found is offered, not fetched.
    c.prefs.auto_update = false;
    c.upd = Upd::Checking { manual: false };
    c.inject(Msg::UpdateChecked(Ok(Some(release("99.0.0")))));
    assert!(matches!(&c.upd, Upd::Available(a) if a.version == "99.0.0"));
    assert!(c.update_note.is_empty(), "a check that worked clears the old failure");
}

#[test]
fn an_install_answer_ends_the_download_installed_offered_again_or_failed() {
    let mut c = Core::for_test();
    let downloading = |c: &mut Core| {
        let prog = std::sync::Arc::new(crate::update::Progress::default());
        c.upd = Upd::Downloading { info: release("99.0.0"), prog: prog.clone(), started: std::time::Instant::now() };
        prog
    };
    downloading(&mut c);
    c.inject(Msg::UpdateInstalled(Ok("done".into())));
    assert!(matches!(c.upd, Upd::Ready(_)));
    // Cancel offers the release again rather than calling it a failure.
    downloading(&mut c).cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    c.inject(Msg::UpdateInstalled(Err("cancelled".into())));
    assert!(matches!(c.upd, Upd::Available(_)));
    downloading(&mut c);
    c.inject(Msg::UpdateInstalled(Err("checksum does not match".into())));
    assert!(matches!(&c.upd, Upd::Failed { info: Some(_), msg } if msg == "checksum does not match"));
}

#[test]
fn restart_waits_for_open_work_and_a_dev_build_checks_in_settings() {
    let mut c = Core::for_test();
    c.upd = Upd::Ready(release("99.0.0"));
    c.insert = Some("{}".into());
    run(&mut c, Action::Update(UpdateA::Press(UpdBtn::Restart)));
    assert!(opened(&mut c).is_empty());
    assert_eq!(c.toasts.last().map(|t| t.title.clone()), Some("Save or discard your edits and wait for running scripts first".into()));
    c.insert = None;
    run(&mut c, Action::Update(UpdateA::Press(UpdBtn::Restart)));
    assert_eq!(opened(&mut c), vec!["restart".to_string()]);
    // Test builds are dev builds, which never install: Check for updates shows Settings > Updates instead.
    c.upd = Upd::Idle;
    run(&mut c, Action::Update(UpdateA::CheckNow));
    assert_eq!((c.settings_cat, opened(&mut c)), (SettingsCat::Updates, vec!["settings".to_string()]));
    assert!(matches!(c.upd, Upd::Idle) && c.update_checked.is_none());
}

#[test]
fn toasts_close_by_id_and_errors_open_their_details() {
    let mut c = Core::for_test();
    c.err("E11000 duplicate key error");
    c.warn("Not connected. Connect to this server first.");
    let (err_toast, warn_toast) = (c.toasts[0].id, c.toasts[1].id);
    let err_id = c.errors[0].id;
    run(&mut c, Action::App(AppA::Dismiss(warn_toast)));
    assert_eq!(c.toasts.iter().map(|t| t.id).collect::<Vec<_>>(), vec![err_toast]);
    run(&mut c, Action::App(AppA::Details(err_id)));
    assert_eq!(opened(&mut c), vec![format!("details {err_id}")]);
    assert_eq!(c.errors_unseen, 1);
    run(&mut c, Action::App(AppA::ErrorsSeen));
    assert_eq!(c.errors_unseen, 0);
    run(&mut c, Action::App(AppA::ClearErrors));
    assert!(c.errors.is_empty());
    // The toast keeps its error after the list is cleared.
    run(&mut c, Action::App(AppA::Details(err_id)));
    assert_eq!(opened(&mut c).len(), 1);
}

#[test]
fn raise_limit_saves_the_new_time_limit() {
    let mut c = Core::for_test();
    c.prefs.timeout_ms = 30_000;
    run(&mut c, Action::App(AppA::RaiseLimit));
    assert_eq!(c.prefs.timeout_ms, 120_000);
    assert!(c.prefs_dirty);
    assert_eq!(c.status, "Time limit raised to 120 s. Change it in Settings.");
}

fn script(id: u64, folder: Option<u64>) -> Script {
    Script { id, name: format!("s{id}"), kind: "sql".into(), body: "SELECT 1".into(), conn: 1, folder, ..Default::default() }
}

#[test]
fn deleting_a_folder_takes_its_scripts_and_offers_them_back() {
    let mut c = Core::for_test();
    c.library.folders.push(Folder { id: 10, name: "reports".into(), parent: None });
    c.library.scripts.extend([script(1, Some(10)), script(2, None)]);
    c.library.reserve_ids();
    c.lib_ui.panel.sel = Some(Node::Script(1));
    run(&mut c, Action::Library(LibA::Delete(Node::Folder(10))));
    assert_eq!(c.library.scripts.iter().map(|s| s.id).collect::<Vec<_>>(), vec![2]);
    assert!(c.lib_ui.panel.sel.is_none(), "the selection went with the folder");
    let t = c.toasts.last().unwrap();
    assert_eq!(t.title, "Deleted folder \"reports\" and 1 script.");
    assert!(matches!(t.acts.first(), Some((l, Act::Restore(_))) if l == "Undo"));
}

#[test]
fn a_new_folder_opens_its_rename_and_a_move_into_it_opens_it() {
    let mut c = Core::for_test();
    c.library.scripts.push(script(1, None));
    c.library.reserve_ids();
    run(&mut c, Action::Library(LibA::NewFolder(None)));
    let f = c.library.folders[0].id;
    assert!(matches!(&c.lib_ui.panel.rename, Some((Node::Folder(id), _, true)) if *id == f));
    run(&mut c, Action::Library(LibA::Move { node: Node::Script(1), at: DropAt::Into(f) }));
    assert_eq!(c.library.scripts[0].folder, Some(f));
    assert!(c.lib_ui.panel.open.contains(&Node::Folder(f)));
    run(&mut c, Action::Library(LibA::Scope(true)));
    assert!(c.prefs.lib_scope_all && c.prefs_dirty);
}

#[test]
fn a_waiting_script_opens_once_its_connection_is_ready_or_is_cancelled() {
    let mut c = Core::for_test();
    c.conns.push(Conn::new(ConnMeta { id: 1, name: "local".into(), ..Default::default() }, Status::Connecting));
    let p = Pending { r: ScriptRef::Saved(1), how: How::Here, conn: 1, uid: None, attempt: 0, since: None, seen_connecting: false, name: "s1".into() };
    c.lib_ui.pending = Some(p.clone());
    assert_eq!(c.library_tick(0.0, true), None);
    assert!(c.lib_ui.pending.as_ref().is_some_and(|p| p.seen_connecting));
    c.conns[0].status = Status::Ready;
    c.library_tick(1.0, true);
    assert!(c.lib_ui.pending.is_none());
    assert_eq!(opened(&mut c), vec!["open on 1".to_string()]);
    c.lib_ui.pending = Some(p);
    run(&mut c, Action::Library(LibA::CancelPending));
    assert!(c.lib_ui.pending.is_none());
}

#[test]
fn a_file_answer_for_a_job_no_longer_waited_for_is_dropped() {
    let mut c = Core::for_test();
    c.inject(Msg::LibFile { job: 42, out: FileOut::Trashed(Ok(Vec::new())) });
    assert!(c.toasts.is_empty() && c.errors.is_empty());
    // A scan of a link that was unlinked meanwhile is not kept.
    c.lib_ui.scanning.insert(7);
    c.inject(Msg::LibScan { link: 7, scan: Default::default() });
    assert!(c.lib_ui.scans.is_empty() && c.lib_ui.scanning.is_empty());
}

#[test]
fn the_conflict_choice_acts_on_the_tab_it_was_shown_on() {
    let mut c = Core::for_test();
    let mut t = crate::state::Tab::new(crate::state::Sel { conn: 1, db: "shop".into(), ..Default::default() }).for_engine(crate::db::sql::Engine::MySql);
    t.view = crate::state::View::Sql;
    if let Some(q) = t.sql.as_mut() {
        q.query = "SELECT 2".into();
    }
    t.script = Some(crate::state::Bound { r: ScriptRef::File { link: 1, rel: "a.sql".into() }, view: crate::state::View::Sql, base: "SELECT 1".into(), disk: None, conflict: Some("SELECT 3".into()) });
    let uid = t.uid;
    c.tabs.push(t);
    run(&mut c, Action::Library(LibA::ConflictCompare { tab: uid }));
    assert_eq!(opened(&mut c), vec!["a.sql in this tab and on disk".to_string()]);
    run(&mut c, Action::Library(LibA::ConflictKeep { tab: uid }));
    let b = c.tabs[0].script.as_ref().unwrap();
    assert_eq!((b.base.as_str(), b.conflict.is_none()), ("SELECT 3", true), "the file's text is the base, so the tab stays edited");
}

#[test]
fn whats_new_remembers_the_app_version_as_seen() {
    let mut c = Core::for_test();
    c.prefs.last_version = "0.0.1".into();
    c.whats_new_seen(false);
    // The app's version, never quarry-core's own.
    assert_eq!(c.prefs.last_version, crate::VERSION);
    assert!(c.prefs_dirty && c.take_ui_fx().is_empty(), "a fresh install skips the notes");
}
