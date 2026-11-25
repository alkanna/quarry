//! State behind src/app/mongo_create.rs, which draws it.

use crate::db::collopts::CollForm;

pub struct MongoNewColl {
    pub conn: u64,
    pub db: String,
    /// Asks for the database name too; the database does not exist yet.
    pub new_db: bool,
    pub name: String,
    pub form: CollForm,
}
