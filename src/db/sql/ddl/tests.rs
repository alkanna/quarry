use super::*;
use crate::db::sql::lex;
use crate::db::sql::structure::{Column, Index, IndexCol};

const M: Dialect = Dialect::MySql;
const B: Mode = Mode::DEFAULT;
const NB: Mode = Mode { bs: false, ansi: false };

fn alt(c: &[Change]) -> String {
    alter(M, "8.4.0", B, "orders", c).unwrap()
}

fn def(name: &str, ty: &str) -> ColDef {
    ColDef::new(name, ty)
}

fn col(name: &str, ty: &str, nullable: bool, default: Option<&str>) -> Column {
    Column { name: name.into(), full_type: ty.into(), nullable, default: default.map(Into::into), ..Default::default() }
}

fn ix(name: &str, unique: bool, cols: &[&str]) -> Index {
    Index { name: name.into(), unique, cols: cols.iter().map(|c| IndexCol { name: (*c).into(), ..Default::default() }).collect(), kind: "BTREE".into(), ..Default::default() }
}

fn table() -> Structure {
    let mut id = col("id", "int unsigned", false, None);
    id.extra = "auto_increment".into();
    let mut total = col("total", "decimal(10,2) unsigned", false, Some("0.00"));
    total.comment = "Incl. tax".into();
    let mut note = col("note", "varchar(50)", true, None);
    note.collation = Some("utf8mb4_0900_ai_ci".into());
    Structure { columns: vec![id, col("customer_id", "int unsigned", false, None), total, note], indexes: vec![ix("PRIMARY", true, &["id"]), ix("idx_cust_total", false, &["customer_id", "total"]), ix("uq_note", true, &["note"])], ..Default::default() }
}

#[test]
fn add_a_column() {
    let mut d = def("qty", "INT");
    d.nullable = false;
    d.default = Dv::Value("1".into());
    assert_eq!(alt(&[Change::Add { def: d.clone(), pos: Pos::Keep }]), "ALTER TABLE `orders`\n  ADD COLUMN `qty` INT NOT NULL DEFAULT 1");
    assert!(alt(&[Change::Add { def: d.clone(), pos: Pos::First }]).ends_with("DEFAULT 1 FIRST"));
    assert!(alt(&[Change::Add { def: d, pos: Pos::After("customer_id".into()) }]).ends_with("DEFAULT 1 AFTER `customer_id`"));
}

#[test]
fn modify_the_design_example() {
    let mut d = def("total", "DECIMAL");
    d.args = "12,2".into();
    d.unsigned = true;
    d.nullable = false;
    d.default = Dv::Value("0.00".into());
    d.comment = "Incl. tax".into();
    assert_eq!(alt(&[Change::Modify { from: "total".into(), def: d, pos: Pos::After("customer_id".into()) }]), "ALTER TABLE `orders`\n  MODIFY `total` DECIMAL(12,2) UNSIGNED NOT NULL DEFAULT 0.00 COMMENT 'Incl. tax' AFTER `customer_id`");
}

#[test]
fn rename_is_change() {
    let c = Change::Modify { from: "note".into(), def: def("memo", "TEXT"), pos: Pos::Keep };
    assert_eq!(alt(&[c]), "ALTER TABLE `orders`\n  CHANGE COLUMN `note` `memo` TEXT NULL DEFAULT NULL");
}

#[test]
fn drop_and_reorder() {
    assert_eq!(alt(&[Change::Drop("note".into())]), "ALTER TABLE `orders`\n  DROP COLUMN `note`");
    let c = Change::Modify { from: "total".into(), def: ColDef::from_column(&table().columns[2]).unwrap(), pos: Pos::First };
    assert_eq!(alt(&[c]), "ALTER TABLE `orders`\n  MODIFY `total` DECIMAL(10,2) UNSIGNED NOT NULL DEFAULT 0.00 COMMENT 'Incl. tax' FIRST");
    assert!(alter(M, "8.4.0", B, "orders", &[]).is_err());
}

#[test]
fn indexes() {
    let i = IndexDef { name: "idx_a".into(), kind: IndexKind::Index, cols: vec![IdxCol { name: "a".into(), prefix: Some(10), desc: false }, IdxCol { name: "b".into(), prefix: None, desc: true }], comment: String::new(), ..Default::default() };
    assert_eq!(alt(&[Change::AddIndex(i.clone())]), "ALTER TABLE `orders`\n  ADD INDEX `idx_a` (`a`(10), `b` DESC)");
    let u = IndexDef { kind: IndexKind::Unique, cols: vec![IdxCol { name: "a".into(), prefix: None, desc: false }], comment: "why".into(), ..i.clone() };
    assert_eq!(alt(&[Change::AddIndex(u)]), "ALTER TABLE `orders`\n  ADD UNIQUE INDEX `idx_a` (`a`) COMMENT 'why'");
    let f = IndexDef { kind: IndexKind::Fulltext, cols: vec![IdxCol { name: "t".into(), prefix: None, desc: false }], ..i.clone() };
    assert_eq!(alt(&[Change::AddIndex(f)]), "ALTER TABLE `orders`\n  ADD FULLTEXT INDEX `idx_a` (`t`)");
    let p = IndexDef { name: "PRIMARY".into(), kind: IndexKind::Primary, cols: vec![IdxCol { name: "id".into(), prefix: None, desc: false }], comment: String::new(), ..Default::default() };
    assert_eq!(alt(&[Change::AddIndex(p)]), "ALTER TABLE `orders`\n  ADD PRIMARY KEY (`id`)");
    assert_eq!(alt(&[Change::DropIndex("idx_a".into())]), "ALTER TABLE `orders`\n  DROP INDEX `idx_a`");
    assert_eq!(alt(&[Change::DropIndex("PRIMARY".into())]), "ALTER TABLE `orders`\n  DROP PRIMARY KEY");
    // A prefix on a FULLTEXT index is refused, and an index needs columns and a name.
    assert!(alter(M, "8.4.0", B, "t", &[Change::AddIndex(IndexDef { kind: IndexKind::Fulltext, ..i.clone() })]).is_err());
    assert!(alter(M, "8.4.0", B, "t", &[Change::AddIndex(IndexDef { cols: vec![], ..i.clone() })]).is_err());
    assert!(alter(M, "8.4.0", B, "t", &[Change::AddIndex(IndexDef { name: String::new(), ..i })]).is_err());
}

#[test]
fn rename_index_by_server_version() {
    let def = IndexDef { name: "old".into(), kind: IndexKind::Unique, cols: vec![IdxCol { name: "a".into(), prefix: None, desc: false }], comment: String::new(), ..Default::default() };
    let c = [Change::RenameIndex { from: "old".into(), to: "new".into(), def }];
    assert_eq!(alter(M, "8.4.0", B, "t", &c).unwrap(), "ALTER TABLE `t`\n  RENAME INDEX `old` TO `new`");
    assert_eq!(alter(Dialect::MariaDb, "10.11.6-MariaDB", B, "t", &c).unwrap(), "ALTER TABLE `t`\n  RENAME INDEX `old` TO `new`");
    assert_eq!(alter(Dialect::MariaDb, "10.4.0-MariaDB", B, "t", &c).unwrap(), "ALTER TABLE `t`\n  DROP INDEX `old`,\n  ADD UNIQUE INDEX `new` (`a`)");
    assert_eq!(alter(M, "5.6.1", B, "t", &c).unwrap(), "ALTER TABLE `t`\n  DROP INDEX `old`,\n  ADD UNIQUE INDEX `new` (`a`)");
}

#[test]
fn several_changes_make_one_statement() {
    let c = [Change::Drop("note".into()), Change::Add { def: def("qty", "INT"), pos: Pos::After("id".into()) }, Change::DropIndex("uq_note".into())];
    assert_eq!(alt(&c), "ALTER TABLE `orders`\n  DROP COLUMN `note`,\n  ADD COLUMN `qty` INT NULL DEFAULT NULL AFTER `id`,\n  DROP INDEX `uq_note`");
}

#[test]
fn types_defaults_and_flags() {
    let b = |f: &dyn Fn(&mut ColDef)| {
        let mut d = def("x", "VARCHAR");
        f(&mut d);
        definition(M, B, &d)
    };
    assert_eq!(b(&|d| d.args = "255".into()).unwrap(), "VARCHAR(255) NULL DEFAULT NULL");
    assert_eq!(b(&|d| (d.ty, d.args, d.collation) = ("char".into(), "3".into(), Some("utf8mb4_bin".into()))).unwrap(), "CHAR(3) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NULL DEFAULT NULL");
    // A collation on a number is dropped rather than sent.
    assert_eq!(b(&|d| (d.ty, d.collation) = ("INT".into(), Some("utf8mb4_bin".into()))).unwrap(), "INT NULL DEFAULT NULL");
    assert_eq!(b(&|d| (d.ty, d.unsigned, d.zerofill, d.nullable, d.default, d.auto_increment) = ("BIGINT".into(), true, true, false, Dv::None, true)).unwrap(), "BIGINT UNSIGNED ZEROFILL NOT NULL AUTO_INCREMENT");
    assert_eq!(b(&|d| (d.ty, d.args, d.nullable, d.default) = ("ENUM".into(), "'a','it''s'".into(), false, Dv::Value("it's".into()))).unwrap(), "ENUM('a','it''s') NOT NULL DEFAULT 'it''s'");
    assert_eq!(b(&|d| (d.ty, d.nullable, d.default, d.on_update) = ("DATETIME".into(), false, Dv::Expr("CURRENT_TIMESTAMP".into()), Some("CURRENT_TIMESTAMP".into()))).unwrap(), "DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP");
    assert!(b(&|d| d.default = Dv::Expr("UUID()".into())).is_err());
    assert_eq!(b(&|d| (d.args, d.default) = ("36".into(), Dv::Expr("UUID()".into()))).unwrap(), "VARCHAR(36) NULL DEFAULT (UUID())");
    assert_eq!(b(&|d| (d.ty, d.args, d.default) = ("BIT".into(), "3".into(), Dv::Value("101".into()))).unwrap(), "BIT(3) NULL DEFAULT b'101'");
    assert_eq!(b(&|d| (d.ty, d.default) = ("INT".into(), Dv::Value("-1.5e3".into()))).unwrap(), "INT NULL DEFAULT -1.5e3");
    // Text for a number column is quoted, not trusted as a number.
    assert_eq!(b(&|d| (d.ty, d.default) = ("INT".into(), Dv::Value("1 OR 1".into()))).unwrap(), "INT NULL DEFAULT '1 OR 1'");
}

#[test]
fn invalid_definitions_are_refused() {
    let bad = |f: &dyn Fn(&mut ColDef)| {
        let mut d = def("x", "INT");
        f(&mut d);
        definition(M, B, &d).is_err()
    };
    assert!(bad(&|d| d.ty = "INT; DROP TABLE x".into()));
    assert!(bad(&|d| d.ty = String::new()));
    assert!(bad(&|d| d.args = "1) , y INT (".into()));
    assert!(bad(&|d| d.args = "1,2,3".into()));
    assert!(bad(&|d| (d.ty, d.args) = ("ENUM".into(), String::new())));
    assert!(bad(&|d| (d.ty, d.args) = ("ENUM".into(), "'a') x (".into())));
    assert!(bad(&|d| (d.nullable, d.default) = (false, Dv::Null)));
    assert!(bad(&|d| (d.ty, d.auto_increment) = ("VARCHAR".into(), true)));
    assert!(bad(&|d| (d.ty, d.collation) = ("VARCHAR".into(), Some("x COLLATE y; --".into()))));
    assert!(bad(&|d| d.name = String::new()));
    assert!(bad(&|d| d.name = "a".repeat(65)));
    assert!(bad(&|d| d.default = Dv::Expr("1); DROP TABLE x; --".into())));
    assert!(bad(&|d| d.default = Dv::Expr("'unclosed".into())));
    assert!(bad(&|d| d.default = Dv::Expr("1 -- c".into())));
    assert!(bad(&|d| d.default = Dv::Expr("1 # c".into())));
    assert!(bad(&|d| d.default = Dv::Expr("1 /* c */".into())));
    assert!(bad(&|d| d.default = Dv::Expr("1) + (2".into())));
    assert!(!bad(&|d| d.default = Dv::Expr("(1 + 2) * ';'".into())));
}

#[test]
fn hostile_names_and_values_stay_inside_their_quotes() {
    let names = ["a`b", "'; DROP TABLE x; --", "x` INT, DROP COLUMN `y", "back\\slash", "new\nline", "\u{1F600} é"];
    for bs in [true, false] {
        for n in names {
            let mut d = def(n, "VARCHAR");
            d.args = "20".into();
            d.default = Dv::Value(n.into());
            d.comment = n.into();
            let ixd = IndexDef { name: n.into(), kind: IndexKind::Index, cols: vec![IdxCol { name: n.into(), prefix: None, desc: false }], comment: n.into(), ..Default::default() };
            let c = [Change::Add { def: d.clone(), pos: Pos::After(n.into()) }, Change::Modify { from: n.into(), def: ColDef { name: format!("{n}2"), ..d }, pos: Pos::Keep }, Change::Drop(n.into()), Change::AddIndex(ixd), Change::DropIndex(n.into())];
            let sql = alter(M, "8.4.0", Mode { bs, ansi: false }, n, &c).unwrap();
            assert_eq!(lex::split_statements(&sql, Dialect::MySql).len(), 1, "{n} bs={bs}: {sql}");
            // Every backtick that is not a doubled one of an identifier closes an identifier we opened.
            assert!(lex::read_only_check(&sql, Dialect::MySql).is_err());
        }
    }
    let mut d = def("c", "VARCHAR");
    d.args = "20".into();
    d.default = Dv::Value("a\\'; DROP TABLE x; --".into());
    d.comment = "it's \\".into();
    assert_eq!(definition(M, B, &d).unwrap(), "VARCHAR(20) NULL DEFAULT 'a\\\\''; DROP TABLE x; --' COMMENT 'it''s \\\\'");
    assert_eq!(definition(M, NB, &d).unwrap(), "VARCHAR(20) NULL DEFAULT 'a\\''; DROP TABLE x; --' COMMENT 'it''s \\'");
    assert_eq!(M.quote_ident("a`b"), "`a``b`");
    assert_eq!(alter(M, "8", B, "a`b", &[Change::Drop("c`d".into())]).unwrap(), "ALTER TABLE `a``b`\n  DROP COLUMN `c``d`");
}

#[test]
fn values_round_trip() {
    let vals = vec!["a".to_string(), "it's".to_string(), "x,y".to_string(), "back\\slash".to_string(), "new\nline".to_string(), "50\\%".to_string(), String::new()];
    assert_eq!(parse_values(&values_text(&vals)).unwrap(), vals);
    assert!(parse_values("a,b").is_err());
    assert!(parse_values("'a'x").is_err());
    assert!(parse_values("'a").is_err());
    assert_eq!(parse_values("'a' , 'b'").unwrap(), ["a", "b"]);
}

/// Review finding 2: COLUMN_TYPE writes `\n` and `\\` in every sql_mode, and an Edit or Move wrote them back as
/// `n` (and kept `\\` doubled under NO_BACKSLASH_ESCAPES).
#[test]
fn enum_values_read_with_escapes_whatever_the_mode() {
    let server = r"'a\nb','c\\d','it''s'";
    assert_eq!(parse_values(server).unwrap(), ["a\nb", "c\\d", "it's"]);
    let mut d = def("e", "ENUM");
    d.args = server.into();
    assert_eq!(definition(M, B, &d).unwrap(), "ENUM('a\nb','c\\\\d','it''s') NULL DEFAULT NULL");
    assert_eq!(definition(M, NB, &d).unwrap(), "ENUM('a\nb','c\\d','it''s') NULL DEFAULT NULL");
    // Read back from what the server shows, a move writes the same values.
    let c = Column { name: "e".into(), full_type: format!("enum({server})"), nullable: true, ..Default::default() };
    let mv = Change::Modify { from: "e".into(), def: ColDef::from_column(&c).unwrap(), pos: Pos::First };
    assert_eq!(alter(M, "8.4.0", NB, "t", &[mv]).unwrap(), "ALTER TABLE `t`\n  MODIFY `e` ENUM('a\nb','c\\d','it''s') NULL DEFAULT NULL FIRST");
}

/// Review finding 3: under ANSI_QUOTES `"` opens an identifier where a backslash escapes nothing, so this closed
/// the expression early and smuggled a second statement.
#[test]
fn expressions_follow_ansi_quotes() {
    let x = "\"\\\"); DROP TABLE victim; SELECT (\"";
    let ansi = Mode { bs: true, ansi: true };
    assert!(check_expr(x, ansi).is_err());
    let mut d = def("c", "VARCHAR");
    d.args = "20".into();
    d.default = Dv::Expr(x.into());
    assert!(definition(M, ansi, &d).is_err());
    // Without ANSI_QUOTES it is one string, and the statement stays one statement.
    let sql = alter(M, "8.4.0", B, "t", &[Change::Add { def: d, pos: Pos::Keep }]).unwrap();
    assert_eq!(lex::count_with(&sql, Dialect::MySql, true, false), 1, "{sql}");
    assert!(check_expr("\"a\"\"b\"", ansi).is_ok() && check_expr("'it''s'", ansi).is_ok());
    assert_eq!(Mode::of("ANSI_QUOTES,NO_BACKSLASH_ESCAPES"), Mode { bs: false, ansi: true });
    assert_eq!(Mode::of("REAL_AS_FLOAT,PIPES_AS_CONCAT,ANSI_QUOTES,IGNORE_SPACE,ONLY_FULL_GROUP_BY,ANSI"), Mode { bs: true, ansi: true });
    assert_eq!(Mode::of("STRICT_TRANS_TABLES,NO_ENGINE_SUBSTITUTION"), B);
}

/// Review finding 4: MySQL reports binary defaults as hex and BIT defaults as b'..', which were quoted as text.
#[test]
fn binary_and_bit_defaults_keep_their_literal() {
    let b = |d: Dialect, ty: &str, args: &str, v: &str| definition(d, B, &ColDef { args: args.into(), default: Dv::Value(v.into()), ..def("x", ty) }).unwrap();
    assert_eq!(b(M, "VARBINARY", "10", "0x6162"), "VARBINARY(10) NULL DEFAULT 0x6162");
    assert_eq!(b(M, "BINARY", "4", "0x6162"), "BINARY(4) NULL DEFAULT 0x6162");
    assert_eq!(b(M, "VARBINARY", "10", "0x"), "VARBINARY(10) NULL DEFAULT X''");
    assert_eq!(b(M, "BIT", "1", "b'1'"), "BIT(1) NULL DEFAULT b'1'");
    assert_eq!(b(Dialect::MariaDb, "BIT", "3", "b'101'"), "BIT(3) NULL DEFAULT b'101'");
    // MariaDB reports the text itself, so text that looks like hex stays text there, and so does a text column.
    assert_eq!(b(Dialect::MariaDb, "VARBINARY", "10", "0x41"), "VARBINARY(10) NULL DEFAULT '0x41'");
    assert_eq!(b(M, "VARCHAR", "10", "0x41"), "VARCHAR(10) NULL DEFAULT '0x41'");
    assert_eq!(b(M, "VARBINARY", "10", "0xZZ"), "VARBINARY(10) NULL DEFAULT '0xZZ'");
}

/// Review finding 5: attributes `from_column` did not read were dropped by an Edit or Move.
#[test]
fn column_and_index_attributes_are_kept_or_refused() {
    let p = Column { name: "p".into(), full_type: "point".into(), srid: Some(4326), ..Default::default() };
    assert_eq!(definition(M, B, &ColDef::from_column(&p).unwrap()).unwrap(), "POINT SRID 4326 NOT NULL");
    let j = Column { name: "j".into(), full_type: "longtext".into(), nullable: true, collation: Some("utf8mb4_bin".into()), check: Some("json_valid(`j`)".into()), ..Default::default() };
    let jd = ColDef::from_column(&j).unwrap();
    assert_eq!(definition(Dialect::MariaDb, B, &jd).unwrap(), "JSON NULL DEFAULT NULL");
    let k = Column { name: "k".into(), full_type: "int(11)".into(), check: Some("`k` > 0".into()), ..Default::default() };
    assert!(ColDef::from_column(&k).unwrap_err().contains("CHECK"));
    // A JSON check that names another column is not the JSON alias.
    assert!(ColDef::from_column(&Column { check: Some("json_valid(`x`)".into()), ..j.clone() }).is_err());
    let z = Column { name: "z".into(), full_type: "blob /*M!100301 COMPRESSED*/".into(), ..Default::default() };
    assert!(ColDef::from_column(&z).unwrap_err().contains("COMPRESSED"));
    let mut ix = IndexDef { name: "i".into(), cols: vec![IdxCol { name: "a".into(), prefix: None, desc: false }], invisible: true, ..Default::default() };
    assert_eq!(alter(M, "8.4.0", B, "t", &[Change::AddIndex(ix.clone())]).unwrap(), "ALTER TABLE `t`\n  ADD INDEX `i` (`a`) INVISIBLE");
    ix.comment = "c".into();
    assert_eq!(alter(Dialect::MariaDb, "11.4.0-MariaDB", B, "t", &[Change::AddIndex(ix)]).unwrap(), "ALTER TABLE `t`\n  ADD INDEX `i` (`a`) COMMENT 'c' IGNORED");
    let read = Index { name: "i".into(), cols: vec![IndexCol { name: "a".into(), ..Default::default() }], kind: "BTREE".into(), invisible: true, ..Default::default() };
    assert!(IndexDef::from_index(&read).invisible);
}

#[test]
fn undo_of_each_shape() {
    let t = table();
    let inv = |c: &[Change]| inverse(M, "8.4.0", B, "orders", &t, c).unwrap().join(";\n");
    assert_eq!(inv(&[Change::Add { def: def("qty", "INT"), pos: Pos::Keep }]), "ALTER TABLE `orders`\n  DROP COLUMN `qty`");
    let mut d = ColDef::from_column(&t.columns[2]).unwrap();
    d.args = "12,2".into();
    assert_eq!(inv(&[Change::Modify { from: "total".into(), def: d.clone(), pos: Pos::Keep }]), "ALTER TABLE `orders`\n  MODIFY `total` DECIMAL(10,2) UNSIGNED NOT NULL DEFAULT 0.00 COMMENT 'Incl. tax'");
    // A move comes back to where the column was, a rename back to the old name.
    d.name = "sum".into();
    assert_eq!(inv(&[Change::Modify { from: "total".into(), def: d, pos: Pos::First }]), "ALTER TABLE `orders`\n  CHANGE COLUMN `sum` `total` DECIMAL(10,2) UNSIGNED NOT NULL DEFAULT 0.00 COMMENT 'Incl. tax' AFTER `customer_id`");
    // Dropping a column in a two-column index and a one-column index brings both back.
    assert_eq!(inv(&[Change::Drop("total".into())]), "ALTER TABLE `orders`\n  ADD COLUMN `total` DECIMAL(10,2) UNSIGNED NOT NULL DEFAULT 0.00 COMMENT 'Incl. tax' AFTER `customer_id`,\n  DROP INDEX `idx_cust_total`,\n  ADD INDEX `idx_cust_total` (`customer_id`, `total`)");
    assert_eq!(inv(&[Change::Drop("note".into())]), "ALTER TABLE `orders`\n  ADD COLUMN `note` VARCHAR(50) CHARACTER SET utf8mb4 COLLATE utf8mb4_0900_ai_ci NULL DEFAULT NULL AFTER `total`,\n  ADD UNIQUE INDEX `uq_note` (`note`)");
    let new_ix = IndexDef { name: "i2".into(), kind: IndexKind::Index, cols: vec![IdxCol { name: "id".into(), prefix: None, desc: false }], comment: String::new(), ..Default::default() };
    assert_eq!(inv(&[Change::AddIndex(new_ix)]), "ALTER TABLE `orders`\n  DROP INDEX `i2`");
    assert_eq!(inv(&[Change::DropIndex("uq_note".into())]), "ALTER TABLE `orders`\n  ADD UNIQUE INDEX `uq_note` (`note`)");
    assert_eq!(inv(&[Change::DropIndex("PRIMARY".into())]), "ALTER TABLE `orders`\n  ADD PRIMARY KEY (`id`)");
    let rn = Change::RenameIndex { from: "uq_note".into(), to: "uq_memo".into(), def: IndexDef::from_index(&t.indexes[2]) };
    assert_eq!(inv(&[rn]), "ALTER TABLE `orders`\n  RENAME INDEX `uq_memo` TO `uq_note`");
}

/// Review finding 14: a collation that is only the table's default is not pinned on undo.
#[test]
fn undo_leaves_the_table_charset_implicit() {
    let mut t = table();
    t.status.collation = "utf8mb4_0900_ai_ci".into();
    assert_eq!(inverse(M, "8.4.0", B, "orders", &t, &[Change::Drop("note".into())]).unwrap()[0], "ALTER TABLE `orders`\n  ADD COLUMN `note` VARCHAR(50) NULL DEFAULT NULL AFTER `total`,\n  ADD UNIQUE INDEX `uq_note` (`note`)");
    t.status.collation = "latin1_swedish_ci".into();
    assert!(inverse(M, "8.4.0", B, "orders", &t, &[Change::Drop("note".into())]).unwrap()[0].contains("CHARACTER SET utf8mb4 COLLATE utf8mb4_0900_ai_ci"));
    assert!(same_name("Note", "NOTE") && !same_name("note", "notes"));
    assert_eq!(free_name(&["A_copy".into()], "a"), "a_copy2");
}

/// Final review finding 6: an undo kept its steps in the mode of the change, and a session whose sql_mode changed
/// since refused it with "Nothing ran" forever. The same inverse is now written again for the new mode.
#[test]
fn an_undo_is_written_again_for_a_changed_sql_mode() {
    let mut t = table();
    let i = t.columns.iter().position(|c| c.name == "total").unwrap();
    t.columns[i].comment = "C:\\temp".into();
    let c = [Change::Drop("total".into())];
    let u = plan(M, "8.4.0", B, "orders", &t, &c).unwrap().undo.unwrap();
    let nb = plan(M, "8.4.0", NB, "orders", &t, &c).unwrap().undo.unwrap().steps;
    assert!(u.steps[0].contains("'C:\\\\temp'") && nb[0].contains("'C:\\temp'"), "{:?} {nb:?}", u.steps);
    assert_eq!(u.steps_for(M, "8.4.0", "orders", B).unwrap(), u.steps);
    assert_eq!(u.steps_for(M, "8.4.0", "orders", NB).unwrap(), nb);
    // One that cannot be written for the new mode says so; the worker then drops the entry.
    assert!(AlterUndo { groups: vec![Vec::new()], ..u }.steps_for(M, "8.4.0", "orders", NB).is_err());
}

/// Review finding 6: the inverse used to be one ALTER, which the server rejects when a clause names a column
/// another clause renames or drops. Each step is now its own statement, in order.
#[test]
fn undo_of_a_combined_change_runs_backwards() {
    let t = table();
    let c = [Change::Add { def: def("qty", "INT"), pos: Pos::After("id".into()) }, Change::Modify { from: "qty".into(), def: def("amount", "INT"), pos: Pos::Keep }, Change::Drop("customer_id".into())];
    let back = inverse(M, "8.4.0", B, "orders", &t, &c).unwrap();
    assert_eq!(back.len(), 3, "{back:?}");
    assert!(back[0].starts_with("ALTER TABLE `orders`\n  ADD COLUMN `customer_id`") && back[0].contains("AFTER `amount`"), "{back:?}");
    assert!(back[0].contains("ADD INDEX `idx_cust_total` (`customer_id`, `total`)"), "{back:?}");
    assert_eq!(back[1], "ALTER TABLE `orders`\n  CHANGE COLUMN `amount` `qty` INT NULL DEFAULT NULL");
    assert_eq!(back[2], "ALTER TABLE `orders`\n  DROP COLUMN `qty`");
    // No undo for a generated column or an unknown one.
    let mut g = table();
    g.columns[3].extra = "STORED GENERATED".into();
    assert!(inverse(M, "8.4.0", B, "orders", &g, &[Change::Drop("note".into())]).is_err());
    assert!(inverse(M, "8.4.0", B, "orders", &t, &[Change::Drop("nope".into())]).is_err());
}

/// The table of the live tests: id, name, note, price, created, kind; idx_name (name), uq_price (price, name).
fn shop() -> Structure {
    let mut id = col("id", "int unsigned", false, None);
    id.extra = "auto_increment".into();
    let cols = vec![id, col("name", "varchar(50)", false, Some("x")), col("note", "text", true, None), col("price", "decimal(10,2)", false, Some("0.00")), col("created", "datetime", true, None), col("kind", "enum('a','b')", true, None)];
    Structure { columns: cols, indexes: vec![ix("PRIMARY", true, &["id"]), ix("idx_name", false, &["name"]), ix("uq_price", true, &["price", "name"])], ..Default::default() }
}

fn renamed(st: &Structure, from: &str, to: &str) -> Change {
    let c = st.columns.iter().find(|c| c.name == from).unwrap();
    Change::Modify { from: from.into(), def: ColDef { name: to.into(), ..ColDef::from_column(c).unwrap() }, pos: Pos::Keep }
}

/// The four sequences the review proved fail as one statement, each step naming columns as the steps before it
/// left them.
#[test]
fn undo_steps_name_columns_as_they_are_then() {
    let t = shop();
    let inv = |c: &[Change]| inverse(M, "8.4.0", B, "t", &t, c).unwrap();
    let name = ColDef::from_column(&t.columns[1]).unwrap();
    let back = inv(&[Change::Add { def: def("x", "INT"), pos: Pos::After("id".into()) }, Change::Modify { from: "name".into(), def: name, pos: Pos::After("id".into()) }]);
    assert_eq!(back.len(), 2);
    assert!(back[0].contains("MODIFY `name`") && back[0].ends_with("AFTER `x`") && back[1].ends_with("DROP COLUMN `x`"), "{back:?}");
    let back = inv(&[renamed(&t, "name", "title"), Change::DropIndex("idx_name".into())]);
    assert_eq!(back, ["ALTER TABLE `t`\n  ADD INDEX `idx_name` (`title`)", "ALTER TABLE `t`\n  CHANGE COLUMN `title` `name` VARCHAR(50) NOT NULL DEFAULT 'x'"]);
    let back = inv(&[renamed(&t, "name", "title"), Change::Drop("note".into())]);
    assert!(back[0].contains("ADD COLUMN `note` TEXT NULL DEFAULT NULL AFTER `title`") && back[1].contains("CHANGE COLUMN `title` `name`"), "{back:?}");
    let back = inv(&[renamed(&t, "name", "title"), Change::Drop("price".into())]);
    assert!(back[0].contains("ADD UNIQUE INDEX `uq_price` (`price`, `title`)") && back[1].contains("CHANGE COLUMN `title` `name`"), "{back:?}");
}

/// Review finding 8: MariaDB's added-back index named a column by the name the same ALTER renames away.
#[test]
fn mariadb_readds_an_index_under_final_names() {
    let t = shop();
    let c = [Change::Drop("price".into()), renamed(&t, "name", "title")];
    let e = expand(Dialect::MariaDb, &t, &c);
    assert!(e.contains(&Change::AddIndex(IndexDef { name: "uq_price".into(), kind: IndexKind::Unique, cols: vec![IdxCol { name: "title".into(), prefix: None, desc: false }], ..Default::default() })), "{e:?}");
    let p = plan(Dialect::MariaDb, "11.4.0-MariaDB", B, "t", &t, &c).unwrap();
    assert!(p.sql().contains("ADD UNIQUE INDEX `uq_price` (`title`)") && !p.sql().contains("(`name`)"), "{}", p.sql());
    assert!(p.undo.is_ok());
}

/// Review finding 9: the staged bar showed the statement without MariaDB's index edits. The preview and Apply
/// both come from `plan`, which expands them.
#[test]
fn preview_is_the_statement_that_runs_on_mariadb() {
    let t = table();
    let c = [Change::Drop("note".into())];
    let p = plan(Dialect::MariaDb, "11.4.0-MariaDB", B, "orders", &t, &c).unwrap();
    assert_eq!(p.sql(), alter(Dialect::MariaDb, "11.4.0-MariaDB", B, "orders", &expand(Dialect::MariaDb, &t, &c)).unwrap());
    assert_eq!(p.sql(), "ALTER TABLE `orders`\n  DROP INDEX `uq_note`,\n  DROP COLUMN `note`");
}

/// Review findings 1 and 10: what the undo loses and why there is none are known before Apply.
#[test]
fn plan_names_what_undo_loses_or_why_there_is_none() {
    let t = table();
    let p = |c: &[Change]| plan(M, "8.4.0", B, "orders", &t, c).unwrap();
    let add = p(&[Change::Add { def: def("qty", "INT"), pos: Pos::Keep }]).undo.unwrap();
    assert_eq!(add.loss, ["Undo drops column qty and every value it holds."]);
    let mut wide = ColDef::from_column(&t.columns[3]).unwrap();
    wide.args = "255".into();
    let u = p(&[Change::Modify { from: "note".into(), def: wide.clone(), pos: Pos::Keep }]).undo.unwrap();
    assert_eq!(u.loss, ["Undo converts note back to VARCHAR(50), which can cut or reject the values it holds now."]);
    // Renaming and moving touch no values, and undoing a drop only adds.
    wide.args = "50".into();
    wide.name = "memo".into();
    assert!(p(&[Change::Modify { from: "note".into(), def: wide, pos: Pos::First }]).undo.unwrap().loss.is_empty());
    assert!(p(&[Change::Drop("note".into())]).undo.unwrap().loss.is_empty());
    // The undo is checked against these names: the old and new name and the column it goes after.
    let u = p(&[renamed(&t, "note", "memo")]).undo.unwrap();
    assert_eq!((u.cols.clone(), u.idx.clone()), (vec!["memo".to_string(), "note".to_string()], vec![]));
    let u = p(&[Change::Drop("note".into())]).undo.unwrap();
    assert_eq!((u.cols, u.idx), (vec!["note".to_string(), "total".to_string()], vec!["uq_note".to_string()]));
    let mut g = table();
    g.columns[3].extra = "STORED GENERATED".into();
    let none = plan(M, "8.4.0", B, "orders", &g, &[Change::Drop("note".into())]).unwrap();
    assert_eq!(none.sql(), "ALTER TABLE `orders`\n  DROP COLUMN `note`");
    assert!(none.undo.unwrap_err().contains("generated"));
}

#[test]
fn snapshots_differ_when_a_touched_column_changed() {
    let t = table();
    let names = (vec!["note".to_string(), "gone".to_string()], vec!["uq_note".to_string()]);
    let a = snapshot(&t, &names.0, &names.1, &[]);
    assert_eq!(a.len(), 3);
    assert!(a[1].1.is_none());
    let mut t2 = t.clone();
    t2.columns[2].comment = "other".into();
    assert_eq!(snapshot(&t2, &names.0, &names.1, &[]), a, "an untouched column does not count");
    t2.columns[3].full_type = "varchar(60)".into();
    assert_ne!(snapshot(&t2, &names.0, &names.1, &[]), a);
    let mut t3 = t.clone();
    t3.indexes[2].invisible = true;
    assert_ne!(snapshot(&t3, &names.0, &names.1, &[]), a);
}

/// Review finding 7: a functional key part has no column name, so its drop has no undo.
#[test]
fn a_functional_index_has_no_undo() {
    let mut t = table();
    t.indexes.push(Index { name: "f".into(), cols: vec![IndexCol { expr: Some("(`id` + 1)".into()), ..Default::default() }], kind: "BTREE".into(), ..Default::default() });
    let p = plan(M, "8.4.0", B, "orders", &t, &[Change::DropIndex("f".into())]).unwrap();
    assert!(p.undo.unwrap_err().contains("expression"));
    assert!(alter(M, "8.4.0", B, "orders", &[Change::AddIndex(IndexDef::from_index(&t.indexes[3]))]).is_err());
}

#[test]
fn mariadb_drops_the_indexes_a_column_leaves_empty() {
    let t = table();
    let c = [Change::Drop("note".into()), Change::Drop("total".into())];
    assert_eq!(expand(M, &t, &c), c);
    let e = expand(Dialect::MariaDb, &t, &c);
    // uq_note holds only note and goes; idx_cust_total holds total next to customer_id and comes back without it.
    let shrunk = IndexDef { cols: vec![IdxCol { name: "customer_id".into(), prefix: None, desc: false }], ..IndexDef::from_index(&t.indexes[1]) };
    assert_eq!(e, [Change::DropIndex("uq_note".into()), Change::Drop("note".into()), Change::DropIndex("idx_cust_total".into()), Change::AddIndex(shrunk), Change::Drop("total".into())]);
    let back = inverse(Dialect::MariaDb, "11.4.0-MariaDB", B, "orders", &t, &e).unwrap().join(";\n");
    assert!(back.find("ADD COLUMN `total`").unwrap() < back.find("ADD COLUMN `note`").unwrap());
    assert!(back.contains("ADD UNIQUE INDEX `uq_note` (`note`)") && back.contains("ADD INDEX `idx_cust_total` (`customer_id`, `total`)"), "{back}");
}

#[test]
fn order_follows_the_staged_moves() {
    let t = table();
    let c = [Change::Modify { from: "total".into(), def: def("total", "INT"), pos: Pos::First }, Change::Add { def: def("qty", "INT"), pos: Pos::After("id".into()) }, Change::Drop("note".into())];
    assert_eq!(columns_after(&t, &c), ["total", "id", "qty", "customer_id"]);
}

#[test]
fn warnings_name_what_cannot_come_back() {
    let t = table();
    assert_eq!(warnings(&t, &[Change::Drop("note".into())]).len(), 1);
    let mut same = ColDef::from_column(&t.columns[2]).unwrap();
    assert!(warnings(&t, &[Change::Modify { from: "total".into(), def: same.clone(), pos: Pos::Keep }]).is_empty());
    same.ty = "VARCHAR".into();
    assert_eq!(warnings(&t, &[Change::Modify { from: "total".into(), def: same, pos: Pos::Keep }]).len(), 1);
    assert!(drops_data(&[Change::Drop("a".into())]) && !drops_data(&[Change::DropIndex("a".into())]));
    assert_eq!(warnings(&t, &[Change::DropIndex("PRIMARY".into())]).len(), 1);
}

#[test]
fn index_kinds_check_their_columns() {
    let c = |t: &str| vec![("a".to_string(), t.to_string())];
    assert!(index_problem(IndexKind::Fulltext, &c("varchar(20)")).is_none());
    assert!(index_problem(IndexKind::Fulltext, &c("int")).is_some());
    assert!(index_problem(IndexKind::Spatial, &c("point")).is_none());
    assert!(index_problem(IndexKind::Spatial, &c("int")).is_some());
    assert!(index_problem(IndexKind::Unique, &c("int")).is_none());
}

#[test]
fn copy_names_are_free() {
    assert_eq!(free_name(&["a".into()], "a"), "a_copy");
    assert_eq!(free_name(&["a_copy".into(), "a_copy2".into()], "a"), "a_copy3");
}

fn fk(name: &str, cols: &[&str], rt: &str, rc: &[&str]) -> ForeignKey {
    ForeignKey { name: name.into(), db: "shop".into(), table: "orders".into(), cols: cols.iter().map(|c| (*c).into()).collect(), ref_db: "shop".into(), ref_table: rt.into(), ref_cols: rc.iter().map(|c| (*c).into()).collect(), on_update: "CASCADE".into(), on_delete: "RESTRICT".into(), ..Default::default() }
}

fn fcol(name: &str, ty: &str, coll: Option<&str>) -> Column {
    Column { name: name.into(), full_type: ty.into(), nullable: false, collation: coll.map(Into::into), ..Default::default() }
}

#[test]
fn add_a_foreign_key() {
    let k = fk("fk_orders_customer", &["customer_id"], "customers", &["id"]);
    assert_eq!(alt(&[Change::AddFk(k.clone())]), "ALTER TABLE `orders`\n  ADD CONSTRAINT `fk_orders_customer`\n    FOREIGN KEY (`customer_id`)\n    REFERENCES `customers` (`id`)\n    ON UPDATE CASCADE ON DELETE RESTRICT");
    // Composite keys keep their column order, and another schema is written out.
    let c = ForeignKey { ref_db: "other".into(), on_delete: "set null".into(), ..fk("fk_c", &["a", "b"], "t", &["x", "y"]) };
    assert_eq!(alt(&[Change::AddFk(c)]), "ALTER TABLE `orders`\n  ADD CONSTRAINT `fk_c`\n    FOREIGN KEY (`a`, `b`)\n    REFERENCES `other`.`t` (`x`, `y`)\n    ON UPDATE CASCADE ON DELETE SET NULL");
    assert_eq!(alt(&[Change::DropFk("fk_orders_customer".into())]), "ALTER TABLE `orders`\n  DROP FOREIGN KEY `fk_orders_customer`");
}

#[test]
fn foreign_key_names_are_quoted_and_checked() {
    // A backtick in any name is doubled, under both escaping modes, since no string literal is involved.
    for bs in [true, false] {
        let k = fk("fk`; DROP TABLE x; --", &["we`ird"], "ta`ble", &["c\"ol"]);
        let sql = alter(M, "8.4.0", Mode { bs, ansi: false }, "or`ders", &[Change::AddFk(k)]).unwrap();
        assert_eq!(sql, "ALTER TABLE `or``ders`\n  ADD CONSTRAINT `fk``; DROP TABLE x; --`\n    FOREIGN KEY (`we``ird`)\n    REFERENCES `ta``ble` (`c\"ol`)\n    ON UPDATE CASCADE ON DELETE RESTRICT", "bs={bs}");
        assert_eq!(alter(M, "8.4.0", Mode { bs, ansi: false }, "t", &[Change::DropFk("a`b".into())]).unwrap(), "ALTER TABLE `t`\n  DROP FOREIGN KEY `a``b`");
    }
    let bad = |k: ForeignKey| alter(M, "8.4.0", B, "t", &[Change::AddFk(k)]);
    assert!(bad(fk("", &["a"], "t", &["b"])).is_err());
    assert!(bad(fk("n", &[], "t", &[])).is_err());
    assert!(bad(fk("n", &["a", "b"], "t", &["x"])).is_err());
    assert!(bad(ForeignKey { on_delete: "CASCADE; DROP".into(), ..fk("n", &["a"], "t", &["b"]) }).is_err());
    assert!(bad(fk("n", &["a"], "", &["b"])).is_err());
}

fn fk_table() -> Structure {
    let mut s = table();
    s.foreign_keys = vec![fk("fk_cust", &["customer_id"], "customers", &["id"])];
    s.indexes.push(ix("fk_cust", false, &["customer_id"]));
    s.indexes.retain(|i| i.name != "idx_cust_total");
    s
}

#[test]
fn a_new_key_gets_the_index_it_needs_written_out() {
    let t = table();
    let k = fk("fk_note", &["total"], "notes", &["n"]);
    let e = expand(M, &t, &[Change::AddFk(k.clone())]);
    assert_eq!(e.len(), 2);
    assert!(matches!(&e[0], Change::AddIndex(d) if d.name == "fk_note" && d.cols[0].name == "total"));
    // idx_cust_total starts with customer_id, so a key on it needs nothing more.
    let served = fk("fk_cust", &["customer_id"], "customers", &["id"]);
    assert_eq!(expand(M, &t, &[Change::AddFk(served)]).len(), 1);
    // The index name follows the key's unless that is taken.
    let taken = fk("uq_note", &["note"], "notes", &["n"]);
    assert!(matches!(&expand(M, &t, &[Change::AddFk(ForeignKey { cols: vec!["total".into()], ..taken })])[0], Change::AddIndex(d) if d.name == "uq_note_idx"));
    let sql = alter(M, "8.4.0", B, "orders", &e).unwrap();
    assert!(sql.starts_with("ALTER TABLE `orders`\n  ADD INDEX `fk_note` (`total`),\n  ADD CONSTRAINT `fk_note`"), "{sql}");
}

#[test]
fn dropping_a_key_takes_only_the_index_made_for_it() {
    let t = fk_table();
    let e = expand(M, &t, &[Change::DropFk("FK_CUST".into())]);
    assert_eq!(e, vec![Change::DropFk("FK_CUST".into()), Change::DropIndex("fk_cust".into())]);
    assert_eq!(alter(M, "8.4.0", B, "orders", &e).unwrap(), "ALTER TABLE `orders`\n  DROP FOREIGN KEY `FK_CUST`,\n  DROP INDEX `fk_cust`");
    assert_eq!(warnings(&t, &[Change::DropFk("fk_cust".into())]).len(), 1);
    // An index with another name, or on more columns, was made by someone, so it stays.
    let mut other = t.clone();
    other.indexes.iter_mut().find(|i| i.name == "fk_cust").unwrap().name = "mine".into();
    assert_eq!(expand(M, &other, &[Change::DropFk("fk_cust".into())]).len(), 1);
    assert!(warnings(&other, &[Change::DropFk("fk_cust".into())]).is_empty());
    let mut wide = t.clone();
    wide.indexes.iter_mut().find(|i| i.name == "fk_cust").unwrap().cols.push(IndexCol { name: "total".into(), ..Default::default() });
    assert_eq!(expand(M, &wide, &[Change::DropFk("fk_cust".into())]).len(), 1);
    // Another key that leans on the same index keeps it.
    let mut shared = t.clone();
    shared.foreign_keys.push(fk("fk_other", &["customer_id"], "customers", &["id"]));
    assert_eq!(expand(M, &shared, &[Change::DropFk("fk_cust".into())]).len(), 1);
}

#[test]
fn foreign_key_undo_is_the_exact_inverse() {
    let t = table();
    let k = fk("fk_note", &["total"], "notes", &["n"]);
    let added = expand(M, &t, &[Change::AddFk(k.clone())]);
    assert_eq!(inverse(M, "8.4.0", B, "orders", &t, &added).unwrap(), ["ALTER TABLE `orders`\n  DROP FOREIGN KEY `fk_note`", "ALTER TABLE `orders`\n  DROP INDEX `fk_note`"]);
    let f = fk_table();
    let dropped = expand(M, &f, &[Change::DropFk("fk_cust".into())]);
    assert_eq!(inverse(M, "8.4.0", B, "orders", &f, &dropped).unwrap(), ["ALTER TABLE `orders`\n  ADD INDEX `fk_cust` (`customer_id`)", "ALTER TABLE `orders`\n  ADD CONSTRAINT `fk_cust`\n    FOREIGN KEY (`customer_id`)\n    REFERENCES `customers` (`id`)\n    ON UPDATE CASCADE ON DELETE RESTRICT"]);
    // A key that is not there cannot be dropped, and one that is cannot be added twice.
    assert!(inverse(M, "8.4.0", B, "orders", &t, &[Change::DropFk("nope".into())]).is_err());
    assert!(inverse(M, "8.4.0", B, "orders", &f, &[Change::AddFk(fk("fk_cust", &["total"], "x", &["y"]))]).is_err());
    // The undo is checked against the key and the index made for it, like any other structure undo.
    let u = plan(M, "8.4.0", B, "orders", &t, &[Change::AddFk(k)]).unwrap().undo.unwrap();
    assert_eq!((u.fks, u.idx, u.cols), (vec!["fk_note".to_string()], vec!["fk_note".to_string()], Vec::<String>::new()));
}

#[test]
fn foreign_key_column_types_must_fit() {
    let p = |a: Column, b: Column| fk_pair_problem(Dialect::MySql, "orders", &a, "customers", &b);
    let int_u = fcol("id", "int unsigned", None);
    assert!(p(fcol("customer_id", "int unsigned", None), int_u.clone()).is_none());
    assert!(p(fcol("customer_id", "int", None), int_u.clone()).unwrap().contains("signedness"));
    assert!(p(fcol("customer_id", "bigint unsigned", None), int_u.clone()).unwrap().contains("same size"));
    assert!(p(fcol("c", "varchar(10)", None), int_u.clone()).unwrap().contains("same kind"));
    assert!(p(fcol("c", "text", None), fcol("d", "text", None)).unwrap().contains("cannot be part"));
    assert!(p(fcol("c", "json", None), fcol("d", "json", None)).is_some());
    // Strings may differ in length, not in collation.
    assert!(p(fcol("c", "char(3)", Some("utf8mb4_0900_ai_ci")), fcol("code", "varchar(10)", Some("utf8mb4_0900_ai_ci"))).is_none());
    assert!(p(fcol("c", "char(3)", Some("utf8mb4_0900_ai_ci")), fcol("code", "char(3)", Some("latin1_swedish_ci"))).unwrap().contains("collation"));
    assert!(p(fcol("c", "decimal(10,2)", None), fcol("d", "decimal(12,2)", None)).is_some());
    assert!(p(fcol("c", "decimal(10,2)", None), fcol("d", "decimal(10,2)", None)).is_none());
    assert!(p(fcol("c", "datetime", None), fcol("d", "datetime(3)", None)).is_some());
    assert!(p(fcol("c", "datetime(3)", None), fcol("d", "DATETIME(3)", None)).is_none());
}

#[test]
fn a_whole_key_is_checked_before_it_is_staged() {
    let own = Structure { columns: vec![fcol("customer_id", "int unsigned", None), Column { nullable: true, ..fcol("opt", "int unsigned", None) }], ..Default::default() };
    let parent = Structure { columns: vec![fcol("id", "int unsigned", None)], indexes: vec![ix("PRIMARY", true, &["id"])], ..Default::default() };
    let k = fk("fk", &["customer_id"], "customers", &["id"]);
    assert!(fk_problem(&own, "orders", &parent, &k, false).is_none());
    let set_null = ForeignKey { on_delete: "SET NULL".into(), ..k.clone() };
    assert!(fk_problem(&own, "orders", &parent, &set_null, false).unwrap().contains("allows NULL"));
    assert!(fk_problem(&own, "orders", &parent, &ForeignKey { cols: vec!["opt".into()], ..set_null }, false).is_none());
    let unindexed = Structure { indexes: vec![], ..parent.clone() };
    assert!(fk_problem(&own, "orders", &unindexed, &k, false).unwrap().contains("no index"));
    assert!(fk_problem(&own, "orders", &parent, &ForeignKey { cols: vec!["gone".into()], ..k }, false).is_some());
}

#[test]
fn postgres_keys_need_types_the_server_compares_and_a_unique_parent() {
    let pgcol = |n: &str, t: &str| Column { name: n.into(), full_type: t.into(), ..fcol(n, t, None) };
    let p = |a: &str, b: &str| fk_pair_problem(Dialect::Postgres, "orders", &pgcol("c", a), "customers", &pgcol("id", b));
    // every integer size meets every other, and an integer meets a numeric, text meets varchar
    assert!(p("integer", "bigint").is_none() && p("smallint", "integer").is_none() && p("integer", "numeric(10,2)").is_none());
    assert!(p("character varying(40)", "text").is_none() && p("date", "timestamp with time zone").is_none());
    // a different family is named with its types
    for (a, b) in [("integer", "text"), ("uuid", "integer"), ("boolean", "integer"), ("bytea", "text"), ("character(3)", "integer")] {
        assert!(p(a, b).unwrap().contains("can compare"), "{a} and {b}");
    }
    // character(n) and text are compared through the implicit cast, so the server takes a key between them
    assert!(p("text", "character(3)").is_none() && p("character(3)", "text").is_none() && p("character(3)", "character varying(9)").is_none());
    // an integer child against a numeric parent is taken, the other way round the server says it "cannot be implemented"
    assert!(p("integer", "numeric(10,2)").is_none() && p("numeric(10,2)", "integer").unwrap().contains("can compare") && p("numeric", "bigint").is_some());
    // a domain, an enum or an extension type is the server's to judge
    assert!(p("my_domain", "integer").is_none() && p("mood", "mood").is_none());
    let own = Structure { columns: vec![pgcol("a", "integer"), Column { nullable: true, ..pgcol("b", "integer") }, pgcol("c", "text")], pg_schema: Some("public".into()), ..Default::default() };
    let key = |name: &str, cols: &[&str]| Index { name: name.into(), unique: true, cols: cols.iter().map(|c| IndexCol { name: (*c).into(), ..Default::default() }).collect(), ..Default::default() };
    let parent = Structure { columns: vec![pgcol("id", "integer"), pgcol("code", "integer"), pgcol("note", "text")], indexes: vec![key("p_pkey", &["id"]), key("p_pair", &["id", "code"]), Index { unique: false, ..key("p_note", &["note"]) }], pg_schema: Some("public".into()), ..Default::default() };
    let k = fk("k", &["a"], "p", &["id"]);
    assert!(fk_problem(&own, "orders", &parent, &k, false).is_none());
    // the referenced columns must be exactly a unique key, in any order, and a plain index does not count
    assert!(fk_problem(&own, "orders", &parent, &fk("k", &["a", "b"], "p", &["code", "id"]), false).is_none());
    assert!(fk_problem(&own, "orders", &parent, &fk("k", &["a"], "p", &["code"]), false).unwrap().contains("exactly (code)"));
    assert!(fk_problem(&own, "orders", &parent, &fk("k", &["c"], "p", &["note"]), false).unwrap().contains("primary key or unique"));
    let deferrable = Structure { indexes: vec![Index { deferrable: true, ..key("p_pkey", &["id"]) }], ..parent.clone() };
    assert!(fk_problem(&own, "orders", &deferrable, &k, false).is_some(), "a deferrable unique constraint cannot be referenced");
    assert!(fk_problem(&own, "orders", &parent, &fk("k", &["a", "a"], "p", &["id", "code"]), false).unwrap().contains("twice"));
    let set_null = ForeignKey { on_delete: "SET NULL".into(), ..k.clone() };
    assert!(fk_problem(&own, "orders", &parent, &set_null, false).unwrap().contains("allows NULL"));
    // MySQL's word for a key on part of a key does not reach PostgreSQL, where the whole key is always needed
    assert!(fk_problem(&own, "orders", &parent, &k, true).is_none());
}

#[test]
fn the_rows_that_stop_a_key_are_selected_as_text() {
    let k = ForeignKey { ref_db: "o\"b".into(), ..fk("n", &["a", "b"], "p", &["x", "y"]) };
    assert_eq!(orphan_sample_sql(Dialect::Postgres, "sh", "t", &k, 5), "SELECT * FROM (SELECT c.\"a\"::text AS k0, c.\"b\"::text AS k1 FROM \"sh\".\"t\" c WHERE c.\"a\" IS NOT NULL AND c.\"b\" IS NOT NULL AND NOT EXISTS (SELECT 1 FROM \"o\"\"b\".\"p\" p WHERE p.\"x\" = c.\"a\" AND p.\"y\" = c.\"b\") LIMIT 5) s");
    assert!(orphans_sql(Dialect::Postgres, "sh", "t", &k).starts_with("SELECT COUNT(*) AS n FROM \"sh\".\"t\" c WHERE"));
}

#[test]
fn a_match_full_key_also_counts_the_rows_that_are_part_null() {
    let k = ForeignKey { match_full: true, ref_db: String::new(), ..fk("n", &["a", "b"], "p", &["x", "y"]) };
    let count = orphans_sql(Dialect::Postgres, "sh", "t", &k);
    // MATCH SIMPLE leaves a row with a NULL part alone, MATCH FULL refuses it unless every part is NULL
    assert!(count.contains("(c.\"a\" IS NULL OR c.\"b\" IS NULL) AND (c.\"a\" IS NOT NULL OR c.\"b\" IS NOT NULL)"), "{count}");
    assert!(count.contains("NOT EXISTS (SELECT 1 FROM \"sh\".\"p\" p WHERE p.\"x\" = c.\"a\" AND p.\"y\" = c.\"b\")"), "{count}");
    assert!(orphan_sample_sql(Dialect::Postgres, "sh", "t", &k, 5).contains("IS NULL OR"));
    // a key of one column, or MATCH SIMPLE, is counted as before
    let simple = ForeignKey { match_full: false, ..k.clone() };
    assert!(!orphans_sql(Dialect::Postgres, "sh", "t", &simple).contains("IS NULL"));
    assert!(!orphans_sql(Dialect::Postgres, "sh", "t", &ForeignKey { match_full: true, ..fk("n", &["a"], "p", &["x"]) }).contains("IS NULL"));
}

#[test]
fn orphan_count_statement_quotes_every_name() {
    let k = ForeignKey { ref_db: "o`b".into(), ..fk("n", &["a`1", "b"], "pa`rent", &["x", "y`2"]) };
    assert_eq!(orphans_sql(M, "sh`op", "or`ders", &k), "SELECT COUNT(*) AS n FROM `sh``op`.`or``ders` c WHERE c.`a``1` IS NOT NULL AND c.`b` IS NOT NULL AND NOT EXISTS (SELECT 1 FROM `o``b`.`pa``rent` p WHERE p.`x` = c.`a``1` AND p.`y``2` = c.`b`)");
    let same = ForeignKey { ref_db: String::new(), ..fk("n", &["a"], "p", &["x"]) };
    assert!(orphans_sql(M, "shop", "orders", &same).contains("FROM `shop`.`p` p"));
}

fn trig(name: &str, body: &str) -> TrigDef {
    TrigDef { name: name.into(), timing: "before".into(), event: "update".into(), body: body.into(), pg: PgDef::default() }
}

fn save(def: TrigDef) -> TrigSave {
    TrigSave { def, definer: None, order: None }
}

#[test]
fn trigger_statements_by_engine() {
    let t = trig("orders_bu", "SET NEW.tag = 'x';");
    let live = crate::db::sql::structure::Trigger { name: "orders_bu".into(), ..Default::default() };
    let create = TrigJob::Create(save(t.clone()));
    let edit = TrigJob::Edit { was: live, save: TrigSave { definer: Some("app@%".into()), order: Some(TrigOrder::Follows("orders_first".into())), ..save(t.clone()) } };
    assert_eq!(trigger_preview(M, B, "orders", &create).unwrap(), ["CREATE TRIGGER `orders_bu` BEFORE UPDATE ON `orders`\nFOR EACH ROW\nSET NEW.tag = 'x'"]);
    // The definer and the place of an edited trigger show in the preview, as they are sent.
    assert_eq!(trigger_preview(M, B, "orders", &edit).unwrap(), ["DROP TRIGGER `orders_bu`", "CREATE DEFINER=`app`@`%` TRIGGER `orders_bu` BEFORE UPDATE ON `orders`\nFOR EACH ROW FOLLOWS `orders_first`\nSET NEW.tag = 'x'"]);
    // MariaDB replaces in place, so an edit is one statement and a new trigger is a plain CREATE.
    assert_eq!(trigger_preview(Dialect::MariaDb, B, "orders", &edit).unwrap(), ["CREATE OR REPLACE DEFINER=`app`@`%` TRIGGER `orders_bu` BEFORE UPDATE ON `orders`\nFOR EACH ROW FOLLOWS `orders_first`\nSET NEW.tag = 'x'"]);
    assert!(trigger_preview(Dialect::MariaDb, B, "orders", &create).unwrap()[0].starts_with("CREATE TRIGGER "));
    assert_eq!(drop_trigger(M, "a`b", false), "DROP TRIGGER `a``b`");
    assert_eq!(drop_trigger(M, "a", true), "DROP TRIGGER IF EXISTS `a`");
    assert_eq!(script(&["A".into(), "B".into()]), "A;\n\nB;");
    let p = create_trigger(M, B, "orders", &TrigSave { order: Some(TrigOrder::Precedes("n`x".into())), ..save(t) }, false).unwrap();
    assert!(p.contains("FOR EACH ROW PRECEDES `n``x`\n"), "{p}");
}

#[test]
fn trigger_names_are_quoted_and_the_body_stays_the_users() {
    for bs in [true, false] {
        let t = TrigDef { name: "t`; DROP TABLE x; --".into(), timing: "AFTER".into(), event: "DELETE".into(), body: "BEGIN\n  INSERT INTO log VALUES ('a''b');\nEND".into(), pg: PgDef::default() };
        let s = create_trigger(M, Mode { bs, ansi: false }, "or`ders", &save(t), false).unwrap();
        assert_eq!(s, "CREATE TRIGGER `t``; DROP TABLE x; --` AFTER DELETE ON `or``ders`\nFOR EACH ROW\nBEGIN\n  INSERT INTO log VALUES ('a''b');\nEND", "bs={bs}");
    }
    let bad = |t: TrigDef| create_trigger(M, B, "orders", &save(t), false);
    assert!(bad(trig("", "SET NEW.a = 1")).is_err());
    assert!(bad(TrigDef { timing: "INSTEAD OF".into(), ..trig("a", "SET NEW.a = 1") }).is_err());
    assert!(bad(TrigDef { event: "TRUNCATE".into(), ..trig("a", "SET NEW.a = 1") }).is_err());
    assert!(bad(trig("a", "  ")).is_err());
    assert!(bad(trig("a", " ; ")).is_err());
    // The definer is quoted part by part, split at its last @.
    assert_eq!(definer_clause(M, "we`ird@user@%").unwrap(), "DEFINER=`we``ird@user`@`%`");
    assert!(definer_clause(M, "nohost").is_err());
}

#[test]
fn a_second_statement_in_a_trigger_body_is_refused() {
    assert_eq!(check_body("SET NEW.a = 1;", B).unwrap(), "SET NEW.a = 1");
    assert!(check_body("SET NEW.a = 1; DROP TABLE users", B).is_err());
    assert_eq!(check_body("-- note\nBEGIN SET NEW.a = 1; SET NEW.b = 2; END;", B).unwrap(), "BEGIN SET NEW.a = 1; SET NEW.b = 2; END");
    assert!(check_body("begin\nset new.a = 1;\nend", B).is_ok());
    // The same text is one statement or two depending on whether a backslash escapes the quote.
    let tricky = r"SET NEW.a = 'x\'; DROP TABLE users; --'";
    assert!(check_body(tricky, B).is_ok());
    assert!(check_body(tricky, NB).is_err());
    // Under ANSI_QUOTES a double quoted ; is an identifier, and either way it is not a statement end.
    assert!(check_body(r#"SET NEW.a = "x;y""#, B).is_ok());
    assert!(check_body(r#"SET NEW.a = "x;y""#, Mode { bs: true, ansi: true }).is_ok());
}

/// Review finding 1: a body that starts with BEGIN was taken whole, so a statement after its END ran too.
#[test]
fn nothing_runs_after_the_end_of_a_compound_body() {
    for body in [
        "BEGIN END; DROP TABLE quarry_rt.rt_victim",
        "BEGIN SET NEW.tag='new'; END; DELIMITER ;",
        "BEGIN END DROP TABLE x",
        "BEGIN END;;",
        // A name after a dot is never a keyword, so it opens no block for a later END to close.
        "BEGIN SET NEW.case = 1; END; DROP TABLE x; END",
        "BEGIN SET NEW.begin = 1; SET NEW.if = 2; END; DROP TABLE x; END",
        // A versioned comment may be skipped by the server, which would see the END after it early.
        "BEGIN /*!99999 BEGIN */ END; DROP TABLE x; END",
        "BEGIN /*M! BEGIN */ END; DROP TABLE x; END",
        // An end label is only the begin label, and a label never hides the END that follows.
        "a: BEGIN END a; DROP TABLE x",
        "BEGIN END END",
        "BEGIN IF 1 THEN SET NEW.a = 1; END; DROP TABLE x; END IF; END",
        "IF 1 THEN SET NEW.a = 1; END IF; DROP TABLE x",
        "BEGIN SET NEW.a = CASE WHEN 1 THEN 2 END; END; DROP TABLE x",
    ] {
        assert!(check_body(body, B).is_err(), "{body}");
        assert!(check_body(body, NB).is_err(), "{body}");
    }
    // Where the string ends depends on the escaping mode, and the check follows the session's.
    let s = r"BEGIN SET NEW.t = 'x\'; END; DROP TABLE y; -- '; END";
    assert!(check_body(s, B).is_ok());
    assert!(check_body(s, NB).is_err());
    let q = r#"BEGIN SET NEW.t = "a; END; DROP TABLE x; "; END"#;
    assert!(check_body(q, B).is_ok());
    assert!(check_body(q, Mode { bs: true, ansi: true }).is_ok());
    // Blocks that do not match, or one left open, are refused.
    for body in ["BEGIN IF 1 THEN SET NEW.a = 1; END; END", "BEGIN SET NEW.a = 1;", "IF 1 THEN SET NEW.a = 1; END LOOP", "BEGIN CASE NEW.a WHEN 1 THEN SET NEW.b = 1; END; END", "lbl: IF 1 THEN SET NEW.a = 1; END IF"] {
        assert!(check_body(body, B).is_err(), "{body}");
    }
    // The whole statement is checked again where it runs, head included.
    let ok = create_trigger(M, B, "t", &save(trig("a", "BEGIN SET NEW.a = 1; END")), false).unwrap();
    assert!(one_trigger_statement(&ok, B).is_ok());
    assert!(one_trigger_statement(&format!("{ok}; DROP TABLE x"), B).is_err());
    assert!(one_trigger_statement("CREATE TRIGGER a BEFORE INSERT ON t; DROP TABLE x; FOR EACH ROW SET NEW.a = 1", B).is_err());
    assert!(one_trigger_statement("CREATE TRIGGER a BEFORE INSERT ON t FOR EACH ROW FOLLOWS b SET NEW.a = 1; DROP TABLE x", B).is_err());
}

/// Review finding 11: a compound statement without BEGIN and a labelled block were refused.
#[test]
fn every_kind_of_compound_body_is_taken() {
    for body in [
        "IF NEW.tag IS NULL THEN SET NEW.tag = 'n'; END IF",
        "IF NEW.tag IS NULL THEN SET NEW.tag = 'n'; ELSEIF NEW.tag = '' THEN SET NEW.tag = 'e'; ELSE SET NEW.tag = UPPER(NEW.tag); END IF;",
        "lbl: BEGIN SET NEW.tag = 'l'; END lbl",
        "`l b`: BEGIN SET NEW.tag = 'l'; END `l b`;",
        "CASE NEW.a WHEN 1 THEN SET NEW.b = 'one'; ELSE BEGIN END; END CASE",
        "BEGIN DECLARE i INT DEFAULT 0; w: WHILE i < 3 DO SET i = i + 1; END WHILE w; REPEAT SET i = i - 1; UNTIL i = 0 END REPEAT; l: LOOP LEAVE l; END LOOP l; END",
        "BEGIN DECLARE CONTINUE HANDLER FOR SQLSTATE VALUE '23000', NOT FOUND, 1062 BEGIN END; SET NEW.a = 1; END",
        "BEGIN DECLARE EXIT HANDLER FOR SQLEXCEPTION SET NEW.a = 0; IF NEW.b THEN SET NEW.a = 1; END IF; END",
        "SET NEW.a = CASE WHEN NEW.b > 0 THEN 1 ELSE CASE NEW.c WHEN 1 THEN 2 END END",
        "BEGIN SET NEW.a = IF(NEW.b, 1, 2); SET NEW.c = REPEAT('x', 2); SET NEW.`end` = 1; SET NEW.end = 2; END",
        "BEGIN IF (SELECT CASE WHEN 1 THEN 1 END) THEN SET NEW.a = 1; END IF; END",
    ] {
        assert!(check_body(body, B).is_ok(), "{body}: {:?}", check_body(body, B));
    }
    assert_eq!(check_body("lbl: BEGIN SET NEW.tag = 'l'; END lbl;  -- done", B).unwrap(), "lbl: BEGIN SET NEW.tag = 'l'; END lbl");
}

/// Review finding 12: the quote rules come from the exact mode names, as the session reports them.
#[test]
fn ansi_quotes_is_read_exactly() {
    assert!(Mode::of("STRICT_TRANS_TABLES,ANSI_QUOTES").ansi);
    assert!(Mode::of("REAL_AS_FLOAT,PIPES_AS_CONCAT,ANSI_QUOTES,IGNORE_SPACE,ANSI").ansi);
    assert!(!Mode::of("STRICT_TRANS_TABLES,NO_ENGINE_SUBSTITUTION").ansi);
    assert!(!Mode::of("ANSI_QUOTES_NOT_A_MODE").ansi);
}

/// Review finding 2 and 3: a stored statement is run again with OR REPLACE on MariaDB and its place among the
/// triggers of its event, whatever order clause it held.
#[test]
fn a_stored_trigger_is_restated() {
    let s = "CREATE DEFINER=`root`@`%` TRIGGER `t` BEFORE INSERT ON `x` FOR EACH ROW SET NEW.tag = 'a' || 'b'";
    assert_eq!(restate(M, s, B, None, false).unwrap(), s);
    assert_eq!(restate(Dialect::MariaDb, s, B, Some(&TrigOrder::Follows("p".into())), true).unwrap(), "CREATE OR REPLACE DEFINER=`root`@`%` TRIGGER `t` BEFORE INSERT ON `x` FOR EACH ROW FOLLOWS `p` SET NEW.tag = 'a' || 'b'");
    let held = "CREATE DEFINER=`root`@`%` TRIGGER `t` BEFORE INSERT ON `x` FOR EACH ROW PRECEDES `old` SET NEW.tag = 'for each row'";
    assert_eq!(restate(M, held, B, Some(&TrigOrder::Precedes("n".into())), false).unwrap(), "CREATE DEFINER=`root`@`%` TRIGGER `t` BEFORE INSERT ON `x` FOR EACH ROW PRECEDES `n` SET NEW.tag = 'for each row'");
    assert_eq!(restate(M, held, B, None, false).unwrap(), "CREATE DEFINER=`root`@`%` TRIGGER `t` BEFORE INSERT ON `x` FOR EACH ROW SET NEW.tag = 'for each row'");
    // Under ANSI_QUOTES a double quoted name is read as one.
    let ansi = "CREATE DEFINER=`root`@`%` TRIGGER \"for each row\" BEFORE INSERT ON `x` FOR EACH ROW SET NEW.\"tag\" = 'q'";
    assert!(restate(M, ansi, Mode { bs: true, ansi: true }, Some(&TrigOrder::Follows("p".into())), false).unwrap().ends_with("FOR EACH ROW FOLLOWS `p` SET NEW.\"tag\" = 'q'"));
    assert!(restate(M, "DROP TABLE x", B, None, false).is_err());
}

fn live_trig(name: &str, order: u64) -> crate::db::sql::structure::Trigger {
    crate::db::sql::structure::Trigger { name: name.into(), timing: "BEFORE".into(), event: "INSERT".into(), body: "SET NEW.a = 1".into(), definer: "root@%".into(), order, sql_mode: "STRICT_TRANS_TABLES".into(), pg: None }
}

/// Review finding 5: a trigger goes back between the triggers it sat between.
#[test]
fn the_order_is_found_from_the_neighbours() {
    let all = [live_trig("a", 1), live_trig("b", 2), live_trig("c", 3), crate::db::sql::structure::Trigger { timing: "AFTER".into(), ..live_trig("z", 1) }];
    assert_eq!(neighbours(&all, "b"), (Some("a".into()), Some("c".into())));
    assert_eq!(neighbours(&all, "a"), (None, Some("b".into())));
    assert_eq!(neighbours(&all, "z"), (None, None));
    let rest = [live_trig("a", 1), live_trig("c", 2)];
    assert_eq!(order_among(&rest, "b", "BEFORE", "INSERT", Some("a"), Some("c")), Some(TrigOrder::Follows("a".into())));
    assert_eq!(order_among(&rest[1..], "b", "BEFORE", "INSERT", Some("a"), Some("c")), Some(TrigOrder::Precedes("c".into())));
    assert_eq!(order_among(&[], "b", "BEFORE", "INSERT", Some("a"), Some("c")), None);
    // The trigger itself, still there for MariaDB's OR REPLACE, is never its own neighbour.
    assert_eq!(order_among(&[live_trig("b", 1)], "b", "BEFORE", "INSERT", Some("b"), None), None);
}

/// Review finding 14: the trigger check compares the definer, the sql_mode and the order too.
#[test]
fn a_trigger_is_the_same_only_when_every_part_is() {
    let t = live_trig("Tr", 1);
    assert!(same_trigger(&crate::db::sql::structure::Trigger { name: "tr".into(), body: " SET NEW.a = 1 ".into(), sql_mode: "strict_trans_tables".into(), ..t.clone() }, &t));
    assert!(!same_trigger(&crate::db::sql::structure::Trigger { body: "SET NEW.a = 2".into(), ..t.clone() }, &t));
    assert!(!same_trigger(&crate::db::sql::structure::Trigger { definer: "app@%".into(), ..t.clone() }, &t));
    assert!(!same_trigger(&crate::db::sql::structure::Trigger { sql_mode: "ANSI_QUOTES".into(), ..t.clone() }, &t));
    assert!(!same_trigger(&crate::db::sql::structure::Trigger { order: 2, ..t.clone() }, &t));
    assert!(same_modes("A,B", " b,a "));
}

/// Review finding 10: MySQL 8.4 takes a key only on a whole primary or unique key.
#[test]
fn a_strict_server_needs_a_whole_unique_key() {
    let own = Structure { columns: vec![fcol("a", "int", None)], ..Default::default() };
    let mut parent = Structure { columns: vec![fcol("x", "int", None), fcol("y", "int", None)], indexes: vec![ix("idx_x", false, &["x"])], ..Default::default() };
    let k = fk("fk", &["a"], "p", &["x"]);
    assert!(fk_problem(&own, "c", &parent, &k, false).is_none());
    assert!(fk_problem(&own, "c", &parent, &k, true).unwrap().contains("whole primary or unique key"));
    parent.indexes = vec![ix("uq_xy", true, &["x", "y"])];
    assert!(fk_problem(&own, "c", &parent, &k, true).is_some(), "part of a unique key is not enough");
    parent.indexes = vec![ix("uq_x", true, &["x"])];
    assert!(fk_problem(&own, "c", &parent, &k, true).is_none());
    assert!(fk_whole_key_default(M, "8.4.11"));
    assert!(fk_whole_key_default(M, "9.1.0"));
    assert!(!fk_whole_key_default(M, "8.0.40"));
    assert!(!fk_whole_key_default(Dialect::MariaDb, "11.4.3-MariaDB"));
}

/// Review finding 4: foreign key changes in a batch take the same plan as the rest, so their undo runs one step
/// at a time, is checked against a snapshot that holds the key, and names what it loses.
#[test]
fn a_batch_with_a_foreign_key_has_one_checked_undo() {
    let t = table();
    let k = fk("fk_mix", &["customer_id"], "customers", &["id"]);
    let u = plan(M, "8.4.0", B, "orders", &t, &[Change::Drop("note".into()), Change::AddFk(k.clone())]).unwrap().undo.unwrap();
    assert_eq!(u.steps.len(), 2, "{:?}", u.steps);
    assert_eq!(u.steps[0], "ALTER TABLE `orders`\n  DROP FOREIGN KEY `fk_mix`");
    assert!(u.steps[1].contains("ADD COLUMN `note`"), "{:?}", u.steps);
    assert_eq!(u.fks, ["fk_mix"]);
    let u = plan(M, "8.4.0", B, "orders", &t, &[Change::Add { def: def("qty", "INT"), pos: Pos::Keep }, Change::AddFk(k)]).unwrap().undo.unwrap();
    assert_eq!(u.loss, ["Undo drops column qty and every value it holds."], "a loss asks for the typed confirmation");
}

#[test]
fn a_foreign_key_snapshot_sees_every_part_of_the_key() {
    let st = fk_table();
    let names = vec!["fk_cust".to_string()];
    let a = snapshot(&st, &[], &[], &names);
    assert!(a[0].1.is_some());
    let mut b = st.clone();
    b.foreign_keys[0].on_delete = "CASCADE".into();
    assert_ne!(snapshot(&b, &[], &[], &names), a);
    b.foreign_keys.clear();
    assert!(snapshot(&b, &[], &[], &names)[0].1.is_none());
}
