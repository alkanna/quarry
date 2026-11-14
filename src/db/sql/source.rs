//! Which single table a SELECT read its columns from, so a value in the result can be turned into a filter on that table.

use super::lex::{self, Tok};
use super::Dialect;

/// The table of a `SELECT` that lists plain columns (or `*`) from exactly one table, as an optional schema or database
/// and the table name. Anything that could make a result column something other than a column of that table gives
/// `None`: expressions, aliases, joins, unions, a comma list, a subquery in FROM or a statement that is not a SELECT.
pub fn single_table(stmt: &str, d: Dialect) -> Option<(Option<String>, String)> {
    let toks: Vec<(Tok, &str)> = lex::tokens(stmt, d).into_iter().filter(|(t, _)| !matches!(t, Tok::Space | Tok::Comment)).map(|(t, r)| (t, &stmt[r])).collect();
    let word = |i: usize, w: &str| toks.get(i).is_some_and(|(t, s)| *t == Tok::Word && s.eq_ignore_ascii_case(w));
    let punct = |i: usize, p: &str| toks.get(i).is_some_and(|(t, s)| *t == Tok::Punct && *s == p);
    let ident = |i: usize| {
        toks.get(i).is_some_and(|(t, s)| match t {
            Tok::Quoted => true,
            Tok::Word => !s.starts_with(|c: char| c.is_ascii_digit()) && !["null", "true", "false"].iter().any(|k| s.eq_ignore_ascii_case(k)),
            _ => false,
        })
    };
    if !word(0, "select") {
        return None;
    }
    // The select list: items of `*` or `name`, `qualifier.name`, `qualifier.*`, nothing else.
    let mut i = 1;
    loop {
        if punct(i, "*") {
            i += 1;
        } else if ident(i) {
            i += 1;
            while punct(i, ".") {
                i += 1;
                if punct(i, "*") || ident(i) {
                    i += 1;
                } else {
                    return None;
                }
            }
        } else {
            return None;
        }
        if punct(i, ",") {
            i += 1;
        } else {
            break;
        }
    }
    if !word(i, "from") {
        return None;
    }
    i += 1;
    if !ident(i) {
        return None;
    }
    let mut parts = vec![lex::unquote_ident(toks[i].1)];
    i += 1;
    while punct(i, ".") {
        if !ident(i + 1) {
            return None;
        }
        parts.push(lex::unquote_ident(toks[i + 1].1));
        i += 2;
    }
    // An alias, with or without AS, may follow the table.
    if word(i, "as") {
        i += 1;
        if !ident(i) {
            return None;
        }
        i += 1;
    } else if ident(i) && !is_clause(toks[i].1) {
        i += 1;
    }
    // What is left may filter, group, order and limit, but never bring in another table.
    let mut depth = 0usize;
    for (t, s) in &toks[i..] {
        match (t, *s) {
            (Tok::Punct, "(") => depth += 1,
            (Tok::Punct, ")") => depth = depth.saturating_sub(1),
            (Tok::Punct, ",") if depth == 0 => return None,
            (Tok::Word, w) if depth == 0 && BRING_IN.iter().any(|k| w.eq_ignore_ascii_case(k)) => return None,
            _ => {}
        }
    }
    match parts.len() {
        1 => Some((None, parts.remove(0))),
        2 => Some((Some(parts.remove(0)), parts.remove(0))),
        _ => None,
    }
}

/// Words that start what follows a table's name, so none of them is taken for its alias.
const CLAUSES: [&str; 13] = ["where", "group", "having", "order", "limit", "offset", "fetch", "for", "window", "lock", "into", "procedure", "partition"];
/// Words that bring another table or query into the statement.
const BRING_IN: [&str; 13] = ["join", "inner", "left", "right", "full", "cross", "natural", "straight_join", "union", "intersect", "except", "using", "lateral"];

fn is_clause(w: &str) -> bool {
    CLAUSES.iter().chain(&BRING_IN).any(|k| w.eq_ignore_ascii_case(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str, d: Dialect) -> Option<(Option<String>, String)> {
        single_table(s, d)
    }

    fn some(ns: Option<&str>, table: &str) -> Option<(Option<String>, String)> {
        Some((ns.map(String::from), table.to_string()))
    }

    #[test]
    fn plain_selects_from_one_table() {
        let m = Dialect::MySql;
        assert_eq!(t("SELECT * FROM orders", m), some(None, "orders"));
        assert_eq!(t("select id, `total` from `shop`.`orders` o where o.id > 3 order by id limit 5;", m), some(Some("shop"), "orders"));
        assert_eq!(t("-- note\nSELECT o.* FROM orders AS o", m), some(None, "orders"));
        assert_eq!(t("SELECT \"id\" FROM \"public\".\"or\"\"ders\" WHERE x IN (SELECT 1)", Dialect::Postgres), some(Some("public"), "or\"ders"));
    }

    #[test]
    fn anything_that_makes_columns_not_plain_or_adds_a_table_is_refused() {
        let m = Dialect::MySql;
        for s in [
            "SELECT id AS n FROM orders",
            "SELECT count(*) FROM orders",
            "SELECT DISTINCT status FROM orders",
            "SELECT a + 1 FROM orders",
            "SELECT 1",
            "SELECT * FROM orders o JOIN users u ON u.id = o.uid",
            "SELECT * FROM orders, users",
            "SELECT * FROM orders UNION SELECT * FROM old_orders",
            "SELECT * FROM (SELECT * FROM orders) x",
            "UPDATE orders SET a = 1",
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "SELECT * FROM a.b.c",
        ] {
            assert_eq!(single_table(s, m), None, "{s}");
        }
    }
}
