use super::*;
use crate::db::sql::structure::{ColDep, Constraint, Index, IndexCol};

#[test]
fn expressions_stay_one_expression() {
    for ok in ["now()", "0", "(a + 1)", "'it''s'", "E'a\\'b'", "$$x; y$$", "$t$ -- not a comment $t$", "coalesce(1, 2)", "'{}'::jsonb", "now() + interval '1 day'"] {
        assert!(check_expr_pg(ok).is_ok(), "{ok}");
    }
    for bad in ["", "  ", "1; DROP TABLE t", "1 -- c", "1 /* c */", "(1", "1)", "'open", "E'a\\'", "\"open", "$$x", "1) , ADD COLUMN x int DEFAULT (2", "'a\\'; DROP TABLE t; --'"] {
        assert!(check_expr_pg(bad).is_err(), "{bad}");
    }
}

#[test]
fn types_stay_one_type_name() {
    for ok in ["integer", "numeric(10,2)", "character varying(255)", "timestamp with time zone", "integer[]", "text[][]", "reporting.mood", "\"My Type\"", "public.\"My Type\"(3)"] {
        assert!(check_type_pg(ok).is_ok(), "{ok}");
    }
    for bad in ["", "integer; DROP TABLE t", "integer, ADD COLUMN x int", "integer -- x", "integer /* x */", "'x'", "integer DEFAULT 'a'", "int::text", "integer)", "numeric(10", "\"open", "integer + 1"] {
        assert!(check_type_pg(bad).is_err(), "{bad}");
    }
}

fn col(name: &str, ty: &str) -> Column {
    Column { name: name.into(), full_type: ty.into(), nullable: true, kind: Some(crate::db::sql::value::Kind::Text), ..Default::default() }
}

fn index(name: &str, def: &str, cols: &[&str]) -> Index {
    Index { name: name.into(), kind: "btree".into(), def: def.into(), cols: cols.iter().map(|c| IndexCol { name: (*c).into(), ..Default::default() }).collect(), ..Default::default() }
}

fn table() -> Structure {
    let mut id = col("id", "integer");
    id.nullable = false;
    id.extra = "auto_increment GENERATED ALWAYS AS IDENTITY".into();
    let mut ix = index("t_pkey", "CREATE UNIQUE INDEX t_pkey ON public.t USING btree (id)", &["id"]);
    (ix.unique, ix.constraint, ix.primary) = (true, true, true);
    let mut by_name = index("t_name", "CREATE INDEX t_name ON public.t USING btree (name DESC, note)", &["name", "note"]);
    by_name.cols[0].desc = true;
    Structure { columns: vec![id, col("name", "character varying(40)"), col("note", "text")], indexes: vec![ix, by_name], pg_schema: Some("public".into()), ..Default::default() }
}

fn steps(changes: &[Change]) -> Vec<String> {
    plan("t", &table(), changes).unwrap().steps
}

fn def(name: &str, ty: &str) -> ColDef {
    ColDef::new(name, ty)
}

#[test]
fn a_table_comment_is_written_and_undone_but_no_other_option() {
    use crate::db::sql::ddl::TableOpt;
    let set = Change::Table { opt: TableOpt::Comment("it's".into()), was: TableOpt::Comment("old".into()) };
    let p = plan("t", &table(), &[set]).unwrap();
    assert_eq!(p.steps, ["COMMENT ON TABLE \"public\".\"t\" IS 'it''s'"]);
    assert_eq!(p.undo.unwrap().steps, ["COMMENT ON TABLE \"public\".\"t\" IS 'old'"]);
    assert!(plan("t", &table(), &[Change::Table { opt: TableOpt::Engine("x".into()), was: TableOpt::Engine("y".into()) }]).is_err());
}

#[test]
fn generated_columns_are_added_read_and_changed_by_their_expression() {
    use crate::db::sql::ddl::Gen;
    let t = "\"public\".\"t\"";
    let computed = |e: &str, stored: bool| ColDef { generated: Some(Gen { expr: e.into(), stored }), default: Dv::None, ..def("dbl", "integer") };
    assert_eq!(steps(&[Change::Add { def: computed("length(note)", true), pos: Pos::Keep }]), [format!("ALTER TABLE {t} ADD COLUMN \"dbl\" integer GENERATED ALWAYS AS (length(note)) STORED")]);
    assert_eq!(steps(&[Change::Add { def: ColDef { nullable: false, ..computed("length(note)", false) }, pos: Pos::Keep }]), [format!("ALTER TABLE {t} ADD COLUMN \"dbl\" integer GENERATED ALWAYS AS (length(note)) VIRTUAL NOT NULL")]);
    assert!(plan("t", &table(), &[Change::Add { def: ColDef { default: Dv::Value("1".into()), ..computed("1", true) }, pos: Pos::Keep }]).is_err());
    assert!(plan("t", &table(), &[Change::Add { def: ColDef { invisible: true, ..def("hidden", "integer") }, pos: Pos::Keep }]).is_err());
    // the catalog's text is read back whole, with a bracket inside the expression
    let mut st = table();
    let mut c = col("dbl", "integer");
    c.extra = "GENERATED ALWAYS AS (length((note)::text)) STORED".into();
    st.columns.push(c);
    assert_eq!(col_def(&st.columns[3]).unwrap().generated, Some(Gen { expr: "length((note)::text)".into(), stored: true }));
    let change = |st: &Structure, d: ColDef| plan("t", st, &[Change::Modify { from: "dbl".into(), def: d, pos: Pos::Keep }]);
    let p = change(&st, computed("length(note) + 1", true)).unwrap();
    assert_eq!(p.steps, [format!("ALTER TABLE {t} ALTER COLUMN \"dbl\" SET EXPRESSION AS (length(note) + 1)")]);
    assert_eq!(p.undo.unwrap().steps, [format!("ALTER TABLE {t} ALTER COLUMN \"dbl\" SET EXPRESSION AS (length((note)::text))")]);
    // a stored column can become a plain one, which is not undone; the other directions are refused
    let plain = change(&st, def("dbl", "integer")).unwrap();
    assert_eq!(plain.steps, [format!("ALTER TABLE {t} ALTER COLUMN \"dbl\" DROP EXPRESSION")]);
    assert!(plain.undo.unwrap_err().contains("Drop and add the column to make it generated"));
    assert!(plan("t", &st, &[Change::Modify { from: "name".into(), def: computed("1", true), pos: Pos::Keep }]).is_err());
    assert!(change(&st, computed("1", false)).unwrap_err().contains("STORED and VIRTUAL"));
    // SET EXPRESSION is from 17 and a virtual column from 18
    st.pg_major = 16;
    assert!(change(&st, computed("1", true)).unwrap_err().contains("needs 17"));
    assert!(plan("t", &st, &[Change::Add { def: computed("1", false), pos: Pos::Keep }]).unwrap_err().contains("needs 18"));
    st.pg_major = 18;
    assert!(change(&st, computed("1", true)).is_ok());
}

#[test]
fn each_kind_of_change_has_its_statements() {
    let t = "\"public\".\"t\"";
    let mut added = def("qty", "integer");
    added.nullable = false;
    added.default = Dv::Value("0".into());
    added.comment = "How many".into();
    assert_eq!(steps(&[Change::Add { def: added, pos: Pos::Keep }]), [format!("ALTER TABLE {t} ADD COLUMN \"qty\" integer DEFAULT '0' NOT NULL"), format!("COMMENT ON COLUMN {t}.\"qty\" IS 'How many'")]);
    assert_eq!(steps(&[Change::Drop("note".into())]), [format!("ALTER TABLE {t} DROP COLUMN \"note\"")]);
    let mut wide = def("full_name", "character varying(80)");
    wide.default = Dv::Expr("lower('X')".into());
    assert_eq!(
        steps(&[Change::Modify { from: "name".into(), def: wide, pos: Pos::Keep }]),
        [
            format!("ALTER TABLE {t} RENAME COLUMN \"name\" TO \"full_name\""),
            format!("ALTER TABLE {t} ALTER COLUMN \"full_name\" TYPE character varying(80) USING \"full_name\"::character varying(80)"),
            format!("ALTER TABLE {t} ALTER COLUMN \"full_name\" SET DEFAULT (lower('X'))"),
        ]
    );
    let mut notnull = def("note", "text");
    notnull.nullable = false;
    assert_eq!(steps(&[Change::Modify { from: "note".into(), def: notnull, pos: Pos::Keep }]), [format!("ALTER TABLE {t} ALTER COLUMN \"note\" SET NOT NULL")]);
    let ix = IndexDef { name: "t_note".into(), kind: IndexKind::Unique, cols: vec![crate::db::sql::ddl::IdxCol { name: "note".into(), prefix: None, desc: true }], comment: "by note".into(), ..Default::default() };
    assert_eq!(steps(&[Change::AddIndex(ix)]), [format!("CREATE UNIQUE INDEX \"t_note\" ON {t} (\"note\" DESC)"), "COMMENT ON INDEX \"public\".\"t_note\" IS 'by note'".to_string()]);
    assert_eq!(steps(&[Change::DropIndex("t_name".into())]), ["DROP INDEX \"public\".\"t_name\"".to_string()]);
    assert_eq!(steps(&[Change::DropIndex("t_pkey".into())]), [format!("ALTER TABLE {t} DROP CONSTRAINT \"t_pkey\"")]);
    let st = table();
    let rename = Change::RenameIndex { from: "t_name".into(), to: "t_by_name".into(), def: IndexDef::from_index(&st.indexes[1]) };
    assert_eq!(steps(&[rename]), ["ALTER INDEX \"public\".\"t_name\" RENAME TO \"t_by_name\"".to_string()]);
}

#[test]
fn identity_and_serial_are_one_auto_increment() {
    let t = "\"public\".\"t\"";
    let mut auto = def("n", "bigint");
    (auto.nullable, auto.auto_increment) = (false, true);
    assert_eq!(steps(&[Change::Add { def: auto.clone(), pos: Pos::Keep }]), [format!("ALTER TABLE {t} ADD COLUMN \"n\" bigint GENERATED BY DEFAULT AS IDENTITY")]);
    let mut serial = ColDef { serial: true, default: Dv::Expr("nextval('t_n_seq'::regclass)".into()), ..auto.clone() };
    assert_eq!(steps(&[Change::Add { def: serial.clone(), pos: Pos::Keep }]), [format!("ALTER TABLE {t} ADD COLUMN \"n\" bigserial")]);
    // a nextval default the person typed names a sequence the column does not own: it is not written as a bigserial
    serial.serial = false;
    assert!(plan("t", &table(), &[Change::Add { def: serial, pos: Pos::Keep }]).err().unwrap_or_default().contains("identity column cannot have a default"));
    // the identity of the table's id goes away and comes back as it was
    let mut plain = def("id", "integer");
    plain.nullable = false;
    let drop = Change::Modify { from: "id".into(), def: plain, pos: Pos::Keep };
    let p = plan("t", &table(), &[drop]).unwrap();
    assert_eq!(p.steps, [format!("ALTER TABLE {t} ALTER COLUMN \"id\" DROP IDENTITY")]);
    let u = p.undo.unwrap();
    // the sequence is moved to the largest value the column holds, or the next insert repeats one
    assert_eq!(u.steps, [format!("ALTER TABLE {t} ALTER COLUMN \"id\" ADD GENERATED ALWAYS AS IDENTITY"), format!("SELECT setval(pg_get_serial_sequence('{t}', 'id'), m) FROM (SELECT max(\"id\") AS m FROM {t}) s WHERE m >= 1")]);
    assert!(u.loss.iter().any(|l| l.contains("new sequence")), "{:?}", u.loss);
    let mut text_auto = def("s", "text");
    (text_auto.nullable, text_auto.auto_increment) = (false, true);
    assert!(plan("t", &table(), &[Change::Add { def: text_auto, pos: Pos::Keep }]).is_err());
}

#[test]
fn what_postgres_cannot_do_is_refused() {
    let st = table();
    let here = |c: Change| plan("t", &st, &[c]).err().unwrap_or_default();
    assert!(here(Change::Add { def: def("x", "integer"), pos: Pos::First }).contains("cannot place"));
    assert!(here(Change::Modify { from: "note".into(), def: def("note", "text"), pos: Pos::After("id".into()) }).contains("cannot place"));
    let mut unsigned = def("x", "integer");
    unsigned.unsigned = true;
    assert!(here(Change::Add { def: unsigned, pos: Pos::Keep }).contains("unsigned"));
    assert!(here(Change::AddFk(Default::default())).contains("needs a name"));
    let spatial = IndexDef { name: "g".into(), kind: IndexKind::Fulltext, cols: vec![crate::db::sql::ddl::IdxCol { name: "note".into(), prefix: None, desc: false }], ..Default::default() };
    assert!(here(Change::AddIndex(spatial)).contains("FULLTEXT"));
    let mut hostile = def("x", "integer; DROP TABLE t");
    assert!(here(Change::Add { def: hostile.clone(), pos: Pos::Keep }).contains("type"));
    hostile.ty = "integer".into();
    hostile.default = Dv::Expr("1); DROP TABLE t; --".into());
    assert!(here(Change::Add { def: hostile, pos: Pos::Keep }).contains("default expression"));
    let long = def(&"x".repeat(64), "integer");
    assert!(here(Change::Add { def: long, pos: Pos::Keep }).contains("63 bytes"));
}

#[test]
fn dropping_a_column_takes_its_indexes_and_undo_brings_them_back_whole() {
    let p = plan("t", &table(), &[Change::Drop("name".into())]).unwrap();
    let u = p.undo.unwrap();
    // the column returns last, then the index it was in, with both its columns
    assert_eq!(u.steps, ["ALTER TABLE \"public\".\"t\" ADD COLUMN \"name\" character varying(40)", "CREATE INDEX \"t_name\" ON \"public\".\"t\" (\"name\" DESC, \"note\")"]);
}

#[test]
fn a_primary_key_undo_gives_back_nullability() {
    let ix = IndexDef { name: "t_note_pkey".into(), kind: IndexKind::Primary, cols: vec![crate::db::sql::ddl::IdxCol { name: "note".into(), prefix: None, desc: false }], ..Default::default() };
    let mut st = table();
    st.indexes.remove(0);
    let p = plan("t", &st, &[Change::AddIndex(ix)]).unwrap();
    assert_eq!(p.steps, ["ALTER TABLE \"public\".\"t\" ADD CONSTRAINT \"t_note_pkey\" PRIMARY KEY (\"note\")"]);
    assert_eq!(p.undo.unwrap().steps, ["ALTER TABLE \"public\".\"t\" DROP CONSTRAINT \"t_note_pkey\"", "ALTER TABLE \"public\".\"t\" ALTER COLUMN \"note\" DROP NOT NULL"]);
}

fn ix_on(name: &str, cols: &[&str]) -> IndexDef {
    IndexDef { name: name.into(), cols: cols.iter().map(|c| crate::db::sql::ddl::IdxCol { name: (*c).into(), prefix: None, desc: false }).collect(), ..Default::default() }
}

#[test]
fn a_create_index_carries_its_method_condition_include_and_concurrently() {
    let t = "\"public\".\"t\"";
    let base = ix_on("t_n", &["name"]);
    let add = |ix: IndexDef| steps(&[Change::AddIndex(ix)]);
    assert_eq!(add(IndexDef { method: "hash".into(), ..base.clone() }), [format!("CREATE INDEX \"t_n\" ON {t} USING hash (\"name\")")]);
    assert_eq!(add(IndexDef { kind: IndexKind::Unique, predicate: "note IS NOT NULL".into(), ..base.clone() }), [format!("CREATE UNIQUE INDEX \"t_n\" ON {t} (\"name\") WHERE note IS NOT NULL")]);
    assert_eq!(add(IndexDef { include: vec!["note".into()], ..base.clone() }), [format!("CREATE INDEX \"t_n\" ON {t} (\"name\") INCLUDE (\"note\")")]);
    let all = IndexDef { kind: IndexKind::Unique, method: "btree".into(), include: vec!["note".into()], predicate: "id > 0".into(), concurrently: true, comment: "why".into(), ..base.clone() };
    assert_eq!(add(all), [format!("CREATE UNIQUE INDEX CONCURRENTLY \"t_n\" ON {t} USING btree (\"name\") INCLUDE (\"note\") WHERE id > 0"), "COMMENT ON INDEX \"public\".\"t_n\" IS 'why'".to_string()]);
    // a unique constraint can carry INCLUDE columns, and nothing else of these
    let uq = IndexDef { kind: IndexKind::Unique, constraint: true, include: vec!["note".into()], ..base.clone() };
    assert_eq!(add(uq.clone()), [format!("ALTER TABLE {t} ADD CONSTRAINT \"t_n\" UNIQUE (\"name\") INCLUDE (\"note\")")]);
    let st = table();
    let refused = |ix: IndexDef| plan("t", &st, &[Change::AddIndex(ix)]).err().unwrap_or_default();
    assert!(refused(IndexDef { method: "hash".into(), include: vec!["note".into()], ..base.clone() }).contains("INCLUDE"));
    assert!(refused(IndexDef { method: "gin".into(), kind: IndexKind::Unique, ..base.clone() }).contains("UNIQUE"));
    assert!(refused(IndexDef { method: "bogus".into(), ..base.clone() }).contains("not an index method"));
    assert!(refused(IndexDef { concurrently: true, ..uq.clone() }).contains("CONCURRENTLY"));
    assert!(refused(IndexDef { predicate: "id > 0".into(), ..uq }).contains("partial"));
    let mut desc = ix_on("t_d", &["name"]);
    desc.cols[0].desc = true;
    assert!(refused(IndexDef { method: "gist".into(), ..desc }).contains("sort order"));
}

#[test]
fn an_index_condition_stays_one_expression() {
    for ok in ["qty > 1", "(note IS NOT NULL)", "status = 'open;closed'", "lower(name) <> 'x'"] {
        assert!(check_predicate_pg(ok).is_ok(), "{ok}");
    }
    for bad in ["", "qty > 1; DROP TABLE t", "qty > 1 -- c", "qty > 1 /* c */", "(qty > 1", "qty > 1)", "name = 'open", "qty > 1) WITH (fillfactor = 10"] {
        assert!(check_predicate_pg(bad).is_err(), "{bad}");
    }
    // the plan refuses the same text before any statement exists
    let ix = IndexDef { predicate: "qty > 1; DROP TABLE t".into(), ..ix_on("t_n", &["name"]) };
    assert!(plan("t", &table(), &[Change::AddIndex(ix)]).err().unwrap_or_default().contains("index condition"));
}

#[test]
fn a_concurrent_index_runs_after_the_transaction_with_its_comment_and_is_undone_by_a_drop() {
    let ix = IndexDef { concurrently: true, comment: "why".into(), ..ix_on("t_n", &["name"]) };
    let p = plan("t", &table(), &[Change::DropIndex("t_name".into()), Change::AddIndex(ix)]).unwrap();
    let (now, later) = split_steps(&p.steps);
    assert_eq!(now.len(), 1);
    assert!(now[0].starts_with("DROP INDEX"));
    assert_eq!(later.len(), 2);
    assert!(later[0].starts_with("CREATE INDEX CONCURRENTLY \"t_n\"") && later[1].starts_with("COMMENT ON INDEX"));
    let sql = p.sql();
    let (drop, note, create) = (sql.find("DROP INDEX").unwrap(), sql.find("cannot run inside a transaction").unwrap(), sql.find("CONCURRENTLY \"t_n\"").unwrap());
    assert!(drop < note && note < create, "{sql}");
    // the undo is a plain DROP, which runs in the transaction
    let undo = p.undo.unwrap();
    assert_eq!(undo.steps[0], "DROP INDEX \"public\".\"t_n\"");
    assert!(undo.steps.iter().all(|s| split_steps(std::slice::from_ref(s)).1.is_empty()), "{:?}", undo.steps);
    // without a concurrent index the preview is the plain script
    assert!(!plan("t", &table(), &[Change::DropIndex("t_name".into())]).unwrap().sql().contains("CONCURRENTLY"));
    // a name with a quote in it is read to its end
    assert_eq!(concurrent_name("CREATE INDEX CONCURRENTLY \"a\"\" ON \" ON t (x)"), Some("\"a\"\" ON \""));
}

#[test]
fn any_index_can_be_dropped_and_undone_from_the_servers_own_text() {
    let raw = "CREATE INDEX t_part ON public.t USING btree (name text_pattern_ops) WHERE (note IS NOT NULL)";
    let mut st = table();
    st.indexes.push(Index { complex: true, ..index("t_part", raw, &["name"]) });
    let p = plan("t", &st, &[Change::DropIndex("t_part".into())]).unwrap();
    assert_eq!(p.steps, ["DROP INDEX \"public\".\"t_part\"".to_string()]);
    assert_eq!(p.undo.unwrap().steps, [raw.to_string()]);
    // a constraint's index is not made by CREATE INDEX, so its drop is still not undoable
    let mut st = table();
    st.indexes.push(Index { complex: true, constraint: true, unique: true, ..index("t_ex", "CREATE UNIQUE INDEX t_ex ON public.t USING btree (name) INCLUDE (note) WITH (fillfactor=70)", &["name"]) });
    assert!(plan("t", &st, &[Change::DropIndex("t_ex".into())]).unwrap().undo.is_err());
}

#[test]
fn a_partial_or_covering_index_is_read_into_the_form_and_written_back_the_same() {
    let ix = Index { predicate: "(note IS NOT NULL)".into(), include: vec!["note".into()], ..index("t_p", "CREATE INDEX t_p ON public.t USING btree (name) INCLUDE (note) WHERE (note IS NOT NULL)", &["name"]) };
    let d = IndexDef::from_index(&ix);
    assert!(!d.functional);
    assert_eq!((d.predicate.as_str(), d.include.as_slice()), ("(note IS NOT NULL)", &["note".to_string()][..]));
    assert_eq!(steps(&[Change::AddIndex(d)]), ["CREATE INDEX \"t_p\" ON \"public\".\"t\" (\"name\") INCLUDE (\"note\") WHERE (note IS NOT NULL)".to_string()]);
}

#[test]
fn indexes_the_editor_cannot_write_back_are_flagged() {
    assert!(!IndexDef::from_index(&table().indexes[1]).functional);
    // the catalog says which indexes have a part the columns do not
    let complex = Index { complex: true, ..index("i", "CREATE INDEX i ON public.t USING btree (name) WHERE (note IS NOT NULL)", &["name"]) };
    assert!(IndexDef::from_index(&complex).functional);
    // a quoted name that holds the words of the text is no longer read as the text
    let tricky = index("a USING (b) WHERE", "CREATE INDEX \"a USING (b) WHERE\" ON public.t USING btree (name)", &["name"]);
    assert!(!IndexDef::from_index(&tricky).functional);
    let gin = Index { kind: "GIN".into(), ..index("i", "CREATE INDEX i ON public.t USING gin (name)", &["name"]) };
    assert_eq!(IndexDef::from_index(&gin).method, "gin");
}

#[test]
fn a_constraint_is_added_back_deferrable_with_its_comment() {
    let ix = IndexDef { name: "t_u".into(), kind: IndexKind::Unique, constraint: true, deferrable: true, deferred: true, con_comment: "one per name".into(), cols: vec![crate::db::sql::ddl::IdxCol { name: "name".into(), prefix: None, desc: false }], ..Default::default() };
    assert_eq!(steps(&[Change::AddIndex(ix.clone())]), ["ALTER TABLE \"public\".\"t\" ADD CONSTRAINT \"t_u\" UNIQUE (\"name\") DEFERRABLE INITIALLY DEFERRED".to_string(), "COMMENT ON CONSTRAINT \"t_u\" ON \"public\".\"t\" IS 'one per name'".to_string()]);
    assert_eq!(steps(&[Change::AddIndex(IndexDef { deferred: false, con_comment: String::new(), ..ix.clone() })]), ["ALTER TABLE \"public\".\"t\" ADD CONSTRAINT \"t_u\" UNIQUE (\"name\") DEFERRABLE".to_string()]);
    assert!(plan("t", &table(), &[Change::AddIndex(IndexDef { constraint: false, ..ix })]).is_err(), "a plain index cannot be deferred");
}

#[test]
fn a_digit_led_word_with_a_dollar_is_refused() {
    for bad in ["1$a$", "x + 1$a$ y", "2$"] {
        assert!(check_expr_pg(bad).is_err(), "{bad}");
    }
    for ok in ["a$b", "now()", "$$1$$"] {
        assert!(check_expr_pg(ok).is_ok(), "{ok}");
    }
}

#[test]
fn a_collation_outside_pg_catalog_is_written_with_its_schema() {
    let mut d = def("s", "text");
    d.collation = Some("\"Other\".\"my \"\"coll\"\"\"".into());
    assert_eq!(steps(&[Change::Add { def: d.clone(), pos: Pos::Keep }]), ["ALTER TABLE \"public\".\"t\" ADD COLUMN \"s\" text COLLATE \"Other\".\"my \"\"coll\"\"\"".to_string()]);
    d.collation = Some("en_US.utf8".into());
    assert_eq!(steps(&[Change::Add { def: d, pos: Pos::Keep }]), ["ALTER TABLE \"public\".\"t\" ADD COLUMN \"s\" text COLLATE \"en_US.utf8\"".to_string()]);
}

#[test]
fn function_calls_in_a_default_are_found() {
    for yes in ["random()", "gen_random_uuid()", "now() + interval '1 day'", "lower(upper('x'))", "1 + abs(-2)"] {
        assert!(calls_function(yes), "{yes}");
    }
    for no in ["0", "'x'::numeric(10,2)", "(1 + 2)", "'{}'::jsonb", "CAST('1' AS integer)", "coalesce(1, 2)", "ARRAY[1, 2]"] {
        assert!(!calls_function(no), "{no}");
    }
}

#[test]
fn a_function_default_on_a_table_with_rows_is_warned_about() {
    let mut st = table();
    let with = |d: Dv| vec![Change::Add { def: ColDef { default: d, ..def("tok", "uuid") }, pos: Pos::Keep }];
    assert!(crate::db::sql::ddl::warnings(&st, &with(Dv::Expr("gen_random_uuid()".into()))).is_empty(), "no rows known, nothing to rewrite");
    st.status.rows = Some(3);
    let w = crate::db::sql::ddl::warnings(&st, &with(Dv::Expr("gen_random_uuid()".into())));
    assert!(w.iter().any(|x| x.contains("once for each of the table's rows") && x.contains("exclusive lock")), "{w:?}");
    assert!(crate::db::sql::ddl::warnings(&st, &with(Dv::Expr("'x'::text".into()))).is_empty());
    assert!(crate::db::sql::ddl::warnings(&st, &with(Dv::Value("x".into()))).is_empty());
}

#[test]
fn a_dropped_column_comes_back_without_what_it_cannot_hold() {
    let mut st = table();
    // a NOT NULL column without a default and its primary key
    st.columns[2].nullable = false;
    let mut pk = index("t_note_pkey", "CREATE UNIQUE INDEX t_note_pkey ON public.t USING btree (note)", &["note"]);
    (pk.unique, pk.constraint, pk.primary) = (true, true, true);
    st.indexes.push(pk);
    let u = plan("t", &st, &[Change::Drop("note".into())]).unwrap().undo.unwrap();
    assert_eq!(u.steps, ["ALTER TABLE \"public\".\"t\" ADD COLUMN \"note\" text".to_string(), "CREATE INDEX \"t_name\" ON \"public\".\"t\" (\"name\" DESC, \"note\")".to_string()]);
    assert!(u.loss.iter().any(|l| l.contains("allowing NULL") && l.contains("primary key")), "{:?}", u.loss);
    // with a default the column keeps NOT NULL
    st.columns[2].default = Some("x".into());
    let u = plan("t", &st, &[Change::Drop("note".into())]).unwrap().undo.unwrap();
    assert!(u.steps[0].ends_with("DEFAULT 'x' NOT NULL"), "{:?}", u.steps);
    assert!(u.steps.iter().any(|s| s.contains("PRIMARY KEY")));
}

#[test]
fn what_the_server_drops_with_a_column_must_be_something_the_undo_restores() {
    let dep = |kind: &str, name: &str| ColDep { col: "note".into(), kind: kind.into(), name: name.into(), what: format!("{kind} {name}"), own: true, ..Default::default() };
    let undo = |deps: Vec<ColDep>| {
        let mut st = table();
        st.pg_deps = deps;
        plan("t", &st, &[Change::Drop("note".into())]).unwrap().undo
    };
    assert!(undo(vec![dep("index", "t_name"), dep("default", "")]).is_ok());
    for bad in [dep("constraint", "t_note_check"), dep("index", "t_partial"), dep("other", "trigger x")] {
        let why = undo(vec![dep("index", "t_name"), bad.clone()]).unwrap_err();
        assert!(why.contains(&bad.what), "{why}");
    }
    // the plan itself is not refused, and it is checked again under the lock
    let mut st = table();
    st.pg_deps = vec![dep("constraint", "t_note_check")];
    let p = plan("t", &st, &[Change::Drop("note".into())]).unwrap();
    assert!(p.expect.unwrap().all.is_some());
}
fn fk(name: &str, cols: &[&str], ref_db: &str, table: &str, ref_cols: &[&str]) -> ForeignKey {
    let own = |v: &[&str]| v.iter().map(|c| (*c).to_string()).collect::<Vec<_>>();
    ForeignKey { name: name.into(), db: "public".into(), table: "t".into(), cols: own(cols), ref_db: ref_db.into(), ref_table: table.into(), ref_cols: own(ref_cols), on_update: "NO ACTION".into(), on_delete: "NO ACTION".into(), ..Default::default() }
}

#[test]
fn a_foreign_key_is_written_with_everything_it_carries() {
    let t = "\"public\".\"t\"";
    let plain = fk("t_note_fk", &["note"], "", "p", &["code"]);
    assert_eq!(steps(&[Change::AddFk(plain.clone())]), [format!("ALTER TABLE {t} ADD CONSTRAINT \"t_note_fk\" FOREIGN KEY (\"note\") REFERENCES \"public\".\"p\" (\"code\") ON DELETE NO ACTION ON UPDATE NO ACTION")]);
    let full = ForeignKey { match_full: true, deferrable: true, deferred: true, not_valid: true, comment: "it's".into(), on_delete: "set null".into(), on_update: "Cascade".into(), ..fk("t_pair_fk", &["name", "note"], "other", "p", &["a", "b"]) };
    assert_eq!(
        steps(&[Change::AddFk(full)]),
        [format!("ALTER TABLE {t} ADD CONSTRAINT \"t_pair_fk\" FOREIGN KEY (\"name\", \"note\") REFERENCES \"other\".\"p\" (\"a\", \"b\") MATCH FULL ON DELETE SET NULL ON UPDATE CASCADE DEFERRABLE INITIALLY DEFERRED NOT VALID"), format!("COMMENT ON CONSTRAINT \"t_pair_fk\" ON {t} IS 'it''s'")]
    );
    let deferrable = ForeignKey { deferrable: true, on_delete: "SET DEFAULT".into(), ..plain.clone() };
    assert!(steps(&[Change::AddFk(deferrable)])[0].ends_with("ON DELETE SET DEFAULT ON UPDATE NO ACTION DEFERRABLE"));
    // a key on a column the table does not have, a bad action, mismatched sides and a name that is too long are refused
    for bad in [fk("k", &["nope"], "", "p", &["a"]), ForeignKey { on_delete: "CASCADE; DROP TABLE t".into(), ..plain.clone() }, fk("k", &["note", "name"], "", "p", &["a"]), fk(&"n".repeat(64), &["note"], "", "p", &["a"]), ForeignKey { deferred: true, ..plain.clone() }] {
        assert!(plan("t", &table(), &[Change::AddFk(bad.clone())]).is_err(), "{bad:?}");
    }
    // a name that needs quoting is quoted, and a key is dropped and renamed as a constraint
    let mut st = table();
    st.foreign_keys = vec![fk("Odd \"name\"", &["note"], "", "p", &["code"])];
    let go = |c: Change| plan("t", &st, &[c]).unwrap().steps;
    assert_eq!(go(Change::DropFk("Odd \"name\"".into())), [format!("ALTER TABLE {t} DROP CONSTRAINT \"Odd \"\"name\"\"\"")]);
    assert_eq!(go(Change::RenameFk { from: "Odd \"name\"".into(), to: "fresh".into() }), [format!("ALTER TABLE {t} RENAME CONSTRAINT \"Odd \"\"name\"\"\" TO \"fresh\"")]);
    assert!(plan("t", &st, &[Change::RenameFk { from: "Odd \"name\"".into(), to: "x".repeat(64) }]).is_err());
    assert!(plan("t", &st, &[Change::DropFk("missing".into())]).is_err());
}

fn chk(name: &str, expr: &str) -> CheckConstraint {
    CheckConstraint { name: name.into(), expr: expr.into(), ..Default::default() }
}

#[test]
fn a_check_is_added_and_dropped_as_a_constraint() {
    let t = "\"public\".\"t\"";
    assert_eq!(steps(&[Change::AddCheck(chk("t_name_check", "length(name) > 0"))]), [format!("ALTER TABLE {t} ADD CONSTRAINT \"t_name_check\" CHECK (length(name) > 0)")]);
    let both = CheckConstraint { not_valid: true, no_inherit: true, ..chk("Odd \"name\"", " price > 0 ") };
    assert_eq!(steps(&[Change::AddCheck(both)]), [format!("ALTER TABLE {t} ADD CONSTRAINT \"Odd \"\"name\"\"\" CHECK (price > 0) NO INHERIT NOT VALID")]);
    assert!(steps(&[Change::AddCheck(CheckConstraint { not_enforced: true, not_valid: true, ..chk("c", "a > 0") })])[0].ends_with("CHECK (a > 0) NOT ENFORCED"));
    // a condition that is not one expression, one that acts outside the row, an empty one and a long name are refused
    for bad in [chk("c", "a > 0); DROP TABLE t; --"), chk("c", "a > 0 -- x"), chk("c", "pg_advisory_lock(1) IS NOT NULL"), chk("c", ""), chk(&"n".repeat(64), "a > 0"), chk("", "a > 0")] {
        assert!(plan("t", &table(), &[Change::AddCheck(bad.clone())]).is_err(), "{bad:?}");
    }
    let mut st = table();
    st.checks = vec![chk("T_chk", "a > 0")];
    assert_eq!(plan("t", &st, &[Change::DropCheck("T_chk".into())]).unwrap().steps, [format!("ALTER TABLE {t} DROP CONSTRAINT \"T_chk\"")]);
    // PostgreSQL tells two names apart that differ only in case
    assert!(plan("t", &st, &[Change::DropCheck("t_chk".into())]).is_err());
    assert!(plan("t", &st, &[Change::AddCheck(chk("t_chk", "a > 1"))]).is_ok());
    assert!(plan("t", &st, &[Change::AddCheck(chk("T_chk", "a > 1"))]).unwrap().undo.is_err());
}

#[test]
fn the_undo_of_a_check_change_is_the_check_as_it_was() {
    let mut st = table();
    let old = CheckConstraint { not_valid: true, ..chk("t_chk", "a > 0") };
    st.checks = vec![old.clone()];
    let undo = plan("t", &st, &[Change::DropCheck("t_chk".into())]).unwrap().undo.unwrap();
    assert_eq!(undo.groups, [vec![Change::AddCheck(old.clone())]]);
    assert_eq!(undo.steps, ["ALTER TABLE \"public\".\"t\" ADD CONSTRAINT \"t_chk\" CHECK (a > 0) NOT VALID"]);
    let undo = plan("t", &table(), &[Change::AddCheck(old.clone())]).unwrap().undo.unwrap();
    assert_eq!(undo.steps, ["ALTER TABLE \"public\".\"t\" DROP CONSTRAINT \"t_chk\""]);
    // an edit is a drop and an add of the same name: undone by dropping the new one and adding the old one back
    let new = chk("t_chk", "a > 5");
    let p = plan("t", &st, &[Change::DropCheck("t_chk".into()), Change::AddCheck(new)]).unwrap();
    assert_eq!(p.steps.len(), 2);
    assert_eq!(p.undo.unwrap().steps, ["ALTER TABLE \"public\".\"t\" DROP CONSTRAINT \"t_chk\"", "ALTER TABLE \"public\".\"t\" ADD CONSTRAINT \"t_chk\" CHECK (a > 0) NOT VALID"]);
    // the plan checks the constraint again under the lock, and adding one takes the exclusive lock
    let e = plan("t", &table(), &[Change::AddCheck(chk("c", "a > 0"))]).unwrap().expect.unwrap();
    assert_eq!(e.checks, ["c"]);
    assert!(!e.own_share);
}

#[test]
fn a_check_is_read_from_the_catalog_text() {
    let con = |def: &str| Constraint { name: "c".into(), kind: 'c', def: def.into() };
    let k = CheckConstraint::from_pg(&con("CHECK ((total >= (0)::numeric))")).unwrap();
    assert_eq!((k.expr.as_str(), k.not_valid, k.no_inherit, k.not_enforced), ("(total >= (0)::numeric)", false, false, false));
    let k = CheckConstraint::from_pg(&con("CHECK (status = ANY (ARRAY['a'::text, 'b)'::text])) NO INHERIT NOT VALID")).unwrap();
    assert_eq!((k.expr.as_str(), k.not_valid, k.no_inherit), ("status = ANY (ARRAY['a'::text, 'b)'::text])", true, true));
    // a quote that is escaped in an E string does not end the string
    let k = CheckConstraint::from_pg(&con("CHECK (note <> E'it\\'s)')")).unwrap();
    assert_eq!(k.expr, "note <> E'it\\'s)'");
    assert!(CheckConstraint::from_pg(&con("CHECK ((a > 0)) NOT ENFORCED")).unwrap().not_enforced);
    assert!(CheckConstraint::from_pg(&Constraint { kind: 'u', ..con("UNIQUE (a)") }).is_none());
}

#[test]
fn the_undo_of_a_key_change_is_the_key_as_it_was() {
    let mut st = table();
    let old = ForeignKey { match_full: true, deferrable: true, comment: "c".into(), on_delete: "CASCADE".into(), ..fk("t_note_fk", &["note"], "other", "p", &["code"]) };
    st.foreign_keys = vec![old.clone()];
    let undo = plan("t", &st, &[Change::DropFk("t_note_fk".into())]).unwrap().undo.unwrap();
    assert_eq!(undo.groups, [vec![Change::AddFk(old.clone())]]);
    assert!(undo.steps[0].contains("MATCH FULL ON DELETE CASCADE ON UPDATE NO ACTION DEFERRABLE") && undo.steps.len() == 2, "{:?}", undo.steps);
    // the undo of a rename renames back, and a key written back from the catalog is dropped again by name
    let undo = plan("t", &st, &[Change::RenameFk { from: "t_note_fk".into(), to: "fresh".into() }]).unwrap().undo.unwrap();
    assert_eq!(undo.steps, ["ALTER TABLE \"public\".\"t\" RENAME CONSTRAINT \"fresh\" TO \"t_note_fk\""]);
    let undo = plan("t", &table(), &[Change::AddFk(old)]).unwrap().undo.unwrap();
    assert_eq!(undo.steps, ["ALTER TABLE \"public\".\"t\" DROP CONSTRAINT \"t_note_fk\""]);
    // what one of the key's own columns can drop: the key, which is put back after the column
    let mut st = table();
    st.foreign_keys = vec![fk("t_note_fk", &["note"], "", "p", &["code"])];
    st.pg_deps = vec![ColDep { col: "note".into(), kind: "constraint".into(), name: "t_note_fk".into(), what: "constraint t_note_fk on table t".into(), own: true, ..Default::default() }];
    let undo = plan("t", &st, &[Change::Drop("note".into())]).unwrap().undo.unwrap();
    // the column first, then the index that held it, then the key
    assert!(undo.steps.len() == 3 && undo.steps[0].contains("ADD COLUMN \"note\"") && undo.steps[1].starts_with("CREATE INDEX") && undo.steps[2].contains("ADD CONSTRAINT \"t_note_fk\""), "{:?}", undo.steps);
    // the same name on a key of another table is not this table's, and the undo says why it has none
    st.pg_deps[0].own = false;
    assert!(plan("t", &st, &[Change::Drop("note".into())]).unwrap().undo.unwrap_err().contains("another table"));
}

#[test]
fn a_plan_locks_every_table_of_its_keys_in_one_order() {
    let r = |s: &str, t: &str, exclusive: bool| RefCheck { schema: s.into(), table: t.into(), cols: Vec::new(), exclusive, seen: None };
    // whatever order the changes name the tables in, the locks go by schema and name, the changed table among them
    let a = lock_order("public", "orders", false, &[r("public", "customers", false), r("archive", "old", false)]);
    let b = lock_order("public", "orders", false, &[r("archive", "old", false), r("public", "customers", false)]);
    assert_eq!(a, b);
    let names: Vec<(&str, &str)> = a.iter().map(|(s, t, _)| (s.as_str(), t.as_str())).collect();
    assert_eq!(names, [("archive", "old"), ("public", "customers"), ("public", "orders")]);
    // the changed table is always exclusive, a parent only where a key is dropped, and a table asked twice is locked once
    assert_eq!(a.iter().map(|x| x.2).collect::<Vec<_>>(), ["SHARE ROW EXCLUSIVE", "SHARE ROW EXCLUSIVE", "ACCESS EXCLUSIVE"]);
    let c = lock_order("public", "orders", false, &[r("public", "customers", false), r("public", "customers", true), r("public", "orders", false)]);
    assert_eq!(c.iter().map(|x| (x.1.as_str(), x.2)).collect::<Vec<_>>(), [("customers", "ACCESS EXCLUSIVE"), ("orders", "ACCESS EXCLUSIVE")]);
}

#[test]
fn a_plan_that_only_adds_keys_locks_each_table_share_row_exclusive() {
    let k = fk("t_note_fk", &["note"], "", "p", &["code"]);
    let p = plan("t", &table(), &[Change::AddFk(k.clone())]).unwrap();
    let e = p.expect.unwrap();
    assert!(e.own_share, "ADD FOREIGN KEY needs no more than SHARE ROW EXCLUSIVE on the child");
    assert_eq!(lock_order("public", "t", e.own_share, &e.refs).iter().map(|x| x.2).collect::<Vec<_>>(), ["SHARE ROW EXCLUSIVE", "SHARE ROW EXCLUSIVE"]);
    // any other change of the table brings ACCESS EXCLUSIVE back, and so does a drop of a key
    let p = plan("t", &table(), &[Change::AddFk(k.clone()), Change::Drop("name".into())]).unwrap();
    assert!(!p.expect.unwrap().own_share);
    let mut st = table();
    st.foreign_keys = vec![k];
    assert!(!plan("t", &st, &[Change::DropFk("t_note_fk".into())]).unwrap().expect.unwrap().own_share);
    // the undo of an add is a drop, which takes the stronger lock on both tables
    let p = plan("t", &table(), &[Change::AddFk(fk("t_note_fk", &["note"], "", "p", &["code"]))]).unwrap();
    assert!(!shares_only(&p.undo.unwrap().groups.concat()));
}

#[test]
fn a_change_of_only_when_a_key_is_checked_is_alter_constraint_and_so_is_its_undo() {
    let t = "\"public\".\"t\"";
    let mut st = table();
    let old = ForeignKey { deferrable: true, comment: "c".into(), ..fk("t_note_fk", &["note"], "public", "p", &["code"]) };
    st.foreign_keys = vec![old.clone()];
    let edit = |new: ForeignKey| vec![Change::DropFk("t_note_fk".into()), Change::AddFk(new)];
    // deferrable and initially immediate becomes initially deferred: no drop, no new check of the rows
    let p = plan("t", &st, &edit(ForeignKey { deferred: true, ..old.clone() })).unwrap();
    assert_eq!(p.steps, [format!("ALTER TABLE {t} ALTER CONSTRAINT \"t_note_fk\" DEFERRABLE INITIALLY DEFERRED")]);
    let e = p.expect.as_ref().unwrap();
    assert!(e.own_share && e.orphans.is_empty() && e.refs.iter().all(|r| !r.exclusive), "{e:?}");
    assert_eq!(p.undo.unwrap().steps, [format!("ALTER TABLE {t} ALTER CONSTRAINT \"t_note_fk\" DEFERRABLE INITIALLY IMMEDIATE")]);
    let p = plan("t", &st, &edit(ForeignKey { deferrable: false, ..old.clone() })).unwrap();
    assert_eq!(p.steps, [format!("ALTER TABLE {t} ALTER CONSTRAINT \"t_note_fk\" NOT DEFERRABLE")]);
    // anything else that changes is still a drop and an add
    let p = plan("t", &st, &edit(ForeignKey { deferred: true, on_delete: "CASCADE".into(), ..old.clone() })).unwrap();
    assert_eq!(p.steps.len(), 3, "{:?}", p.steps);
    assert!(plan("t", &st, &edit(ForeignKey { deferred: true, deferrable: false, ..old })).is_err());
    assert!(plan("t", &st, &[Change::SetFkDeferral { name: "t_note_fk".into(), deferrable: false, deferred: true }]).is_err());
}

#[test]
fn a_key_whose_name_differs_only_in_case_is_found_by_its_exact_name() {
    let t = "\"public\".\"t\"";
    let mut st = table();
    st.foreign_keys = vec![fk("T_fk", &["note"], "", "p", &["code"]), fk("t_fk", &["name"], "", "p", &["code"])];
    let go = |c: Change| plan("t", &st, &[c]).unwrap();
    // each name finds its own key, in the statement and in the locks and the undo
    assert_eq!(go(Change::DropFk("t_fk".into())).steps, [format!("ALTER TABLE {t} DROP CONSTRAINT \"t_fk\"")]);
    assert_eq!(go(Change::DropFk("T_fk".into())).steps, [format!("ALTER TABLE {t} DROP CONSTRAINT \"T_fk\"")]);
    assert_eq!(go(Change::RenameFk { from: "t_fk".into(), to: "other".into() }).steps, [format!("ALTER TABLE {t} RENAME CONSTRAINT \"t_fk\" TO \"other\"")]);
    let undo = go(Change::DropFk("t_fk".into())).undo.unwrap();
    assert!(undo.steps[0].contains("FOREIGN KEY (\"name\")"), "{:?}", undo.steps);
    // a name that is not there is not matched by another case
    assert!(plan("t", &st, &[Change::DropFk("T_FK".into())]).is_err());
    // the snapshot the transaction compares reads the right key for each
    let snap = snapshot(&st, &[], &[], &["t_fk".to_string(), "T_fk".to_string()], &[]);
    assert!(snap[0].1.as_deref().unwrap().contains("|name|") && snap[1].1.as_deref().unwrap().contains("|note|"), "{snap:?}");
    // a key of one case can be added beside one of another
    let two = plan("t", &st, &[Change::AddFk(fk("T_FK", &["id"], "", "p", &["id"]))]).unwrap();
    assert!(two.steps[0].contains("\"T_FK\""));
}

#[test]
fn a_column_that_changes_type_locks_the_tables_of_the_keys_on_it() {
    let mut st = table();
    st.foreign_keys = vec![fk("t_note_fk", &["note"], "other", "p", &["code"])];
    st.pg_deps = vec![
        ColDep { col: "note".into(), kind: "constraint".into(), name: "t_note_fk".into(), what: "constraint t_note_fk on table t".into(), own: true, ..Default::default() },
        ColDep { col: "id".into(), kind: "constraint".into(), name: "c_tid_fk".into(), what: "constraint c_tid_fk on table c".into(), own: false, rel_schema: "kids".into(), rel_table: "c".into() },
    ];
    let locks = |col: &str, ty: &str| {
        let mut d = def(col, ty);
        d.nullable = col != "id";
        let e = plan("t", &st, &[Change::Modify { from: col.into(), def: d, pos: Pos::Keep }]).unwrap().expect.unwrap();
        lock_order("public", "t", e.own_share, &e.refs).into_iter().map(|(s, t, m)| format!("{s}.{t} {m}")).collect::<Vec<_>>()
    };
    // retyping a column of an own key takes the parent, retyping a referenced column takes the child of the other table
    assert_eq!(locks("note", "character varying(9)"), ["other.p ACCESS EXCLUSIVE", "public.t ACCESS EXCLUSIVE"]);
    assert_eq!(locks("id", "bigint"), ["kids.c ACCESS EXCLUSIVE", "public.t ACCESS EXCLUSIVE"]);
    // a column no key holds takes nothing else, and a change that keeps the type neither
    assert_eq!(locks("name", "text"), ["public.t ACCESS EXCLUSIVE"]);
    assert_eq!(locks("note", "text"), ["public.t ACCESS EXCLUSIVE"]);
}

#[test]
fn a_refused_column_drop_leaves_the_simulated_table_as_it_was() {
    let mut st = table();
    let mut opaque = fk("t_note_fk", &["note"], "", "p", &["code"]);
    opaque.opaque = "ON DELETE SET NULL (note)".into();
    st.foreign_keys = vec![opaque, fk("t_name_fk", &["name"], "", "p", &["code"])];
    let mut sim = Sim::of(&st);
    assert!(sim.apply(&Change::Drop("note".into())).unwrap_err().contains("cannot put back"));
    assert_eq!((sim.cols.len(), sim.fks.len(), sim.idx.len()), (3, 2, 2), "a refused change changed the simulation");
    // the plan still runs the drop, and the steps after it were written against the table as it was
    let p = plan("t", &st, &[Change::Drop("note".into()), Change::DropFk("t_name_fk".into())]).unwrap();
    assert_eq!(p.steps.len(), 2);
    assert!(p.undo.is_err());
}
