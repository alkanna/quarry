//! State behind src/app/sql/create_pg.rs, which draws it.

use crate::state::sql::new_keys::NewKeys;
use crate::db::sql::pg::create::{self, DbChoices, DbOpts, TableSpec, Unique};
use crate::db::sql::ddl::{self, ColDef};
use crate::db;

pub struct PgNewDb {
    pub conn: u64,
    pub name: String,
    /// The template is None for the server's own default, template1, and every other None is the template's own setting.
    pub opts: DbOpts,
    /// None while the server's lists load.
    pub choices: Option<DbChoices>,
    pub load_err: Option<String>,
    pub focused: bool,
}

pub struct PgNewSchema {
    pub conn: u64,
    pub database: String,
    pub name: String,
    pub focused: bool,
}

/// One column of the builder: the structure editor's definition and the two marks the table level needs.
#[derive(Clone, PartialEq)]
pub struct PgCol {
    pub def: ColDef,
    pub pk: bool,
    pub unique: bool,
}

pub struct PgNewTable {
    pub conn: u64,
    pub database: String,
    pub schema: String,
    pub name: String,
    pub cols: Vec<PgCol>,
    /// The UNIQUE constraints over several columns (one over a single column is also its column's Unique box), the CHECKs, the
    /// foreign keys and the indexes.
    pub keys: NewKeys,
    pub comment: String,
    pub show_sql: bool,
    pub focused: bool,
    /// The statements of the form as it was last built, with the spec they came from.
    pub built: Option<(TableSpec, db::Res<Vec<String>>)>,
}

impl PgNewTable {
    pub fn new(conn: u64, database: String, schema: String) -> Self {
        PgNewTable { conn, database, schema, name: String::new(), cols: vec![id_col()], keys: NewKeys::default(), comment: String::new(), show_sql: false, focused: false, built: None }
    }

    /// Builds the statements again only when the spec changed since the last frame.
    pub fn refresh_built(&mut self) {
        let spec = self.spec();
        if self.built.as_ref().is_some_and(|(k, _)| *k == spec) {
            return;
        }
        let stmts = create::create_table_stmts(&spec);
        self.built = Some((spec, stmts));
    }

    pub fn spec(&self) -> TableSpec {
        let cols: Vec<ColDef> = self.cols.iter().map(|c| ColDef { nullable: c.def.nullable && !c.pk && !c.def.auto_increment, ..c.def.clone() }).collect();
        let marked = self.cols.iter().filter(|c| c.unique).map(|c| Unique { name: String::new(), cols: vec![c.def.name.clone()] });
        let mut fks = self.keys.fk_keys(&self.schema, &self.name);
        for (key, k) in fks.iter_mut().zip(&self.keys.fks) {
            // The parent as the dialog saw it, so the transaction can refuse when it changed before the table was made.
            key.ref_seen = k.parent.as_ref().filter(|p| !k.own && p.pg_schema.is_some()).map(|p| ddl::ref_state(p, &key.ref_cols)).unwrap_or_default();
        }
        TableSpec { schema: self.schema.clone(), name: self.name.clone(), cols, pk: self.cols.iter().filter(|c| c.pk).map(|c| c.def.name.clone()).collect(), uniques: marked.chain(self.keys.uniques.iter().cloned()).collect(), checks: self.keys.checks.clone(), fks, indexes: self.keys.indexes.clone(), comment: self.comment.clone() }
    }

    /// The first thing stopping a key that the statements cannot see: a part not picked, a parent not read yet or one that does not
    /// fit. The second value is true while a parent is still loading, which is not a mistake to point at.
    pub fn key_problem(&self, spec: &TableSpec) -> Option<(String, bool)> {
        self.keys.key_problem(&spec.fks, &spec.as_structure(), &spec.name, false, None)
    }

    /// A column was renamed from `from`. Its picks follow, unless another column still has that name and the picks may be its.
    pub fn col_renamed(&mut self, from: &str, to: &str) {
        self.keys.renamed(self.cols.iter().map(|c| c.def.name.as_str()), from, to);
    }

    pub fn remove_col(&mut self, i: usize) {
        let gone = self.cols.remove(i).def.name;
        self.keys.removed(self.cols.iter().map(|c| c.def.name.as_str()), &gone);
    }
}

/// The first column of a new table: an integer identity primary key, which is what most tables want.
pub fn id_col() -> PgCol {
    PgCol { def: ColDef { auto_increment: true, nullable: false, ..ColDef::new("id", "integer") }, pk: true, unique: false }
}
