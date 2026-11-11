//! The MySQL and MariaDB text of a structure change: one ALTER TABLE for the staged `Change`s. The shared types,
//! the simulation and the undo are in `ddl`; PostgreSQL has its own emitter in `ddl_pg`.

use super::ddl::{check_expr, check_ident, is_binary, is_geometry, is_hex_lit, is_bit_lit, is_integer, is_number, is_numeric, is_text, parse_values, Change, ColDef, Dv, IndexDef, IndexKind, Mode, Pos, FK_ACTIONS};
use super::structure::ForeignKey;
use super::value::lit;
use super::Dialect;

/// Everything after the column name: `DECIMAL(12,2) UNSIGNED NOT NULL DEFAULT 0.00 COMMENT '...'`.
pub fn definition(d: Dialect, m: Mode, c: &ColDef) -> Result<String, String> {
    check_ident(&c.name, "A column")?;
    let ty = c.ty.trim().to_ascii_uppercase();
    if ty.is_empty() || !ty.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') || !ty.as_bytes()[0].is_ascii_alphabetic() {
        return Err(format!("{:?} is not a column type.", c.ty));
    }
    let mut s = ty.clone();
    let args = c.args.trim();
    if !args.is_empty() {
        let inner = if matches!(ty.as_str(), "ENUM" | "SET") {
            parse_values(args)?.iter().map(|v| lit(v, m.bs)).collect::<Vec<_>>().join(",")
        } else {
            let ok = args.split(',').count() <= 2 && args.split(',').all(|p| !p.trim().is_empty() && p.trim().bytes().all(|b| b.is_ascii_digit()));
            if !ok {
                return Err("The length is a number, or two numbers like 10,2.".into());
            }
            args.split(',').map(str::trim).collect::<Vec<_>>().join(",")
        };
        s.push_str(&format!("({inner})"));
    } else if matches!(ty.as_str(), "ENUM" | "SET") {
        return Err(format!("{ty} needs its values, like 'a','b'."));
    } else if matches!(ty.as_str(), "VARCHAR" | "VARBINARY") {
        return Err(format!("{ty} needs a length."));
    }
    if c.unsigned && is_numeric(&ty) {
        s.push_str(" UNSIGNED");
    }
    if c.zerofill && is_numeric(&ty) {
        s.push_str(" ZEROFILL");
    }
    if let (Some(n), Dialect::MySql, true) = (c.srid, d, is_geometry(&ty)) {
        s.push_str(&format!(" SRID {n}"));
    }
    if let (Some(coll), true) = (&c.collation, is_text(&ty)) {
        if coll.is_empty() || !coll.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(format!("{coll:?} is not a collation name."));
        }
        s.push_str(&format!(" CHARACTER SET {} COLLATE {coll}", coll.split('_').next().unwrap_or(coll)));
    }
    s.push_str(if c.nullable { " NULL" } else { " NOT NULL" });
    match &c.default {
        Dv::None => {}
        Dv::Null if c.nullable => s.push_str(" DEFAULT NULL"),
        Dv::Null => return Err("A NOT NULL column cannot default to NULL.".into()),
        Dv::Value(v) if is_numeric(&ty) && is_number(v.trim()) => s.push_str(&format!(" DEFAULT {}", v.trim())),
        // MySQL reports a binary default as hex, which read as text would store the digits.
        Dv::Value(v) if matches!(d, Dialect::MySql) && is_binary(&ty) && is_hex_lit(v) => s.push_str(&format!(" DEFAULT {}", if v.len() == 2 { "X''" } else { v })),
        Dv::Value(v) if ty == "BIT" && is_bit_lit(v) => s.push_str(&format!(" DEFAULT {v}")),
        Dv::Value(v) if ty == "BIT" && !v.is_empty() && v.bytes().all(|b| b == b'0' || b == b'1') => s.push_str(&format!(" DEFAULT b'{v}'")),
        Dv::Value(v) => s.push_str(&format!(" DEFAULT {}", lit(v, m.bs))),
        Dv::Expr(x) => {
            check_expr(x, m)?;
            s.push_str(&format!(" DEFAULT {}", d.default_expr(x)));
        }
    }
    if c.auto_increment {
        if !is_integer(&ty) {
            return Err("Only an integer column can auto increment.".into());
        }
        s.push_str(" AUTO_INCREMENT");
    }
    if let Some(u) = c.on_update.as_deref().filter(|u| !u.is_empty()) {
        check_expr(u, m)?;
        s.push_str(&format!(" ON UPDATE {u}"));
    }
    if !c.comment.is_empty() {
        s.push_str(&format!(" COMMENT {}", lit(&c.comment, m.bs)));
    }
    Ok(s)
}

pub(super) fn position(d: Dialect, p: &Pos) -> String {
    match p {
        Pos::Keep => String::new(),
        Pos::First => " FIRST".into(),
        Pos::After(c) => format!(" AFTER {}", d.quote_ident(c)),
    }
}

pub(super) fn index_cols(d: Dialect, ix: &IndexDef) -> Result<String, String> {
    if ix.cols.is_empty() {
        return Err("An index needs at least one column.".into());
    }
    let parts: Vec<String> = ix.cols.iter().map(|c| format!("{}{}{}", d.quote_ident(&c.name), c.prefix.map_or(String::new(), |n| format!("({n})")), if c.desc { " DESC" } else { "" })).collect();
    Ok(parts.join(", "))
}

pub(super) fn add_index(d: Dialect, m: Mode, ix: &IndexDef) -> Result<String, String> {
    if ix.functional {
        return Err(format!("Index {} has an expression part, which this editor cannot write.", ix.name));
    }
    let cols = index_cols(d, ix)?;
    if matches!(ix.kind, IndexKind::Fulltext | IndexKind::Spatial) && ix.cols.iter().any(|c| c.prefix.is_some() || c.desc) {
        return Err("A FULLTEXT or SPATIAL index has no prefix lengths or sort order.".into());
    }
    let comment = if ix.comment.is_empty() { String::new() } else { format!(" COMMENT {}", lit(&ix.comment, m.bs)) };
    if ix.kind == IndexKind::Primary {
        return Ok(format!("ADD PRIMARY KEY ({cols}){comment}"));
    }
    check_ident(&ix.name, "An index")?;
    let kind = match ix.kind {
        IndexKind::Unique => "UNIQUE ",
        IndexKind::Fulltext => "FULLTEXT ",
        IndexKind::Spatial => "SPATIAL ",
        _ => "",
    };
    let hidden = match (ix.invisible, d) {
        (false, _) => "",
        (true, Dialect::MySql) => " INVISIBLE",
        (true, Dialect::MariaDb) => " IGNORED",
        // PostgreSQL has no hidden indexes
        (true, Dialect::Postgres) => "",
    };
    Ok(format!("ADD {kind}INDEX {} ({cols}){comment}{hidden}", d.quote_ident(&ix.name)))
}

pub(super) fn add_fk(d: Dialect, k: &ForeignKey) -> Result<String, String> {
    check_ident(&k.name, "A foreign key")?;
    check_ident(&k.ref_table, "The referenced table")?;
    if k.cols.is_empty() || k.cols.len() != k.ref_cols.len() {
        return Err("Pick the same number of columns on both sides.".into());
    }
    let action = |a: &str| FK_ACTIONS.iter().find(|x| x.eq_ignore_ascii_case(a.trim())).copied().ok_or_else(|| format!("{a:?} is not a foreign key action."));
    let names = |v: &[String]| v.iter().map(|c| d.quote_ident(c)).collect::<Vec<_>>().join(", ");
    let target = if k.ref_db.is_empty() || k.ref_db == k.db { d.quote_ident(&k.ref_table) } else { d.qualified(&k.ref_db, &k.ref_table) };
    Ok(format!("ADD CONSTRAINT {}\n    FOREIGN KEY ({})\n    REFERENCES {} ({})\n    ON UPDATE {} ON DELETE {}", d.quote_ident(&k.name), names(&k.cols), target, names(&k.ref_cols), action(&k.on_update)?, action(&k.on_delete)?))
}

pub(super) fn drop_index(d: Dialect, name: &str) -> String {
    if name == "PRIMARY" { "DROP PRIMARY KEY".into() } else { format!("DROP INDEX {}", d.quote_ident(name)) }
}

/// One clause of the ALTER, or two for a rename the server cannot do directly.
pub(super) fn clause(d: Dialect, m: Mode, can_rename: bool, c: &Change) -> Result<Vec<String>, String> {
    Ok(match c {
        Change::Add { def, pos } => vec![format!("ADD COLUMN {} {}{}", d.quote_ident(&def.name), definition(d, m, def)?, position(d, pos))],
        Change::Modify { from, def, pos } if *from == def.name => vec![format!("MODIFY {} {}{}", d.quote_ident(from), definition(d, m, def)?, position(d, pos))],
        Change::Modify { from, def, pos } => vec![format!("CHANGE COLUMN {} {} {}{}", d.quote_ident(from), d.quote_ident(&def.name), definition(d, m, def)?, position(d, pos))],
        Change::Drop(n) => vec![format!("DROP COLUMN {}", d.quote_ident(n))],
        Change::AddIndex(ix) => vec![add_index(d, m, ix)?],
        Change::DropIndex(n) => vec![drop_index(d, n)],
        Change::AddFk(k) => vec![add_fk(d, k)?],
        Change::DropFk(n) => vec![format!("DROP FOREIGN KEY {}", d.quote_ident(n))],
        Change::RenameFk { .. } => return Err("MySQL and MariaDB cannot rename a foreign key. Drop it and add it again under the new name.".into()),
        Change::RenameIndex { from, to, def } => {
            check_ident(to, "An index")?;
            if can_rename {
                vec![format!("RENAME INDEX {} TO {}", d.quote_ident(from), d.quote_ident(to))]
            } else {
                vec![drop_index(d, from), add_index(d, m, &IndexDef { name: to.clone(), ..def.clone() })?]
            }
        }
    })
}

/// The statement for the staged changes, one clause per line. The table is unqualified: the writer's
/// session is switched to the schema first, which is what the preview shows.
pub fn alter(d: Dialect, version: &str, m: Mode, table: &str, changes: &[Change]) -> Result<String, String> {
    if changes.is_empty() {
        return Err("Nothing is staged.".into());
    }
    let can = d.can_rename_index(version);
    let mut lines = Vec::new();
    for c in changes {
        lines.extend(clause(d, m, can, c)?);
    }
    Ok(format!("ALTER TABLE {}\n  {}", d.quote_ident(table), lines.join(",\n  ")))
}

