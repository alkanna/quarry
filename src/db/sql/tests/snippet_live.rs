//! Live tests of snippet values: what a value turns into reads back as the same value on every MySQL, MariaDB and
//! PostgreSQL server named by the QUARRY_TEST_ variables, whatever quotes and backslashes it holds.

use crate::db::sql::testing::{every_pg, every_server};
use crate::db::sql::Dialect;
use crate::library::snippet::{fill, parse};

const TEXT: &str = "SELECT {{q}} AS v";
const EVIL: [&str; 4] = ["it's", "back\\slash \\' end", "x' OR '1'='1", "-- not a comment /* nor this */"];

fn filled(t: &str, v: &str, d: Dialect) -> String {
    fill(t, &parse(t, d), &[v.to_string()], d, true).unwrap()
}

#[test]
fn live_a_text_value_reads_back_unchanged_on_mysql_and_mariadb() {
    every_server(|l, var| {
        for v in EVIL {
            assert_eq!(l.one(&filled(TEXT, v, Dialect::MySql)), v, "{var}: {v}");
        }
        let t = "SELECT {{d:date}} AS d, {{n:int}} + 1 AS n, {{c:identifier}} FROM (SELECT 7 AS `odd``name`) x WHERE 7 IN ({{ids:int list}})";
        let p = parse(t, Dialect::MySql);
        let sql = fill(t, &p, &["2026-11-03".into(), "41".into(), "odd`name".into(), "7, 8".into()], Dialect::MySql, true).unwrap();
        assert_eq!((l.cell(&sql, 0), l.cell(&sql, 1), l.cell(&sql, 2)), ("2026-11-03".into(), "42".into(), "7".into()), "{var}");
    });
}

#[test]
fn live_a_text_value_reads_back_unchanged_on_postgres() {
    every_pg(|l, var| {
        for v in EVIL {
            assert_eq!(l.one(&filled(TEXT, v, Dialect::Postgres)), v, "{var}: {v}");
        }
        assert_eq!(l.one(&filled("SELECT {{d:date}}::date::text", "2026-11-03", Dialect::Postgres)), "2026-11-03", "{var}");
        assert_eq!(l.one(&filled("SELECT 1 FROM (SELECT 1 AS \"a\"\"b\") x WHERE {{c:identifier}} = 1", "a\"b", Dialect::Postgres)), "1", "{var}");
    });
}
