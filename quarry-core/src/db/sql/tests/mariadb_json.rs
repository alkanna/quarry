//! Live test of MariaDB's JSON columns, which are LONGTEXT with a json_valid CHECK and read as JSON everywhere a column kind
//! comes from. Needs QUARRY_TEST_MARIADB (QUARRY_TEST_MYSQL is skipped: MySQL has a real JSON type).

use crate::db::sql::testing::every_server;
use crate::db::sql::value::Kind;
use crate::db::sql::{PageQuery, Dialect};
use crate::db::Msg;

#[test]
fn live_mariadb_json_columns_are_json_and_plain_longtext_is_not() {
    every_server(|l, var| {
        if l.db.dialect != Dialect::MariaDb {
            return;
        }
        let schema = "quarry_mdb_json";
        l.sql(&format!("DROP DATABASE IF EXISTS {schema}; CREATE DATABASE {schema}"));
        l.sql(&format!("CREATE TABLE {schema}.t (id INT PRIMARY KEY, j JSON, c LONGTEXT CHECK (json_valid(c)), plain LONGTEXT, other LONGTEXT CHECK (CHAR_LENGTH(other) < 100))"));
        l.sql(&format!("INSERT INTO {schema}.t VALUES (1, '{{\"a\": [1, 2]}}', '[true]', 'not json', 'x')"));
        let kind = |cols: &[crate::db::sql::Col], n: &str| cols.iter().find(|c| c.name == n).map(|c| c.kind);
        // The grid's columns.
        l.w.sql_page(1, l.db.clone(), schema.into(), "t".into(), PageQuery::default(), 10, 0);
        let Msg::SqlCols { cols, .. } = l.rx.recv_timeout(std::time::Duration::from_secs(15)).unwrap() else { panic!("{var}: expected SqlCols") };
        let Msg::Docs { res: Ok((rows, _, _)), .. } = l.rx.recv_timeout(std::time::Duration::from_secs(15)).unwrap() else { panic!("{var}: expected Docs") };
        assert_eq!((kind(&cols, "j"), kind(&cols, "c"), kind(&cols, "plain"), kind(&cols, "other")), (Some(Kind::Json), Some(Kind::Json), Some(Kind::Text), Some(Kind::Text)), "{var}");
        // A JSON value still reads as the text the server holds.
        let doc = rows[0].decode().unwrap();
        assert!(doc.get_str("j").unwrap().contains("\"a\""), "{var}");
        // The structure view and the schema (autocomplete and rule chips) agree.
        let st = l.w.rt.block_on(crate::db::sql::structure::load(&l.db, schema, "t")).unwrap();
        let sk = |n: &str| st.columns.iter().find(|c| c.name == n).and_then(|c| c.kind);
        assert_eq!((sk("j"), sk("c"), sk("plain"), sk("other")), (Some(Kind::Json), Some(Kind::Json), None, None), "{var}");
        let pairs = l.w.rt.block_on(crate::db::sql::worker::json_alias_cols(&l.db, schema, None));
        assert!(pairs.contains(&("t".to_string(), "j".to_string())) && pairs.contains(&("t".to_string(), "c".to_string())) && !pairs.iter().any(|(_, c)| c == "plain" || c == "other"), "{var}: {pairs:?}");
        l.sql(&format!("DROP DATABASE {schema}"));
    });
}
