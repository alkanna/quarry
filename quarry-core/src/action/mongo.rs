//! The MongoDB views around the Documents view: Compare, Schema with its validator, Import and the Documents menu's
//! Export, and the conveniences of a document's field menu (group, distinct, go to a reference, index, rename).
//! src/app/compare.rs, schema.rs, import.rs and tree.rs draw them.

mod compare;
mod fields;
mod schema;
mod transfer;

pub use compare::CmpA;
pub use fields::{col_title, FieldA};
pub use schema::SchemaA;
pub use transfer::ImportA;

use super::{Refusal, Reply};
use crate::core::Core;
use crate::state::Danger;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum MongoA {
    Compare(CmpA),
    Schema(SchemaA),
    Import(ImportA),
    Field(FieldA),
}

impl Core {
    pub(super) fn mongo(&mut self, a: MongoA) -> Result<Reply, Refusal> {
        match a {
            MongoA::Compare(a) => self.compare(a),
            MongoA::Schema(a) => self.schema(a),
            MongoA::Import(a) => self.import(a),
            MongoA::Field(a) => self.field(a),
        }
    }

    /// Ties a confirmation to the current tab, as the app's `pinned` does, so a tab switch before Confirm cannot redirect it.
    fn mongo_pinned(&self, d: Danger) -> Danger {
        match self.t() {
            Some(t) => Danger::At(t.sel.clone(), Box::new(d)),
            None => d,
        }
    }

    /// A refusal that the drawing already prevents (a disabled button), so it says nothing.
    fn mongo_quiet() -> Refusal {
        Refusal { kind: super::RefusalKind::Invalid, msg: String::new() }
    }
}
