//! Renaming and duplicating SQL tables, and the worker calls for them and for emptying a PostgreSQL table. MySQL and MariaDB are
//! here; PostgreSQL's statements and transactions are in `pg::tableops`, and the worker calls pick by dialect.

use super::create::{check_existing, check_name, created, folds_case, fresh_stats, not_system, split, stored_name, table_names, create_time, Ddl};
use super::ddl;
use super::worker::text;
use super::write::{writer_conn, SqlUndo};
use super::{se, Db, Dialect};
use crate::db::{Msg, Res, UndoAction, UndoKind, Worker};
use sqlx::Connection;

/// `RENAME TABLE a TO b`, which MySQL and MariaDB also use for a view. The new name stays in the same database.
pub fn rename_table_sql(d: Dialect, schema: &str, from: &str, to: &str, fold: bool) -> Res<String> {
    check_existing("database", schema)?;
    not_system(schema, fold)?;
    check_existing("table", from)?;
    check_name("table", to, true)?;
    Ok(format!("RENAME TABLE {} TO {}", d.qualified(schema, from), d.qualified(schema, to)))
}

/// The statements of a copy: the structure with its indexes (`CREATE TABLE ... LIKE`, which leaves foreign keys and triggers behind),
/// then the rows. `cols` are the columns to copy, listed one by one because a generated column refuses a value, and none means
/// structure only.
pub fn duplicate_stmts(d: Dialect, schema: &str, from: &str, to: &str, cols: Option<&[String]>, fold: bool) -> Res<Vec<String>> {
    check_existing("database", schema)?;
    not_system(schema, fold)?;
    check_existing("table", from)?;
    check_name("table", to, true)?;
    if from == to || (fold && from.eq_ignore_ascii_case(to)) {
        return Err("Give the copy a name other than the original's.".into());
    }
    let (a, b) = (d.qualified(schema, from), d.qualified(schema, to));
    let mut v = vec![format!("CREATE TABLE {b} LIKE {a}")];
    if let Some(cols) = cols.filter(|c| !c.is_empty()) {
        let list = cols.iter().map(|c| d.quote_ident(c)).collect::<Vec<_>>().join(", ");
        v.push(format!("INSERT INTO {b} ({list}) SELECT {list} FROM {a}"));
    }
    Ok(v)
}

/// The columns an INSERT can give a value: not the virtual or stored generated ones. MySQL marks a column with a default
/// expression `DEFAULT_GENERATED`, which still takes a value, so only the two generated kinds and MariaDB's older words are left out.
const INSERTABLE: &str = "SELECT COLUMN_NAME AS c FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND EXTRA NOT LIKE '%VIRTUAL%' AND EXTRA NOT LIKE '%STORED GENERATED%' AND EXTRA NOT LIKE '%PERSISTENT%' ORDER BY ORDINAL_POSITION";

async fn insertable(c: &mut sqlx::MySqlConnection, schema: &str, table: &str) -> Res<Vec<String>> {
    let rows = sqlx::query(INSERTABLE).bind(schema).bind(table).persistent(false).fetch_all(&mut *c).await.map_err(se)?;
    Ok(rows.iter().filter_map(|r| text(r, "c")).collect())
}

/// Renames on one connection. The answer is the stored name and the CREATE_TIME the undo checks.
async fn rename_my(db: &Db, schema: &str, from: &str, to: &str, read_only: bool) -> Res<(String, String)> {
    let mut c = writer_conn(db, "", read_only).await?.into_my()?;
    let out = async {
        let sql = rename_table_sql(db.dialect, schema, from, to, folds_case(&mut c).await?)?;
        ddl::exec(&mut c, &sql).await?;
        // A server that folds case stores the name in lower case, and the tabs, the sidebar and the undo use the stored one.
        let name = stored_name(&table_names(&mut c, db.dialect, schema).await.unwrap_or_default(), to);
        fresh_stats(&mut c, db.dialect).await;
        let token = create_time(&mut c, schema, &name).await.unwrap_or_default();
        Ok((name, token))
    }
    .await;
    let _ = c.close().await;
    out
}

/// Copies on one connection. MySQL's DDL commits by itself, so a failed INSERT leaves the new table behind; it is dropped again.
async fn duplicate_my(db: &Db, schema: &str, from: &str, to: &str, data: bool, read_only: bool) -> Res<(String, Option<String>)> {
    let mut c = writer_conn(db, "", read_only).await?.into_my()?;
    let out = async {
        let fold = folds_case(&mut c).await?;
        let cols = if data { Some(insertable(&mut c, schema, from).await?) } else { None };
        let stmts = duplicate_stmts(db.dialect, schema, from, to, cols.as_deref(), fold)?;
        ddl::exec(&mut c, &stmts[0]).await?;
        for s in &stmts[1..] {
            if let Err(e) = ddl::exec(&mut c, s).await {
                let _ = ddl::exec(&mut c, &format!("DROP TABLE {}", db.dialect.qualified(schema, to))).await;
                return Err(format!("{e}\nThe copy was removed again. Nothing was created."));
            }
        }
        let name = stored_name(&table_names(&mut c, db.dialect, schema).await.unwrap_or_default(), to);
        fresh_stats(&mut c, db.dialect).await;
        let made = create_time(&mut c, schema, &name).await;
        Ok((name, made))
    }
    .await;
    let _ = c.close().await;
    out
}

/// Renames a table back, while it is still the one that was renamed.
async fn undo_rename_my(db: &Db, schema: &str, now: &str, from: &str, token: &str, read_only: bool) -> Res<()> {
    let mut c = writer_conn(db, "", read_only).await?.into_my()?;
    let out = async {
        fresh_stats(&mut c, db.dialect).await;
        if !token.is_empty() && create_time(&mut c, schema, now).await.as_deref() != Some(token) {
            return Err(format!("Not undone: {now} was dropped and made again since, so it is not the table that was renamed."));
        }
        let sql = rename_table_sql(db.dialect, schema, now, from, folds_case(&mut c).await?)?;
        ddl::exec(&mut c, &sql).await
    }
    .await;
    let _ = c.close().await;
    out
}

pub(super) fn renamed(conn: u64, database: &str, schema: &str, now: &str, from: &str, token: String) -> UndoAction {
    UndoAction { agent: None, at: std::time::Instant::now(), conn, db: schema.into(), coll: now.into(), kind: UndoKind::Sql { database: database.into(), cols: Vec::new(), undo: SqlUndo::Renamed { from: from.into(), token }, ids: Default::default() } }
}

impl Worker {
    /// Renames a table or view of `schema`, or the PostgreSQL schema `schema` itself (`whole`, with `from` the same name).
    #[allow(clippy::too_many_arguments)]
    pub fn sql_rename(&self, conn: u64, db: Db, schema: String, from: String, to: String, whole: bool, read_only: bool) {
        self.spawn_sql(db.conn, &schema.clone(), async move {
            let database = db.pg_database();
            let res = if db.dialect == Dialect::Postgres { super::pg::tableops::rename(&db, &schema, &from, &to, whole, read_only).await } else { rename_my(&db, &schema, &from, &to, read_only).await };
            let (res, undo, now) = match res {
                Ok((now, token)) => {
                    // The entry of a schema rename sits on the new schema and has no table.
                    let u = if whole { renamed(conn, &database, &now, "", &schema, token) } else { renamed(conn, &database, &schema, &now, &from, token) };
                    (Ok(format!("Renamed {from} to {now}")), Some(u), now)
                }
                Err(e) => (Err(e), None, to.clone()),
            };
            Msg::SqlRenamed { conn, database, schema, from, to: now, whole, res, undo }
        });
    }

    /// Undoes a rename while the object is still the one renamed. A refused undo hands the entry back.
    pub(in crate::db::sql) fn sql_undo_rename(&self, db: Db, a: UndoAction, read_only: bool) {
        let scope = a.db.clone();
        self.spawn_sql(db.conn, &scope, async move {
            let UndoAction { at, conn, db: schema, coll: now, kind, .. } = a;
            let UndoKind::Sql { database, undo: SqlUndo::Renamed { from, token }, .. } = &kind else { return Msg::SqlRenamed { conn, database: String::new(), schema, from: now.clone(), to: now, whole: false, res: Err("Not a rename".into()), undo: None } };
            let (from, token, database) = (from.clone(), token.clone(), database.clone());
            let whole = now.is_empty();
            let res = if db.dialect == Dialect::Postgres { super::pg::tableops::undo_rename(&db, &schema, &now, &from, &token, read_only).await } else { undo_rename_my(&db, &schema, &now, &from, &token, read_only).await };
            let (to, said) = (from.clone(), format!("Renamed back to {from}"));
            let undo = res.is_err().then(|| UndoAction { agent: None, at, conn, db: schema.clone(), coll: now.clone(), kind });
            let old = if whole { schema.clone() } else { now.clone() };
            Msg::SqlRenamed { conn, database, schema, from: old, to, whole, res: res.map(|_| said), undo }
        });
    }

    /// Copies a table of `schema`, with its rows when `data`. Only a copy of the structure gets an undo: it is dropped while it is
    /// still empty, which a copy with rows never is.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_duplicate(&self, conn: u64, db: Db, schema: String, from: String, to: String, data: bool, read_only: bool) {
        self.spawn_sql(db.conn, &schema.clone(), async move {
            let database = db.pg_database();
            let (res, undo, name) = if db.dialect == Dialect::Postgres {
                match super::pg::tableops::duplicate(&db, conn, &schema, &from, &to, data, read_only).await {
                    Ok(u) => (Ok(None), u, to.clone()),
                    Err(e) => (Err(e), None, to.clone()),
                }
            } else {
                match duplicate_my(&db, &schema, &from, &to, data, read_only).await {
                    Ok((name, made)) => (Ok(None), (!data).then(|| created(conn, &schema, &name, SqlUndo::CreatedTable { made })), name),
                    Err(e) => (Err(e), None, to.clone()),
                }
            };
            let (res, _) = split(res, format!("Copied {from} to {name}{}", if data { " with its rows" } else { "" }));
            Msg::SqlDdl { conn, database, schema, table: name, what: Ddl::CreatedTable, res, undo, dbs: None, sql_mode: None }
        });
    }

    /// TRUNCATE of a PostgreSQL table.
    #[allow(clippy::too_many_arguments)]
    pub fn pg_truncate(&self, conn: u64, db: Db, schema: String, table: String, restart: bool, cascade: bool, read_only: bool) {
        self.spawn_sql(db.conn, &schema.clone(), async move {
            let res = super::pg::tableops::truncate(&db, &schema, &table, restart, cascade, read_only).await;
            let (res, _) = split(res.map(|_| None), format!("Emptied table {table}"));
            Msg::SqlDdl { conn, database: db.pg_database(), schema, table, what: Ddl::Emptied, res, undo: None, dbs: None, sql_mode: None }
        });
    }

    /// What emptying a PostgreSQL table reaches: its rows and the tables that point at it.
    pub fn pg_truncate_info(&self, conn: u64, db: Db, database: String, schema: String, table: String) {
        self.spawn_sql(db.conn, &schema.clone(), async move {
            let res = super::pg::tableops::truncate_info(&db, &schema, &table).await;
            Msg::SqlDropInfo { conn, database, schema, table, res }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MY: Dialect = Dialect::MySql;

    #[test]
    fn a_rename_stays_in_its_database_and_is_quoted_per_dialect() {
        assert_eq!(rename_table_sql(MY, "shop", "orders", "orders_old", false).unwrap(), "RENAME TABLE `shop`.`orders` TO `shop`.`orders_old`");
        assert_eq!(rename_table_sql(Dialect::MariaDb, "sh`op", "a", "b`c", false).unwrap(), "RENAME TABLE `sh``op`.`a` TO `sh``op`.`b``c`");
        assert!(rename_table_sql(MY, "mysql", "user", "u", false).is_err(), "a system database is left alone");
        assert!(rename_table_sql(MY, "MySQL", "user", "u", true).is_err(), "and so is its other spelling where names fold case");
        assert!(rename_table_sql(MY, "shop", "a", "", false).is_err());
        assert!(rename_table_sql(MY, "shop", "a", "x.y", false).is_err(), "a dot in a new name would read as a database");
    }

    #[test]
    fn a_copy_lists_its_columns_and_never_copies_onto_itself() {
        let cols = vec!["id".to_string(), "to`tal".to_string()];
        let v = duplicate_stmts(MY, "shop", "orders", "orders_copy", Some(&cols), false).unwrap();
        assert_eq!(v, vec!["CREATE TABLE `shop`.`orders_copy` LIKE `shop`.`orders`".to_string(), "INSERT INTO `shop`.`orders_copy` (`id`, `to``tal`) SELECT `id`, `to``tal` FROM `shop`.`orders`".to_string()]);
        assert_eq!(duplicate_stmts(MY, "shop", "orders", "orders_copy", None, false).unwrap().len(), 1, "structure only has no INSERT");
        assert_eq!(duplicate_stmts(MY, "shop", "orders", "orders_copy", Some(&[]), false).unwrap().len(), 1, "a table of generated columns has no row to copy");
        assert!(duplicate_stmts(MY, "shop", "orders", "orders", None, false).is_err());
        assert!(duplicate_stmts(MY, "shop", "orders", "ORDERS", None, true).is_err(), "the same name where case folds");
        assert!(duplicate_stmts(MY, "shop", "orders", "ORDERS", None, false).is_ok());
    }

    #[test]
    fn only_generated_columns_are_left_out_of_a_copy() {
        assert!(INSERTABLE.contains("NOT LIKE '%VIRTUAL%'") && INSERTABLE.contains("NOT LIKE '%STORED GENERATED%'"));
        assert!(!INSERTABLE.contains("NOT LIKE '%GENERATED%'"), "DEFAULT_GENERATED columns take values");
    }
}
