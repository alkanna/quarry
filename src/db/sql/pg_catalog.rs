//! The PostgreSQL catalog queries behind `Dialect`. They return the same columns as the MySQL ones so the readers
//! in `structure` and `worker` serve both, with `$1` placeholders and every selected column cast to text or to an
//! integer, because a bound query comes back as binary rows and only those two read the same on both engines.
//! Values the MySQL queries have no word for (engine, charset, auto increment) stay empty.

/// Which relations are tables for the sidebar: tables, partitioned tables, views, materialized views and foreign
/// tables. Partitions are left out, their parent stands for them.
macro_rules! listed {
    () => {
        "c.relkind IN ('r', 'p', 'v', 'm', 'f') AND NOT c.relispartition"
    };
}

/// PRI for a primary key column, UNI for the only column of a unique index, MUL for the first column of any other index.
macro_rules! col_key {
    () => {
        "CASE WHEN EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = a.attrelid AND i.indisprimary AND a.attnum = ANY (i.indkey)) THEN 'PRI' \
              WHEN EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = a.attrelid AND i.indisunique AND i.indnkeyatts = 1 AND i.indpred IS NULL AND i.indkey[0] = a.attnum) THEN 'UNI' \
              WHEN EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = a.attrelid AND i.indkey[0] = a.attnum) THEN 'MUL' ELSE '' END"
    };
}

/// The type of a column resolved through up to two levels of domains, for `Kind::from_pg`. The category of a domain
/// is its base type's, so only the name and the type kind need the walk.
macro_rules! pg_type {
    () => {
        "COALESCE(b2.typname, b1.typname, t.typname)::text AS pg_type, t.typcategory::text AS pg_cat, COALESCE(b2.typtype, b1.typtype, t.typtype)::text AS pg_typtype"
    };
}

macro_rules! type_joins {
    () => {
        "JOIN pg_type t ON t.oid = a.atttypid LEFT JOIN pg_type b1 ON b1.oid = t.typbasetype LEFT JOIN pg_type b2 ON b2.oid = b1.typbasetype"
    };
}

/// Params: [schema]. Columns: name, kind ("BASE TABLE" or "VIEW"), engine, row_estimate, data_bytes, index_bytes,
/// comment. Sizes are left out here because the list runs for every table of a schema; `TABLE_STATUS` has them. A
/// row estimate of zero or less is unknown (a table never analyzed is -1 from 14 on and 0 on 13), so it is NULL.
pub const TABLES: &str = concat!(
    "SELECT c.relname::text AS name, CASE WHEN c.relkind IN ('v', 'm') THEN 'VIEW' ELSE 'BASE TABLE' END AS kind, ''::text AS engine, \
     CASE WHEN c.reltuples > 0 THEN c.reltuples::bigint END AS row_estimate, NULL::bigint AS data_bytes, NULL::bigint AS index_bytes, \
     COALESCE(obj_description(c.oid, 'pg_class'), '') AS comment \
     FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND ",
    listed!(),
    " ORDER BY c.relname"
);

/// Params: [schema, table]. Columns: name, full_type, nullable, col_key, col_default, extra, collation, comment, position,
/// pg_type, pg_cat, pg_typtype for the kind, and attidentity ('a' always, 'd' by default) and attgenerated ('s' stored), which
/// the write path reads instead of the words of `extra`. A serial or identity column says auto_increment in `extra`, an identity
/// that is always generated and a computed column say GENERATED, and neither has a `col_default` of its own.
pub const COLUMNS: &str = concat!(
    "SELECT a.attname::text AS name, format_type(a.atttypid, a.atttypmod) AS full_type, (NOT a.attnotnull)::int AS nullable, ",
    col_key!(),
    " AS col_key, CASE WHEN a.attidentity <> '' OR a.attgenerated <> '' THEN NULL ELSE pg_get_expr(d.adbin, d.adrelid) END AS col_default, \
     concat_ws(' ', \
       CASE WHEN a.attidentity = 'a' THEN 'auto_increment GENERATED ALWAYS AS IDENTITY' \
            WHEN a.attidentity = 'd' THEN 'auto_increment IDENTITY BY DEFAULT' \
            WHEN pg_get_expr(d.adbin, d.adrelid) LIKE 'nextval(%' THEN 'auto_increment' END, \
       CASE WHEN a.attgenerated <> '' THEN 'GENERATED ALWAYS AS (' || pg_get_expr(d.adbin, d.adrelid) || ') ' || CASE a.attgenerated WHEN 's' THEN 'STORED' ELSE 'VIRTUAL' END END) AS extra, \
     CASE WHEN a.attcollation <> t.typcollation THEN co.collname::text END AS collation, \
     COALESCE(col_description(a.attrelid, a.attnum), '') AS comment, a.attnum::bigint AS position, ",
    pg_type!(),
    ", a.attidentity::text AS attidentity, a.attgenerated::text AS attgenerated FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace ",
    type_joins!(),
    " LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum LEFT JOIN pg_collation co ON co.oid = a.attcollation \
     WHERE n.nspname = $1 AND c.relname = $2 AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum"
);

/// Params: [schema, table]. One row per key column, the primary key first. Columns: name, non_unique, seq, col,
/// direction, cardinality, sub_part, index_type, nullable, comment, and expr (the text of an expression key part, whose
/// `col` is empty), def (`pg_get_indexdef`), is_primary and backs_constraint (a primary key, unique or exclusion
/// constraint owns the index, so a CREATE TABLE writes it as a constraint and not as CREATE INDEX).
pub const INDEXES: &str = "SELECT ic.relname::text AS name, (NOT i.indisunique)::int AS non_unique, k.n::bigint AS seq, \
     COALESCE(a.attname::text, '') AS col, CASE WHEN (i.indoption[k.n - 1]::int & 1) = 1 THEN 'D' ELSE 'A' END AS direction, \
     NULL::bigint AS cardinality, NULL::bigint AS sub_part, upper(am.amname)::text AS index_type, \
     CASE WHEN a.attnotnull THEN '' ELSE 'YES' END AS nullable, COALESCE(obj_description(ic.oid, 'pg_class'), '') AS comment, \
     CASE WHEN i.indkey[k.n - 1] = 0 THEN pg_get_indexdef(i.indexrelid, k.n, true) END AS expr, \
     pg_get_indexdef(i.indexrelid) AS def, i.indisprimary::int AS is_primary, \
     EXISTS (SELECT 1 FROM pg_constraint co WHERE co.conindid = i.indexrelid AND co.conrelid = i.indrelid AND co.contype IN ('p', 'u', 'x'))::int AS backs_constraint \
     FROM pg_index i JOIN pg_class tc ON tc.oid = i.indrelid JOIN pg_namespace n ON n.oid = tc.relnamespace \
     JOIN pg_class ic ON ic.oid = i.indexrelid JOIN pg_am am ON am.oid = ic.relam \
     CROSS JOIN LATERAL generate_series(1, i.indnkeyatts::int) AS k(n) \
     LEFT JOIN pg_attribute a ON a.attrelid = tc.oid AND a.attnum = i.indkey[k.n - 1] \
     WHERE n.nspname = $1 AND tc.relname = $2 ORDER BY NOT i.indisprimary, ic.relname, k.n";

/// The columns of both key queries. A constraint that a partition inherited from its parent is left out, so the
/// parent's own is not shown twice.
macro_rules! fk_cols {
    () => {
        "SELECT co.conname::text AS name, n.nspname::text AS db, c.relname::text AS tbl, a.attname::text AS col, k.ord::bigint AS pos, \
         rn.nspname::text AS ref_db, rc.relname::text AS ref_tbl, ra.attname::text AS ref_col, \
         CASE co.confupdtype WHEN 'a' THEN 'NO ACTION' WHEN 'r' THEN 'RESTRICT' WHEN 'c' THEN 'CASCADE' WHEN 'n' THEN 'SET NULL' WHEN 'd' THEN 'SET DEFAULT' END AS on_update, \
         CASE co.confdeltype WHEN 'a' THEN 'NO ACTION' WHEN 'r' THEN 'RESTRICT' WHEN 'c' THEN 'CASCADE' WHEN 'n' THEN 'SET NULL' WHEN 'd' THEN 'SET DEFAULT' END AS on_delete \
         FROM pg_constraint co JOIN pg_class c ON c.oid = co.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
         JOIN pg_class rc ON rc.oid = co.confrelid JOIN pg_namespace rn ON rn.oid = rc.relnamespace \
         CROSS JOIN LATERAL unnest(co.conkey, co.confkey) WITH ORDINALITY AS k(attnum, refattnum, ord) \
         JOIN pg_attribute a ON a.attrelid = co.conrelid AND a.attnum = k.attnum \
         JOIN pg_attribute ra ON ra.attrelid = co.confrelid AND ra.attnum = k.refattnum \
         WHERE co.contype = 'f' AND co.conparentid = 0 AND "
    };
}

/// Params: [schema, table]. Columns: name, db, tbl, col, pos, ref_db, ref_tbl, ref_col, on_update, on_delete.
pub const FOREIGN_KEYS: &str = concat!(fk_cols!(), "n.nspname = $1 AND c.relname = $2 ORDER BY co.conname, k.ord");

/// Params: [schema, table] of the referenced table. Same columns.
pub const REFERENCED_BY: &str = concat!(fk_cols!(), "rn.nspname = $1 AND rc.relname = $2 ORDER BY n.nspname, c.relname, co.conname, k.ord");

/// Params: [schema, table]. Columns: name, timing, event (several joined with OR), body (the whole CREATE TRIGGER
/// statement, since the function is separate), definer (empty), ord (the order among the triggers of one timing, which
/// is by name), sql_mode (empty).
pub const TRIGGERS: &str = "SELECT t.tgname::text AS name, CASE WHEN (t.tgtype::int & 2) = 2 THEN 'BEFORE' WHEN (t.tgtype::int & 64) = 64 THEN 'INSTEAD OF' ELSE 'AFTER' END AS timing, \
     concat_ws(' OR ', CASE WHEN (t.tgtype::int & 4) = 4 THEN 'INSERT' END, CASE WHEN (t.tgtype::int & 8) = 8 THEN 'DELETE' END, \
       CASE WHEN (t.tgtype::int & 16) = 16 THEN 'UPDATE' END, CASE WHEN (t.tgtype::int & 32) = 32 THEN 'TRUNCATE' END) AS event, \
     pg_get_triggerdef(t.oid, true) AS body, ''::text AS definer, \
     (row_number() OVER (PARTITION BY (t.tgtype::int & 66) ORDER BY t.tgname))::bigint AS ord, ''::text AS sql_mode \
     FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
     WHERE NOT t.tgisinternal AND n.nspname = $1 AND c.relname = $2 ORDER BY 2, 3, 6";

/// Params: [schema, table]. The columns of the MySQL status, plus total_bytes (heap, TOAST and indexes) and kind (the
/// relkind). The estimate is NULL while the planner has no count, and the caller counts then.
pub const TABLE_STATUS: &str = "SELECT c.relname::text AS name, ''::text AS engine, ''::text AS row_format, \
     CASE WHEN c.reltuples > 0 THEN c.reltuples::bigint END AS row_estimate, NULL::bigint AS avg_row_bytes, \
     CASE WHEN c.relkind IN ('r', 'm', 'p') THEN pg_relation_size(c.oid) END AS data_bytes, \
     CASE WHEN c.relkind IN ('r', 'm', 'p') THEN pg_indexes_size(c.oid) END AS index_bytes, NULL::bigint AS free_bytes, NULL::bigint AS auto_increment, \
     NULL::text AS created, NULL::text AS updated, NULL::text AS collation, ''::text AS options, COALESCE(obj_description(c.oid, 'pg_class'), '') AS comment, \
     CASE WHEN c.relkind IN ('r', 'm', 'p') THEN pg_total_relation_size(c.oid) END AS total_bytes, c.relkind::text AS kind \
     FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relname = $2";

/// Params: [schema]. Columns: tbl, name, full_type, col_key, nullable, and pg_type, pg_cat, pg_typtype.
pub const ALL_COLUMNS: &str = concat!(
    "SELECT c.relname::text AS tbl, a.attname::text AS name, format_type(a.atttypid, a.atttypmod) AS full_type, ",
    col_key!(),
    " AS col_key, (NOT a.attnotnull)::int AS nullable, ",
    pg_type!(),
    " FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace ",
    type_joins!(),
    " WHERE n.nspname = $1 AND ",
    listed!(),
    " AND a.attnum > 0 AND NOT a.attisdropped ORDER BY c.relname, a.attnum"
);

/// Params: [schema]. Columns: tbl, name.
pub const ALL_FOREIGN_KEYS: &str = "SELECT c.relname::text AS tbl, a.attname::text AS name FROM pg_constraint co JOIN pg_class c ON c.oid = co.conrelid \
     JOIN pg_namespace n ON n.oid = c.relnamespace CROSS JOIN LATERAL unnest(co.conkey) AS k(attnum) \
     JOIN pg_attribute a ON a.attrelid = co.conrelid AND a.attnum = k.attnum WHERE co.contype = 'f' AND n.nspname = $1";

/// Params: [column, schema, table]. Columns: est (NULL while unknown), leads (indexes that lead with the column and can
/// answer for every row, so not partial ones).
pub const VALUE_GATE: &str = "SELECT CASE WHEN c.reltuples > 0 THEN c.reltuples::bigint END AS est, \
     (SELECT count(*) FROM pg_index i JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = i.indkey[0] \
      WHERE i.indrelid = c.oid AND a.attname::text = $1 AND i.indisvalid AND i.indpred IS NULL)::bigint AS leads \
     FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $2 AND c.relname = $3";

/// No parameters. The schemas of the database the connection points at that hold user objects: not the system ones,
/// not TOAST or temporary ones, and only those the login may use.
pub const SCHEMAS: &str = "SELECT n.nspname::text AS name FROM pg_namespace n WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') \
     AND n.nspname NOT LIKE 'pg\\_toast%' AND n.nspname NOT LIKE 'pg\\_temp\\_%' AND has_schema_privilege(n.oid, 'USAGE') ORDER BY n.nspname";

/// No parameters. Every schema the login may use, with how many tables and views the sidebar would list in it and
/// whether it is a system one (the catalogs, TOAST and temporary schemas), so the schema switcher can hide those until
/// asked. Columns: name, system (0 or 1), tables.
pub const SCHEMAS_ALL: &str = concat!(
    "SELECT n.nspname::text AS name, (n.nspname IN ('pg_catalog', 'information_schema') OR n.nspname LIKE 'pg\\_toast%' OR n.nspname LIKE 'pg\\_temp\\_%')::int AS system, \
     (SELECT count(*) FROM pg_class c WHERE c.relnamespace = n.oid AND ",
    listed!(),
    ")::bigint AS tables FROM pg_namespace n WHERE has_schema_privilege(n.oid, 'USAGE') ORDER BY n.nspname"
);

/// Params: [schema, table]. The constraints a CREATE TABLE writes in its own text: primary key, unique, check and
/// exclusion, as `pg_get_constraintdef` words them. Foreign keys come from `FOREIGN_KEYS`, which spells the referenced
/// table with its schema whatever the search path is. Columns: name, kind (p, u, c or x), def.
pub const CONSTRAINTS: &str = "SELECT co.conname::text AS name, co.contype::text AS kind, pg_get_constraintdef(co.oid, true) AS def \
     FROM pg_constraint co JOIN pg_class c ON c.oid = co.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
     WHERE co.contype IN ('p', 'u', 'c', 'x') AND co.conparentid = 0 AND n.nspname = $1 AND c.relname = $2 \
     ORDER BY CASE co.contype WHEN 'p' THEN 0 WHEN 'u' THEN 1 WHEN 'x' THEN 2 ELSE 3 END, co.conname";
