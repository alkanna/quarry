//! End-to-end journeys through groups, the script library and linked folders, on the real app against live servers. Each
//! journey owns its tables, collections and folders, since the live tests run in parallel on shared servers, and checks
//! the server or the disk at the end. They skip without QUARRY_TEST_MYSQL, QUARRY_TEST_POSTGRES or QUARRY_TEST_MONGO.

use super::sql_edit::Fx;
use super::*;
use crate::app::library::open::How;
use crate::library::{Link, Script, ScriptRef};
use std::path::{Path, PathBuf};

const MY: &str = "QUARRY_TEST_MYSQL";
const PG: &str = "QUARRY_TEST_POSTGRES";
const MONGO: &str = "QUARRY_TEST_MONGO";

/// A folder of its own under the temp directory, removed when the journey ends, also when it fails.
struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Dir {
        let d = std::env::temp_dir().join(format!("quarry-journey-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    fn name(&self) -> String {
        self.0.file_name().unwrap().to_string_lossy().into_owned()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl UiApp {
    /// The current tab's SQL Query state.
    fn query(&self) -> Option<&SqlTab> {
        self.app().t().and_then(|t| t.sql.as_deref())
    }

    /// Waits for the current tab's Query run to come back.
    fn wait_run(&mut self) {
        self.wait("the query to run", |a| a.t().and_then(|t| t.sql.as_ref()).is_some_and(|s| s.run_req == 0 && !s.sets.is_empty()));
    }

    /// One column of every row of the current tab's first result set.
    fn column(&self, col: &str) -> Vec<String> {
        let set = &self.query().expect("a Query tab").sets[0];
        assert!(set.error.is_none(), "the query failed: {:?}", set.error);
        set.rows.iter().map(|r| r.decode().unwrap().get(col).map(|v| v.to_string().trim_matches('"').to_string()).unwrap_or_default()).collect()
    }

    /// Waits until the linked-file reads and writes on the worker are back.
    fn wait_files(&mut self) {
        self.wait("the linked-file jobs", |a| a.lib_ui.jobs.is_empty());
        self.settle();
    }

    /// Selects a row of the Library panel with a click, which hands the panel the keys.
    fn library_row(&mut self, label: &str) {
        self.click(label);
        self.wait("the row to be selected", |a| a.lib_ui.panel.focus);
    }

    fn show_library(&mut self) {
        if self.app().prefs.side_mode() != SideMode::Library || self.app().prefs.sidebar_hidden {
            self.click("Script library");
        }
    }

    /// Puts `text` in the current tab's Query editor, as typing it would.
    fn set_query(&mut self, text: &str) {
        let a = self.h.state_mut();
        if let Some(s) = a.tm().and_then(|t| t.sql.as_mut()) {
            s.query = text.to_string();
            s.caret = 0;
        }
        self.settle();
    }
}

#[test]
fn mysql_a_query_saved_in_a_new_folder_reopens_from_the_library_and_runs() {
    let Some(mut fx) = Fx::new(MY) else { return };
    let table = fx.table();
    let mut ui = ui_app(MY).unwrap();

    // A group for the connection, made in the Connections panel, and the connection dragged into it.
    ui.click("Saved connections");
    ui.click("New group");
    ui.show("New group name");
    ui.type_text("Journey");
    ui.h.key_press(egui::Key::Enter);
    ui.settle();
    ui.show("Group Journey");
    ui.drag("test", "Group Journey", 0.5, 1);
    assert_eq!(ui.app().groups.group_of(CONN).map(|g| g.name.as_str()), Some("Journey"), "the drag did not put the connection in the group");

    // A Query tab with a SELECT, saved with Cmd+S into a new folder of the library.
    ui.h.state_mut().sql_new_query(CONN, "quarry_test".into());
    ui.set_query(&format!("SELECT name FROM quarry_test.{table} ORDER BY id"));
    ui.key(egui::Modifiers::COMMAND, egui::Key::S);
    ui.wait("the Save dialog", |a| matches!(a.modal, Some(Modal::SaveScript(_))));
    ui.click("Script name");
    ui.h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    ui.type_text("Names");
    if let Some(Modal::SaveScript(f)) = &mut ui.h.state_mut().modal {
        f.new_folder = Some(String::new());
    }
    ui.fill("New folder name", "Journeys");
    ui.click_last("Save");
    ui.wait("the dialog to close", |a| a.modal.is_none());
    let lib = &ui.app().library;
    let folder = lib.folders.iter().find(|f| f.name == "Journeys").map(|f| f.id).expect("the new folder");
    let s = lib.scripts.iter().find(|s| s.name == "Names").cloned().expect("the saved script");
    assert_eq!((s.folder, s.kind.as_str(), s.conn, s.db.as_str()), (Some(folder), "sql", CONN, "quarry_test"));

    // The tab moves on to other work, then the script comes back from the Library panel with Cmd+Enter, which runs it.
    ui.set_query("SELECT 0");
    ui.show_library();
    ui.click("Journeys");
    ui.library_row("Names");
    ui.key(egui::Modifiers::COMMAND, egui::Key::Enter);
    ui.wait("the script's tab", |a| a.t().and_then(|t| t.script.as_ref()).is_some_and(|b| b.r == ScriptRef::Saved(s.id)));
    ui.wait_run();
    assert_eq!(ui.column("name"), ["alpha", "beta", "gamma"]);
}

#[test]
fn mysql_a_linked_file_is_edited_saved_and_reloaded_after_a_change_on_disk() {
    let Some(mut fx) = Fx::new(MY) else { return };
    let table = fx.table();
    let dir = Dir::new("linked");
    let file = dir.0.join("q.sql");
    std::fs::write(&file, format!("SELECT name FROM quarry_test.{table} WHERE id = 1")).unwrap();
    let mut ui = ui_app(MY).unwrap();

    // Linked through the dialog, used on the test connection.
    ui.h.state_mut().open_link_dialog(None);
    ui.fill("Folder path", &dir.0.to_string_lossy());
    ui.show("1 script in 1 folder");
    ui.click("Link folder");
    let link = ui.app().library.links.first().map(|l| l.id).expect("a link");
    assert_eq!(ui.app().library.links[0].conns, [CONN]);

    // Opened from the tree with Return.
    ui.show_library();
    ui.click(&dir.name());
    ui.library_row("q.sql");
    ui.key(egui::Modifiers::NONE, egui::Key::Enter);
    ui.wait_files();
    ui.wait("the file's tab", |a| a.t().and_then(|t| t.script.as_ref()).is_some_and(|b| matches!(&b.r, ScriptRef::File { link: l, rel } if *l == link && rel == Path::new("q.sql"))));

    // Edited and saved with Cmd+S: the file holds the text and nothing else.
    let edited = format!("SELECT name FROM quarry_test.{table} WHERE id = 2");
    ui.set_query(&edited);
    ui.key(egui::Modifiers::COMMAND, egui::Key::S);
    ui.wait_files();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), edited, "Cmd+S did not write the file as typed");
    assert!(ui.app().modal.is_none(), "a bound file saved through the dialog");

    // Changed on disk while the tab holds edits: the conflict, then Reload takes the file's text.
    ui.set_query(&format!("SELECT name FROM quarry_test.{table} WHERE id = 3"));
    std::thread::sleep(Duration::from_millis(30));
    let theirs = format!("SELECT name, qty FROM quarry_test.{table} WHERE id = 3 -- from the team");
    std::fs::write(&file, &theirs).unwrap();
    // As the window getting the focus back would, so the journey does not wait on the 2 s poll.
    ui.h.state_mut().rescan_link(link);
    ui.wait("the conflict", |a| a.t().and_then(|t| t.script.as_ref()).is_some_and(|b| b.conflict.as_deref() == Some(theirs.as_str())));
    if ui.h.query_all_by_label("Reload").next().is_some() {
        ui.click_last("Reload");
    } else {
        ui.h.state_mut().conflict_reload();
    }
    ui.wait_files();
    ui.wait("the reload", |a| a.t().is_some_and(|t| t.sql.as_ref().is_some_and(|s| s.query == theirs) && t.script.as_ref().is_some_and(|b| b.conflict.is_none())));
    ui.h.state_mut().sql_run(true, false);
    ui.wait_run();
    assert_eq!(ui.column("name"), ["gamma"]);
}

#[test]
fn mysql_a_write_run_from_the_library_on_a_locked_connection_changes_nothing() {
    let Some(mut fx) = Fx::new(MY) else { return };
    let table = fx.table();
    let mut ui = ui_app(MY).unwrap();
    assert!(!ui.app().conn(CONN).unwrap().writable, "the connection starts read-only");
    let a = ui.h.state_mut();
    let id = a.library.next_id();
    a.library.scripts.push(Script { id, name: "Raise qty".into(), kind: "sql".into(), body: format!("UPDATE quarry_test.{table} SET qty = 99 WHERE id = 1"), conn: CONN, db: "quarry_test".into(), ..Default::default() });
    ui.show_library();
    ui.library_row("Raise qty");
    ui.key(egui::Modifiers::COMMAND, egui::Key::Enter);
    ui.wait("the script's tab", |a| a.t().and_then(|t| t.script.as_ref()).is_some_and(|b| b.r == ScriptRef::Saved(id)));
    // Refused before it reaches the server, or not run at all; either way the person is told why.
    ui.wait("the read-only notice", |a| {
        let refused = a.t().and_then(|t| t.sql.as_ref()).is_some_and(|s| s.sets.iter().any(|x| x.refused));
        refused || a.last_problem().to_lowercase().contains("read-only")
    });
    fx.expect_unchanged(&mut ui, &table, super::sql_edit::ORIGINAL);
}

#[test]
fn postgres_a_library_script_with_a_schema_runs_in_that_schema() {
    let Some(fx) = Fx::new(PG) else { return };
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let schema = format!("journey_{}_{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let table = format!("{schema}_t");
    /// Drops what the journey made, also when it fails.
    struct Cleanup<'a>(&'a Fx, String, String);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            self.0.sql(&format!("DROP SCHEMA IF EXISTS {} CASCADE", self.1));
            self.0.sql(&format!("DROP TABLE IF EXISTS public.{}", self.2));
        }
    }
    let _clean = Cleanup(&fx, schema.clone(), table.clone());
    fx.sql(&format!("CREATE SCHEMA {schema}"));
    // The same table name in public and in the schema, so only the schema the script names gives its row.
    fx.sql(&format!("CREATE TABLE {schema}.{table} (v text)"));
    fx.sql(&format!("INSERT INTO {schema}.{table} VALUES ('in the schema')"));
    fx.sql(&format!("CREATE TABLE public.{table} (v text)"));
    fx.sql(&format!("INSERT INTO public.{table} VALUES ('in public')"));
    let mut ui = ui_app(PG).unwrap();
    let a = ui.h.state_mut();
    let id = a.library.next_id();
    a.library.scripts.push(Script { id, name: "Where am I".into(), kind: "sql".into(), body: format!("SELECT v FROM {table}"), conn: CONN, db: "quarry_test".into(), schema: schema.clone(), ..Default::default() });
    a.open_script(&ScriptRef::Saved(id), How::Run);
    ui.wait("the script's tab", |a| a.t().and_then(|t| t.script.as_ref()).is_some_and(|b| b.r == ScriptRef::Saved(id)));
    ui.wait_run();
    assert_eq!(ui.column("v"), ["in the schema"]);
}

#[test]
fn mongo_a_filter_with_a_date_shortcut_is_saved_to_a_linked_file_and_reopens() {
    use mongodb::bson::{Document, doc};
    let Ok(uri) = std::env::var(MONGO) else { return };
    let db_name = "quarry_journey";
    let coll = format!("today_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
    let rt = tokio::runtime::Runtime::new().unwrap();
    let c = rt.block_on(async {
        let client = mongodb::Client::with_uri_str(&uri).await.unwrap();
        let c = client.database(db_name).collection::<Document>(&coll);
        let old = mongodb::bson::DateTime::from_millis(978_307_200_000);
        c.insert_many(vec![doc! { "_id": 1, "created": mongodb::bson::DateTime::now() }, doc! { "_id": 2, "created": old }]).await.unwrap();
        c
    });
    /// Drops the collection, also when the journey fails.
    struct Drop2<'a>(&'a tokio::runtime::Runtime, mongodb::Collection<Document>);
    impl Drop for Drop2<'_> {
        fn drop(&mut self) {
            self.0.block_on(async { self.1.drop().await.ok() });
        }
    }
    let _clean = Drop2(&rt, c.clone());
    let dir = Dir::new("mongo");
    let mut ui = ui_app(MONGO).unwrap();
    let a = ui.h.state_mut();
    let link = a.library.next_id();
    a.library.links.push(Link { id: link, path: dir.0.clone(), name: "filters".into(), on: "conns".into(), conns: vec![CONN], exts: vec!["json".into()], follow: true, ..Default::default() });
    a.rescan_link(link);

    // The collection, filtered with #today.
    ui.open_table(db_name, "", &coll);
    let ids = |a: &App| a.t().map(|t| t.docs.iter().filter_map(|d| d.decode().ok()?.get_i32("_id").ok()).collect::<Vec<i32>>()).unwrap_or_default();
    let a = ui.h.state_mut();
    if let Some(t) = a.tm() {
        t.filter_txt = "{ created: #today }".into();
    }
    a.run_query();
    ui.wait("today's document", |a| a.t().is_some_and(|t| !t.loading) && ids(a) == [1]);

    // Cmd+S into the linked folder.
    ui.key(egui::Modifiers::COMMAND, egui::Key::S);
    ui.wait("the Save dialog", |a| matches!(a.modal, Some(Modal::SaveScript(_))));
    if let Some(Modal::SaveScript(f)) = &mut ui.h.state_mut().modal {
        (f.link, f.name) = (Some(link), "today".into());
    }
    ui.settle();
    ui.click_last("Save");
    ui.wait_files();
    let saved = std::fs::read_to_string(dir.0.join("today.json")).expect("the file");
    // The shortcut stays a shortcut, so the filter means today on the day it runs, not the day it was saved.
    assert!(saved.contains("#today") && saved.contains(&coll), "{saved}");

    // The tab moves on to another filter and lets go of the file, then the file is opened again: the filter as written,
    // and today's document only.
    let a = ui.h.state_mut();
    if let Some(t) = a.tm() {
        t.filter_txt = "{}".into();
        t.script = None;
    }
    a.open_script(&ScriptRef::File { link, rel: "today.json".into() }, How::NewTab);
    ui.wait_files();
    ui.wait("the file's tab", |a| a.t().and_then(|t| t.script.as_ref()).is_some_and(|b| matches!(&b.r, ScriptRef::File { rel, .. } if rel == Path::new("today.json"))));
    assert_eq!(ui.app().t().unwrap().filter_txt, "{ created: #today }");
    ui.wait("today's document again", |a| a.t().is_some_and(|t| !t.loading) && ids(a) == [1]);
}
