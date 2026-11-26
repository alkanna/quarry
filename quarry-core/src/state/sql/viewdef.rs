//! State behind src/app/sql/viewdef.rs, which draws it.

use crate::db::sql::view::{ViewDef, ViewSpec};

pub struct ViewForm {
    pub conn: u64,
    /// PostgreSQL: the database the schema is in. Empty on MySQL, where `schema` is the database.
    pub database: String,
    pub schema: String,
    pub name: String,
    pub body: String,
    /// An edit of a view that exists; a new view otherwise.
    pub edit: bool,
    /// The definition is being read.
    pub loading: bool,
    /// Why the definition could not be read, or why the save was refused.
    pub error: Option<String>,
    /// The definition as the server holds it, which an edit keeps what the dialog has no field for from.
    pub def: ViewDef,
    pub algorithm: String,
    pub security: String,
    pub check: String,
    pub keep_definer: bool,
    pub focused: bool,
    /// A save is out, so the buttons wait for its answer.
    pub busy: bool,
    /// PostgreSQL refused CREATE OR REPLACE because the columns changed, and the person is asked whether to drop and create instead.
    pub ask_drop: bool,
}

impl ViewForm {
    pub fn new(conn: u64, database: String, schema: String, name: String, body: String, edit: bool) -> Self {
        ViewForm { conn, database, schema, name, body, edit, loading: edit, error: None, def: ViewDef::default(), algorithm: "UNDEFINED".into(), security: "DEFINER".into(), check: "NONE".into(), keep_definer: true, focused: false, busy: false, ask_drop: false }
    }

    pub fn spec(&self, pg: bool, drop_first: bool) -> ViewSpec {
        // An edit keeps what the dialog has no field for from the definition the server holds.
        let old = self.edit.then_some(&self.def);
        let mut def = ViewDef { schema: self.schema.clone(), name: self.name.clone(), body: self.body.clone(), materialized: old.is_some_and(|d| d.materialized), options: old.map(|d| d.options.clone()).unwrap_or_default(), definer: old.and_then(|d| d.definer.clone()), ..Default::default() };
        if !pg {
            (def.algorithm, def.security, def.check) = (self.algorithm.clone(), self.security.clone(), self.check.clone());
            if !self.keep_definer {
                def.definer = None;
            }
        }
        ViewSpec { def, replace: self.edit, drop_first }
    }
}
