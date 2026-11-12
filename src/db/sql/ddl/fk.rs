//! Foreign keys: which column pairs can be a key, and the orphan rows that would stop one being added.

use super::*;

/// Why a column pair cannot be the two sides of a foreign key, in the words of the person adding it. The
/// server's own answer ("incompatible") names no column, so the rules it enforces are checked first. On PostgreSQL a key needs
/// types that the server can compare with `=`, which is any two integer sizes, an integer child against a numeric parent, any
/// text type against another, and the same type otherwise. A numeric child against an integer parent is refused by the server
/// ("cannot be implemented"), so it is refused here. Types it cannot place are left to the server's own answer.
pub fn fk_pair_problem(d: Dialect, child_table: &str, child: &Column, parent_table: &str, parent: &Column) -> Option<String> {
    if d == Dialect::Postgres {
        let (fc, fp) = (pg_family(&child.full_type)?, pg_family(&parent.full_type)?);
        if fc == fp || (fc, fp) == ("int", "numeric") {
            return None;
        }
        return Some(format!("{child_table}.{} is {} but {parent_table}.{} is {}. A foreign key needs two types that PostgreSQL can compare, such as two integers or two text types.", child.name, child.full_type, parent.name, parent.full_type));
    }
    let (c, p) = (format!("{child_table}.{}", child.name), format!("{parent_table}.{}", parent.name));
    let (ct, cargs, cu) = split_type(&child.full_type);
    let (pt, pargs, pu) = split_type(&parent.full_type);
    let show = |col: &Column| col.full_type.to_ascii_uppercase();
    #[derive(PartialEq)]
    enum Fam {
        Int,
        Decimal,
        Float,
        Double,
        Text,
        Bin,
        Other(String),
    }
    let fam = |t: &str| -> Option<Fam> {
        Some(match t {
            "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "INTEGER" | "BIGINT" => Fam::Int,
            "DECIMAL" | "NUMERIC" => Fam::Decimal,
            "FLOAT" => Fam::Float,
            "DOUBLE" | "REAL" => Fam::Double,
            "CHAR" | "VARCHAR" => Fam::Text,
            "BINARY" | "VARBINARY" => Fam::Bin,
            "DATE" | "DATETIME" | "TIMESTAMP" | "TIME" | "YEAR" | "BIT" | "ENUM" | "SET" => Fam::Other(t.to_string()),
            _ => return None,
        })
    };
    for (name, t, col) in [(&c, &ct, child), (&p, &pt, parent)] {
        if fam(t).is_none() {
            return Some(format!("{name} is {}, which cannot be part of a foreign key.", show(col)));
        }
    }
    let (fc, fp) = (fam(&ct)?, fam(&pt)?);
    if fc != fp {
        return Some(format!("{c} is {} but {p} is {}. A foreign key needs the same kind of type on both sides.", show(child), show(parent)));
    }
    let size = |t: &str| if t == "INTEGER" { "INT".to_string() } else { t.to_string() };
    match fc {
        Fam::Int if size(&ct) != size(&pt) || cu != pu => Some(format!("{c} is {} but {p} is {}. Integer columns must have the same size and the same signedness.", show(child), show(parent))),
        Fam::Decimal | Fam::Float | Fam::Double if cargs != pargs || cu != pu || (fc == Fam::Float && ct != pt) => Some(format!("{c} is {} but {p} is {}. Number columns must match exactly.", show(child), show(parent))),
        Fam::Other(_) if !child.full_type.eq_ignore_ascii_case(&parent.full_type) => Some(format!("{c} is {} but {p} is {}. These types must match exactly.", show(child), show(parent))),
        Fam::Text => {
            let (a, b) = (child.collation.as_deref().map(str::to_ascii_lowercase), parent.collation.as_deref().map(str::to_ascii_lowercase));
            (a != b).then(|| format!("{c} uses {} but {p} uses {}. Give both columns the same collation.", child.collation.as_deref().unwrap_or("the default collation"), parent.collation.as_deref().unwrap_or("the default collation")))
        }
        _ => None,
    }
}

/// The comparison family of a PostgreSQL column type, for the types the server compares across names: the integers, the
/// numeric types, the floats, text and varchar, and the date and time types of one family. A type outside the list (a domain,
/// an enum, an extension type) has none, so a key on it is left to the server.
pub(super) fn pg_family(full_type: &str) -> Option<&'static str> {
    let base = full_type.split('(').next().unwrap_or("").trim().to_ascii_lowercase();
    Some(match base.as_str() {
        "smallint" | "integer" | "bigint" => "int",
        "numeric" | "decimal" => "numeric",
        "real" | "double precision" => "float",
        // character(n) is compared with text through the implicit cast, so a key between the two is accepted.
        "text" | "character varying" | "character" => "text",
        "date" | "timestamp without time zone" | "timestamp with time zone" => "datetime",
        "uuid" => "uuid",
        "boolean" => "boolean",
        "bytea" => "bytea",
        "time without time zone" => "time",
        "interval" => "interval",
        _ => return None,
    })
}

/// The first problem with a whole key, before the server is asked: column pairs that do not fit, SET NULL on a
/// column that cannot be NULL, and a referenced side with no index to lean on. `whole_key` is MySQL 8.4's
/// restrict_fk_on_non_standard_key: the referenced columns must then be exactly the primary key or a unique key,
/// where older servers take any index that starts with them.
pub fn fk_problem(own: &Structure, table: &str, parent: &Structure, fk: &ForeignKey, whole_key: bool) -> Option<String> {
    let find = |st: &Structure, n: &str| st.columns.iter().find(|c| same_name(&c.name, n)).cloned();
    let pg = own.is_pg();
    for (a, b) in fk.cols.iter().zip(&fk.ref_cols) {
        let (Some(ca), Some(cb)) = (find(own, a), find(parent, b)) else { return Some("A picked column is not in its table any more.".into()) };
        if let Some(p) = fk_pair_problem(if pg { Dialect::Postgres } else { Dialect::MySql }, table, &ca, &fk.ref_table, &cb) {
            return Some(p);
        }
        let set_null = fk.on_delete.eq_ignore_ascii_case("SET NULL") || fk.on_update.eq_ignore_ascii_case("SET NULL");
        if set_null && !ca.nullable {
            return Some(format!("SET NULL needs a column that allows NULL, and {table}.{a} does not."));
        }
    }
    if pg {
        let twice = |v: &[String]| v.iter().enumerate().find(|(i, c)| v[..*i].iter().any(|o| same_name(o, c))).map(|(_, c)| c.clone());
        if let Some(c) = twice(&fk.cols).or_else(|| twice(&fk.ref_cols)) {
            return Some(format!("The column {c} is picked twice in one key."));
        }
        // PostgreSQL wants a primary key or unique constraint (or a unique index) on exactly the referenced columns, in any
        // order. A condition on the index is not visible here, so the server has the last word on that.
        let exact = parent.indexes.iter().any(|ix| ix.unique && !ix.deferrable && ix.cols.len() == fk.ref_cols.len() && ix.cols.iter().all(|c| c.expr.is_none() && fk.ref_cols.iter().any(|r| same_name(r, &c.name))));
        return (!exact).then(|| format!("PostgreSQL needs a primary key or unique constraint on exactly ({}) of {}, and there is none. Add one there first.", fk.ref_cols.join(", "), fk.ref_table));
    }
    let starts = |ix: &Index| ix.cols.len() >= fk.ref_cols.len() && ix.cols.iter().zip(&fk.ref_cols).all(|(x, y)| x.prefix.is_none() && x.expr.is_none() && same_name(&x.name, y));
    if whole_key {
        let exact = parent.indexes.iter().any(|ix| ix.unique && ix.cols.len() == fk.ref_cols.len() && starts(ix));
        return (!exact).then(|| format!("This server only lets a key point at a whole primary or unique key, and {} has none on exactly ({}). Add a unique index there first.", fk.ref_table, fk.ref_cols.join(", ")));
    }
    let led = parent.indexes.iter().any(starts);
    (!led).then(|| format!("{} has no index that starts with ({}). Add an index there first.", fk.ref_table, fk.ref_cols.join(", ")))
}

/// Whether a server is taken to refuse keys on part of a key until it says: MySQL 8.4 and later turn
/// restrict_fk_on_non_standard_key on by default.
pub fn fk_whole_key_default(d: Dialect, version: &str) -> bool {
    let mut n = version.split(|c: char| !c.is_ascii_digit()).filter_map(|p| p.parse::<u32>().ok());
    matches!(d, Dialect::MySql) && (n.next().unwrap_or(0), n.next().unwrap_or(0)) >= (8, 4)
}

/// The condition that makes a row of the child fail `fk`, written for the alias `c` of the child. MATCH SIMPLE skips a row with
/// any NULL key column and needs a parent for the others. MATCH FULL needs a parent for a row with none, and also refuses
/// one with some key columns NULL and some not.
pub(super) fn orphan_cond(d: Dialect, parent: &str, fk: &ForeignKey) -> String {
    let q = |n: &str| d.quote_ident(n);
    let own: Vec<String> = fk.cols.iter().map(|c| format!("c.{}", q(c))).collect();
    let present = own.iter().map(|c| format!("{c} IS NOT NULL")).collect::<Vec<_>>().join(" AND ");
    let same = own.iter().zip(&fk.ref_cols).map(|(a, b)| format!("p.{} = {a}", q(b))).collect::<Vec<_>>().join(" AND ");
    let missing = format!("{present} AND NOT EXISTS (SELECT 1 FROM {parent} p WHERE {same})");
    if !fk.match_full || fk.cols.len() < 2 {
        return missing;
    }
    let some_null = own.iter().map(|c| format!("{c} IS NULL")).collect::<Vec<_>>().join(" OR ");
    let some_set = own.iter().map(|c| format!("{c} IS NOT NULL")).collect::<Vec<_>>().join(" OR ");
    format!("(({missing}) OR (({some_null}) AND ({some_set})))")
}

/// Counts the rows of the table that `fk` would refuse, which is what makes ADD FOREIGN KEY fail.
pub fn orphans_sql(d: Dialect, schema: &str, table: &str, fk: &ForeignKey) -> String {
    let parent = d.qualified(if fk.ref_db.is_empty() { schema } else { &fk.ref_db }, &fk.ref_table);
    format!("SELECT COUNT(*) AS n FROM {} c WHERE {}", d.qualified(schema, table), orphan_cond(d, &parent, fk))
}

/// A few of the rows `orphans_sql` counts, each as its key values written as text, for the message that shows the person
/// which rows stop the key from being added. PostgreSQL only. It is a subselect so the LIMIT the metadata reads append
/// still fits.
pub fn orphan_sample_sql(d: Dialect, schema: &str, table: &str, fk: &ForeignKey, limit: u32) -> String {
    let q = |n: &str| d.quote_ident(n);
    let parent = d.qualified(if fk.ref_db.is_empty() { schema } else { &fk.ref_db }, &fk.ref_table);
    let shown = fk.cols.iter().enumerate().map(|(i, c)| format!("c.{}::text AS k{i}", q(c))).collect::<Vec<_>>().join(", ");
    format!("SELECT * FROM (SELECT {shown} FROM {} c WHERE {} LIMIT {limit}) s", d.qualified(schema, table), orphan_cond(d, &parent, fk))
}

/// What the orphan count depends on: the name and the actions of a key do not change which rows point at nothing,
/// so typing a name does not count again.
pub fn orphan_key(fk: &ForeignKey) -> (Vec<String>, String, String, Vec<String>) {
    (fk.cols.clone(), fk.ref_db.clone(), fk.ref_table.clone(), fk.ref_cols.clone())
}
