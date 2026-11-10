//! The PostgreSQL word lists of SQL autocomplete. The MySQL ones live in `db::sql`.

/// Keywords of PostgreSQL 13 to 18, the ones worth offering while typing a statement. Phrases are offered as one entry so the common pair is a single pick.
pub(crate) const KEYWORDS_PG: &[&str] = &[
    "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "NULL", "IS", "IN", "BETWEEN", "LIKE", "ILIKE", "SIMILAR", "TO", "EXISTS", "ANY", "SOME", "ALL", "DISTINCT", "ON", "AS", "AT", "TIME", "ZONE", "SYMMETRIC", "ASYMMETRIC",
    "ORDER", "BY", "GROUP", "HAVING", "LIMIT", "OFFSET", "FETCH", "FIRST", "NEXT", "ROW", "ROWS", "ONLY", "TIES", "ASC", "DESC", "NULLS", "LAST", "USING", "COLLATE", "GROUPING", "SETS", "ROLLUP", "CUBE",
    "UNION", "INTERSECT", "EXCEPT", "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "OUTER", "CROSS", "NATURAL", "LATERAL", "TABLESAMPLE", "REPEATABLE", "WITHIN", "FILTER", "OVER", "PARTITION", "WINDOW", "RANGE", "GROUPS",
    "UNBOUNDED", "PRECEDING", "FOLLOWING", "CURRENT", "EXCLUDE", "OTHERS", "WITH", "RECURSIVE", "MATERIALIZED", "SEARCH", "BREADTH", "DEPTH", "CYCLE", "IS DISTINCT FROM", "IS NOT DISTINCT FROM",
    "INSERT", "INTO", "VALUES", "DEFAULT", "RETURNING", "CONFLICT", "DO", "NOTHING", "UPDATE", "SET", "EXCLUDED", "DELETE", "MERGE", "MATCHED", "TARGET", "SOURCE", "OVERRIDING", "SYSTEM", "VALUE", "TRUNCATE", "RESTART", "IDENTITY", "CONTINUE",
    "CREATE", "ALTER", "DROP", "TABLE", "VIEW", "INDEX", "CONCURRENTLY", "UNIQUE", "SCHEMA", "DATABASE", "SEQUENCE", "TYPE", "DOMAIN", "EXTENSION", "FUNCTION", "PROCEDURE", "TRIGGER", "RULE", "POLICY", "ROLE", "USER", "TEMPORARY", "TEMP", "UNLOGGED",
    "IF", "ADD", "COLUMN", "RENAME", "OWNER", "CONSTRAINT", "PRIMARY", "KEY", "FOREIGN", "REFERENCES", "CHECK", "DEFERRABLE", "INITIALLY", "DEFERRED", "IMMEDIATE", "CASCADE", "RESTRICT", "NO", "ACTION", "GENERATED", "ALWAYS", "STORED", "BY DEFAULT", "AS IDENTITY",
    "INCLUDE", "INHERITS", "TABLESPACE", "PARTITION BY", "ATTACH", "DETACH", "FINALIZE", "ENABLE", "DISABLE", "FORCE", "VALIDATE", "COMMENT", "ON CONFLICT", "DO NOTHING", "DO UPDATE", "NULLS FIRST", "NULLS LAST", "FETCH FIRST", "SIMILAR TO",
    "CASE", "WHEN", "THEN", "ELSE", "END", "CAST", "COALESCE", "NULLIF", "GREATEST", "LEAST", "ARRAY", "TRUE", "FALSE", "UNKNOWN", "INTERVAL", "ESCAPE", "COLLATION", "VARIADIC", "ORDINALITY", "WITHIN GROUP",
    "BEGIN", "START", "TRANSACTION", "COMMIT", "ROLLBACK", "SAVEPOINT", "RELEASE", "ISOLATION", "LEVEL", "READ", "WRITE", "COMMITTED", "UNCOMMITTED", "SERIALIZABLE", "LOCK", "SHARE", "EXCLUSIVE", "NOWAIT", "SKIP", "LOCKED", "FOR", "KEY SHARE", "NO KEY UPDATE",
    "EXPLAIN", "ANALYZE", "VERBOSE", "BUFFERS", "COSTS", "FORMAT", "SETTINGS", "WAL", "TIMING", "SUMMARY", "VACUUM", "REINDEX", "CLUSTER", "REFRESH", "COPY", "SHOW", "RESET", "LISTEN", "NOTIFY", "UNLISTEN", "PREPARE", "EXECUTE", "DEALLOCATE", "DECLARE", "CURSOR", "CLOSE", "MOVE",
    "GRANT", "REVOKE", "PRIVILEGES", "PUBLIC", "USAGE", "CONNECT", "SECURITY", "DEFINER", "INVOKER", "LANGUAGE", "RETURNS", "RETURN", "IMMUTABLE", "STABLE", "VOLATILE", "STRICT", "PARALLEL", "SAFE", "UNSAFE", "RESTRICTED",
    "BEFORE", "AFTER", "INSTEAD", "OF", "EACH", "STATEMENT", "REFERENCING", "OLD", "NEW", "WHILE", "LOOP", "EXIT", "PERFORM", "RAISE", "NOTICE", "EXCEPTION",
];

/// Words PostgreSQL will not take as a bare name: the "reserved" and "reserved (can be function or type)" columns of the SQL Key Words appendix. A table or column called this gets double quotes.
pub(crate) const RESERVED_PG: &[&str] = &[
    "ALL", "ANALYSE", "ANALYZE", "AND", "ANY", "ARRAY", "AS", "ASC", "ASYMMETRIC", "AUTHORIZATION", "BINARY", "BOTH", "CASE", "CAST", "CHECK", "COLLATE", "COLLATION", "COLUMN", "CONCURRENTLY", "CONSTRAINT", "CREATE", "CROSS", "CURRENT_CATALOG",
    "CURRENT_DATE", "CURRENT_ROLE", "CURRENT_SCHEMA", "CURRENT_TIME", "CURRENT_TIMESTAMP", "CURRENT_USER", "DEFAULT", "DEFERRABLE", "DESC", "DISTINCT", "DO", "ELSE", "END", "EXCEPT", "FALSE", "FETCH", "FOR", "FOREIGN", "FREEZE", "FROM", "FULL", "GRANT",
    "GROUP", "HAVING", "ILIKE", "IN", "INITIALLY", "INNER", "INTERSECT", "INTO", "IS", "ISNULL", "JOIN", "LATERAL", "LEADING", "LEFT", "LIKE", "LIMIT", "LOCALTIME", "LOCALTIMESTAMP", "NATURAL", "NOT", "NOTNULL", "NULL", "OFFSET", "ON", "ONLY", "OR", "ORDER",
    "OUTER", "OVERLAPS", "PLACING", "PRIMARY", "REFERENCES", "RETURNING", "RIGHT", "SELECT", "SESSION_USER", "SIMILAR", "SOME", "SYMMETRIC", "SYSTEM_USER", "TABLE", "TABLESAMPLE", "THEN", "TO", "TRAILING", "TRUE", "UNION", "UNIQUE", "USER", "USING", "VARIADIC",
    "VERBOSE", "WHEN", "WHERE", "WINDOW", "WITH",
];

/// Built-in functions as (name, signature), in the same shape as `db::sql::FUNCTIONS`. A signature with no parenthesis is a keyword form that is written without a call. Nothing the read-only check refuses is listed.
pub(crate) const FUNCTIONS_PG: &[(&str, &str)] = &[
    // Strings.
    ("CONCAT", "CONCAT(value, ...) -> text"), ("CONCAT_WS", "CONCAT_WS(sep, value, ...) -> text"), ("FORMAT", "FORMAT(template, value, ...) -> text"), ("LEFT", "LEFT(string, n) -> text"), ("RIGHT", "RIGHT(string, n) -> text"),
    ("LOWER", "LOWER(string) -> text"), ("UPPER", "UPPER(string) -> text"), ("INITCAP", "INITCAP(string) -> text"), ("LENGTH", "LENGTH(string) -> integer"), ("CHAR_LENGTH", "CHAR_LENGTH(string) -> integer"),
    ("POSITION", "POSITION(substring IN string) -> integer"), ("STRPOS", "STRPOS(string, substring) -> integer"), ("SUBSTR", "SUBSTR(string, start, count) -> text"), ("SUBSTRING", "SUBSTRING(string, start, count) -> text"),
    ("REPLACE", "REPLACE(string, from, to) -> text"), ("REGEXP_REPLACE", "REGEXP_REPLACE(string, pattern, replacement, flags) -> text"), ("REGEXP_MATCH", "REGEXP_MATCH(string, pattern, flags) -> text[]"),
    ("REGEXP_MATCHES", "REGEXP_MATCHES(string, pattern, flags) -> setof text[]"), ("SPLIT_PART", "SPLIT_PART(string, delimiter, n) -> text"), ("STRING_AGG", "STRING_AGG(expr, delimiter) -> text"), ("TRIM", "TRIM(string) -> text"),
    ("BTRIM", "BTRIM(string, characters) -> text"), ("LTRIM", "LTRIM(string, characters) -> text"), ("RTRIM", "RTRIM(string, characters) -> text"), ("LPAD", "LPAD(string, length, fill) -> text"), ("RPAD", "RPAD(string, length, fill) -> text"),
    ("MD5", "MD5(string) -> text"), ("ENCODE", "ENCODE(data, format) -> text"), ("DECODE", "DECODE(string, format) -> bytea"), ("TO_HEX", "TO_HEX(n) -> text"),
    // Numbers.
    ("ABS", "ABS(x) -> numeric"), ("CEIL", "CEIL(x) -> numeric"), ("FLOOR", "FLOOR(x) -> numeric"), ("ROUND", "ROUND(x, digits) -> numeric"), ("TRUNC", "TRUNC(x, digits) -> numeric"), ("MOD", "MOD(y, x) -> numeric"), ("POWER", "POWER(a, b) -> numeric"),
    ("SQRT", "SQRT(x) -> numeric"), ("RANDOM", "RANDOM() -> double precision"), ("GREATEST", "GREATEST(value, ...) -> anyelement"), ("LEAST", "LEAST(value, ...) -> anyelement"), ("COALESCE", "COALESCE(value, ...) -> anyelement"), ("NULLIF", "NULLIF(a, b) -> anyelement"),
    // Dates and times.
    ("NOW", "NOW() -> timestamptz"), ("CURRENT_DATE", "CURRENT_DATE -> date"), ("CURRENT_TIMESTAMP", "CURRENT_TIMESTAMP -> timestamptz"), ("CLOCK_TIMESTAMP", "CLOCK_TIMESTAMP() -> timestamptz"), ("DATE_TRUNC", "DATE_TRUNC(field, source) -> timestamp"),
    ("DATE_PART", "DATE_PART(field, source) -> double precision"), ("EXTRACT", "EXTRACT(field FROM source) -> numeric"), ("AGE", "AGE(timestamp, timestamp) -> interval"), ("MAKE_DATE", "MAKE_DATE(year, month, day) -> date"),
    ("MAKE_TIMESTAMP", "MAKE_TIMESTAMP(year, month, day, hour, min, sec) -> timestamp"), ("MAKE_INTERVAL", "MAKE_INTERVAL(years, months, weeks, days, hours, mins, secs) -> interval"), ("TO_CHAR", "TO_CHAR(value, format) -> text"),
    ("TO_DATE", "TO_DATE(text, format) -> date"), ("TO_TIMESTAMP", "TO_TIMESTAMP(text, format) -> timestamptz"), ("TO_NUMBER", "TO_NUMBER(text, format) -> numeric"),
    // Aggregates.
    ("COUNT", "COUNT(expr) -> bigint"), ("SUM", "SUM(expr) -> numeric"), ("AVG", "AVG(expr) -> numeric"), ("MIN", "MIN(expr) -> anyelement"), ("MAX", "MAX(expr) -> anyelement"), ("ARRAY_AGG", "ARRAY_AGG(expr) -> anyarray"),
    ("BOOL_AND", "BOOL_AND(expr) -> boolean"), ("BOOL_OR", "BOOL_OR(expr) -> boolean"), ("EVERY", "EVERY(expr) -> boolean"), ("JSON_AGG", "JSON_AGG(expr) -> json"), ("JSONB_AGG", "JSONB_AGG(expr) -> jsonb"),
    ("JSON_OBJECT_AGG", "JSON_OBJECT_AGG(key, value) -> json"), ("JSONB_OBJECT_AGG", "JSONB_OBJECT_AGG(key, value) -> jsonb"), ("PERCENTILE_CONT", "PERCENTILE_CONT(fraction) -> double precision"), ("PERCENTILE_DISC", "PERCENTILE_DISC(fraction) -> anyelement"), ("MODE", "MODE() -> anyelement"),
    // Window functions.
    ("ROW_NUMBER", "ROW_NUMBER() -> bigint"), ("RANK", "RANK() -> bigint"), ("DENSE_RANK", "DENSE_RANK() -> bigint"), ("LAG", "LAG(value, offset, default) -> anyelement"), ("LEAD", "LEAD(value, offset, default) -> anyelement"),
    ("FIRST_VALUE", "FIRST_VALUE(value) -> anyelement"), ("LAST_VALUE", "LAST_VALUE(value) -> anyelement"), ("NTILE", "NTILE(buckets) -> integer"),
    // JSON.
    ("JSONB_BUILD_OBJECT", "JSONB_BUILD_OBJECT(key, value, ...) -> jsonb"), ("JSONB_BUILD_ARRAY", "JSONB_BUILD_ARRAY(value, ...) -> jsonb"), ("JSONB_SET", "JSONB_SET(target, path, new_value, create_missing) -> jsonb"),
    ("JSONB_INSERT", "JSONB_INSERT(target, path, new_value, insert_after) -> jsonb"), ("JSONB_PATH_QUERY", "JSONB_PATH_QUERY(target, path) -> setof jsonb"), ("JSONB_ARRAY_ELEMENTS", "JSONB_ARRAY_ELEMENTS(array) -> setof jsonb"),
    ("JSONB_EACH", "JSONB_EACH(object) -> setof record"), ("JSONB_EACH_TEXT", "JSONB_EACH_TEXT(object) -> setof record"), ("JSONB_EXTRACT_PATH", "JSONB_EXTRACT_PATH(from, path, ...) -> jsonb"),
    ("JSONB_EXTRACT_PATH_TEXT", "JSONB_EXTRACT_PATH_TEXT(from, path, ...) -> text"), ("JSONB_TYPEOF", "JSONB_TYPEOF(value) -> text"), ("JSONB_PRETTY", "JSONB_PRETTY(value) -> text"), ("TO_JSONB", "TO_JSONB(value) -> jsonb"),
    ("TO_JSON", "TO_JSON(value) -> json"), ("JSON_BUILD_OBJECT", "JSON_BUILD_OBJECT(key, value, ...) -> json"), ("JSON_ARRAY_ELEMENTS", "JSON_ARRAY_ELEMENTS(array) -> setof json"),
    // Arrays.
    ("ARRAY_LENGTH", "ARRAY_LENGTH(array, dimension) -> integer"), ("ARRAY_POSITION", "ARRAY_POSITION(array, element) -> integer"), ("ARRAY_REMOVE", "ARRAY_REMOVE(array, element) -> anyarray"), ("ARRAY_APPEND", "ARRAY_APPEND(array, element) -> anyarray"),
    ("ARRAY_CAT", "ARRAY_CAT(array, array) -> anyarray"), ("UNNEST", "UNNEST(array) -> setof anyelement"), ("CARDINALITY", "CARDINALITY(array) -> integer"),
    // Other.
    ("GENERATE_SERIES", "GENERATE_SERIES(start, stop, step) -> setof anyelement"), ("GEN_RANDOM_UUID", "GEN_RANDOM_UUID() -> uuid"), ("PG_TYPEOF", "PG_TYPEOF(value) -> regtype"), ("VERSION", "VERSION() -> text"),
    ("CURRENT_USER", "CURRENT_USER -> name"), ("CURRENT_SCHEMA", "CURRENT_SCHEMA -> name"), ("CURRENT_DATABASE", "CURRENT_DATABASE() -> name"),
];

/// Type names offered after `::`.
pub(crate) const TYPES_PG: &[&str] = &[
    "int", "integer", "bigint", "smallint", "numeric", "real", "double precision", "text", "varchar", "char", "boolean", "date", "timestamp", "timestamptz", "time", "interval", "uuid", "json", "jsonb", "bytea", "inet", "cidr", "int[]", "text[]", "regclass",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_are_well_formed() {
        for (name, sig) in FUNCTIONS_PG {
            assert!(sig.starts_with(name), "{name}: {sig}");
            assert!(sig.contains(" -> "), "{sig}");
            assert!(name.bytes().all(|b| b.is_ascii_uppercase() || b == b'_' || b.is_ascii_digit()), "{name}");
        }
        let mut seen = std::collections::HashSet::new();
        assert!(FUNCTIONS_PG.iter().all(|(n, _)| seen.insert(*n)));
        let mut seen = std::collections::HashSet::new();
        assert!(KEYWORDS_PG.iter().all(|k| seen.insert(*k)), "duplicate keyword");
        assert!(RESERVED_PG.iter().all(|w| w.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')));
    }
}
