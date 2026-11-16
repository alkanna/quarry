//! Live tests of the filter panel's SQL: every operator the rules write runs on every server, case matching finds the
//! rows it says, and the count and column facts read what the panel shows. Each test makes its own database or schema.

use crate::db::sql::facts::{col_facts, filter_count};
use crate::db::sql::rules::{self, group_of, Group, Op, Rule};
use crate::db::sql::testing::{every_pg, every_server, unique_name, MyFixture};
use crate::db::sql::value::Kind;
use crate::db::sql::Dialect;

/// Values an operator can run with on these tables: numbers in number columns, dates in date ones, and for LIKE a quote,
/// wildcards and a backslash.
fn values(op: Op, kind: Kind) -> Vec<String> {
    use Op::*;
    let g = group_of(kind);
    let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect();
    match op {
        Between if g == Group::Number => v(&["1", "9"]),
        Between => v(&["2024-01-01", "2030-01-01"]),
        In | NotIn | NotInOrNull if g == Group::Number => v(&["1", "2"]),
        In | NotIn | NotInOrNull if kind == Kind::Bool => v(&["true", "false"]),
        In | NotIn | NotInOrNull => v(&["a", "50%_off\\"]),
        ArrayHasAll | ArrayHasAny => v(&["1", "a"]),
        Last => v(&["3", "weeks"]),
        AfterCol => v(&["seen_at"]),
        JsonPathEq | JsonPathNe => v(&["$.a.b", "v"]),
        JsonPathGt | JsonPathLt => v(&["$.n", "5"]),
        JsonContains => v(&["{\"a\": {}}"]),
        JsonHasPath => v(&["$.a"]),
        JsonHasKey => v(&["a"]),
        LenEq | LenGt | LenLt | ArrayLenEq | ArrayLenGt | ArrayLenLt => v(&["3"]),
        Eq | Ne if kind == Kind::Binary => v(&["0x5A3F"]),
        Eq | Ne if kind == Kind::Json => v(&["{\"a\": 1}"]),
        Eq | Ne | Gt | Lt | Ge | Le | Distinct if g == Group::Number => v(&["5.5"]),
        Eq | Ne | Gt | Lt | Distinct if g == Group::Date => v(&["2024-01-02 03:04:05"]),
        Eq | Ne | Distinct if kind == Kind::Uuid => v(&["00000000-0000-0000-0000-000000000000"]),
        Eq | Ne | Distinct if kind == Kind::Bool => v(&["true"]),
        Contains | NotContains | StartsWith | NotStartsWith | EndsWith | NotEndsWith => v(&["a%b_c'd\\"]),
        Regexp | NotRegexp => v(&["^a"]),
        _ => rules::default_values(op).into_iter().map(|_| "a%b_c'd".to_string()).collect(),
    }
}

/// Every rule of every column, with case matched and not where the operator has the choice.
fn every_rule(cols: &[(&str, Kind)], d: Dialect) -> Vec<Rule> {
    let mut out = Vec::new();
    for &(col, kind) in cols {
        for &op in rules::ops_for(kind, d) {
            for case in [false, true] {
                if case && !rules::has_case(kind, op) {
                    continue;
                }
                out.push(Rule { col: col.into(), kind, op, values: values(op, kind), case: if rules::has_case(kind, op) { case } else { rules::default_case(op, d) } });
            }
        }
    }
    out
}

#[test]
fn live_every_filter_op_runs_on_mysql_and_mariadb() {
    every_server(|l, var| {
        let s = unique_name("qfl");
        let _fx = MyFixture::new(l, &s, "");
        l.sql(&format!("CREATE TABLE `{s}`.t (id INT PRIMARY KEY, total DECIMAL(8,2), name VARCHAR(20), created_at DATETIME, seen_at DATETIME, status ENUM('a','b'), tags SET('a','b'), data JSON, `blob` BLOB)"));
        l.sql(&format!("INSERT INTO `{s}`.t VALUES (1, 5.5, 'Ann', NOW(), NOW(), 'a', 'a,b', '{{\"a\": {{\"b\": \"v\"}}, \"n\": 7}}', 0x5A3F), (2, 1, 'ann', NOW(), NOW(), 'b', 'b', '{{\"c\": 1}}', NULL)"));
        let cols = [("id", Kind::Int), ("total", Kind::Decimal), ("name", Kind::Text), ("created_at", Kind::DateTime), ("status", Kind::Enum), ("tags", Kind::Set), ("data", Kind::Json), ("blob", Kind::Binary)];
        let d = l.db.dialect;
        let bs = l.db.backslash_escapes();
        for r in every_rule(&cols, d) {
            let sql = rules::rule_sql(&r, d, bs);
            l.one(&format!("SELECT COUNT(*) FROM `{s}`.t WHERE {sql}"));
        }
        // Case: 'Ann' and 'ann' are one row each, and the default collation finds both unless case is matched.
        let n = |op: Op, v: &str, case: bool| {
            let r = Rule { col: "name".into(), kind: Kind::Text, op, values: vec![v.into()], case };
            l.one(&format!("SELECT COUNT(*) FROM `{s}`.t WHERE {}", rules::rule_sql(&r, d, bs)))
        };
        for (op, v) in [(Op::Eq, "Ann"), (Op::Contains, "An"), (Op::StartsWith, "A"), (Op::Regexp, "^A")] {
            assert_eq!((n(op, v, false), n(op, v, true)), ("2".into(), "1".into()), "{var}: {op:?}");
        }
        // The panel's count, exact on a small table, and a server error worded with the text it came from.
        let count = |f: &str| l.w.rt.block_on(filter_count(&l.db, &s, "t", f));
        assert_eq!(count("`total` > 2"), Ok(crate::db::sql::facts::FilterCount { n: 1, estimated: false }), "{var}");
        assert_eq!(count("").map(|c| c.n), Ok(2), "{var}");
        let err = count("`name` REGEXP '('").unwrap_err();
        assert!(!err.msg.is_empty(), "{var}");
        // The planner's estimate reads from a real plan.
        let plan = l.one(&format!("EXPLAIN FORMAT=JSON SELECT 1 FROM `{s}`.t WHERE `id` > 0"));
        assert!(crate::db::sql::plan::est_rows(d, &plan).is_some(), "{var}: {plan}");
        // Column facts: JSON keys from a sample, and a number's range.
        let facts = |c: &str, k: Kind| l.w.rt.block_on(col_facts(&l.db, &s, "t", c, k)).unwrap();
        assert_eq!(facts("data", Kind::Json).keys, ["a", "c", "n"], "{var}");
        let range = facts("total", Kind::Decimal).range.expect("range");
        assert_eq!((range.0.as_str(), range.1.as_str()), ("1.00", "5.50"), "{var}");
        assert!(range.2.is_some(), "{var}");
    });
}

#[test]
fn live_every_filter_op_runs_on_postgres() {
    every_pg(|l, var| {
        let (db, pool) = l.fresh_schema(
            "t4_filter",
            "CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy'); CREATE DOMAIN dmood AS mood; \
             CREATE TABLE t (id int, total numeric(8,2), name text, u uuid, created_at timestamptz, seen_at timestamptz, status text, tags text, ok bool, data jsonb, blob bytea, ai int[], au uuid[], at text[], m mood, dm dmood); \
             INSERT INTO t VALUES (1, 5.5, 'Ann', gen_random_uuid(), now(), now(), 'a', 'a,b', true, '{\"a\": {\"b\": \"v\"}, \"n\": 7}', '\\x5a3f', '{1,2}', '{}', '{a,b}', 'ok', 'happy'), \
             (2, 1, 'ann', gen_random_uuid(), now(), now(), 'b', 'b', false, '{\"c\": 1}', NULL, '{3}', NULL, '{b,c}', 'sad', 'sad');",
        );
        let cols = [
            ("id", Kind::Int),
            ("total", Kind::Decimal),
            ("name", Kind::Text),
            ("u", Kind::Uuid),
            ("created_at", Kind::DateTime),
            ("status", Kind::Enum),
            ("tags", Kind::Set),
            ("ok", Kind::Bool),
            ("data", Kind::Json),
            ("blob", Kind::Binary),
            ("ai", Kind::Array),
            ("au", Kind::Array),
            ("at", Kind::Array),
        ];
        let d = Dialect::Postgres;
        let run = |sql: &str| l.w.rt.block_on(async { sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SELECT count(*)::text FROM t4_filter.t WHERE {sql}"))).fetch_all(&pool).await });
        let n = |sql: &str| crate::db::sql::worker::text_at(&run(sql).unwrap_or_else(|x| panic!("{var}: {sql}: {x}"))[0], 0).unwrap();
        for r in every_rule(&cols, d) {
            // A uuid[] holds no '1' or 'a', which the cast to text[] lets the server say without an error.
            let sql = rules::rule_sql(&r, d, false);
            assert!(run(&sql).is_ok(), "{var}: {:?} on {} ({:?}): {sql}: {:?}", r.op, r.col, r.kind, run(&sql).err());
        }
        let rule = |col: &str, kind: Kind, op: Op, v: &[&str], case: bool| rules::rule_sql(&Rule { col: col.into(), kind, op, values: v.iter().map(|s| s.to_string()).collect(), case }, d, false);
        for (op, v) in [(Op::Eq, "Ann"), (Op::Contains, "An"), (Op::StartsWith, "A"), (Op::Regexp, "^A")] {
            assert_eq!((n(&rule("name", Kind::Text, op, &[v], false)), n(&rule("name", Kind::Text, op, &[v], true))), ("2".into(), "1".into()), "{var}: {op:?}");
        }
        // Arrays of any element type compare as text.
        assert_eq!(n(&rule("ai", Kind::Array, Op::ArrayHasAll, &["1", "2"], false)), "1", "{var}");
        assert_eq!(n(&rule("at", Kind::Array, Op::ArrayHasAny, &["a", "c"], false)), "2", "{var}");
        assert_eq!(n(&rule("ai", Kind::Array, Op::ArrayLenGt, &["1"], false)), "1", "{var}");
        assert_eq!(n(&rule("data", Kind::Json, Op::JsonHasKey, &["c"], false)), "1", "{var}");
        assert_eq!(n(&rule("data", Kind::Json, Op::JsonPathGt, &["$.n", "5"], false)), "1", "{var}");
        assert_eq!(n(&rule("total", Kind::Decimal, Op::Distinct, &["1"], false)), "1", "{var}");
        // The panel's count, and an error pinned to the text that caused it.
        let count = |f: &str| l.w.rt.block_on(filter_count(&db, "t4_filter", "t", f));
        assert_eq!(count("\"total\" > 2").map(|c| c.n), Ok(1), "{var}");
        let err = count("\"total\" > 'abc'").unwrap_err();
        assert_eq!(err.at, Some(10), "{var}: {}", err.msg);
        let plan = l.w.rt.block_on(async { sqlx::raw_sql("EXPLAIN (FORMAT JSON) SELECT 1 FROM t4_filter.t WHERE id > 0").fetch_all(&pool).await }).unwrap();
        let plan = crate::db::sql::worker::text_at(&plan[0], 0).unwrap();
        assert!(crate::db::sql::plan::est_rows(d, &plan).is_some(), "{var}: {plan}");
        // Column facts: enum labels in order, through a domain too, JSON keys and array items from a sample.
        let facts = |c: &str, k: Kind| l.w.rt.block_on(col_facts(&db, "t4_filter", "t", c, k)).unwrap();
        assert_eq!(facts("m", Kind::Enum).labels, ["sad", "ok", "happy"], "{var}");
        assert_eq!(facts("dm", Kind::Text).labels, ["sad", "ok", "happy"], "{var}");
        assert_eq!(facts("data", Kind::Json).keys, ["a", "c", "n"], "{var}");
        assert_eq!(facts("at", Kind::Array).items, ["a", "b", "c"], "{var}");
        assert_eq!(facts("total", Kind::Decimal).range.map(|r| (r.0, r.1)), Some(("1.00".to_string(), "5.50".to_string())), "{var}");
    });
}
