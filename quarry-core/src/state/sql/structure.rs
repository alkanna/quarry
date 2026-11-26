//! State behind src/app/sql/structure.rs, which draws it.

use crate::db::sql::ddl::Change;
use crate::db::sql::structure::Structure;
use crate::db;
use std::collections::HashMap;

/// What the four views keep per tab.
#[derive(Default)]
pub struct StructState {
    pub load: Load,
    pub data: Option<Structure>,
    /// Column and index changes waiting to become one ALTER TABLE.
    pub staged: Vec<Change>,
    /// The ALTER or trigger statement is running.
    pub applying: bool,
    /// Trigger edits not saved yet, by trigger name; the new trigger's is under the empty name.
    pub trig: HashMap<String, db::sql::ddl::TrigDef>,
    /// The editor shows the new trigger instead of the selected one.
    pub trig_new: bool,
    /// The name a trigger that is being made will have, so its draft is cleared when the structure lists it.
    pub trig_made: Option<String>,
    /// The draft key of a trigger edit on its way, dropped once the server took it: the draft is the live trigger then,
    /// and left behind it would show an Undo or someone else's later change as a pending edit of ours.
    pub trig_saving: Option<String>,
}

#[derive(Default)]
pub enum Load {
    #[default]
    Idle,
    Loading,
    Failed(String),
}

impl StructState {
    pub fn finish(&mut self, res: db::Res<Structure>) {
        match res {
            Ok(s) => (self.data, self.load) = (Some(s), Load::Idle),
            Err(e) => self.load = Load::Failed(e),
        }
    }
}
