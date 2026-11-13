//! Reading a SQL value in full: the dialog that opens it, which needs no write access, and the menu of a value that is
//! only read.

use crate::app::sql::edit::edit_text;
use crate::app::*;
use crate::ui::json_tree;
use db::sql::value::Kind;

/// The text a copy puts on the clipboard: the value as the cell editor shows it, without the quotes it adds around a
/// text null, and nothing for NULL.
pub(in crate::app) fn plain_text(v: &Bson) -> String {
    match v {
        Bson::Null => String::new(),
        Bson::String(s) => s.clone(),
        other => edit_text(other, Kind::Text),
    }
}

/// Copy value and View value… for a value that is only read. Returns whether View was picked.
pub(in crate::app) fn read_menu(ui: &mut egui::Ui, v: Option<Val>) -> bool {
    if ui.button("Copy value").clicked() {
        ui.ctx().copy_text(v.map(|v| plain_text(&v.bson())).unwrap_or_default());
        ui.close();
    }
    let view = ui.button("View value…").clicked();
    if view {
        ui.close();
    }
    view
}

impl App {
    /// The whole value of a SQL cell in a dialog, which needs no write access. JSON is laid out on several lines, its
    /// numbers and strings kept as stored.
    pub(in crate::app) fn sql_view_value(&mut self, col: &str, v: &Bson) {
        let body = match v {
            Bson::Null => "NULL".to_string(),
            Bson::String(s) if json_tree::looks_like(s) => json_tree::pretty(s),
            other => plain_text(other),
        };
        let code = json_tree::looks_like(&body);
        self.modal = Some(Modal::Text { title: col.to_string(), body, code });
    }
}
