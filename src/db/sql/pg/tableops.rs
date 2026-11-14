//! PostgreSQL: renaming a table, view or schema, copying a table, and emptying one. Each runs in one transaction, so a failed
//! step leaves nothing behind; `db::sql::tableops` picks these for a PostgreSQL connection.

use super::create::{create_schema_sql, drop_word, entry, in_tx, namespace_oid, relation_oid, state_in, system_schema};
use crate::db::errors::truncate_refused;
use crate::db::sql::create::DropInfo;
use crate::db::sql::exec::{sql_state, Cells};
use crate::db::sql::pg::ddl::{exec, lock_error, name_ok, q};
use crate::db::sql::structure::num;
use crate::db::sql::value::Param;
use crate::db::sql::worker::text;
use crate::db::sql::write::SqlUndo;
use crate::db::sql::{se, Db, Dialect};
use crate::db::{Res, UndoAction};

const D: Dialect = Dialect::Postgres;

/// A name this app gives: PostgreSQL would take a tab or a newline, but nobody could type it back.
fn new_name(what: &str, name: &str) -> Res<()> {
    name_ok(name, what)?;
    if name.chars().any(|c| c.is_control()) {
        return Err(format!("{what} name cannot contain control characters or line breaks."));
    }
    Ok(())
}

fn existing(what: &str, name: &str) -> Res<()> {
    if name.is_empty() {
        return Err(format!("{what} needs a name."));
    }
    if system_schema(name) && what == "A schema" {
        return Err(format!("{name} belongs to the server and is left alone."));
    }
    Ok(())
}

/// `word` is the keyword of the relation (`create::drop_word`), because the server refuses ALTER VIEW on a table and the reverse.
/// The new name is not qualified: the object stays in its schema.
pub fn rename_sql(word: &str, schema: &str, from: &str, to: &str) -> Res<String> {
    existing("A schema", schema)?;
    existing("A table", from)?;
    new_name("A table", to)?;
    if from == to {
        return Err("Give it a name other than the current one.".into());
    }
    Ok(format!("ALTER {word} {} RENAME TO {}", D.qualified(schema, from), q(to)))
}

pub fn rename_schema_sql(from: &str, to: &str) -> Res<String> {
    existing("A schema", from)?;
    // The same rules as for a new schema: the names PostgreSQL keeps for itself stay out.
    create_schema_sql(to)?;
    if from == to {
        return Err("Give it a name other than the current one.".into());
    }
    Ok(format!("ALTER SCHEMA {} RENAME TO {}", q(from), q(to)))
}

/// `restart` is RESTART IDENTITY, which puts the sequences of the table's columns back to their start; PostgreSQL's own default
/// keeps them going. `cascade` also empties every table with a foreign key to this one, however deep.
pub fn truncate_sql(schema: &str, table: &str, restart: bool, cascade: bool) -> Res<String> {
    existing("A schema", schema)?;
    existing("A table", table)?;
    if system_schema(schema) {
        return Err(format!("{schema} belongs to the server and is left alone."));
    }
    Ok(format!("TRUNCATE TABLE {}{}{}", D.qualified(schema, table), if restart { " RESTART IDENTITY" } else { "" }, if cascade { " CASCADE" } else { "" }))
}

/// One column a copy gives a value: its name and `attidentity` ('a' ALWAYS, 'd' BY DEFAULT, or none).
#[derive(Clone, Debug, PartialEq)]
pub struct CopyCol {
    pub name: String,
    pub identity: Option<char>,
}

/// The statements of a copy. `LIKE ... INCLUDING ALL` takes the columns, defaults, identity and generated columns, check
/// constraints, indexes and comments, and never a foreign key or a trigger. With `cols` the rows follow, in the columns that take a
/// value (a generated column refuses one), with OVERRIDING SYSTEM VALUE when an identity column is ALWAYS. The copy's identity
/// sequences are new and start at 1, so each is moved up to the largest value copied or the next insert would collide.
pub fn duplicate_stmts(schema: &str, from: &str, to: &str, cols: Option<&[CopyCol]>) -> Res<Vec<String>> {
    existing("A schema", schema)?;
    existing("A table", from)?;
    new_name("A table", to)?;
    if system_schema(schema) {
        return Err(format!("{schema} belongs to the server and is left alone."));
    }
    if from == to {
        return Err("Give the copy a name other than the original's.".into());
    }
    let (a, b) = (D.qualified(schema, from), D.qualified(schema, to));
    let mut v = vec![format!("CREATE TABLE {b} (LIKE {a} INCLUDING ALL)")];
    let Some(cols) = cols.filter(|c| !c.is_empty()) else { return Ok(v) };
    let list = cols.iter().map(|c| q(&c.name)).collect::<Vec<_>>().join(", ");
    let over = if cols.iter().any(|c| c.identity == Some('a')) { "OVERRIDING SYSTEM VALUE " } else { "" };
    v.push(format!("INSERT INTO {b} ({list}) {over}SELECT {list} FROM {a}"));
    let lit = |s: &str| format!("'{}'", s.replace('\'', "''"));
    for c in cols.iter().filter(|c| c.identity.is_some()) {
        v.push(format!("SELECT setval(pg_get_serial_sequence({}, {}), m) FROM (SELECT max({}) AS m FROM {b}) t WHERE m IS NOT NULL", lit(&b), lit(&c.name), q(&c.name)));
    }
    Ok(v)
}

const KIND: &str = "SELECT c.relkind::text AS kind FROM pg_class c WHERE c.oid = to_regclass($1)";

/// The columns a copy gives a value, in table order.
const COPY_COLS: &str = "SELECT a.attname::text AS name, a.attidentity::text AS ident FROM pg_attribute a WHERE a.attrelid = to_regclass($1) AND a.attnum > 0 AND NOT a.attisdropped AND a.attgenerated = '' ORDER BY a.attnum";

/// The table `to_regclass` finds for the name, and every table that points at it with a foreign key, through any number of steps:
/// what TRUNCATE ... CASCADE empties besides it. UNION drops a row it has seen, so a key that loops back ends the walk.
const REFERENCING: &str = "WITH RECURSIVE r(oid) AS (
    SELECT to_regclass($1)::oid
    UNION
    SELECT k.conrelid FROM pg_constraint k JOIN r ON k.confrelid = r.oid WHERE k.contype = 'f'
)
SELECT r.oid::regclass::text AS name, c.reltuples::bigint AS est FROM r JOIN pg_class c ON c.oid = r.oid WHERE r.oid <> to_regclass($1)::oid ORDER BY 1";

/// Renames, and answers the stored name and the oid the undo checks.
pub(in crate::db::sql) async fn rename(db: &Db, schema: &str, from: &str, to: &str, whole: bool, read_only: bool) -> Res<(String, String)> {
    if whole {
        let sql = rename_schema_sql(from, to)?;
        let oid = in_tx(db, from, read_only, async |tx| {
            exec(tx, db, from, &sql).await.map_err(|x| lock_error(&x, from).unwrap_or_else(|| se(x)))?;
            namespace_oid(tx, to).await?.ok_or_else(|| "The schema was renamed but cannot be read back.".to_string())
        })
        .await?;
        return Ok((to.to_string(), oid));
    }
    let oid = in_tx(db, schema, read_only, async |tx| {
        let kind = tx.fetch_bound(KIND, &[Param::Text(D.qualified(schema, from))]).await.map_err(se)?;
        let Some(kind) = kind.first().and_then(|r| text(r, "kind")) else { return Err(format!("{from} is not in {schema} any more.")) };
        let sql = rename_sql(drop_word(&kind), schema, from, to)?;
        exec(tx, db, schema, &sql).await.map_err(|x| lock_error(&x, from).unwrap_or_else(|| se(x)))?;
        relation_oid(tx, &D.qualified(schema, to)).await?.ok_or_else(|| "The table was renamed but cannot be read back.".to_string())
    })
    .await?;
    Ok((to.to_string(), oid))
}

/// Renames back while the object is still the one that was renamed, by oid. `now` is empty for a schema, whose current name is `schema`.
pub(in crate::db::sql) async fn undo_rename(db: &Db, schema: &str, now: &str, from: &str, token: &str, read_only: bool) -> Res<()> {
    let gone = |what: &str| format!("Not undone: {what} was dropped and made again since, so it is not the one that was renamed.");
    if now.is_empty() {
        let sql = rename_schema_sql(schema, from)?;
        return in_tx(db, schema, read_only, async |tx| {
            if !token.is_empty() && namespace_oid(tx, schema).await?.as_deref() != Some(token) {
                return Err(gone(schema));
            }
            exec(tx, db, schema, &sql).await.map_err(|x| lock_error(&x, schema).unwrap_or_else(|| se(x)))
        })
        .await;
    }
    in_tx(db, schema, read_only, async |tx| {
        let name = D.qualified(schema, now);
        let kind = tx.fetch_bound(KIND, &[Param::Text(name.clone())]).await.map_err(se)?;
        let Some(kind) = kind.first().and_then(|r| text(r, "kind")) else { return Err(format!("Not undone: {now} is not in {schema} any more.")) };
        if !token.is_empty() && relation_oid(tx, &name).await?.as_deref() != Some(token) {
            return Err(gone(now));
        }
        let sql = rename_sql(drop_word(&kind), schema, now, from)?;
        exec(tx, db, schema, &sql).await.map_err(|x| lock_error(&x, now).unwrap_or_else(|| se(x)))
    })
    .await
}

/// Copies in one transaction, so a failed INSERT takes the new table with it. A copy without rows answers the undo that drops it.
pub(in crate::db::sql) async fn duplicate(db: &Db, conn: u64, schema: &str, from: &str, to: &str, data: bool, read_only: bool) -> Res<Option<UndoAction>> {
    let made = in_tx(db, schema, read_only, async |tx| {
        let name = D.qualified(schema, from);
        let kind = tx.fetch_bound(KIND, &[Param::Text(name.clone())]).await.map_err(se)?;
        match kind.first().and_then(|r| text(r, "kind")).as_deref() {
            Some("r" | "p") => {}
            Some(_) => return Err(format!("{from} is not a table, and only a table can be copied.")),
            None => return Err(format!("{from} is not in {schema} any more.")),
        }
        let cols = if data {
            let rows = tx.fetch_bound(COPY_COLS, &[Param::Text(name)]).await.map_err(se)?;
            Some(rows.iter().filter_map(|r| Some(CopyCol { name: text(r, "name")?, identity: text(r, "ident").and_then(|s| s.chars().next()).filter(|c| matches!(c, 'a' | 'd')) })).collect::<Vec<_>>())
        } else {
            None
        };
        let stmts = duplicate_stmts(schema, from, to, cols.as_deref())?;
        for (i, s) in stmts.iter().enumerate() {
            exec(tx, db, schema, s).await.map_err(|x| lock_error(&x, from).unwrap_or_else(|| if stmts.len() > 1 { format!("{} (statement {} of {}). Nothing was created.", se(x), i + 1, stmts.len()) } else { se(x) }))?;
        }
        let oid = relation_oid(tx, &D.qualified(schema, to)).await?.ok_or_else(|| "The table was made but cannot be read back.".to_string())?;
        let state = state_in(tx, schema, to).await?;
        Ok((oid, state))
    })
    .await?;
    Ok((!data).then(|| entry(db, conn, schema, to, SqlUndo::PgCreatedTable { oid: made.0, state: made.1 })))
}

pub(in crate::db::sql) async fn truncate(db: &Db, schema: &str, table: &str, restart: bool, cascade: bool, read_only: bool) -> Res<()> {
    let sql = truncate_sql(schema, table, restart, cascade)?;
    in_tx(db, schema, read_only, async |tx| {
        exec(tx, db, schema, &sql).await.map_err(|x| match (sql_state(&x).as_deref(), lock_error(&x, table)) {
            (_, Some(why)) => why,
            // 0A000 is feature_not_supported, which is what a table other tables point at answers to a TRUNCATE without CASCADE.
            (Some("0A000"), _) => truncate_refused(&se(x)),
            _ => se(x),
        })
    })
    .await
}

/// The rows of the table, and the tables an emptying with CASCADE reaches besides it (`referenced_by`, with their estimated rows).
pub(in crate::db::sql) async fn truncate_info(db: &Db, schema: &str, table: &str) -> Res<DropInfo> {
    let name = D.qualified(schema, table);
    let sql = "SELECT c.reltuples::bigint AS est FROM pg_class c WHERE c.oid = to_regclass($1) AND c.relkind IN ('r', 'p')";
    let found = super::create::read(db, schema, &[(sql, vec![Param::Text(name.clone())])]).await?.remove(0);
    let Some(row) = found.first() else { return Ok(DropInfo::default()) };
    let mut info = DropInfo { exists: true, rows: row.int("est").filter(|n| *n >= 0).map(|n| n as u64), rows_estimated: true, ..Default::default() };
    // An exact count within a short wait, else the estimate stays and the dialog says "about".
    let count = format!("SELECT count(*)::bigint AS n FROM {name}");
    let exact = super::create::read(db, schema, &[("SET LOCAL statement_timeout = '2s'", Vec::new()), (&count, Vec::new())]).await.ok().and_then(|mut all| all.pop()).and_then(|rows| rows.first().and_then(|r| num(r, "n")));
    if let Some(n) = exact {
        (info.rows, info.rows_estimated) = (Some(n), false);
    }
    let refs = super::create::read(db, schema, &[(REFERENCING, vec![Param::Text(name)])]).await?.remove(0);
    info.referenced_by = refs
        .iter()
        .filter_map(|r| {
            let n = text(r, "name")?;
            Some(match r.int("est").filter(|e| *e > 0) {
                Some(e) => format!("{n} (about {e} row{})", if e == 1 { "" } else { "s" }),
                None => n,
            })
        })
        .collect();
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rename_uses_the_keyword_of_the_relation_and_quotes_both_names() {
        assert_eq!(rename_sql("TABLE", "public", "orders", "Orders \"old\"").unwrap(), "ALTER TABLE \"public\".\"orders\" RENAME TO \"Orders \"\"old\"\"\"");
        assert_eq!(rename_sql(drop_word("v"), "app", "v1", "v2").unwrap(), "ALTER VIEW \"app\".\"v1\" RENAME TO \"v2\"");
        assert_eq!(rename_sql(drop_word("m"), "app", "m1", "m2").unwrap(), "ALTER MATERIALIZED VIEW \"app\".\"m1\" RENAME TO \"m2\"");
        assert!(rename_sql("TABLE", "pg_catalog", "pg_class", "x").is_err(), "a system schema is left alone");
        assert!(rename_sql("TABLE", "public", "a", "a").is_err());
        assert!(rename_sql("TABLE", "public", "a", "").is_err());
        assert!(rename_sql("TABLE", "public", "a", &"x".repeat(64)).unwrap_err().contains("63 bytes"));
    }

    #[test]
    fn a_schema_rename_keeps_to_the_rules_of_a_new_schema() {
        assert_eq!(rename_schema_sql("sales", "Sales 2").unwrap(), "ALTER SCHEMA \"sales\" RENAME TO \"Sales 2\"");
        assert!(rename_schema_sql("public", "x").is_ok(), "public is an ordinary schema, only the server's own are refused");
        assert!(rename_schema_sql("pg_catalog", "x").is_err());
        assert!(rename_schema_sql("information_schema", "x").is_err());
        assert!(rename_schema_sql("sales", "pg_mine").is_err(), "pg_ is kept for the server");
        assert!(rename_schema_sql("sales", "sales").is_err());
    }

    #[test]
    fn an_empty_names_its_options_and_never_defaults_to_cascade() {
        assert_eq!(truncate_sql("public", "orders", false, false).unwrap(), "TRUNCATE TABLE \"public\".\"orders\"");
        assert_eq!(truncate_sql("public", "orders", true, false).unwrap(), "TRUNCATE TABLE \"public\".\"orders\" RESTART IDENTITY");
        assert_eq!(truncate_sql("public", "orders", true, true).unwrap(), "TRUNCATE TABLE \"public\".\"orders\" RESTART IDENTITY CASCADE");
        assert_eq!(truncate_sql("public", "orders", false, true).unwrap(), "TRUNCATE TABLE \"public\".\"orders\" CASCADE");
        assert!(truncate_sql("pg_catalog", "pg_class", false, false).is_err());
    }

    fn col(name: &str, identity: Option<char>) -> CopyCol {
        CopyCol { name: name.into(), identity }
    }

    #[test]
    fn a_copy_takes_everything_but_keys_to_other_tables() {
        let v = duplicate_stmts("public", "orders", "orders_copy", None).unwrap();
        assert_eq!(v, vec!["CREATE TABLE \"public\".\"orders_copy\" (LIKE \"public\".\"orders\" INCLUDING ALL)".to_string()]);
        assert!(duplicate_stmts("public", "orders", "orders", None).is_err());
        assert!(duplicate_stmts("pg_catalog", "a", "b", None).is_err());
    }

    #[test]
    fn a_copy_with_rows_lists_its_columns_and_moves_the_identity_on() {
        let cols = [col("id", Some('a')), col("note", None), col("it's", Some('d'))];
        let v = duplicate_stmts("app", "t", "t2", Some(&cols)).unwrap();
        assert_eq!(v[1], "INSERT INTO \"app\".\"t2\" (\"id\", \"note\", \"it's\") OVERRIDING SYSTEM VALUE SELECT \"id\", \"note\", \"it's\" FROM \"app\".\"t\"");
        assert_eq!(v[2], "SELECT setval(pg_get_serial_sequence('\"app\".\"t2\"', 'id'), m) FROM (SELECT max(\"id\") AS m FROM \"app\".\"t2\") t WHERE m IS NOT NULL");
        assert!(v[3].contains("pg_get_serial_sequence('\"app\".\"t2\"', 'it''s')"), "{}", v[3]);
        assert_eq!(v.len(), 4);
        // BY DEFAULT identity and plain columns take values without the override.
        let v = duplicate_stmts("app", "t", "t2", Some(&[col("id", Some('d')), col("n", None)])).unwrap();
        assert!(!v[1].contains("OVERRIDING") && v.len() == 3, "{v:?}");
        let v = duplicate_stmts("app", "t", "t2", Some(&[col("n", None)])).unwrap();
        assert_eq!(v.len(), 2, "no identity, no sequence to move");
        assert_eq!(duplicate_stmts("app", "t", "t2", Some(&[])).unwrap().len(), 1, "nothing to copy into");
    }
}
