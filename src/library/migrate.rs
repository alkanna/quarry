//! Moves the saved queries that older builds kept in ui.json (`UiPrefs::library`) into the script library. Pure: the app
//! hands in the old entries as plain values and writes the result. It runs once, when library.json does not exist yet and
//! the old map is not empty; the old map is never written again and stays in ui.json as a backup for older builds.

use super::{Library, Script};

/// One entry of the old library, with the fields a script needs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OldEntry {
    pub name: String,
    pub filter: String,
    pub sort: String,
    pub project: String,
    pub pipeline: Option<String>,
    pub sql: Option<String>,
}

/// Adds the scripts of the old library to `lib` and returns how many were new. A key is `"{conn}/#sql"` for the SQL scripts
/// of a connection, or `"{conn}/{db}/{coll}"` for the queries and pipelines of one collection; for PostgreSQL the last
/// part is the `schema.coll` form it always was. A key of a removed connection is kept, so its scripts show as such. An
/// entry already in `lib` (same connection, kind, name, text and collection) is skipped, so a second run imports nothing.
pub fn migrate_into(lib: &mut Library, old: &[(String, Vec<OldEntry>)], now: u64) -> usize {
    // A map has no order; sorted keys make the ids the same on every run.
    let mut keys: Vec<&(String, Vec<OldEntry>)> = old.iter().collect();
    keys.sort_by(|a, b| a.0.cmp(&b.0));
    let mut added = 0;
    for (key, entries) in keys {
        let mut parts = key.splitn(3, '/');
        let Some(conn) = parts.next().and_then(|c| c.parse::<u64>().ok()) else { continue };
        let (db, coll) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
        let sql_key = db == "#sql" && coll.is_empty();
        for e in entries {
            let (kind, body) = match (&e.sql, &e.pipeline) {
                (Some(s), _) => ("sql", s.clone()),
                // Under the SQL key a Mongo entry has no meaning.
                _ if sql_key => continue,
                (None, Some(p)) => ("pipeline", p.clone()),
                (None, None) => ("filter", e.filter.clone()),
            };
            let (db, coll) = if sql_key || kind == "sql" { ("", "") } else { (db, coll) };
            let (sort, project) = if kind == "filter" { (e.sort.clone(), e.project.clone()) } else { Default::default() };
            if lib.scripts.iter().any(|s| s.conn == conn && s.kind == kind && s.name == e.name && s.body == body && s.db == db && s.coll == coll) {
                continue;
            }
            let id = lib.next_id();
            lib.scripts.push(Script { id, name: e.name.clone(), kind: kind.into(), body, sort, project, conn, db: db.into(), coll: coll.into(), updated: now, ..Default::default() });
            added += 1;
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, filter: &str, pipeline: Option<&str>, sql: Option<&str>) -> OldEntry {
        OldEntry { name: name.into(), filter: filter.into(), pipeline: pipeline.map(Into::into), sql: sql.map(Into::into), ..Default::default() }
    }

    fn run(old: Vec<(&str, Vec<OldEntry>)>) -> Library {
        let mut l = Library::default();
        migrate_into(&mut l, &old.into_iter().map(|(k, v)| (k.to_string(), v)).collect::<Vec<_>>(), 7);
        l
    }

    #[test]
    fn a_sql_key_becomes_sql_scripts_of_that_connection() {
        let l = run(vec![("3/#sql", vec![entry("Late", "", None, Some("SELECT 1"))])]);
        let s = &l.scripts[0];
        assert_eq!((s.kind.as_str(), s.conn, s.body.as_str(), s.db.as_str(), s.coll.as_str(), s.folder, s.updated), ("sql", 3, "SELECT 1", "", "", None, 7));
    }

    #[test]
    fn a_collection_key_splits_into_connection_database_and_collection() {
        let mut q = entry("Paid", "{ status: 'paid' }", None, None);
        (q.sort, q.project) = ("{ total: -1 }".into(), "{ total: 1 }".into());
        let l = run(vec![("1/shop/orders", vec![q, entry("Totals", "", Some("[{ $group: { _id: 1 } }]"), None)])]);
        let by = |n: &str| l.scripts.iter().find(|s| s.name == n).unwrap();
        let (f, p) = (by("Paid"), by("Totals"));
        assert_eq!((f.kind.as_str(), f.conn, f.db.as_str(), f.coll.as_str(), f.body.as_str(), f.sort.as_str(), f.project.as_str()), ("filter", 1, "shop", "orders", "{ status: 'paid' }", "{ total: -1 }", "{ total: 1 }"));
        assert_eq!((p.kind.as_str(), p.coll.as_str(), p.body.as_str(), p.sort.as_str()), ("pipeline", "orders", "[{ $group: { _id: 1 } }]", ""));
    }

    #[test]
    fn a_slash_in_the_collection_name_stays_in_it() {
        let l = run(vec![("2/shop/a/b", vec![entry("x", "{}", None, None)])]);
        assert_eq!((l.scripts[0].db.as_str(), l.scripts[0].coll.as_str()), ("shop", "a/b"));
    }

    #[test]
    fn the_same_script_on_two_databases_stays_two_scripts() {
        let l = run(vec![("1/shop/orders", vec![entry("q", "{}", None, None)]), ("1/test/orders", vec![entry("q", "{}", None, None)])]);
        assert_eq!(l.scripts.len(), 2);
    }

    #[test]
    fn a_postgres_collection_keeps_its_schema_form() {
        let l = run(vec![("4/app/public.users", vec![entry("x", "{}", None, None)])]);
        assert_eq!(l.scripts[0].coll, "public.users");
    }

    #[test]
    fn a_key_holding_every_kind_gives_one_script_of_each() {
        let l = run(vec![("1/shop/orders", vec![entry("q", "{}", None, None), entry("p", "", Some("[]"), None), entry("s", "", None, Some("SELECT 2"))])]);
        let mut kinds: Vec<&str> = l.scripts.iter().map(|s| s.kind.as_str()).collect();
        kinds.sort();
        assert_eq!(kinds, ["filter", "pipeline", "sql"]);
        // A SQL entry kept on a collection key names no collection.
        assert_eq!(l.scripts.iter().find(|s| s.kind == "sql").unwrap().coll, "");
    }

    #[test]
    fn ids_are_unique_and_a_second_run_imports_nothing() {
        let old: Vec<(String, Vec<OldEntry>)> = vec![("1/shop/orders".into(), vec![entry("q", "{}", None, None)]), ("3/#sql".into(), vec![entry("s", "", None, Some("SELECT 1")), entry("t", "", None, Some("SELECT 2"))])];
        let mut l = Library::default();
        assert_eq!(migrate_into(&mut l, &old, 1), 3);
        let ids: std::collections::HashSet<u64> = l.scripts.iter().map(|s| s.id).collect();
        assert_eq!(ids.len(), 3);
        assert_eq!(migrate_into(&mut l, &old, 2), 0);
        assert_eq!(l.scripts.len(), 3);
    }

    #[test]
    fn a_bad_key_is_skipped_and_the_rest_imported() {
        let l = run(vec![("junk", vec![entry("a", "{}", None, None)]), ("5/#sql", vec![entry("b", "", None, Some("SELECT 1")), entry("c", "{}", None, None)])]);
        assert_eq!(l.scripts.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["b"]);
    }

    #[test]
    fn short_keys_give_scripts_with_what_they_name() {
        // A key with only a connection, or a database and no collection, still keeps its entries: the script opens with a
        // note that it names no collection instead of being lost.
        let l = run(vec![("1", vec![entry("a", "{}", None, None)]), ("2/shop", vec![entry("b", "{}", None, None)]), ("3/shop/", vec![entry("c", "{}", None, None)])]);
        let by = |n: &str| l.scripts.iter().find(|s| s.name == n).unwrap();
        assert_eq!((by("a").conn, by("a").db.as_str(), by("a").coll.as_str()), (1, "", ""));
        assert_eq!((by("b").kind.as_str(), by("b").db.as_str(), by("b").coll.as_str()), ("filter", "shop", ""));
        assert_eq!((by("c").db.as_str(), by("c").coll.as_str()), ("shop", ""));
    }

    #[test]
    fn sql_wins_over_a_pipeline_and_an_unnamed_entry_gets_a_name() {
        let l = run(vec![("1/shop/orders", vec![entry("both", "", Some("[]"), Some("SELECT 1")), entry("", "{ a: 1 }", None, None), entry("empty", "", None, None)])]);
        let both = l.scripts.iter().find(|s| s.name == "both").unwrap();
        assert_eq!((both.kind.as_str(), both.body.as_str()), ("sql", "SELECT 1"));
        assert!(l.scripts.iter().any(|s| s.name == "Untitled" && s.body == "{ a: 1 }"), "{:?}", l.scripts.iter().map(|s| &s.name).collect::<Vec<_>>());
        // An empty filter is still a query someone saved, with its sort and projection.
        assert!(l.scripts.iter().any(|s| s.name == "empty"));
    }

    #[test]
    fn a_duplicate_inside_one_key_is_imported_once() {
        let l = run(vec![("1/shop/orders", vec![entry("q", "{}", None, None), entry("q", "{}", None, None)])]);
        assert_eq!(l.scripts.len(), 1);
    }
}
