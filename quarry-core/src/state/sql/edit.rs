//! State behind src/app/sql/edit.rs, which draws it.

use crate::db::sql::Col;
use crate::db;
use mongodb::bson::{Bson, Document};

/// A cell being edited in place. `done` is set by the editor: Some(true) to commit, Some(false) to cancel.
#[derive(Default, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CellEdit {
    /// The row's key, because a reload while the editor is open moves rows around.
    pub id: Option<Bson>,
    pub col: String,
    pub text: String,
    pub err: Option<String>,
    pub focus: bool,
    pub done: Option<bool>,
    /// Shown in the large editor instead of over the cell.
    pub big: bool,
    /// Edited since it opened, so the side panel must not drop it for another value.
    pub dirty: bool,
    /// The large editor's live check of a JSON column: None until the text is checked again after an edit.
    pub json_err: Option<Option<String>>,
    /// The calendar is open, holding the text from before it opened for Cancel.
    pub pick: Option<String>,
    /// The row of the value list the arrows moved to, None until they are used.
    pub hl: Option<usize>,
    /// What the helpers under the cell covered last frame, so a click in them is not a click outside the editor.
    /// As min x, min y, max x, max y.
    pub pop: Option<[f32; 4]>,
    /// Give the text box focus back without selecting its text, after a helper button took it.
    pub refocus: bool,
    /// A helper reads plain keys (arrows, Space, letters) with no text box focused, so the page must not.
    pub grabs_keys: bool,
    pub keys: Option<KeyLook>,
}

/// Rows of the table a foreign key column points at, looked up as the key is typed.
#[derive(Default, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct KeyLook {
    /// The single-column foreign key of the edited column, found once the table structure is loaded.
    pub fk: Option<db::sql::structure::ForeignKey>,
    pub req: u64,
    /// The text the last lookup was for, and whether its answer is still on its way.
    pub asked: Option<String>,
    pub busy: bool,
    /// What the first answer learned about the referenced table, sent with the next lookups.
    pub table: Option<db::sql::KeyTable>,
    pub rows: Vec<(String, String)>,
}

impl CellEdit {
    /// Whether this editor belongs to the cell. A missing key never matches: two keyless rows are not the same row.
    pub fn is_at(&self, id: Option<Bson>, col: &str) -> bool {
        self.id.is_some() && self.id == id && self.col == col
    }
}

/// The "Set value on N rows" dialog.
pub struct SetForm {
    pub rows: Vec<Document>,
    pub col: String,
    pub text: String,
    pub null: bool,
    pub err: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Default,
    Null,
    Value,
}

pub struct RowField {
    pub col: Col,
    pub mode: Mode,
    pub text: String,
}

/// The "+ Row" dialog: one line per column.
pub struct RowForm {
    pub title: String,
    pub fields: Vec<RowField>,
    pub err: Option<String>,
}

use crate::db::sql::value::{self, Kind};

pub fn is_numeric(k: Kind) -> bool {
    matches!(k, Kind::Int | Kind::UInt | Kind::Float | Kind::Decimal)
}

/// Text for the editor of a `kind` column: what the cell shows, as typed back it parses to the same value.
pub fn edit_text(b: &Bson, kind: Kind) -> String {
    match b {
        Bson::Null => "NULL".into(),
        Bson::String(s) if value::text_like(kind) && value::null_like(s) => format!("'{s}'"),
        Bson::String(s) if kind == Kind::Json && s.trim() == "null" => "'null'".into(),
        Bson::String(s) => s.clone(),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Double(f) => f.to_string(),
        Bson::Binary(x) => format!("0x{}", x.bytes.iter().map(|b| format!("{b:02X}")).collect::<String>()),
        other => db::short(other),
    }
}
