//! State behind src/ui/complete.rs, which draws it.

use crate::db;
use mongodb::bson::Bson;
use std::collections::HashMap;

/// What the loaded documents hold in one field: its most common kind and its most common values.
#[derive(Default, Clone)]
pub struct FieldInfo {
    pub kind: &'static str,
    pub values: Vec<Bson>,
    /// SQL only: how many rows hold each of `values`, in the same order.
    pub freq: Vec<u64>,
}

pub type Fields = HashMap<String, FieldInfo>;

/// One autocomplete suggestion: what is shown, a short note, what is inserted and where the
/// caret lands inside it (in characters).
pub struct Sug {
    /// SQL only: what the row shows at the right instead of the kind, and the footer line while it is selected.
    pub sql: Option<SqlInfo>,
    /// Characters before the typed word that the insert also replaces, like the dot before `["my-field"]`.
    pub back: usize,
    pub text: String,
    pub note: &'static str,
    pub insert: String,
    pub caret: usize,
}

pub struct SqlInfo {
    pub detail: String,
    pub foot: String,
}

/// Open suggestion list of one query input.
#[derive(Default)]
pub struct Ac {
    /// The input it belongs to, as its egui id value.
    pub id: Option<u64>,
    /// Length in characters of the word being completed, just before `cursor`.
    pub token: usize,
    pub items: Vec<Sug>,
    pub sel: usize,
    pub cursor: usize,
    /// Where the list was drawn last frame, as min x, min y, max x, max y.
    pub rect: Option<[f32; 4]>,
    /// Signature and docs of the item at this index, shown beside the list.
    pub doc: Option<(usize, db::JsDoc)>,
    /// The field's values are being fetched from the server; the list stays open and says so.
    pub loading: bool,
    /// Why the server was not asked for values matching the typed text, shown under the list.
    pub note: Option<&'static str>,
    /// SQL only: the heading row above the list and the footer when the selected row has none.
    pub title: String,
    pub foot: String,
    /// SQL only: the "table.column" whose values the list is waiting for.
    pub want: String,
}

impl Ac {
    pub fn close(&mut self) {
        self.items.clear();
        self.loading = false;
        self.note = None;
    }
}
