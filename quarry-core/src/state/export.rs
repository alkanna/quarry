//! State behind src/app/export.rs, which draws it.

use crate::db::copy::Fmt;
use std::sync::atomic::{AtomicBool, AtomicU64};
use crate::db;
use mongodb::bson::Document;
use std::sync::Arc;

/// A format of the dialog: the table formats Copy as shares, or Mongo's own text forms.
#[derive(Clone, Copy, PartialEq)]
pub enum Format {
    T(Fmt),
    /// An Excel workbook: a typed, banded table with a frozen header.
    Xlsx,
    MongoJs,
    MongoJson,
    MongoLines,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::T(Fmt::SqlInsert) => "SQL INSERT",
            Format::T(Fmt::SqlUpdate) => "SQL UPDATE",
            Format::T(Fmt::Csv) => "CSV",
            Format::T(Fmt::Tsv) => "TSV",
            Format::T(Fmt::Json) | Format::MongoJson => "JSON",
            Format::T(Fmt::JsonLines) | Format::MongoLines => "JSON lines",
            Format::T(Fmt::Markdown) => "Markdown table",
            Format::T(Fmt::Html) => "HTML table",
            Format::T(Fmt::Values) => "Values only",
            Format::MongoJs => "JS",
            Format::Xlsx => "Excel workbook (xlsx)",
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Format::T(Fmt::SqlInsert | Fmt::SqlUpdate) => "sql",
            Format::T(Fmt::Csv) => "csv",
            Format::T(Fmt::Tsv) => "tsv",
            Format::T(Fmt::Json) | Format::MongoJson => "json",
            Format::T(Fmt::JsonLines) | Format::MongoLines => "jsonl",
            Format::T(Fmt::Markdown) => "md",
            Format::T(Fmt::Html) => "html",
            Format::T(Fmt::Values) => "txt",
            Format::MongoJs => "js",
            Format::Xlsx => "xlsx",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Scope {
    Selected,
    Shown,
    All,
    /// Every statement of the Query editor, run in order on one connection.
    Script,
}

/// Which grid the rows come from.
#[derive(Clone, Copy, PartialEq)]
pub enum Src {
    Query,
    Shell,
    Content,
}

/// What it takes to run the statement of a Query result again for every row.
pub struct AllRows {
    pub db: db::sql::Db,
    pub schema: String,
    pub stmt: String,
    pub total: Option<u64>,
    pub paged: bool,
    /// Why this statement may not be streamed, when it may not.
    pub refused: Option<String>,
}

/// The Query editor's whole text as an export source.
pub struct Script {
    pub db: db::sql::Db,
    pub schema: String,
    /// The statements, or why the script may not be exported.
    pub stmts: Result<Vec<String>, String>,
}

pub struct ExportForm {
    pub src: Src,
    pub sql: bool,
    pub fmt: Format,
    pub header: bool,
    pub scope: Scope,
    pub selected: Vec<Document>,
    pub shown: usize,
    pub all: Option<AllRows>,
    pub script: Option<Script>,
    /// For the script: one file instead of one per result.
    pub single: bool,
    /// The result is one page of a longer one.
    pub paged: bool,
    /// An xlsx file gets an About sheet with where its rows came from.
    pub about: bool,
    /// The table SQL INSERT and UPDATE write to.
    pub table: String,
}

/// A Query result streaming to a file or the clipboard, with what Cancel needs.
pub struct ExportJob {
    pub req: u64,
    pub count: Arc<AtomicU64>,
    pub cancel: Arc<AtomicBool>,
    pub id: Arc<AtomicU64>,
    /// The connection a SQL stream reads, None for Shell rows written from memory.
    pub db: Option<db::sql::Db>,
    pub copy: bool,
    /// Progress of a script: the statement being run and how many there are. `of` is 0 for a single statement.
    pub step: Arc<AtomicU64>,
    pub of: usize,
}
