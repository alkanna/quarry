//! Inline editing of SQL grid cells, from the first click to the row on the server.

use super::*;
use crate::db::sql::testing::{PG_ENVS, live_pg};

#[test]
fn pg_inline_edit_saves() {
    for var in PG_ENVS {
        let Some(l) = live_pg(var) else { continue };
        l.quarry_test();
        let pool = l.fixture_pool();
        let table = format!("ui_edit_{}_{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
        l.qsql(&pool, &format!("CREATE TABLE {table} (id int PRIMARY KEY, name text); INSERT INTO {table} VALUES (1, 'alpha'), (2, 'beta')"));
        let mut ui = ui_app(var).unwrap();
        ui.open_table("quarry_test", "public", &table);
        ui.unlock();
        ui.edit_cell("name", 1, "gamma");
        ui.save();
        let rows = l.qone(&pool, &format!("SELECT string_agg(id || ':' || name, ',' ORDER BY id) FROM {table}"));
        l.qsql(&pool, &format!("DROP TABLE {table}"));
        assert_eq!(rows, "1:gamma,2:beta", "{var}");
    }
}
