//! Editing MongoDB documents through the UI: inline edit, delete, insert, undo and the locked connection.
//! Each test owns one collection in its own database, and checks the server with the driver afterwards.

use super::*;
use futures_util::TryStreamExt;
use mongodb::bson::{Document, doc};

const VAR: &str = "QUARRY_TEST_MONGO";
pub(super) const DB: &str = "quarry_ui_edit";

/// Collections of one test in `DB`, each seeded with its documents, with their own runtime and client, dropped on drop.
pub(super) struct Fixture {
    pub rt: tokio::runtime::Runtime,
    db: mongodb::Database,
    pub names: Vec<String>,
}

impl Fixture {
    /// One collection seeded with `docs`.
    pub fn new(test: &str, docs: Vec<Document>) -> Option<Fixture> {
        Self::many(test, vec![docs])
    }

    /// One collection per list of documents, named after `test` with a letter for each.
    pub fn many(test: &str, colls: Vec<Vec<Document>>) -> Option<Fixture> {
        let uri = std::env::var(VAR).ok()?;
        // Unique per run so a crashed earlier run and parallel tests never share a collection.
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let names: Vec<String> = (0..colls.len()).map(|i| format!("{test}_{}_{stamp}", (b'a' + i as u8) as char)).collect();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let db = rt.block_on(async {
            let db = mongodb::Client::with_uri_str(&uri).await.unwrap().database(DB);
            for (n, docs) in names.iter().zip(colls) {
                db.collection::<Document>(n).insert_many(docs).await.unwrap();
            }
            db
        });
        Some(Fixture { rt, db, names })
    }

    pub fn coll(&self, i: usize) -> mongodb::Collection<Document> {
        self.db.collection(&self.names[i])
    }

    /// The documents of the first collection, by _id.
    pub fn all(&self) -> Vec<Document> {
        self.all_in(0)
    }

    pub fn all_in(&self, i: usize) -> Vec<Document> {
        self.rt.block_on(async { self.coll(i).find(doc! {}).sort(doc! { "_id": 1 }).await.unwrap().try_collect().await.unwrap() })
    }

    pub fn exists(&self, i: usize) -> bool {
        self.rt.block_on(async { self.db.list_collection_names().await.unwrap() }).contains(&self.names[i])
    }

    /// Steps the UI until the first collection holds `want`.
    pub fn expect(&self, ui: &mut UiApp, want: Vec<Document>) {
        self.expect_in(ui, 0, want);
    }

    /// Steps the UI until collection `i` holds `want`. A write lands some frames after its click, and the undo entry leaves the list
    /// when the undo is sent, not when the server has applied it, so reading the collection right after the click races a loaded machine.
    pub fn expect_in(&self, ui: &mut UiApp, i: usize, want: Vec<Document>) {
        ui.wait(&format!("{} to hold {want:?}", self.names[i]), |_| self.all_in(i) == want);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for i in 0..self.names.len() {
            self.rt.block_on(async { self.coll(i).drop().await.ok() });
        }
    }
}

pub(super) fn seed() -> Vec<Document> {
    vec![doc! { "_id": 1, "name": "alpha", "qty": 1 }, doc! { "_id": 2, "name": "beta", "qty": 2 }, doc! { "_id": 3, "name": "gamma", "qty": 3 }]
}

fn open(f: &Fixture) -> UiApp {
    let mut ui = ui_app(VAR).unwrap();
    ui.open_table(DB, "", &f.names[0]);
    ui
}

impl UiApp {
    /// Opens the inline editor of a cell, replaces its text and commits with Enter, which stages the edit.
    fn mongo_edit_cell(&mut self, col: &str, row: usize, text: &str) {
        self.double_click_cell(col, row);
        self.wait("the inline editor", |a| a.inline.is_some());
        self.type_over(text);
        self.h.key_press(egui::Key::Enter);
        self.wait("the edit to be staged", |a| a.inline.is_none() && !a.drafts.is_empty());
    }

    /// Replaces the text of the focused editor. The inline editor does not select its text when it opens.
    pub(super) fn type_over(&mut self, text: &str) {
        self.h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        self.h.event(egui::Event::Text(text.into()));
        self.settle();
    }

    /// Ticks the checkbox of a row, numbered as the grid shows it.
    fn check_row(&mut self, row: usize) {
        self.click(&format!("Select row {row}"));
    }

    /// Deletes the checked rows through the selection bar and its dialog, then waits for the grid to reload.
    fn delete_checked(&mut self) {
        self.click("Delete…");
        self.click("Delete");
    }

    /// Takes back the most recent action through the toolbar and its dialog, then closes the dialog.
    fn undo_latest(&mut self) {
        self.undo_latest_open();
        self.close_recent_changes();
    }

    /// Takes back the most recent action through the toolbar and Recent changes, leaving that dialog open for a test that waits for
    /// the answer first.
    pub(super) fn undo_latest_open(&mut self) {
        let before = self.app().undo.len();
        self.click(&format!("Undo ({before})"));
        self.click("Undo change 1");
    }

    /// Closes Recent changes. A toast for the last write may still show a Close button of its own, so the dialog's is the one
    /// nearest its title.
    pub(super) fn close_recent_changes(&mut self) {
        self.click_near("Close", "Recent changes");
        self.wait("Recent changes to close", |a| a.modal.is_none());
    }

    /// Clicks the widget named `label` that sits closest, vertically, to the one named `anchor`.
    fn click_near(&mut self, label: &str, anchor: &str) {
        let y = self.h.get_by_label(anchor).rect().center().y;
        let buttons = self.h.get_all_by_label(label);
        buttons.min_by(|a, b| (a.rect().center().y - y).abs().total_cmp(&(b.rect().center().y - y).abs())).unwrap().click();
        self.settle();
    }

    /// Clicks the JSON text of the insert dialog. The query bar behind it is also a multiline input, so the one holding the template is picked.
    fn focus_insert_editor(&mut self) {
        let all = self.h.get_all_by_role(egui::accesskit::Role::MultilineTextInput);
        all.into_iter().find(|n| n.value().is_some_and(|v| v.contains("{\n  \n}"))).expect("the insert editor").click();
    }

    fn waits_for_rows(&mut self, n: usize) {
        self.wait("the grid to reload", |a| a.t().is_some_and(|t| !t.loading && t.docs.len() == n));
    }
}

#[test]
fn inline_edit_saves_one_field() {
    let Some(f) = Fixture::new("edit", seed()) else { return };
    let mut ui = open(&f);
    ui.unlock();
    ui.mongo_edit_cell("name", 2, "delta");
    // Staged only: the server still has the old value until Save.
    assert_eq!(f.all(), seed());
    ui.save();
    assert_eq!(f.all(), vec![doc! { "_id": 1, "name": "alpha", "qty": 1 }, doc! { "_id": 2, "name": "delta", "qty": 2 }, doc! { "_id": 3, "name": "gamma", "qty": 3 }]);
}

#[test]
fn inline_edit_keeps_the_type_of_a_number() {
    let Some(f) = Fixture::new("edit_num", seed()) else { return };
    let mut ui = open(&f);
    ui.unlock();
    ui.mongo_edit_cell("qty", 1, "42");
    ui.save();
    assert_eq!(f.all()[0], doc! { "_id": 1, "name": "alpha", "qty": 42 });
}

#[test]
fn inline_edit_after_cancelling_with_escape() {
    let Some(f) = Fixture::new("edit_esc", seed()) else { return };
    let mut ui = open(&f);
    ui.unlock();
    ui.double_click_cell("name", 1);
    ui.wait("the inline editor", |a| a.inline.is_some());
    ui.type_over("discarded");
    ui.h.key_press(egui::Key::Escape);
    ui.wait("the editor to close", |a| a.inline.is_none());
    assert!(ui.app().drafts.is_empty(), "Esc must not stage the typed text");
    assert_eq!(f.all(), seed());
    // The editor must open again, with the stored value and not the abandoned text.
    ui.double_click_cell("name", 3);
    ui.wait("the inline editor again", |a| a.inline.as_ref().is_some_and(|i| i.text == "gamma"));
    ui.type_over("omega");
    ui.h.key_press(egui::Key::Enter);
    ui.wait("the edit to be staged", |a| a.inline.is_none() && !a.drafts.is_empty());
    ui.save();
    assert_eq!(f.all(), vec![doc! { "_id": 1, "name": "alpha", "qty": 1 }, doc! { "_id": 2, "name": "beta", "qty": 2 }, doc! { "_id": 3, "name": "omega", "qty": 3 }]);
}

#[test]
fn delete_one_document() {
    let Some(f) = Fixture::new("del", seed()) else { return };
    let mut ui = open(&f);
    ui.unlock();
    ui.check_row(2);
    ui.delete_checked();
    ui.waits_for_rows(2);
    assert_eq!(f.all(), vec![doc! { "_id": 1, "name": "alpha", "qty": 1 }, doc! { "_id": 3, "name": "gamma", "qty": 3 }]);
}

#[test]
fn delete_several_documents() {
    let Some(f) = Fixture::new("del_many", seed()) else { return };
    let mut ui = open(&f);
    ui.unlock();
    ui.check_row(1);
    ui.check_row(3);
    ui.delete_checked();
    ui.waits_for_rows(1);
    assert_eq!(f.all(), vec![doc! { "_id": 2, "name": "beta", "qty": 2 }]);
}

#[test]
fn insert_a_new_document() {
    let Some(f) = Fixture::new("ins", seed()) else { return };
    let mut ui = open(&f);
    ui.unlock();
    ui.click("+ Insert");
    ui.show("Insert a document");
    ui.focus_insert_editor();
    ui.type_over("{ _id: 10, name: \"delta\", qty: 4 }");
    ui.click("Insert");
    ui.waits_for_rows(4);
    let mut want = seed();
    want.push(doc! { "_id": 10, "name": "delta", "qty": 4 });
    assert_eq!(f.all(), want);
}

#[test]
fn undo_an_edit() {
    let Some(f) = Fixture::new("undo_edit", seed()) else { return };
    let mut ui = open(&f);
    ui.unlock();
    ui.mongo_edit_cell("name", 1, "changed");
    ui.save();
    assert_eq!(f.all()[0].get_str("name"), Ok("changed"));
    ui.wait("the undo entry", |a| a.undo.len() == 1);
    ui.undo_latest();
    f.expect(&mut ui, seed());
}

#[test]
fn undo_a_delete() {
    let Some(f) = Fixture::new("undo_del", seed()) else { return };
    let mut ui = open(&f);
    ui.unlock();
    ui.check_row(2);
    ui.delete_checked();
    ui.waits_for_rows(2);
    ui.wait("the undo entry", |a| a.undo.len() == 1);
    ui.undo_latest();
    ui.waits_for_rows(3);
    f.expect(&mut ui, seed());
}

#[test]
fn undo_deleting_several_documents() {
    let Some(f) = Fixture::new("undo_del_many", seed()) else { return };
    let mut ui = open(&f);
    ui.unlock();
    ui.check_row(1);
    ui.check_row(2);
    ui.delete_checked();
    ui.waits_for_rows(1);
    ui.wait("the undo entry", |a| a.undo.len() == 1);
    ui.undo_latest();
    ui.waits_for_rows(3);
    f.expect(&mut ui, seed());
}

#[test]
fn undo_an_insert() {
    let Some(f) = Fixture::new("undo_ins", seed()) else { return };
    let mut ui = open(&f);
    ui.unlock();
    ui.click("+ Insert");
    ui.show("Insert a document");
    ui.focus_insert_editor();
    ui.type_over("{ _id: 10, name: \"delta\" }");
    ui.click("Insert");
    ui.waits_for_rows(4);
    ui.wait("the undo entry", |a| a.undo.len() == 1);
    ui.undo_latest();
    ui.waits_for_rows(3);
    f.expect(&mut ui, seed());
}

#[test]
fn locked_connection_asks_before_editing() {
    let Some(f) = Fixture::new("locked_edit", seed()) else { return };
    let mut ui = open(&f);
    ui.double_click_cell("name", 1);
    ui.show("Stay read-only");
    assert!(ui.app().inline.is_none());
    ui.click("Stay read-only");
    assert!(ui.app().modal.is_none() && ui.app().inline.is_none() && ui.app().drafts.is_empty());
    assert!(!ui.app().conn(CONN).unwrap().writable);
    assert_eq!(f.all(), seed());
}

#[test]
fn locked_connection_asks_before_adding_a_field() {
    let Some(f) = Fixture::new("locked_field", seed()) else { return };
    let mut ui = open(&f);
    // A single click opens the document under its row; its actions are writes.
    ui.click("name row 1");
    ui.click("+ Add field");
    ui.show("Stay read-only");
    ui.click("Stay read-only");
    assert!(ui.app().new_field.is_none() && ui.app().drafts.is_empty());
    ui.click("Edit JSON");
    ui.show("Stay read-only");
    ui.click("Stay read-only");
    assert!(ui.app().json_edit.is_none() && ui.app().drafts.is_empty());
    assert!(!ui.app().conn(CONN).unwrap().writable);
    assert_eq!(f.all(), seed());
}

#[test]
fn locked_connection_asks_before_deleting() {
    let Some(f) = Fixture::new("locked_del", seed()) else { return };
    let mut ui = open(&f);
    ui.check_row(2);
    ui.click("Delete…");
    ui.show("Stay read-only");
    assert!(!matches!(ui.app().modal, Some(Modal::Delete(..))));
    ui.click("Stay read-only");
    assert!(ui.app().modal.is_none() && !ui.app().conn(CONN).unwrap().writable);
    assert_eq!(f.all(), seed());
}

#[test]
fn locked_connection_asks_before_inserting() {
    let Some(f) = Fixture::new("locked_ins", seed()) else { return };
    let mut ui = open(&f);
    ui.click("+ Insert");
    ui.show("Stay read-only");
    assert!(ui.app().insert.is_none());
    ui.click("Stay read-only");
    assert!(ui.app().modal.is_none() && ui.app().insert.is_none() && !ui.app().conn(CONN).unwrap().writable);
    // Allowing writes carries on to the insert dialog the click asked for.
    ui.click("+ Insert");
    ui.click("Allow writes");
    ui.show("Insert a document");
    assert!(ui.app().conn(CONN).unwrap().writable);
    assert_eq!(f.all(), seed());
}

#[test]
fn expand_button_moves_the_inline_edit_to_the_dialog() {
    let Some(f) = Fixture::new("expand", seed()) else { return };
    let mut ui = open(&f);
    ui.unlock();
    ui.double_click_cell("name", 2);
    ui.wait("the inline editor", |a| a.inline.is_some());
    ui.click("Open in editor");
    ui.wait("the dialog", |a| a.inline.is_none() && a.json_edit.as_ref().is_some_and(|j| j.kind == "string" && j.text == "beta"));
    ui.type_over("two\nlines");
    ui.click("Apply");
    ui.wait("the edit to be staged", |a| a.json_edit.is_none() && !a.drafts.is_empty());
    ui.save();
    f.expect(&mut ui, vec![doc! { "_id": 1, "name": "alpha", "qty": 1 }, doc! { "_id": 2, "name": "two\nlines", "qty": 2 }, doc! { "_id": 3, "name": "gamma", "qty": 3 }]);
}

#[test]
fn set_null_in_the_dialog_stores_null() {
    let Some(f) = Fixture::new("setnull", seed()) else { return };
    let mut ui = open(&f);
    ui.unlock();
    ui.double_click_cell("name", 1);
    ui.wait("the inline editor", |a| a.inline.is_some());
    ui.h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Enter);
    ui.wait("the dialog", |a| a.json_edit.is_some());
    ui.click("Set null");
    ui.wait("the edit to be staged", |a| a.json_edit.is_none() && !a.drafts.is_empty());
    ui.save();
    f.expect(&mut ui, vec![doc! { "_id": 1, "name": null, "qty": 1 }, doc! { "_id": 2, "name": "beta", "qty": 2 }, doc! { "_id": 3, "name": "gamma", "qty": 3 }]);
}

#[test]
fn a_long_string_opens_the_side_panel_directly() {
    let long = "x".repeat(600);
    let Some(f) = Fixture::new("panel", vec![doc! { "_id": 1, "note": &long }]) else { return };
    let mut ui = open(&f);
    ui.unlock();
    ui.h.state_mut().prefs.value_panel = true;
    ui.double_click_cell("note", 1);
    ui.wait("the panel", |a| a.inline.is_none() && a.value_in_panel());
    assert!(!ui.app().window_open(), "the panel blocks the page like a dialog");
    assert_eq!(ui.app().json_edit.as_ref().unwrap().text, long);
}
