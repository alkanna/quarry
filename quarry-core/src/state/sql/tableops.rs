//! State behind src/app/sql/tableops.rs, which draws it.

use crate::state::sql::create::DropKind;
use crate::db::sql::create::DropInfo;
use crate::db::sql::pg::tableops as pgops;
use crate::db::sql::tableops as myops;
use crate::db;

/// Rename a table, view or (PostgreSQL) schema. `database` is the PostgreSQL database and empty on MySQL, where `schema` is the database.
pub struct RenameForm {
    pub conn: u64,
    pub database: String,
    pub schema: String,
    /// Table, View or Schema.
    pub kind: DropKind,
    pub from: String,
    pub to: String,
    pub focused: bool,
    /// The statement shown, kept while the new name stays as it is.
    pub preview: Option<(String, Result<String, String>)>,
}

pub struct DupForm {
    pub conn: u64,
    pub database: String,
    pub schema: String,
    pub from: String,
    pub to: String,
    pub data: bool,
    pub focused: bool,
    /// The statements shown, kept while the new name and the choice of rows stay as they are.
    pub preview: Option<((String, bool), Result<String, String>)>,
}

/// Empty a PostgreSQL table, with the two options TRUNCATE has.
pub struct TruncForm {
    pub conn: u64,
    pub database: String,
    pub schema: String,
    pub table: String,
    pub typed: String,
    pub restart: bool,
    pub cascade: bool,
    /// None while the counts load.
    pub info: Option<Result<DropInfo, String>>,
}

impl RenameForm {
    /// The statement for the current name, built again only when the name changes.
    pub fn preview(&mut self, pg: bool, d: db::sql::Dialect) -> Result<String, String> {
        let (key, built) = self.preview.take().filter(|p| p.0 == self.to).unwrap_or_else(|| {
            let built = match (pg, self.kind) {
                (true, DropKind::Schema) => pgops::rename_schema_sql(&self.from, &self.to),
                (true, k) => pgops::rename_sql(if k == DropKind::View { "VIEW" } else { "TABLE" }, &self.schema, &self.from, &self.to),
                (false, _) => myops::rename_table_sql(d, &self.schema, &self.from, &self.to, false),
            };
            (self.to.clone(), built.map(|sql| format!("{sql};")))
        });
        self.preview.insert((key, built)).1.clone()
    }
}

impl DupForm {
    /// Both statements for the current name and choice, built again only when one of them changes.
    pub fn preview(&mut self, pg: bool, d: db::sql::Dialect) -> Result<String, String> {
        let key = (self.to.clone(), self.data);
        let (key, built) = self.preview.take().filter(|p| p.0 == key).unwrap_or_else(|| {
            let built = if pg { pgops::duplicate_preview(&self.schema, &self.from, &self.to, self.data) } else { myops::duplicate_preview(d, &self.schema, &self.from, &self.to, self.data, false) };
            (key, built.map(|v| v.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n")))
        });
        self.preview.insert((key, built)).1.clone()
    }
}
