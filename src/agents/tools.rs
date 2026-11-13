//! The v1 tools: their schemas as the bridge serves them, their arguments parsed into `Call`, and the class each
//! call needs permission for. Arguments come from an untrusted agent, so unknown keys and odd types are refused
//! here, before any policy check.

use super::config::OpClass;
use crate::db::sql::Dialect;
use crate::db::sql::lex::{Tok, read_only_check, split_statements, tokens};
use mongodb::bson::{Bson, Document};
use serde_json::{Map, Value, json};
use std::sync::LazyLock;

#[derive(Clone, Debug, PartialEq)]
pub struct ArgErr(pub String);

impl std::fmt::Display for ArgErr {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConnRef {
    Id(u64),
    Name(String),
}

/// A collection or table. For PostgreSQL `db` is the schema, as in the rest of Quarry's SQL code.
#[derive(Clone, Debug, PartialEq)]
pub struct Ns {
    pub conn: ConnRef,
    pub db: String,
    pub coll: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    Doc(Document),
    Pipeline(Vec<Document>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CondOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Like,
    In,
    IsNull,
    NotNull,
}

/// One `column op value` test of a structured SQL tool; tests are joined with AND and values are bound, never
/// written into the text.
#[derive(Clone, Debug, PartialEq)]
pub struct Cond {
    pub col: String,
    pub op: CondOp,
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnDef {
    pub name: String,
    pub ty: String,
    pub nullable: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    ListConnections,
    ListDatabases { conn: ConnRef },
    ListCollections { conn: ConnRef, db: String },
    Describe { ns: Ns },
    Find { ns: Ns, filter: Document, projection: Option<Document>, sort: Option<Document>, skip: u64, limit: Option<u32> },
    Aggregate { ns: Ns, pipeline: Vec<Document>, limit: Option<u32> },
    Count { ns: Ns, filter: Document },
    Explain { ns: Ns, filter: Document, sort: Option<Document>, pipeline: Option<Vec<Document>> },
    SelectRows { ns: Ns, columns: Vec<String>, wher: Vec<Cond>, order: Vec<(String, bool)>, limit: Option<u32>, offset: u64 },
    QuerySql { conn: ConnRef, db: Option<String>, sql: String, limit: Option<u32> },
    Insert { ns: Ns, docs: Vec<Document> },
    Update { ns: Ns, filter: Document, update: Update, many: bool },
    Delete { ns: Ns, filter: Document, many: bool },
    AggregateInto { ns: Ns, pipeline: Vec<Document> },
    SqlInsertRows { ns: Ns, rows: Vec<Map<String, Value>> },
    SqlUpdateRows { ns: Ns, set: Map<String, Value>, wher: Vec<Cond> },
    SqlDeleteRows { ns: Ns, wher: Vec<Cond> },
    CreateCollection { ns: Ns },
    DropCollection { ns: Ns },
    CreateIndex { ns: Ns, keys: Document, name: Option<String>, unique: bool },
    DropIndex { ns: Ns, name: String },
    CreateTable { ns: Ns, columns: Vec<ColumnDef>, primary_key: Vec<String> },
    DropTable { ns: Ns },
    RunCommand { conn: ConnRef, db: String, command: Document },
    RunSql { conn: ConnRef, db: Option<String>, sql: String },
    RunShell { conn: ConnRef, db: Option<String>, script: String },
}

/// A namespace a call reads or writes. `coll` is None for calls on a whole database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub db: String,
    pub coll: Option<String>,
}

pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub read_only: bool,
    pub destructive: bool,
}

impl ToolDef {
    /// The MCP `tools/list` entry.
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema,
            "annotations": { "readOnlyHint": self.read_only, "destructiveHint": self.destructive, "openWorldHint": false },
        })
    }
}

pub const REASON_MAX: usize = 300;
const MAX_DOCS: usize = 1000;

fn props(extra: Value, required: &[&str]) -> Value {
    let mut p = json!({
        "reason": { "type": "string", "maxLength": REASON_MAX, "description": "Why you are making this call, shown to the user when Quarry asks for approval." },
    });
    p.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
    json!({ "type": "object", "properties": p, "required": required, "additionalProperties": false })
}

fn conn_p() -> Value {
    json!({ "type": ["string", "integer"], "description": "Connection name or id, from list_connections." })
}

fn ns_props(coll: &str) -> Value {
    let what = if coll == "table" { "Table name" } else { "Collection name" };
    let db = if coll == "table" { "Database (MySQL) or schema (PostgreSQL)." } else { "Database name." };
    json!({ "connection": conn_p(), "database": { "type": "string", "description": db }, coll: { "type": "string", "description": what } })
}

fn with(mut base: Value, extra: Value) -> Value {
    base.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
    base
}

fn query_t(what: &str) -> Value {
    json!({ "type": ["object", "string"], "description": format!("{what} as MongoDB relaxed Extended JSON.") })
}

fn limit_p() -> Value {
    json!({ "type": "integer", "minimum": 1, "description": "Most rows to return; Quarry also caps rows and bytes." })
}

fn where_p() -> Value {
    json!({ "type": "array", "description": "Tests joined with AND. Quarry quotes every value itself.", "items": {
        "type": "object", "additionalProperties": false, "required": ["column"],
        "properties": {
            "column": { "type": "string" },
            "op": { "type": "string", "enum": ["=", "!=", "<", "<=", ">", ">=", "like", "in", "is null", "is not null"], "default": "=" },
            "value": {},
        },
    } })
}

fn sql_p() -> Value {
    json!({ "connection": conn_p(), "database": { "type": "string", "description": "Database (MySQL) or schema (PostgreSQL) to run in." }, "sql": { "type": "string" } })
}

static TOOLS: LazyLock<Vec<ToolDef>> = LazyLock::new(build_tools);

pub fn tool_list() -> &'static [ToolDef] {
    &TOOLS
}

pub fn tool(name: &str) -> Option<&'static ToolDef> {
    TOOLS.iter().find(|t| t.name == name)
}

fn build_tools() -> Vec<ToolDef> {
    let t = |name, description, input_schema, read_only, destructive| ToolDef { name, description, input_schema, read_only, destructive };
    let coll = |extra: Value, req: &[&str]| props(with(ns_props("collection"), extra), &[&["connection", "database", "collection"][..], req].concat());
    let table = |extra: Value, req: &[&str]| props(with(ns_props("table"), extra), &[&["connection", "database", "table"][..], req].concat());
    vec![
        t("list_connections", "Connections the user shared with agents, with engine and access. Never includes passwords or URIs.", props(json!({}), &[]), true, false),
        t("list_databases", "Databases on a connection (schemas on PostgreSQL).", props(json!({ "connection": conn_p() }), &["connection"]), true, false),
        t("list_collections", "Collections or tables in a database.", props(json!({ "connection": conn_p(), "database": { "type": "string" } }), &["connection", "database"]), true, false),
        t("describe", "Fields from a sample (MongoDB) or columns (SQL), indexes and counts of one collection or table.", props(with(ns_props("collection"), json!({ "table": { "type": "string" } })), &["connection", "database"]), true, false),
        t("find", "Find documents in a MongoDB collection.", coll(json!({ "filter": query_t("Query filter"), "projection": query_t("Projection"), "sort": query_t("Sort"), "skip": { "type": "integer", "minimum": 0 }, "limit": limit_p() }), &[]), true, false),
        t("aggregate", "Run a read-only aggregation pipeline. $out and $merge are refused; use aggregate_into.", coll(json!({ "pipeline": { "type": ["array", "string"], "description": "Pipeline stages as relaxed Extended JSON." }, "limit": limit_p() }), &["pipeline"]), true, false),
        t("count", "Count documents matching a filter.", coll(json!({ "filter": query_t("Query filter") }), &[]), true, false),
        t("explain", "Explain plan of a find (filter, sort) or of a pipeline.", coll(json!({ "filter": query_t("Query filter"), "sort": query_t("Sort"), "pipeline": { "type": ["array", "string"] } }), &[]), true, false),
        t("select_rows", "Read rows of a SQL table without writing SQL.", table(json!({ "columns": { "type": "array", "items": { "type": "string" }, "description": "Columns to return; all when left out." }, "where": where_p(), "order": { "type": "array", "items": { "type": "object", "required": ["column"], "additionalProperties": false, "properties": { "column": { "type": "string" }, "desc": { "type": "boolean" } } } }, "limit": limit_p(), "offset": { "type": "integer", "minimum": 0 } }), &[]), true, false),
        t("query_sql", "Run read-only SQL (SELECT, SHOW, EXPLAIN without ANALYZE). Refused on connections whose rules hide data.", props(with(sql_p(), json!({ "limit": limit_p() })), &["connection", "sql"]), true, false),
        t("insert", "Insert documents into a MongoDB collection.", coll(json!({ "documents": { "type": "array", "items": { "type": "object" }, "maxItems": MAX_DOCS } }), &["documents"]), false, false),
        t("update", "Update documents with update operators or an update pipeline.", coll(json!({ "filter": query_t("Query filter"), "update": { "type": ["object", "array"], "description": "Update operators like {\"$set\": {...}} or a pipeline." }, "many": { "type": "boolean", "default": false } }), &["filter", "update"]), false, true),
        t("delete", "Delete documents matching a filter.", coll(json!({ "filter": query_t("Query filter"), "many": { "type": "boolean", "default": false } }), &["filter"]), false, true),
        t("aggregate_into", "Run a pipeline that ends in $out or $merge.", coll(json!({ "pipeline": { "type": ["array", "string"] } }), &["pipeline"]), false, true),
        t("sql_insert_rows", "Insert rows into a SQL table. Values are bound as parameters.", table(json!({ "rows": { "type": "array", "items": { "type": "object" }, "maxItems": MAX_DOCS } }), &["rows"]), false, false),
        t("sql_update_rows", "Set columns on the rows of a SQL table that match where.", table(json!({ "set": { "type": "object" }, "where": where_p() }), &["set", "where"]), false, true),
        t("sql_delete_rows", "Delete the rows of a SQL table that match where.", table(json!({ "where": where_p() }), &["where"]), false, true),
        t("create_collection", "Create a MongoDB collection.", coll(json!({}), &[]), false, false),
        t("drop_collection", "Drop a MongoDB collection and all its documents. Cannot be undone.", coll(json!({}), &[]), false, true),
        t("create_index", "Create an index on a MongoDB collection or a SQL table (pass `table`). keys: {field: 1} ascending, -1 descending.", props(with(ns_props("collection"), json!({ "table": { "type": "string" }, "keys": query_t("Index keys"), "name": { "type": "string" }, "unique": { "type": "boolean" } })), &["connection", "database", "keys"]), false, false),
        t("drop_index", "Drop an index by name, from a MongoDB collection or a SQL table (pass `table`).", props(with(ns_props("collection"), json!({ "table": { "type": "string" }, "name": { "type": "string" } })), &["connection", "database", "name"]), false, true),
        t("create_table", "Create a SQL table.", table(json!({ "columns": { "type": "array", "items": { "type": "object", "required": ["name", "type"], "additionalProperties": false, "properties": { "name": { "type": "string" }, "type": { "type": "string" }, "nullable": { "type": "boolean" } } } }, "primary_key": { "type": "array", "items": { "type": "string" } } }), &["columns"]), false, false),
        t("drop_table", "Drop a SQL table and all its rows. Cannot be undone.", table(json!({}), &[]), false, true),
        t("run_command", "Run a MongoDB database command.", props(json!({ "connection": conn_p(), "database": { "type": "string" }, "command": query_t("Command document") }), &["connection", "database", "command"]), false, true),
        t("run_sql", "Run SQL that may change data or structure. Cannot be undone.", props(sql_p(), &["connection", "sql"]), false, true),
        t("run_shell", SHELL_DESCRIPTION, props(json!({ "connection": conn_p(), "database": { "type": "string", "description": "The database `db` stands for." }, "script": { "type": "string", "description": "A JavaScript function expression, such as async function run() { ... }." } }), &["connection", "script"]), false, true),
    ]
}

const SHELL_DESCRIPTION: &str = "Run JavaScript on a MongoDB connection in Quarry's shell (Node.js MongoDB driver). `script` is a function expression that Quarry calls, such as async function run() { return await db.collection(\"users\").find().limit(5).toArray() }. Its return value and its console.log output come back. `db` is the current database (the `database` argument) and each of its collections is also a global by name. The user approves every script, it is refused where rules hide data, and on a locked connection allowing it unlocks writes.";

/// The shell runs `return (<script>)` and calls what that gives, so a bare statement fails only after the user approved
/// it. This cheap check refuses that before any approval, with the form that works.
fn shell_fn(script: &str) -> Result<String, ArgErr> {
    let t = script.trim();
    let after = |s: &str, w: &str| s.strip_prefix(w).is_some_and(|r| !r.starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '$'));
    let head = if after(t, "async") { t["async".len()..].trim_start() } else { t };
    let opens = after(head, "function") || head.starts_with('(');
    if !opens || !(t.contains("=>") || t.contains("function")) {
        return bad("`script` must be a JavaScript function that Quarry calls, with `db` as the current database, such as: async function run() { return await db.collection(\"users\").find().limit(5).toArray() }. Its return value and console.log output come back.");
    }
    Ok(script.to_string())
}

/// The agent's stated reason, untrusted text shown under Quarry's own facts: control characters become spaces and
/// it is cut to 300 characters.
pub fn reason(args: &Value) -> Option<String> {
    let r = args.get("reason")?.as_str()?;
    let r: String = r.chars().map(|c| if c.is_control() { ' ' } else { c }).take(REASON_MAX).collect();
    let r = r.trim();
    (!r.is_empty()).then(|| r.to_string())
}

struct Args<'a> {
    m: &'a Map<String, Value>,
}

fn bad<T>(msg: impl Into<String>) -> Result<T, ArgErr> {
    Err(ArgErr(msg.into()))
}

impl Args<'_> {
    fn has(&self, k: &str) -> bool {
        self.m.get(k).is_some_and(|v| !v.is_null())
    }

    fn opt_str(&self, k: &str) -> Result<Option<String>, ArgErr> {
        match self.m.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => bad(format!("`{k}` must be a string")),
        }
    }

    fn str(&self, k: &str) -> Result<String, ArgErr> {
        self.opt_str(k)?.filter(|s| !s.is_empty()).map_or_else(|| bad(format!("`{k}` is required")), Ok)
    }

    /// Database, collection, table and column names: the executors quote them, but NUL and absurd lengths never
    /// reach a server.
    fn name(&self, k: &str) -> Result<String, ArgErr> {
        let s = self.str(k)?;
        ident(&s, k)?;
        Ok(s)
    }

    fn opt_name(&self, k: &str) -> Result<Option<String>, ArgErr> {
        let s = self.opt_str(k)?.filter(|s| !s.is_empty());
        if let Some(s) = &s {
            ident(s, k)?;
        }
        Ok(s)
    }

    fn conn(&self) -> Result<ConnRef, ArgErr> {
        match self.m.get("connection") {
            Some(Value::Number(n)) => n.as_u64().map(ConnRef::Id).map_or_else(|| bad("`connection` must be a name or a positive id"), Ok),
            Some(Value::String(s)) if !s.trim().is_empty() => Ok(ConnRef::Name(s.trim().to_string())),
            _ => bad("`connection` is required: a name or id from list_connections"),
        }
    }

    fn ns(&self, coll: &str) -> Result<Ns, ArgErr> {
        Ok(Ns { conn: self.conn()?, db: self.name("database")?, coll: self.name(coll)? })
    }

    fn bool(&self, k: &str) -> Result<bool, ArgErr> {
        match self.m.get(k) {
            None | Some(Value::Null) => Ok(false),
            Some(Value::Bool(b)) => Ok(*b),
            Some(_) => bad(format!("`{k}` must be true or false")),
        }
    }

    fn uint(&self, k: &str) -> Result<Option<u64>, ArgErr> {
        match self.m.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v.as_u64().map(Some).map_or_else(|| bad(format!("`{k}` must be a whole number")), Ok),
        }
    }

    fn limit(&self) -> Result<Option<u32>, ArgErr> {
        match self.uint("limit")? {
            Some(0) => bad("`limit` must be at least 1"),
            n => Ok(n.map(|n| n.min(u32::MAX as u64) as u32)),
        }
    }

    fn doc(&self, k: &str) -> Result<Option<Document>, ArgErr> {
        match self.m.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => crate::db::parse_doc(s).map(Some).map_err(|e| ArgErr(format!("`{k}`: {e}"))),
            Some(v @ Value::Object(_)) => match Bson::try_from(v.clone()) {
                Ok(Bson::Document(d)) => Ok(Some(d)),
                Ok(_) => bad(format!("`{k}` must be an object")),
                Err(e) => bad(format!("`{k}` is not valid Extended JSON: {e}")),
            },
            Some(_) => bad(format!("`{k}` must be an object")),
        }
    }

    fn pipeline(&self, k: &str) -> Result<Option<Vec<Document>>, ArgErr> {
        match self.m.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => crate::db::parse_pipeline(s).map(Some).map_err(|e| ArgErr(format!("`{k}`: {e}"))),
            Some(Value::Array(a)) => a.iter().map(|s| match Bson::try_from(s.clone()) {
                Ok(Bson::Document(d)) => Ok(d),
                _ => bad(format!("every stage of `{k}` must be an object")),
            }).collect::<Result<_, _>>().map(Some),
            Some(_) => bad(format!("`{k}` must be an array of stages")),
        }
    }

    fn rows(&self, k: &str) -> Result<Vec<Map<String, Value>>, ArgErr> {
        let Some(Value::Array(a)) = self.m.get(k) else { return bad(format!("`{k}` must be an array of objects")) };
        if a.is_empty() || a.len() > MAX_DOCS {
            return bad(format!("`{k}` must hold 1 to {MAX_DOCS} items"));
        }
        a.iter().map(|r| match r {
            Value::Object(m) => Ok(m.clone()),
            _ => bad(format!("every item of `{k}` must be an object")),
        }).collect()
    }

    fn wher(&self) -> Result<Vec<Cond>, ArgErr> {
        let Some(v) = self.m.get("where").filter(|v| !v.is_null()) else { return Ok(Vec::new()) };
        let Value::Array(a) = v else { return bad("`where` must be an array of {column, op, value}") };
        a.iter().map(|c| {
            let Value::Object(m) = c else { return bad("every `where` item must be an object") };
            if let Some(k) = m.keys().find(|k| !matches!(k.as_str(), "column" | "op" | "value")) {
                return bad(format!("unknown key `{k}` in `where`"));
            }
            let a = Args { m };
            let col = a.name("column")?;
            let op = match a.opt_str("op")?.as_deref().map(str::to_ascii_lowercase).as_deref() {
                None | Some("=") => CondOp::Eq,
                Some("!=") | Some("<>") => CondOp::Ne,
                Some("<") => CondOp::Lt,
                Some("<=") => CondOp::Le,
                Some(">") => CondOp::Gt,
                Some(">=") => CondOp::Ge,
                Some("like") => CondOp::Like,
                Some("in") => CondOp::In,
                Some("is null") => CondOp::IsNull,
                Some("is not null") => CondOp::NotNull,
                Some(o) => return bad(format!("unknown operator `{o}` in `where`")),
            };
            let value = m.get("value").cloned().unwrap_or(Value::Null);
            match (op, &value) {
                (CondOp::IsNull | CondOp::NotNull, _) => {}
                (CondOp::In, Value::Array(a)) if !a.is_empty() && a.iter().all(scalar) => {}
                (CondOp::In, _) => return bad(format!("`in` on `{col}` needs a non-empty array of values")),
                (_, v) if scalar(v) && !v.is_null() => {}
                _ => return bad(format!("`{col}` needs a string, number or boolean value")),
            }
            Ok(Cond { col, op, value })
        }).collect()
    }
}

fn scalar(v: &Value) -> bool {
    !matches!(v, Value::Array(_) | Value::Object(_))
}

fn ident(s: &str, k: &str) -> Result<(), ArgErr> {
    if s.contains('\0') || s.chars().count() > 128 {
        return bad(format!("`{k}` is not a valid name"));
    }
    Ok(())
}

/// Words that may follow a type name, as in `int unsigned`, `double precision` or `timestamp with time zone`.
const TYPE_WORDS: [&str; 8] = ["unsigned", "zerofill", "varying", "precision", "with", "without", "time", "zone"];

/// Column types are written into DDL as they are, so only a strict grammar passes: a type name, then only the words
/// above, and at most one `(n)` or `(n, m)` of digits: `varchar(255)`, `numeric(10, 2)`, `int unsigned`,
/// `character varying(20)`, `timestamp(3) with time zone`. Any other punctuation or word could carry a second clause,
/// like `int) SELECT password FROM users WHERE (1`, which MySQL reads as CREATE TABLE ... SELECT.
pub fn col_type(s: &str) -> Result<String, ArgErr> {
    let refuse = || bad(format!("column type `{s}` is not allowed: write a type name, an optional (n) or (n, m), and only words such as unsigned, varying, precision or with time zone"));
    let t = s.trim();
    if t.is_empty() || t.len() > 64 {
        return refuse();
    }
    let (mut words, mut size, mut rest) = (0, false, t);
    while !rest.is_empty() {
        rest = rest.trim_start_matches(' ');
        if let Some(r) = rest.strip_prefix('(') {
            let Some((inner, after)) = r.split_once(')') else { return refuse() };
            let parts: Vec<&str> = inner.split(',').map(|p| p.trim_matches(' ')).collect();
            if words == 0 || size || parts.len() > 2 || parts.iter().any(|p| p.is_empty() || p.len() > 9 || !p.bytes().all(|b| b.is_ascii_digit())) {
                return refuse();
            }
            size = true;
            rest = after;
            continue;
        }
        let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(rest.len());
        let w = &rest[..end];
        let first_ok = words == 0 && w.starts_with(|c: char| c.is_ascii_alphabetic());
        if w.is_empty() || !(first_ok || (words > 0 && TYPE_WORDS.iter().any(|k| k.eq_ignore_ascii_case(w)))) {
            return refuse();
        }
        words += 1;
        rest = &rest[end..];
    }
    Ok(t.to_string())
}

fn no_empty_where(w: &[Cond], tool: &str) -> Result<(), ArgErr> {
    if w.is_empty() {
        return bad(format!("{tool} needs a `where`; use run_sql to change every row"));
    }
    Ok(())
}

pub fn parse(tool: &str, args: &Value) -> Result<Call, ArgErr> {
    let Some(def) = self::tool(tool) else { return bad(format!("Unknown tool `{tool}`")) };
    let empty = Map::new();
    let m = match args {
        Value::Object(m) => m,
        Value::Null => &empty,
        _ => return bad("arguments must be an object"),
    };
    let known = def.input_schema["properties"].as_object().expect("schema has properties");
    if let Some(k) = m.keys().find(|k| !known.contains_key(*k)) {
        return bad(format!("Unknown argument `{k}` for {tool}"));
    }
    if m.get("reason").is_some_and(|r| !r.is_string() && !r.is_null()) {
        return bad("`reason` must be a string");
    }
    let a = Args { m };
    let filter = || a.doc("filter").map(Option::unwrap_or_default);
    Ok(match tool {
        "list_connections" => Call::ListConnections,
        "list_databases" => Call::ListDatabases { conn: a.conn()? },
        "list_collections" => Call::ListCollections { conn: a.conn()?, db: a.name("database")? },
        "describe" => {
            let coll = if a.has("table") { a.name("table")? } else { a.name("collection")? };
            Call::Describe { ns: Ns { conn: a.conn()?, db: a.name("database")?, coll } }
        }
        "find" => Call::Find { ns: a.ns("collection")?, filter: filter()?, projection: a.doc("projection")?, sort: a.doc("sort")?, skip: a.uint("skip")?.unwrap_or(0), limit: a.limit()? },
        "aggregate" => Call::Aggregate { ns: a.ns("collection")?, pipeline: a.pipeline("pipeline")?.unwrap_or_default(), limit: a.limit()? },
        "count" => Call::Count { ns: a.ns("collection")?, filter: filter()? },
        "explain" => Call::Explain { ns: a.ns("collection")?, filter: filter()?, sort: a.doc("sort")?, pipeline: a.pipeline("pipeline")? },
        "select_rows" => {
            let columns = match m.get("columns") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(c)) => c.iter().map(|c| c.as_str().filter(|s| !s.is_empty() && ident(s, "columns").is_ok()).map(String::from).map_or_else(|| bad("`columns` must be names"), Ok)).collect::<Result<_, _>>()?,
                Some(_) => return bad("`columns` must be an array of names"),
            };
            let order = match m.get("order") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(o)) => o.iter().map(|o| {
                    let Value::Object(om) = o else { return bad("every `order` item must be {column, desc}") };
                    let oa = Args { m: om };
                    Ok((oa.name("column")?, oa.bool("desc")?))
                }).collect::<Result<_, _>>()?,
                Some(_) => return bad("`order` must be an array"),
            };
            Call::SelectRows { ns: a.ns("table")?, columns, wher: a.wher()?, order, limit: a.limit()?, offset: a.uint("offset")?.unwrap_or(0) }
        }
        "query_sql" => Call::QuerySql { conn: a.conn()?, db: a.opt_name("database")?, sql: a.str("sql")?, limit: a.limit()? },
        "insert" => {
            let docs = a.rows("documents")?.into_iter().map(|d| match Bson::try_from(Value::Object(d)) {
                Ok(Bson::Document(d)) => Ok(d),
                _ => bad("every document must be valid Extended JSON"),
            }).collect::<Result<_, _>>()?;
            Call::Insert { ns: a.ns("collection")?, docs }
        }
        "update" => {
            if !a.has("filter") {
                return bad("`filter` is required; pass {} to match every document");
            }
            let update = match m.get("update") {
                Some(Value::Array(_)) => Update::Pipeline(a.pipeline("update")?.unwrap_or_default()),
                _ => {
                    let d = a.doc("update")?.map_or_else(|| bad("`update` is required"), Ok)?;
                    if d.is_empty() || d.keys().any(|k| !k.starts_with('$')) {
                        return bad("`update` must use update operators such as $set; replacing whole documents is not supported");
                    }
                    Update::Doc(d)
                }
            };
            Call::Update { ns: a.ns("collection")?, filter: filter()?, update, many: a.bool("many")? }
        }
        "delete" => {
            if !a.has("filter") {
                return bad("`filter` is required; pass {} to match every document");
            }
            Call::Delete { ns: a.ns("collection")?, filter: filter()?, many: a.bool("many")? }
        }
        "aggregate_into" => {
            let pipeline = a.pipeline("pipeline")?.unwrap_or_default();
            if !pipeline.last().is_some_and(|s| s.contains_key("$out") || s.contains_key("$merge")) {
                return bad("aggregate_into needs a pipeline that ends in $out or $merge");
            }
            Call::AggregateInto { ns: a.ns("collection")?, pipeline }
        }
        "sql_insert_rows" => {
            let rows = a.rows("rows")?;
            for r in &rows {
                if r.is_empty() || !r.values().all(scalar) {
                    return bad("every row must be a non-empty object of plain values");
                }
                for k in r.keys() {
                    ident(k, "rows")?;
                }
            }
            Call::SqlInsertRows { ns: a.ns("table")?, rows }
        }
        "sql_update_rows" => {
            let Some(Value::Object(set)) = m.get("set") else { return bad("`set` must be an object of column values") };
            if set.is_empty() || !set.values().all(scalar) {
                return bad("`set` must be a non-empty object of plain values");
            }
            for k in set.keys() {
                ident(k, "set")?;
            }
            let wher = a.wher()?;
            no_empty_where(&wher, tool)?;
            Call::SqlUpdateRows { ns: a.ns("table")?, set: set.clone(), wher }
        }
        "sql_delete_rows" => {
            let wher = a.wher()?;
            no_empty_where(&wher, tool)?;
            Call::SqlDeleteRows { ns: a.ns("table")?, wher }
        }
        "create_collection" => Call::CreateCollection { ns: a.ns("collection")? },
        "drop_collection" => Call::DropCollection { ns: a.ns("collection")? },
        "create_index" => {
            let keys = a.doc("keys")?.filter(|k| !k.is_empty()).map_or_else(|| bad("`keys` must name at least one field"), Ok)?;
            let coll = if a.has("table") { a.name("table")? } else { a.name("collection")? };
            Call::CreateIndex { ns: Ns { conn: a.conn()?, db: a.name("database")?, coll }, keys, name: a.opt_name("name")?, unique: a.bool("unique")? }
        }
        "drop_index" => {
            let coll = if a.has("table") { a.name("table")? } else { a.name("collection")? };
            Call::DropIndex { ns: Ns { conn: a.conn()?, db: a.name("database")?, coll }, name: a.name("name")? }
        }
        "create_table" => {
            let Some(Value::Array(cols)) = m.get("columns") else { return bad("`columns` must be an array") };
            if cols.is_empty() {
                return bad("`columns` must hold at least one column");
            }
            let columns = cols.iter().map(|c| {
                let Value::Object(cm) = c else { return bad("every column must be {name, type}") };
                let ca = Args { m: cm };
                Ok(ColumnDef { name: ca.name("name")?, ty: col_type(&ca.str("type")?)?, nullable: cm.get("nullable").and_then(Value::as_bool).unwrap_or(true) })
            }).collect::<Result<Vec<_>, _>>()?;
            let primary_key = match m.get("primary_key") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(k)) => k.iter().map(|k| k.as_str().filter(|k| columns.iter().any(|c| c.name == *k)).map(String::from).map_or_else(|| bad("`primary_key` must name columns of the table"), Ok)).collect::<Result<_, _>>()?,
                Some(_) => return bad("`primary_key` must be an array of column names"),
            };
            Call::CreateTable { ns: a.ns("table")?, columns, primary_key }
        }
        "drop_table" => Call::DropTable { ns: a.ns("table")? },
        "run_command" => {
            let command = a.doc("command")?.filter(|c| !c.is_empty()).map_or_else(|| bad("`command` must be a command document"), Ok)?;
            Call::RunCommand { conn: a.conn()?, db: a.name("database")?, command }
        }
        "run_sql" => Call::RunSql { conn: a.conn()?, db: a.opt_name("database")?, sql: a.str("sql")? },
        "run_shell" => Call::RunShell { conn: a.conn()?, db: a.opt_name("database")?, script: shell_fn(&a.str("script")?)? },
        _ => unreachable!("every listed tool is parsed"),
    })
}

/// MongoDB commands that read or change users and roles: the Server and users class, which has no tools yet.
const USER_COMMANDS: [&str; 16] = ["createUser", "updateUser", "dropUser", "dropAllUsersFromDatabase", "grantRolesToUser", "revokeRolesFromUser", "createRole", "updateRole", "dropRole", "dropAllRolesFromDatabase", "grantPrivilegesToRole", "revokePrivilegesFromRole", "grantRolesToRole", "revokeRolesFromRole", "usersInfo", "rolesInfo"];

/// The class a call needs. `dialect` is the connection's SQL engine, None for MongoDB; free SQL that the read-only
/// lexer does not pass is never Read data, whatever tool it came through.
pub fn classify(c: &Call, dialect: Option<Dialect>) -> OpClass {
    match c {
        Call::ListConnections | Call::ListDatabases { .. } | Call::ListCollections { .. } | Call::Describe { .. } | Call::Find { .. } | Call::Aggregate { .. } | Call::Count { .. } | Call::Explain { .. } | Call::SelectRows { .. } => OpClass::ReadData,
        Call::QuerySql { sql, .. } => match dialect {
            Some(d) if read_only_check(sql, d).is_ok() => OpClass::ReadData,
            _ => OpClass::RunScripts,
        },
        Call::Insert { .. } | Call::Update { .. } | Call::Delete { .. } | Call::AggregateInto { .. } | Call::SqlInsertRows { .. } | Call::SqlUpdateRows { .. } | Call::SqlDeleteRows { .. } => OpClass::ChangeData,
        Call::CreateCollection { .. } | Call::DropCollection { .. } | Call::CreateIndex { .. } | Call::DropIndex { .. } | Call::CreateTable { .. } | Call::DropTable { .. } => OpClass::ChangeStructure,
        Call::RunCommand { command, .. } => command_class(command),
        Call::RunSql { sql, .. } if dialect.is_some_and(|d| touches_users(sql, d)) => OpClass::ServerUsers,
        Call::RunSql { .. } | Call::RunShell { .. } => OpClass::RunScripts,
    }
}

/// MongoDB commands that make, drop, rename or reshape collections and indexes.
const STRUCTURE_COMMANDS: [&str; 8] = ["create", "drop", "createIndexes", "dropIndexes", "collMod", "renameCollection", "dropDatabase", "convertToCapped"];

/// The class of a run_command. A command that changes structure, or a pipeline that writes with $out or $merge (which
/// can make or replace a collection), needs Change structure: it always asks and never takes a session grant, like the
/// structure tools it could stand in for. An explain is classed by the command it wraps.
fn command_class(command: &Document) -> OpClass {
    let Some((name, _)) = command.iter().next() else { return OpClass::RunScripts };
    let is = |list: &[&str]| list.iter().any(|c| c.eq_ignore_ascii_case(name));
    if is(&USER_COMMANDS) {
        return OpClass::ServerUsers;
    }
    if is(&STRUCTURE_COMMANDS) {
        return OpClass::ChangeStructure;
    }
    if name.eq_ignore_ascii_case("aggregate") {
        let stages: Vec<Document> = command.get_array("pipeline").map(|a| a.iter().filter_map(|s| s.as_document().cloned()).collect()).unwrap_or_default();
        let mut writes = false;
        each_stage(&stages, &mut |n, _| writes |= matches!(n, "$out" | "$merge"));
        if writes {
            return OpClass::ChangeStructure;
        }
    }
    if name.eq_ignore_ascii_case("explain")
        && let Ok(inner) = command.get_document(name)
    {
        return command_class(inner);
    }
    OpClass::RunScripts
}

/// GRANT, REVOKE and user or role statements, by their first words.
fn touches_users(sql: &str, d: Dialect) -> bool {
    split_statements(sql, d).into_iter().any(|(_, stmt)| {
        let w: Vec<String> = tokens(stmt, d).into_iter().filter(|(t, _)| *t == Tok::Word).take(2).map(|(_, r)| stmt[r].to_ascii_uppercase()).collect();
        let first = w.first().map(String::as_str).unwrap_or("");
        let second = w.get(1).map(String::as_str).unwrap_or("");
        matches!(first, "GRANT" | "REVOKE") || (matches!(first, "CREATE" | "ALTER" | "DROP" | "RENAME") && matches!(second, "USER" | "ROLE" | "GROUP")) || (first == "SET" && matches!(second, "PASSWORD" | "ROLE"))
    })
}

impl Call {
    pub fn conn(&self) -> Option<&ConnRef> {
        match self {
            Call::ListConnections => None,
            Call::ListDatabases { conn } | Call::ListCollections { conn, .. } | Call::QuerySql { conn, .. } | Call::RunCommand { conn, .. } | Call::RunSql { conn, .. } | Call::RunShell { conn, .. } => Some(conn),
            _ => self.ns().map(|n| &n.conn),
        }
    }

    pub fn ns(&self) -> Option<&Ns> {
        match self {
            Call::Describe { ns } | Call::Find { ns, .. } | Call::Aggregate { ns, .. } | Call::Count { ns, .. } | Call::Explain { ns, .. } | Call::SelectRows { ns, .. } | Call::Insert { ns, .. } | Call::Update { ns, .. } | Call::Delete { ns, .. } | Call::AggregateInto { ns, .. } | Call::SqlInsertRows { ns, .. } | Call::SqlUpdateRows { ns, .. } | Call::SqlDeleteRows { ns, .. } | Call::CreateCollection { ns } | Call::DropCollection { ns } | Call::CreateIndex { ns, .. } | Call::DropIndex { ns, .. } | Call::CreateTable { ns, .. } | Call::DropTable { ns } => Some(ns),
            _ => None,
        }
    }

    /// Whether the call runs text Quarry does not parse into fields (SQL, scripts, commands), so field rules cannot
    /// guard it.
    pub fn is_free_text(&self) -> bool {
        matches!(self, Call::QuerySql { .. } | Call::RunSql { .. } | Call::RunCommand { .. } | Call::RunShell { .. })
    }

    /// Every namespace the call reads or writes: its own, plus pipeline lookups and $out or $merge targets, so the
    /// policy checks scope and system namespaces on all of them.
    pub fn targets(&self) -> Vec<Target> {
        let mut out = Vec::new();
        if let Some(ns) = self.ns() {
            out.push(Target { db: ns.db.clone(), coll: Some(ns.coll.clone()) });
        }
        match self {
            Call::ListCollections { db, .. } => out.push(Target { db: db.clone(), coll: None }),
            Call::QuerySql { db: Some(db), .. } | Call::RunSql { db: Some(db), .. } | Call::RunShell { db: Some(db), .. } => out.push(Target { db: db.clone(), coll: None }),
            Call::RunCommand { db, command, .. } => {
                out.push(Target { db: db.clone(), coll: None });
                if let Some(Bson::String(c)) = command.iter().next().map(|(_, v)| v) {
                    out.push(Target { db: db.clone(), coll: Some(c.clone()) });
                }
            }
            Call::Aggregate { ns, pipeline, .. } | Call::AggregateInto { ns, pipeline } | Call::Explain { ns, pipeline: Some(pipeline), .. } => pipeline_targets(&ns.db, pipeline, &mut out),
            Call::Update { ns, update: Update::Pipeline(p), .. } => pipeline_targets(&ns.db, p, &mut out),
            _ => {}
        }
        out
    }
}

/// Calls `f` with every stage of the pipeline and of the pipelines nested in $facet, $lookup and $unionWith.
pub fn each_stage(p: &[Document], f: &mut dyn FnMut(&str, &Bson)) {
    for stage in p {
        for (name, body) in stage {
            f(name, body);
            let nested: Vec<&Bson> = match (name.as_str(), body) {
                ("$facet", Bson::Document(d)) => d.values().collect(),
                ("$lookup" | "$unionWith", Bson::Document(d)) => d.get("pipeline").into_iter().collect(),
                _ => Vec::new(),
            };
            for n in nested {
                if let Bson::Array(a) = n {
                    let docs: Vec<Document> = a.iter().filter_map(|s| s.as_document().cloned()).collect();
                    each_stage(&docs, f);
                }
            }
        }
    }
}

/// The namespace a $lookup, $graphLookup, $unionWith, $out or $merge stage names, in `db` unless it says otherwise.
pub fn stage_target(db: &str, name: &str, body: &Bson) -> Option<Target> {
    let named = |v: Option<&Bson>| -> Option<Target> {
        match v? {
            Bson::String(c) => Some(Target { db: db.into(), coll: Some(c.clone()) }),
            Bson::Document(d) => Some(Target { db: d.get_str("db").unwrap_or(db).into(), coll: Some(d.get_str("coll").ok()?.into()) }),
            _ => None,
        }
    };
    match (name, body) {
        ("$lookup" | "$graphLookup", Bson::Document(d)) => named(d.get("from")),
        ("$unionWith", Bson::String(_)) | ("$out", _) => named(Some(body)),
        ("$unionWith", Bson::Document(d)) => named(d.get("coll")),
        ("$merge", Bson::Document(d)) => named(d.get("into")),
        ("$merge", Bson::String(_)) => named(Some(body)),
        _ => None,
    }
}

fn pipeline_targets(db: &str, p: &[Document], out: &mut Vec<Target>) {
    each_stage(p, &mut |name, body| {
        if let Some(t) = stage_target(db, name, body)
            && !out.contains(&t)
        {
            out.push(t);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;

    fn p(tool: &str, v: Value) -> Result<Call, ArgErr> {
        parse(tool, &v)
    }

    #[test]
    fn every_tool_has_a_schema_and_parses() {
        let names: Vec<&str> = tool_list().iter().map(|t| t.name).collect();
        assert_eq!(names.len(), 26);
        for t in tool_list() {
            let j = t.to_json();
            assert_eq!(j["annotations"]["openWorldHint"], false);
            assert_eq!(j["inputSchema"]["type"], "object");
            assert!(!(t.read_only && t.destructive), "{}", t.name);
        }
        assert_eq!(p("list_connections", Value::Null), Ok(Call::ListConnections));
        assert!(p("nope", json!({})).is_err());
    }

    #[test]
    fn find_takes_extended_json_objects_or_text() {
        let c = p("find", json!({ "connection": 3, "database": "shop", "collection": "users", "filter": { "_id": { "$oid": "64b7f0c2a1b2c3d4e5f60718" }, "n": { "$gt": 5 } }, "sort": "{ n: -1 }", "limit": 10 })).unwrap();
        let Call::Find { ns, filter, sort, limit, .. } = c else { panic!() };
        assert_eq!(ns.conn, ConnRef::Id(3));
        assert!(matches!(filter.get("_id"), Some(Bson::ObjectId(_))));
        assert_eq!(filter.get_document("n").unwrap(), &doc! { "$gt": 5 });
        assert_eq!(sort, Some(doc! { "n": -1 }));
        assert_eq!(limit, Some(10));
        let c = p("find", json!({ "connection": "prod", "database": "shop", "collection": "users" })).unwrap();
        assert!(matches!(c, Call::Find { ns: Ns { conn: ConnRef::Name(n), .. }, .. } if n == "prod"));
    }

    #[test]
    fn bad_arguments() {
        let base = json!({ "connection": 1, "database": "d", "collection": "c" });
        let with = |k: &str, v: Value| {
            let mut o = base.clone();
            o[k] = v;
            o
        };
        assert!(p("find", with("filer", json!({}))).unwrap_err().0.contains("Unknown argument"));
        assert!(p("find", with("limit", json!(0))).is_err());
        assert!(p("find", with("filter", json!([1]))).is_err());
        assert!(p("find", with("database", json!(""))).is_err());
        assert!(p("find", with("database", json!("a\0b"))).is_err());
        assert!(p("find", with("connection", json!(-1))).is_err());
        assert!(p("find", with("reason", json!(5))).is_err());
        assert!(p("update", with("update", json!({ "name": "x" }))).is_err(), "a replacement is not an update");
        assert!(p("update", with("filter", json!({}))).is_err(), "update needs `update`");
        assert!(p("delete", base.clone()).is_err(), "delete needs an explicit filter");
        assert!(p("aggregate_into", with("pipeline", json!([{ "$match": {} }]))).is_err());
        assert!(p("find", json!("text")).is_err());
    }

    #[test]
    fn updates_and_pipelines() {
        let base = json!({ "connection": 1, "database": "d", "collection": "c", "filter": {} });
        let mut a = base.clone();
        a["update"] = json!({ "$set": { "n": 1 } });
        assert!(matches!(p("update", a).unwrap(), Call::Update { update: Update::Doc(_), many: false, .. }));
        let mut a = base.clone();
        a["update"] = json!([{ "$set": { "n": "$m" } }]);
        a["many"] = json!(true);
        assert!(matches!(p("update", a).unwrap(), Call::Update { update: Update::Pipeline(_), many: true, .. }));
        let c = p("aggregate", json!({ "connection": 1, "database": "d", "collection": "c", "pipeline": "[{ $match: { a: 1 } }]" })).unwrap();
        assert!(matches!(c, Call::Aggregate { pipeline, .. } if pipeline == vec![doc! { "$match": { "a": 1 } }]));
    }

    #[test]
    fn sql_rows() {
        let c = p("select_rows", json!({ "connection": 1, "database": "shop", "table": "orders", "columns": ["id", "total"], "where": [{ "column": "total", "op": ">", "value": 5 }, { "column": "id", "op": "in", "value": [1, 2] }, { "column": "note", "op": "is null" }], "order": [{ "column": "id", "desc": true }] })).unwrap();
        let Call::SelectRows { wher, order, columns, .. } = c else { panic!() };
        assert_eq!(columns, ["id", "total"]);
        assert_eq!(wher.iter().map(|c| c.op).collect::<Vec<_>>(), [CondOp::Gt, CondOp::In, CondOp::IsNull]);
        assert_eq!(order, [("id".to_string(), true)]);
        let t = json!({ "connection": 1, "database": "shop", "table": "orders" });
        let mut a = t.clone();
        a["where"] = json!([{ "column": "id", "op": "between", "value": 1 }]);
        assert!(p("select_rows", a).is_err());
        let mut a = t.clone();
        a["where"] = json!([{ "column": "id", "value": { "a": 1 } }]);
        assert!(p("select_rows", a).is_err());
        let mut a = t.clone();
        a["where"] = json!([]);
        assert!(p("sql_delete_rows", a).is_err(), "deleting every row goes through run_sql");
        let mut a = t.clone();
        a["rows"] = json!([{ "id": 1, "tags": [1] }]);
        assert!(p("sql_insert_rows", a).is_err());
        let mut a = t.clone();
        a["columns"] = json!([{ "name": "id", "type": "int); DROP TABLE x; --" }]);
        assert!(p("create_table", a).is_err());
        let mut a = t.clone();
        a["columns"] = json!([{ "name": "id", "type": "numeric(10, 2)" }]);
        a["primary_key"] = json!(["id"]);
        assert!(p("create_table", a).is_ok());
    }

    #[test]
    fn column_types_follow_a_strict_grammar() {
        for ok in ["int", "INT", "varchar(255)", "varchar (255)", "numeric(10, 2)", "decimal(10,2) unsigned zerofill", "int unsigned", "double precision", "character varying(20)", "timestamp(3) with time zone", "timestamp without time zone", "big_int2"] {
            assert_eq!(col_type(ok).as_deref(), Ok(ok), "{ok}");
        }
        for no in ["int) SELECT password AS v FROM users WHERE (1", "int, secret text", "int; DROP TABLE x", "int -- x", "int /* x */", "text[]", "int default 1", "int primary key", "(5)", "1int", "varchar(a)", "varchar(1, 2, 3)", "varchar()", "varchar(1)(2)", "int\tunsigned", "enum('a')", ""] {
            assert!(col_type(no).is_err(), "{no}");
        }
        let a = json!({ "connection": 1, "database": "shop", "table": "t", "columns": [{ "name": "v", "type": "int) SELECT password AS v FROM users WHERE (1" }] });
        assert!(p("create_table", a).unwrap_err().0.contains("is not allowed"));
    }

    /// The shell calls what the script evaluates to, so anything but a function is refused before an approval, with the
    /// form that works.
    #[test]
    fn run_shell_takes_a_function() {
        let shell = |s: &str| p("run_shell", json!({ "connection": 1, "database": "shop", "script": s }));
        for ok in ["async function run() { return await db.collection('users').find().limit(5).toArray() }", "  function run() { return 1 }", "async () => db.collection('x').countDocuments()", "(async () => { return 1 })", "async(x) => 1", "() => 1"] {
            assert!(shell(ok).is_ok(), "{ok}");
        }
        for no in ["db.users.find().limit(5).toArray()", "return 1", "await db.collection('x').findOne()", "(1)", "asyncfunction run() { }", "functional()", "x => 1", "async x => 1"] {
            let e = shell(no).unwrap_err().0;
            assert!(e.contains("async function run()") && e.contains("`db`"), "{no}: {e}");
        }
        let d = tool("run_shell").unwrap().description;
        assert!(d.contains("function expression") && d.contains("async function run()") && d.contains("`db` is the current database") && d.contains("return value"), "{d}");
    }

    #[test]
    fn reason_is_capped_and_cleaned() {
        assert_eq!(reason(&json!({ "reason": "fix\nthe\u{7}typo " })).as_deref(), Some("fix the typo"));
        assert_eq!(reason(&json!({ "reason": "é".repeat(400) })).unwrap().chars().count(), REASON_MAX);
        assert_eq!(reason(&json!({})), None);
    }

    #[test]
    fn classes() {
        let ns = Ns { conn: ConnRef::Id(1), db: "d".into(), coll: "c".into() };
        let q = |sql: &str| Call::QuerySql { conn: ConnRef::Id(1), db: None, sql: sql.into(), limit: None };
        assert_eq!(classify(&q("SELECT 1"), Some(Dialect::MySql)), OpClass::ReadData);
        assert_eq!(classify(&q("SELECT 1"), Some(Dialect::Postgres)), OpClass::ReadData);
        assert_eq!(classify(&q("SELECT 1; DELETE FROM t"), Some(Dialect::MySql)), OpClass::RunScripts);
        assert_eq!(classify(&q("SELECT * INTO OUTFILE '/tmp/x' FROM t"), Some(Dialect::MySql)), OpClass::RunScripts);
        assert_eq!(classify(&q("SELECT 1"), None), OpClass::RunScripts);
        assert_eq!(classify(&Call::Find { ns: ns.clone(), filter: doc! {}, projection: None, sort: None, skip: 0, limit: None }, None), OpClass::ReadData);
        assert_eq!(classify(&Call::Delete { ns: ns.clone(), filter: doc! {}, many: true }, None), OpClass::ChangeData);
        assert_eq!(classify(&Call::AggregateInto { ns: ns.clone(), pipeline: vec![doc! { "$out": "x" }] }, None), OpClass::ChangeData);
        assert_eq!(classify(&Call::DropTable { ns: ns.clone() }, Some(Dialect::MySql)), OpClass::ChangeStructure);
        assert_eq!(classify(&Call::RunCommand { conn: ConnRef::Id(1), db: "admin".into(), command: doc! { "createUser": "x", "pwd": "y" } }, None), OpClass::ServerUsers);
        let cmd = |c: Document| classify(&Call::RunCommand { conn: ConnRef::Id(1), db: "d".into(), command: c }, None);
        for c in [doc! { "drop": "c" }, doc! { "create": "c" }, doc! { "createIndexes": "c", "indexes": [] }, doc! { "dropIndexes": "c", "index": "*" }, doc! { "collMod": "c" }, doc! { "renameCollection": "d.c", "to": "d.e" }, doc! { "dropDatabase": 1 }, doc! { "DROP": "c" }] {
            assert_eq!(cmd(c.clone()), OpClass::ChangeStructure, "{c}");
        }
        assert_eq!(cmd(doc! { "aggregate": "c", "pipeline": [{ "$match": {} }, { "$out": "e" }], "cursor": {} }), OpClass::ChangeStructure);
        assert_eq!(cmd(doc! { "aggregate": "c", "pipeline": [{ "$facet": { "a": [{ "$merge": { "into": "e" } }] } }], "cursor": {} }), OpClass::ChangeStructure);
        assert_eq!(cmd(doc! { "explain": { "drop": "c" } }), OpClass::ChangeStructure);
        assert_eq!(cmd(doc! { "aggregate": "c", "pipeline": [{ "$match": {} }], "cursor": {} }), OpClass::RunScripts);
        assert_eq!(cmd(doc! { "find": "c" }), OpClass::RunScripts);
        assert_eq!(cmd(doc! { "ping": 1 }), OpClass::RunScripts);
        let run = |sql: &str| Call::RunSql { conn: ConnRef::Id(1), db: None, sql: sql.into() };
        assert_eq!(classify(&run("UPDATE t SET a = 1; GRANT ALL ON *.* TO x"), Some(Dialect::MySql)), OpClass::ServerUsers);
        assert_eq!(classify(&run("alter role app with password 'x'"), Some(Dialect::Postgres)), OpClass::ServerUsers);
        assert_eq!(classify(&run("UPDATE t SET a = 1"), Some(Dialect::MySql)), OpClass::RunScripts);
    }

    #[test]
    fn targets_include_lookups_and_outputs() {
        let ns = Ns { conn: ConnRef::Id(1), db: "shop".into(), coll: "orders".into() };
        let pipeline = vec![
            doc! { "$lookup": { "from": "users", "localField": "u", "foreignField": "_id", "as": "u" } },
            doc! { "$facet": { "a": [{ "$unionWith": { "coll": "archive", "pipeline": [{ "$lookup": { "from": { "db": "hr", "coll": "staff" }, "pipeline": [], "as": "s" } }] } }] } },
            doc! { "$merge": { "into": { "db": "bi", "coll": "out" } } },
        ];
        let t = Call::AggregateInto { ns, pipeline }.targets();
        let names: Vec<String> = t.iter().map(|t| format!("{}.{}", t.db, t.coll.as_deref().unwrap_or(""))).collect();
        assert_eq!(names, ["shop.orders", "shop.users", "shop.archive", "hr.staff", "bi.out"]);
        let t = Call::RunCommand { conn: ConnRef::Id(1), db: "admin".into(), command: doc! { "find": "system.users" } }.targets();
        assert_eq!(t[1], Target { db: "admin".into(), coll: Some("system.users".into()) });
    }
}
