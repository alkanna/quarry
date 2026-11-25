//! State behind src/app/sql/create.rs, which draws it.

use crate::state::sql::new_keys::NewKeys;
use crate::db::sql::create::{ColumnSpec, DropInfo, Meta, TableSpec};
use crate::db::sql::ddl::{IdxCol, IndexDef, IndexKind};
use crate::db::sql::structure::{CheckConstraint, ForeignKey};
use crate::db;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DropKind {
    Database,
    Table,
    View,
    /// Delete every row and keep the table.
    Truncate,
    /// A PostgreSQL schema.
    Schema,
}

pub struct NewDb {
    pub conn: u64,
    pub name: String,
    pub charset: String,
    pub collation: String,
    /// None while the server's lists load.
    pub meta: Option<Meta>,
    pub load_err: Option<String>,
}

pub struct NewTable {
    pub conn: u64,
    pub schema: String,
    pub name: String,
    pub engine: String,
    pub collation: String,
    pub cols: Vec<ColumnSpec>,
    /// The UNIQUE keys, CHECKs, foreign keys and indexes, collected the way the PostgreSQL dialog does.
    pub keys: NewKeys,
    pub comment: String,
    pub meta: Option<Meta>,
    pub load_err: Option<String>,
    pub show_sql: bool,
    /// The statement of the form as it was last built, with the spec it came from, made again only when the spec changed. None
    /// inside until the server's lists arrived. The key check is not kept: it reads the parents as they are and is cheap.
    pub built: Option<(TableSpec, Option<db::Res<String>>)>,
}

impl NewTable {
    /// What the form says, with the keys written the way the statement takes them. A key that points at the table itself follows
    /// the name typed.
    pub fn spec(&self) -> TableSpec {
        let cols = |v: &[String]| v.iter().map(|n| IdxCol { name: n.clone(), prefix: None, desc: false }).collect::<Vec<_>>();
        let uniques = self.keys.uniques.iter().map(|u| IndexDef { name: u.name.clone(), kind: IndexKind::Unique, cols: cols(&u.cols), ..Default::default() });
        let indexes = self.keys.indexes.iter().map(|x| IndexDef { name: x.name.clone(), kind: x.kind, cols: cols(&x.cols), ..Default::default() });
        let checks = self.keys.checks.iter().map(|c| CheckConstraint { name: c.name.clone(), expr: c.expr.trim().to_string(), ..Default::default() }).collect();
        let fks = self.keys.fk_keys(&self.schema, &self.name).into_iter().map(|key| ForeignKey { db: self.schema.clone(), ..key }).collect();
        TableSpec { schema: self.schema.clone(), name: self.name.clone(), engine: self.engine.clone(), collation: self.collation.clone(), columns: self.cols.clone(), fks, checks, indexes: uniques.chain(indexes).collect(), comment: self.comment.clone() }
    }

}

pub struct DropForm {
    pub conn: u64,
    /// PostgreSQL: the database the schema or table is in, or the database itself for a database drop. Empty on MySQL.
    pub database: String,
    pub schema: String,
    /// Empty for a database.
    pub table: String,
    pub kind: DropKind,
    pub typed: String,
    /// PostgreSQL: CASCADE for a schema or table, WITH (FORCE) for a database. Ticked inside the dialog, never by default.
    pub opt: bool,
    /// None while the counts load.
    pub info: Option<Result<DropInfo, String>>,
}
