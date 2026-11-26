//! State behind src/app/shell_attach.rs, which draws it.

use crate::db;
use crate::state::complete::Fields;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// A connection attached to a shell tab, saved with it. `name` is the global the script uses.
#[derive(Serialize, Deserialize, Clone, PartialEq, Default, Debug)]
#[serde(default)]
pub struct Attached {
    pub conn: u64,
    pub db: String,
    pub name: String,
    /// Writes also need the connection itself to be unlocked, so this alone never allows one.
    pub allow_writes: bool,
    /// PostgreSQL only: the schema the script runs in (its search_path). `db` is then the database.
    pub schema: String,
    /// PostgreSQL only: the tables of `schema`, listed when attaching and before a run. Not saved, since they go stale.
    #[serde(skip)]
    pub tables: Vec<String>,
}

/// The state of the Attach dialog.
#[derive(Default)]
pub struct AttachForm {
    pub filter: String,
    pub conn: Option<u64>,
    pub db: String,
    /// PostgreSQL only: the schema to attach, and the schemas of `db`: None while listing, an error when that failed.
    pub schema: String,
    pub schemas: Option<Result<Vec<String>, String>>,
    /// The (connection, database) whose schemas were asked for, so each is asked once.
    pub asked: Option<(u64, String)>,
    pub name: String,
    /// The name was typed, so a new database no longer renames it.
    pub name_set: bool,
    pub writes: bool,
    pub focused: bool,
}

/// What the language server was told about an attachment, kept to tell when it changed. Holding the
/// Arcs keeps each pointer unique, so pointer equality means unchanged.
pub enum AttTyped {
    Mongo(Vec<(String, Arc<Fields>)>),
    Sql(Option<Arc<db::sql::schema::Schema>>),
}

impl AttTyped {
    pub fn same(&self, o: &AttTyped) -> bool {
        match (self, o) {
            (AttTyped::Mongo(a), AttTyped::Mongo(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.0 == y.0 && Arc::ptr_eq(&x.1, &y.1)),
            (AttTyped::Sql(Some(a)), AttTyped::Sql(Some(b))) => Arc::ptr_eq(a, b),
            (AttTyped::Sql(None), AttTyped::Sql(None)) => true,
            _ => false,
        }
    }
}

/// The schema a PostgreSQL script starts in when none was picked, as a new database has it.
pub const PG_SCHEMA: &str = "public";
