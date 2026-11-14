//! The SQL text that differs between MySQL and MariaDB, plus the keyword and function lists for
//! autocomplete. Queries take values as `?` parameters, never interpolated. Result columns are
//! aliased to lower case names because information_schema reports them upper case or lower case
//! depending on the server version.

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum Dialect {
    #[default]
    MySql,
    MariaDb,
    /// Chosen by the connection's engine, never by a version string.
    Postgres,
}

/// Table maintenance statements of the Info page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Maint {
    Analyze,
    Optimize,
    Check,
    Vacuum,
    Reindex,
}

impl Maint {
    /// Whether the statement rebuilds the table or holds a lock on it for a while, so it asks first.
    pub fn heavy(self) -> bool {
        matches!(self, Maint::Optimize | Maint::Vacuum | Maint::Reindex)
    }

    /// What the confirmation tells about the cost.
    pub fn warning(self) -> &'static str {
        match self {
            Maint::Optimize => "OPTIMIZE TABLE rebuilds the table and locks it while it runs on most storage engines. Large tables can take a long time.",
            Maint::Vacuum => "VACUUM reclaims dead rows and can take a long time on a large table. It runs outside a transaction and cannot be undone.",
            Maint::Reindex => "REINDEX TABLE rebuilds every index of the table and blocks writes to it while it runs.",
            Maint::Analyze | Maint::Check => "",
        }
    }
}

impl Dialect {
    /// The statement for `what` on a table, or None where the engine has no such command.
    pub fn maintenance(self, what: Maint, space: &str, table: &str) -> Option<String> {
        let t = self.qualified(space, table);
        Some(match (self.is_mysql_family(), what) {
            (true, Maint::Analyze) => format!("ANALYZE TABLE {t}"),
            (true, Maint::Optimize) => format!("OPTIMIZE TABLE {t}"),
            (true, Maint::Check) => format!("CHECK TABLE {t}"),
            (false, Maint::Analyze) => format!("ANALYZE {t}"),
            (false, Maint::Vacuum) => format!("VACUUM {t}"),
            (false, Maint::Reindex) => format!("REINDEX TABLE {t}"),
            _ => return None,
        })
    }

    /// Users, server variables, sql_mode and charsets exist only on these two. Anything gated on it
    /// takes the not supported path for PostgreSQL.
    pub fn is_mysql_family(self) -> bool {
        matches!(self, Dialect::MySql | Dialect::MariaDb)
    }

    /// From `SELECT VERSION()`, such as "8.4.0" or "10.11.6-MariaDB".
    pub fn from_version(version: &str) -> Dialect {
        if version.to_ascii_lowercase().contains("mariadb") { Dialect::MariaDb } else { Dialect::MySql }
    }

    pub fn quote_ident(self, name: &str) -> String {
        match self {
            Dialect::MySql | Dialect::MariaDb => format!("`{}`", name.replace('`', "``")),
            Dialect::Postgres => format!("\"{}\"", name.replace('"', "\"\"")),
        }
    }

    pub fn qualified(self, db: &str, table: &str) -> String {
        format!("{}.{}", self.quote_ident(db), self.quote_ident(table))
    }

    /// The paging clause. Both are numbers, so they are written into the text.
    pub fn page(self, limit: u64, offset: u64) -> String {
        format!("LIMIT {limit} OFFSET {offset}")
    }

    /// Tables and views of a database. Params: [schema]. Columns: name, kind ("BASE TABLE", "VIEW"
    /// or "SYSTEM VIEW"), engine, row_estimate, data_bytes, index_bytes, comment. The row estimate is
    /// approximate for InnoDB.
    pub fn tables(self) -> &'static str {
        if self == Dialect::Postgres { return super::pg::catalog::TABLES }
        "SELECT TABLE_NAME AS name, TABLE_TYPE AS kind, ENGINE AS engine, TABLE_ROWS AS row_estimate, DATA_LENGTH AS data_bytes, INDEX_LENGTH AS index_bytes, TABLE_COMMENT AS comment FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME"
    }

    /// Columns of one table. Params: [schema, table]. Columns: name, full_type, nullable (1 or 0),
    /// col_key ("PRI", "UNI", "MUL" or empty), col_default, extra, collation, comment, position.
    pub fn columns(self) -> &'static str {
        if self == Dialect::Postgres { return super::pg::catalog::COLUMNS }
        "SELECT COLUMN_NAME AS name, COLUMN_TYPE AS full_type, IS_NULLABLE = 'YES' AS nullable, COLUMN_KEY AS col_key, COLUMN_DEFAULT AS col_default, EXTRA AS extra, COLLATION_NAME AS collation, COLUMN_COMMENT AS comment, ORDINAL_POSITION AS position FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION"
    }

    /// The CHECK constraints of MariaDB 10.2 and later, for finding the columns declared JSON. Params: [schema] or, with
    /// `by_table`, [schema, table]. Columns: tbl, clause.
    pub fn json_checks(self, by_table: bool) -> &'static str {
        if by_table {
            "SELECT TABLE_NAME AS tbl, CHECK_CLAUSE AS clause FROM information_schema.CHECK_CONSTRAINTS WHERE CONSTRAINT_SCHEMA = ? AND TABLE_NAME = ?"
        } else {
            "SELECT TABLE_NAME AS tbl, CHECK_CLAUSE AS clause FROM information_schema.CHECK_CONSTRAINTS WHERE CONSTRAINT_SCHEMA = ?"
        }
    }

    /// One row per indexed column, the primary key first. Params: [schema, table]. Columns: name,
    /// non_unique (1 or 0), seq, col, direction ("A", "D" or NULL), cardinality, sub_part,
    /// index_type, nullable ("YES" or empty), comment.
    pub fn indexes(self) -> &'static str {
        if self == Dialect::Postgres { return super::pg::catalog::INDEXES }
        "SELECT INDEX_NAME AS name, NON_UNIQUE AS non_unique, SEQ_IN_INDEX AS seq, COLUMN_NAME AS col, COLLATION AS direction, CARDINALITY AS cardinality, SUB_PART AS sub_part, INDEX_TYPE AS index_type, NULLABLE AS nullable, INDEX_COMMENT AS comment FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY INDEX_NAME <> 'PRIMARY', INDEX_NAME, SEQ_IN_INDEX"
    }

    /// Foreign keys defined on a table. Params: [schema, table]. Columns: name, db, tbl, col, pos,
    /// ref_db, ref_tbl, ref_col, on_update, on_delete, where db and tbl are the table that owns the key.
    pub fn foreign_keys(self) -> &'static str {
        if self == Dialect::Postgres { return super::pg::catalog::FOREIGN_KEYS }
        "SELECT k.CONSTRAINT_NAME AS name, k.TABLE_SCHEMA AS db, k.TABLE_NAME AS tbl, k.COLUMN_NAME AS col, k.ORDINAL_POSITION AS pos, k.REFERENCED_TABLE_SCHEMA AS ref_db, k.REFERENCED_TABLE_NAME AS ref_tbl, k.REFERENCED_COLUMN_NAME AS ref_col, r.UPDATE_RULE AS on_update, r.DELETE_RULE AS on_delete FROM information_schema.KEY_COLUMN_USAGE k JOIN information_schema.REFERENTIAL_CONSTRAINTS r ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME AND r.TABLE_NAME = k.TABLE_NAME WHERE k.TABLE_SCHEMA = ? AND k.TABLE_NAME = ? AND k.REFERENCED_TABLE_NAME IS NOT NULL ORDER BY k.CONSTRAINT_NAME, k.ORDINAL_POSITION"
    }

    /// Foreign keys in other tables that point to this one. Params: [schema, table] of the
    /// referenced table. Same columns as `foreign_keys`.
    pub fn referenced_by(self) -> &'static str {
        if self == Dialect::Postgres { return super::pg::catalog::REFERENCED_BY }
        "SELECT k.CONSTRAINT_NAME AS name, k.TABLE_SCHEMA AS db, k.TABLE_NAME AS tbl, k.COLUMN_NAME AS col, k.ORDINAL_POSITION AS pos, k.REFERENCED_TABLE_SCHEMA AS ref_db, k.REFERENCED_TABLE_NAME AS ref_tbl, k.REFERENCED_COLUMN_NAME AS ref_col, r.UPDATE_RULE AS on_update, r.DELETE_RULE AS on_delete FROM information_schema.KEY_COLUMN_USAGE k JOIN information_schema.REFERENTIAL_CONSTRAINTS r ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME AND r.TABLE_NAME = k.TABLE_NAME WHERE k.REFERENCED_TABLE_SCHEMA = ? AND k.REFERENCED_TABLE_NAME = ? ORDER BY k.TABLE_SCHEMA, k.TABLE_NAME, k.CONSTRAINT_NAME, k.ORDINAL_POSITION"
    }

    /// Triggers on a table. Params: [schema, table]. Columns: name, timing ("BEFORE" or "AFTER"),
    /// event ("INSERT", "UPDATE" or "DELETE"), body, definer, ord (ACTION_ORDER), sql_mode.
    pub fn triggers(self) -> &'static str {
        if self == Dialect::Postgres { return super::pg::catalog::TRIGGERS }
        "SELECT TRIGGER_NAME AS name, ACTION_TIMING AS timing, EVENT_MANIPULATION AS event, ACTION_STATEMENT AS body, DEFINER AS definer, ACTION_ORDER AS ord, SQL_MODE AS sql_mode FROM information_schema.TRIGGERS WHERE EVENT_OBJECT_SCHEMA = ? AND EVENT_OBJECT_TABLE = ? ORDER BY ACTION_TIMING, EVENT_MANIPULATION, ACTION_ORDER"
    }

    /// What SHOW TABLE STATUS shows, from information_schema because SHOW cannot bind the schema.
    /// Params: [schema, table]. Columns: name, engine, row_format, row_estimate, avg_row_bytes,
    /// data_bytes, index_bytes, free_bytes, auto_increment, created, updated, collation, options,
    /// comment.
    pub fn table_status(self) -> &'static str {
        if self == Dialect::Postgres { return super::pg::catalog::TABLE_STATUS }
        "SELECT TABLE_NAME AS name, ENGINE AS engine, ROW_FORMAT AS row_format, TABLE_ROWS AS row_estimate, AVG_ROW_LENGTH AS avg_row_bytes, DATA_LENGTH AS data_bytes, INDEX_LENGTH AS index_bytes, DATA_FREE AS free_bytes, AUTO_INCREMENT AS auto_increment, CAST(CREATE_TIME AS CHAR) AS created, CAST(UPDATE_TIME AS CHAR) AS updated, TABLE_COLLATION AS collation, CREATE_OPTIONS AS options, TABLE_COMMENT AS comment FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?"
    }

    /// Every column of a database in one round trip, for autocomplete. Params: [schema]. Columns:
    /// tbl, name, full_type, col_key, nullable (1 or 0).
    pub fn all_columns(self) -> &'static str {
        if self == Dialect::Postgres { return super::pg::catalog::ALL_COLUMNS }
        "SELECT TABLE_NAME AS tbl, COLUMN_NAME AS name, COLUMN_TYPE AS full_type, COLUMN_KEY AS col_key, IS_NULLABLE = 'YES' AS nullable FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME, ORDINAL_POSITION"
    }

    /// Which columns of a database hold a foreign key, for autocomplete. Params: [schema]. Columns:
    /// tbl, name.
    pub fn all_foreign_keys(self) -> &'static str {
        if self == Dialect::Postgres { return super::pg::catalog::ALL_FOREIGN_KEYS }
        "SELECT TABLE_NAME AS tbl, COLUMN_NAME AS name FROM information_schema.KEY_COLUMN_USAGE WHERE TABLE_SCHEMA = ? AND REFERENCED_TABLE_NAME IS NOT NULL"
    }

    /// What decides whether a column's values may be looked up: the table's row estimate and how
    /// many indexes lead with the column. Params: [column, schema, table]. Columns: est, leads.
    pub fn value_gate(self) -> &'static str {
        if self == Dialect::Postgres { return super::pg::catalog::VALUE_GATE }
        "SELECT t.TABLE_ROWS AS est, (SELECT COUNT(*) FROM information_schema.STATISTICS s WHERE s.TABLE_SCHEMA = t.TABLE_SCHEMA AND s.TABLE_NAME = t.TABLE_NAME AND s.COLUMN_NAME = ? AND s.SEQ_IN_INDEX = 1) AS leads FROM information_schema.TABLES t WHERE t.TABLE_SCHEMA = ? AND t.TABLE_NAME = ?"
    }

    /// The most frequent values of a column, for autocomplete. With `prefix` one parameter binds the
    /// LIKE pattern. Identifiers cannot be bound, so the names are quoted into the text.
    pub fn value_counts(self, db: &str, table: &str, col: &str, prefix: bool, limit: u32) -> String {
        let c = self.quote_ident(col);
        if self == Dialect::Postgres {
            // Grouped by the text, so a json column (which has no equality) and every other type counts the same way.
            let filter = if prefix { format!(" WHERE {c}::text LIKE $1 ESCAPE '!'") } else { format!(" WHERE {c} IS NOT NULL") };
            return format!("SELECT {c}::text AS v, COUNT(*) AS n FROM {}{filter} GROUP BY 1 ORDER BY 2 DESC LIMIT {limit}", self.qualified(db, table));
        }
        let filter = if prefix { format!(" WHERE {c} LIKE ? ESCAPE '!'") } else { format!(" WHERE {c} IS NOT NULL") };
        format!("SELECT CAST({c} AS CHAR) AS v, COUNT(*) AS n FROM {}{filter} GROUP BY {c} ORDER BY 2 DESC LIMIT {limit}", self.qualified(db, table))
    }

    /// Identifiers cannot be bound, so the names are quoted into the text.
    pub fn create_stmt(self, is_view: bool, db: &str, table: &str) -> String {
        if self == Dialect::Postgres {
            // A view's text comes from the server in one cell. A table has no such function: `structure::pg_create_table`
            // writes it from the columns, constraints and indexes, so this statement asks for nothing.
            if !is_view {
                return "SELECT NULL::text".to_string();
            }
            let name = super::value::lit(&self.qualified(db, table), false);
            return format!("SELECT 'CREATE ' || CASE c.relkind WHEN 'm' THEN 'MATERIALIZED ' ELSE '' END || 'VIEW ' || {name} || E' AS\\n' || pg_get_viewdef(c.oid, true) FROM pg_class c WHERE c.oid = {name}::regclass");
        }
        format!("SHOW CREATE {} {}", if is_view { "VIEW" } else { "TABLE" }, self.qualified(db, table))
    }

    /// Appended to a metadata query to read every row. A LIMIT of its own overrides MySQL's sql_select_limit, and
    /// PostgreSQL has no such setting, so there it only says what is meant.
    pub fn all_rows(self) -> &'static str {
        match self {
            Dialect::Postgres => " LIMIT ALL",
            Dialect::MySql | Dialect::MariaDb => " LIMIT 18446744073709551615",
        }
    }

    /// The row count of a LIMIT that cuts nothing.
    pub fn no_limit(self) -> &'static str {
        match self {
            Dialect::Postgres => "ALL",
            Dialect::MySql | Dialect::MariaDb => "18446744073709551615",
        }
    }

    /// The schemas of the connected database that hold user objects, PostgreSQL only. Empty on the MySQL family, whose
    /// databases are listed by `SHOW DATABASES`.
    pub fn schemas(self) -> &'static str {
        if self == Dialect::Postgres { super::pg::catalog::SCHEMAS } else { "" }
    }

    /// Like `schemas` with the system schemas flagged and a table count per schema, for the sidebar's switcher.
    pub fn schemas_all(self) -> &'static str {
        if self == Dialect::Postgres { super::pg::catalog::SCHEMAS_ALL } else { "" }
    }

    /// Works on both engines and stops writes inside the session.
    pub fn session_read_only(self, on: bool) -> &'static str {
        match (self, on) {
            (Dialect::Postgres, true) => "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY",
            (Dialect::Postgres, false) => "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE",
            (_, true) => "SET SESSION TRANSACTION READ ONLY",
            (_, false) => "SET SESSION TRANSACTION READ WRITE",
        }
    }

    /// MariaDB keeps the status variables in information_schema, MySQL 8 only in performance_schema.
    pub fn global_status_table(self) -> &'static str {
        if self == Dialect::MariaDb { "information_schema.GLOBAL_STATUS" } else { "performance_schema.global_status" }
    }

    /// MySQL drops a CHECK by its own keyword, MariaDB by the generic CONSTRAINT.
    pub fn drop_check_word(self) -> &'static str {
        if self == Dialect::MySql { "CHECK" } else { "CONSTRAINT" }
    }

    /// MySQL counts milliseconds but only limits SELECT statements. MariaDB counts seconds, as a
    /// float, and limits every statement.
    pub fn session_timeout(self, ms: u64) -> String {
        match self {
            Dialect::MySql => format!("SET SESSION max_execution_time = {ms}"),
            Dialect::MariaDb => format!("SET SESSION max_statement_time = {}", ms as f64 / 1000.0),
            Dialect::Postgres => format!("SET statement_timeout = {ms}"),
        }
    }

    /// MySQL drops an index along with its last column. MariaDB refuses to drop a column an index still needs.
    pub fn drops_index_with_column(self) -> bool {
        // PostgreSQL drops the index together with its last column
        matches!(self, Dialect::MySql | Dialect::Postgres)
    }

    /// RENAME INDEX exists from MySQL 5.7 and MariaDB 10.5.2. Older servers need a drop and an add in one ALTER.
    pub fn can_rename_index(self, version: &str) -> bool {
        let v = version.strip_prefix("5.5.5-").unwrap_or(version);
        let nums: Vec<u32> = v.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).take(3).map(|p| p.parse().unwrap_or(0)).collect();
        let at = |i: usize| nums.get(i).copied().unwrap_or(0);
        match self {
            Dialect::MySql => (at(0), at(1)) >= (5, 7),
            Dialect::MariaDb => (at(0), at(1), at(2)) >= (10, 5, 2),
            Dialect::Postgres => true,
        }
    }

    /// MySQL keeps and enforces a CHECK from 8.0.16 and parses and ignores it before. MariaDB enforces it from 10.2.1.
    pub fn enforces_checks(self, version: &str) -> bool {
        let v = version.strip_prefix("5.5.5-").unwrap_or(version);
        let nums: Vec<u32> = v.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).take(3).map(|p| p.parse().unwrap_or(0)).collect();
        let at = |i: usize| nums.get(i).copied().unwrap_or(0);
        match self {
            Dialect::MySql => (at(0), at(1), at(2)) >= (8, 0, 16),
            Dialect::MariaDb => (at(0), at(1), at(2)) >= (10, 2, 1),
            Dialect::Postgres => true,
        }
    }

    /// A CHECK can be NOT ENFORCED on MySQL from 8.0.16. MariaDB has no such clause, and PostgreSQL 18 has one the editor leaves to a query.
    pub fn can_not_enforce(self, version: &str) -> bool {
        match self {
            Dialect::MySql => self.enforces_checks(version),
            Dialect::MariaDb | Dialect::Postgres => false,
        }
    }

    /// A DEFAULT that is an expression. Both engines take the timestamp functions bare; MySQL 8 wants every
    /// other expression in parentheses and MariaDB accepts them.
    pub fn default_expr(self, e: &str) -> String {
        let t = e.trim();
        let lower = t.to_ascii_lowercase();
        let (name, rest) = lower.split_once('(').map_or((lower.as_str(), ""), |(a, b)| (a.trim_end(), b));
        let bare = matches!(name, "current_timestamp" | "now" | "localtime" | "localtimestamp") && (rest.is_empty() || (rest.ends_with(')') && rest[..rest.len() - 1].bytes().all(|b| b.is_ascii_digit())));
        if bare { t.to_string() } else { format!("({t})") }
    }

    /// Stops the running statement and keeps the connection. Run it from a second connection.
    pub fn cancel(self, connection_id: u64) -> String {
        match self {
            Dialect::Postgres => format!("SELECT pg_cancel_backend({connection_id})"),
            _ => format!("KILL QUERY {connection_id}"),
        }
    }

    /// Ends a connection, and with it the statement it runs. Same text on both engines.
    pub fn kill_connection(self, connection_id: u64) -> String {
        match self {
            Dialect::Postgres => format!("SELECT pg_terminate_backend({connection_id})"),
            _ => format!("KILL {connection_id}"),
        }
    }
}

pub const KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "NULL", "IS", "IN", "BETWEEN", "LIKE", "REGEXP", "RLIKE", "EXISTS", "ANY", "SOME",
    "AS", "DISTINCT", "ALL", "ORDER", "BY", "GROUP", "HAVING", "LIMIT", "OFFSET", "ASC", "DESC", "UNION", "INTERSECT", "EXCEPT",
    "JOIN", "INNER", "LEFT", "RIGHT", "OUTER", "CROSS", "NATURAL", "STRAIGHT_JOIN", "ON", "USING", "USE", "FORCE", "IGNORE", "INDEX", "KEY",
    "INSERT", "INTO", "VALUES", "VALUE", "UPDATE", "SET", "DELETE", "REPLACE", "DUPLICATE", "LOW_PRIORITY", "DELAYED",
    "CREATE", "ALTER", "DROP", "TRUNCATE", "RENAME", "TABLE", "VIEW", "DATABASE", "SCHEMA", "TRIGGER", "PROCEDURE", "FUNCTION", "EVENT",
    "ADD", "COLUMN", "MODIFY", "CHANGE", "AFTER", "FIRST", "TEMPORARY", "IF", "THEN", "ELSE", "ELSEIF", "END", "CASE", "WHEN",
    "PRIMARY", "FOREIGN", "REFERENCES", "UNIQUE", "CHECK", "CONSTRAINT", "DEFAULT", "AUTO_INCREMENT", "UNSIGNED", "ZEROFILL", "COMMENT",
    "ENGINE", "CHARSET", "CHARACTER", "COLLATE", "CASCADE", "RESTRICT", "FULLTEXT", "SPATIAL", "GENERATED", "ALWAYS", "STORED", "VIRTUAL",
    "WITH", "RECURSIVE", "OVER", "PARTITION", "WINDOW", "ROWS", "RANGE", "UNBOUNDED", "PRECEDING", "FOLLOWING", "CURRENT", "ROW",
    "SHOW", "TABLES", "COLUMNS", "DATABASES", "STATUS", "VARIABLES", "PROCESSLIST", "WARNINGS", "ERRORS", "GRANTS", "INDEXES", "FULL",
    "DESCRIBE", "EXPLAIN", "ANALYZE", "OPTIMIZE", "CHECKSUM", "REPAIR", "FLUSH", "KILL", "LOCK", "UNLOCK", "FOR", "SHARE", "MODE", "NOWAIT", "SKIP", "LOCKED",
    "BEGIN", "START", "TRANSACTION", "COMMIT", "ROLLBACK", "SAVEPOINT", "RELEASE", "READ", "WRITE", "ONLY", "ISOLATION", "LEVEL",
    "GRANT", "REVOKE", "PRIVILEGES", "TO", "IDENTIFIED", "USER", "ROLE", "CALL", "PREPARE", "EXECUTE", "DEALLOCATE", "DECLARE", "CURSOR", "HANDLER",
    "TRUE", "FALSE", "UNKNOWN", "DIV", "MOD", "XOR", "BINARY", "INTERVAL", "ESCAPE", "OUTFILE", "DUMPFILE", "INFILE", "LOAD", "DATA", "LOCAL",
];

/// Common functions as (name, signature). The signature is what autocomplete shows beside the name.
pub const FUNCTIONS: &[(&str, &str)] = &[
    ("COUNT", "COUNT(expr) -> BIGINT"),
    ("SUM", "SUM(expr) -> DECIMAL"),
    ("AVG", "AVG(expr) -> DOUBLE"),
    ("MIN", "MIN(expr) -> same as expr"),
    ("MAX", "MAX(expr) -> same as expr"),
    ("GROUP_CONCAT", "GROUP_CONCAT(expr ORDER BY ... SEPARATOR str) -> TEXT"),
    ("STDDEV", "STDDEV(expr) -> DOUBLE"),
    ("VARIANCE", "VARIANCE(expr) -> DOUBLE"),
    ("ROW_NUMBER", "ROW_NUMBER() OVER (...) -> BIGINT"),
    ("RANK", "RANK() OVER (...) -> BIGINT"),
    ("DENSE_RANK", "DENSE_RANK() OVER (...) -> BIGINT"),
    ("LAG", "LAG(expr, n, default) OVER (...) -> same as expr"),
    ("LEAD", "LEAD(expr, n, default) OVER (...) -> same as expr"),
    ("CONCAT", "CONCAT(str1, str2, ...) -> VARCHAR"),
    ("CONCAT_WS", "CONCAT_WS(sep, str1, str2, ...) -> VARCHAR"),
    ("LENGTH", "LENGTH(str) -> INT"),
    ("CHAR_LENGTH", "CHAR_LENGTH(str) -> INT"),
    ("LOWER", "LOWER(str) -> VARCHAR"),
    ("UPPER", "UPPER(str) -> VARCHAR"),
    ("TRIM", "TRIM([BOTH | LEADING | TRAILING] [remstr FROM] str) -> VARCHAR"),
    ("LTRIM", "LTRIM(str) -> VARCHAR"),
    ("RTRIM", "RTRIM(str) -> VARCHAR"),
    ("SUBSTRING", "SUBSTRING(str, pos, len) -> VARCHAR"),
    ("LEFT", "LEFT(str, len) -> VARCHAR"),
    ("RIGHT", "RIGHT(str, len) -> VARCHAR"),
    ("REPLACE", "REPLACE(str, from, to) -> VARCHAR"),
    ("INSTR", "INSTR(str, substr) -> INT"),
    ("LOCATE", "LOCATE(substr, str, pos) -> INT"),
    ("LPAD", "LPAD(str, len, pad) -> VARCHAR"),
    ("RPAD", "RPAD(str, len, pad) -> VARCHAR"),
    ("REPEAT", "REPEAT(str, count) -> VARCHAR"),
    ("REVERSE", "REVERSE(str) -> VARCHAR"),
    ("FORMAT", "FORMAT(number, decimals) -> VARCHAR"),
    ("REGEXP_REPLACE", "REGEXP_REPLACE(str, pattern, repl) -> VARCHAR"),
    ("NOW", "NOW() -> DATETIME"),
    ("CURDATE", "CURDATE() -> DATE"),
    ("CURTIME", "CURTIME() -> TIME"),
    ("CURRENT_TIMESTAMP", "CURRENT_TIMESTAMP() -> DATETIME"),
    ("UTC_TIMESTAMP", "UTC_TIMESTAMP() -> DATETIME"),
    ("DATE", "DATE(expr) -> DATE"),
    ("TIME", "TIME(expr) -> TIME"),
    ("YEAR", "YEAR(date) -> INT"),
    ("MONTH", "MONTH(date) -> INT"),
    ("DAY", "DAY(date) -> INT"),
    ("HOUR", "HOUR(time) -> INT"),
    ("MINUTE", "MINUTE(time) -> INT"),
    ("DAYOFWEEK", "DAYOFWEEK(date) -> INT"),
    ("WEEK", "WEEK(date, mode) -> INT"),
    ("DATE_FORMAT", "DATE_FORMAT(date, format) -> VARCHAR"),
    ("STR_TO_DATE", "STR_TO_DATE(str, format) -> DATETIME"),
    ("DATE_ADD", "DATE_ADD(date, INTERVAL n unit) -> DATETIME"),
    ("DATE_SUB", "DATE_SUB(date, INTERVAL n unit) -> DATETIME"),
    ("DATEDIFF", "DATEDIFF(date1, date2) -> INT"),
    ("TIMESTAMPDIFF", "TIMESTAMPDIFF(unit, dt1, dt2) -> BIGINT"),
    ("UNIX_TIMESTAMP", "UNIX_TIMESTAMP(date) -> BIGINT"),
    ("FROM_UNIXTIME", "FROM_UNIXTIME(ts, format) -> DATETIME"),
    ("LAST_DAY", "LAST_DAY(date) -> DATE"),
    ("JSON_EXTRACT", "JSON_EXTRACT(doc, path, ...) -> JSON"),
    ("JSON_UNQUOTE", "JSON_UNQUOTE(json) -> VARCHAR"),
    ("JSON_OBJECT", "JSON_OBJECT(key, val, ...) -> JSON"),
    ("JSON_ARRAY", "JSON_ARRAY(val, ...) -> JSON"),
    ("JSON_SET", "JSON_SET(doc, path, val, ...) -> JSON"),
    ("JSON_CONTAINS", "JSON_CONTAINS(doc, candidate, path) -> INT"),
    ("JSON_LENGTH", "JSON_LENGTH(doc, path) -> INT"),
    ("JSON_KEYS", "JSON_KEYS(doc, path) -> JSON"),
    ("JSON_TABLE", "JSON_TABLE(doc, path COLUMNS (...)) -> table"),
    ("IF", "IF(cond, then, else) -> same as then"),
    ("IFNULL", "IFNULL(expr, fallback) -> same as expr"),
    ("NULLIF", "NULLIF(expr1, expr2) -> same as expr1"),
    ("COALESCE", "COALESCE(expr, ...) -> same as expr"),
    ("CASE", "CASE WHEN cond THEN val ELSE val END -> same as val"),
    ("CAST", "CAST(expr AS type) -> type"),
    ("CONVERT", "CONVERT(expr, type) -> type"),
    ("ABS", "ABS(x) -> number"),
    ("ROUND", "ROUND(x, decimals) -> number"),
    ("CEIL", "CEIL(x) -> BIGINT"),
    ("FLOOR", "FLOOR(x) -> BIGINT"),
    ("MOD", "MOD(n, m) -> number"),
    ("POW", "POW(x, y) -> DOUBLE"),
    ("SQRT", "SQRT(x) -> DOUBLE"),
    ("RAND", "RAND(seed) -> DOUBLE"),
    ("GREATEST", "GREATEST(v1, v2, ...) -> same as v1"),
    ("LEAST", "LEAST(v1, v2, ...) -> same as v1"),
    ("UUID", "UUID() -> VARCHAR"),
    ("MD5", "MD5(str) -> VARCHAR"),
    ("SHA2", "SHA2(str, bits) -> VARCHAR"),
    ("LAST_INSERT_ID", "LAST_INSERT_ID() -> BIGINT"),
    ("VERSION", "VERSION() -> VARCHAR"),
    ("DATABASE", "DATABASE() -> VARCHAR"),
    ("CURRENT_USER", "CURRENT_USER() -> VARCHAR"),
    ("FOUND_ROWS", "FOUND_ROWS() -> BIGINT"),
];

/// What the app hides or disables per engine. Each gate in the UI asks `supports`, so a feature PostgreSQL gains is
/// switched on in one place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Feature {
    IndexHint,
    ColumnOrder,
    Unsigned,
    Collation,
    IndexCardinality,
}

/// The gated features PostgreSQL has so far. Add one here when it gains one.
const PG_FEATURES: &[Feature] = &[];

impl Dialect {
    pub fn supports(self, f: Feature) -> bool {
        match self {
            Dialect::Postgres => PG_FEATURES.contains(&f),
            Dialect::MySql | Dialect::MariaDb => true,
        }
    }
}

impl super::Engine {
    /// Mongo has no SQL features to lack, so it always answers yes and keeps its own paths.
    pub fn supports(self, f: Feature) -> bool {
        match self {
            super::Engine::Mongo => true,
            super::Engine::MySql => Dialect::MySql.supports(f),
            super::Engine::Postgres => Dialect::Postgres.supports(f),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintenance_text_per_dialect() {
        assert_eq!(Dialect::MySql.maintenance(Maint::Analyze, "shop", "orders").as_deref(), Some("ANALYZE TABLE `shop`.`orders`"));
        assert_eq!(Dialect::MariaDb.maintenance(Maint::Optimize, "shop", "a`b").as_deref(), Some("OPTIMIZE TABLE `shop`.`a``b`"));
        assert_eq!(Dialect::MySql.maintenance(Maint::Check, "shop", "orders").as_deref(), Some("CHECK TABLE `shop`.`orders`"));
        assert_eq!(Dialect::MySql.maintenance(Maint::Vacuum, "shop", "orders"), None);
        assert_eq!(Dialect::Postgres.maintenance(Maint::Vacuum, "public", "orders").as_deref(), Some("VACUUM \"public\".\"orders\""));
        assert_eq!(Dialect::Postgres.maintenance(Maint::Analyze, "public", "orders").as_deref(), Some("ANALYZE \"public\".\"orders\""));
        assert_eq!(Dialect::Postgres.maintenance(Maint::Reindex, "public", "o\"x").as_deref(), Some("REINDEX TABLE \"public\".\"o\"\"x\""));
        assert_eq!(Dialect::Postgres.maintenance(Maint::Optimize, "public", "orders"), None);
        assert!(Maint::Vacuum.heavy() && Maint::Optimize.heavy() && !Maint::Analyze.heavy() && !Maint::Check.heavy());
    }

    #[test]
    fn version_string_tells_the_engine() {
        assert_eq!(Dialect::from_version("10.11.6-MariaDB"), Dialect::MariaDb);
        assert_eq!(Dialect::from_version("10.5.8-MariaDB-1:10.5.8+maria~focal"), Dialect::MariaDb);
        assert_eq!(Dialect::from_version("8.4.0"), Dialect::MySql);
        assert_eq!(Dialect::from_version("5.7.44-log"), Dialect::MySql);
    }

    #[test]
    fn identifiers_are_quoted_with_backticks() {
        let d = Dialect::MySql;
        assert_eq!(d.quote_ident("orders"), "`orders`");
        assert_eq!(d.quote_ident("we`ird"), "`we``ird`");
        assert_eq!(d.quote_ident("`"), "````");
        assert_eq!(d.qualified("shop", "or`ders"), "`shop`.`or``ders`");
        assert_eq!(d.create_stmt(false, "shop", "t"), "SHOW CREATE TABLE `shop`.`t`");
        assert_eq!(d.create_stmt(true, "a`b", "v"), "SHOW CREATE VIEW `a``b`.`v`");
    }

    #[test]
    fn paging_and_session_statements() {
        let d = Dialect::MySql;
        assert_eq!(d.page(50, 100), "LIMIT 50 OFFSET 100");
        assert_eq!(d.session_read_only(true), "SET SESSION TRANSACTION READ ONLY");
        assert_eq!(d.session_read_only(false), "SET SESSION TRANSACTION READ WRITE");
        assert_eq!(d.cancel(42), "KILL QUERY 42");
        assert_eq!(Dialect::MySql.session_timeout(30000), "SET SESSION max_execution_time = 30000");
        assert_eq!(Dialect::MariaDb.session_timeout(30000), "SET SESSION max_statement_time = 30");
        assert_eq!(Dialect::MariaDb.session_timeout(2500), "SET SESSION max_statement_time = 2.5");
        assert_eq!(Dialect::MariaDb.session_timeout(0), "SET SESSION max_statement_time = 0");
    }

    #[test]
    fn postgres_text() {
        let d = Dialect::Postgres;
        assert_eq!(d.quote_ident("we\"ird"), "\"we\"\"ird\"");
        assert_eq!(d.qualified("public", "or\"ders"), "\"public\".\"or\"\"ders\"");
        assert_eq!(d.page(50, 100), "LIMIT 50 OFFSET 100");
        assert_eq!(d.session_timeout(30000), "SET statement_timeout = 30000");
        assert_eq!(d.cancel(42), "SELECT pg_cancel_backend(42)");
        assert_eq!(d.kill_connection(42), "SELECT pg_terminate_backend(42)");
        assert_eq!(d.session_read_only(true), "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY");
        assert_eq!(d.session_read_only(false), "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE");
        assert!(!d.is_mysql_family() && Dialect::MariaDb.is_mysql_family());
        assert_eq!(Dialect::from_version("16.2"), Dialect::MySql);
    }

    #[test]
    fn metadata_queries_bind_values() {
        let d = Dialect::MySql;
        for (q, params) in [(d.tables(), 1), (d.columns(), 2), (d.indexes(), 2), (d.foreign_keys(), 2), (d.referenced_by(), 2), (d.triggers(), 2), (d.table_status(), 2), (d.all_columns(), 1), (d.all_foreign_keys(), 1), (d.value_gate(), 3)] {
            assert_eq!(q.matches('?').count(), params, "{q}");
            assert!(q.starts_with("SELECT "));
            assert!(q.contains("information_schema."));
        }
    }

    #[test]
    fn postgres_queries_bind_dollar_placeholders() {
        let d = Dialect::Postgres;
        for (q, params) in [(d.tables(), 1), (d.columns(), 2), (d.indexes(), 2), (d.foreign_keys(), 2), (d.referenced_by(), 2), (d.triggers(), 2), (d.table_status(), 2), (d.all_columns(), 1), (d.all_foreign_keys(), 1), (d.value_gate(), 3), (super::super::pg::catalog::CONSTRAINTS, 2)] {
            assert!(!q.contains('?'), "{q}");
            assert!(q.contains(&format!("${params}")) && !q.contains(&format!("${}", params + 1)), "{q}");
            assert!(q.starts_with("SELECT "));
        }
        assert_eq!(d.all_rows(), " LIMIT ALL");
        assert_eq!(d.value_counts("s", "t", "c", false, 5), "SELECT \"c\"::text AS v, COUNT(*) AS n FROM \"s\".\"t\" WHERE \"c\" IS NOT NULL GROUP BY 1 ORDER BY 2 DESC LIMIT 5");
        assert!(d.value_counts("s", "t", "c", true, 5).contains("WHERE \"c\"::text LIKE $1"));
        assert!(d.create_stmt(true, "s", "v").contains("pg_get_viewdef") && d.create_stmt(true, "s", "o'v").contains("'\"s\".\"o''v\"'"));
        assert!(d.schemas().contains("pg_namespace") && Dialect::MySql.schemas().is_empty());
        assert_eq!(crate::db::sql::PageQuery { skip: 5, ..Default::default() }.select_sql(d, "s", "t", &[], None), "SELECT *\nFROM \"s\".\"t\"\nLIMIT ALL OFFSET 5");
        assert!(!crate::db::sql::PageQuery { index_hint: "k".into(), ..Default::default() }.select_sql(d, "s", "t", &[], None).contains("USE INDEX"));
    }

    #[test]
    fn rename_index_and_default_expressions() {
        assert!(Dialect::MySql.can_rename_index("8.4.3"));
        assert!(Dialect::MySql.can_rename_index("5.7.44-log"));
        assert!(!Dialect::MySql.can_rename_index("5.6.51"));
        assert!(Dialect::MariaDb.can_rename_index("10.11.6-MariaDB"));
        assert!(Dialect::MariaDb.can_rename_index("10.5.2-MariaDB"));
        assert!(!Dialect::MariaDb.can_rename_index("10.5.1-MariaDB"));
        assert!(!Dialect::MariaDb.can_rename_index("10.4.32-MariaDB"));
        assert!(Dialect::MariaDb.can_rename_index("5.5.5-10.11.6-MariaDB"));
        assert_eq!(Dialect::MySql.default_expr("CURRENT_TIMESTAMP"), "CURRENT_TIMESTAMP");
        assert_eq!(Dialect::MySql.default_expr("current_timestamp(6)"), "current_timestamp(6)");
        assert_eq!(Dialect::MySql.default_expr("now()"), "now()");
        assert_eq!(Dialect::MySql.default_expr("UUID()"), "(UUID())");
        assert_eq!(Dialect::MariaDb.default_expr("curdate() + interval 1 day"), "(curdate() + interval 1 day)");
    }

    #[test]
    fn lists_are_usable() {
        assert!(KEYWORDS.len() >= 150, "{}", KEYWORDS.len());
        assert!(FUNCTIONS.len() >= 80, "{}", FUNCTIONS.len());
        for (name, sig) in FUNCTIONS {
            assert!(sig.to_ascii_uppercase().starts_with(&format!("{name}(")) || sig.starts_with("CASE"), "{name}: {sig}");
            assert!(sig.contains(" -> "), "{sig}");
        }
        let mut seen = std::collections::HashSet::new();
        assert!(KEYWORDS.iter().all(|k| seen.insert(*k)));
        assert!(KEYWORDS.iter().all(|k| k.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')));
    }
}
