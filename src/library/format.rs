//! What a file of a linked folder means. `.sql` and `.js` files are the script as written. `.json` files hold a Mongo
//! filter or pipeline in a small shape. Quarry never writes a header into a file; it only reads one the person wrote on
//! the first line (`-- quarry: db=shop schema=x`, or `// quarry: db=shop` in a .js file).
use super::Link;
use crate::db::sql::Engine;
use mongodb::bson::{Bson, Document};
use serde_json::Value;

/// Where a script wants to run, from a header line or the keys of a .json file. Empty fields mean "wherever the tab is".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Target {
    pub db: String,
    pub schema: String,
    pub coll: String,
}

/// The target a first line `-- quarry: db=shop schema=x` or `// quarry: db=shop` names. Values may be quoted. Any other
/// first line, an unknown key or a missing `quarry:` gives the empty target.
pub fn header(text: &str) -> Target {
    let mut t = Target::default();
    let line = text.trim_start_matches('\u{feff}').lines().next().unwrap_or("").trim();
    let Some(rest) = line.strip_prefix("--").or_else(|| line.strip_prefix("//")) else { return t };
    let Some(rest) = rest.trim().strip_prefix("quarry:") else { return t };
    let mut chars = rest.trim().chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let key: String = std::iter::from_fn(|| chars.next_if(|c| *c != '=' && !c.is_whitespace())).collect();
        if key.is_empty() || chars.next_if_eq(&'=').is_none() {
            break;
        }
        let mut val = String::new();
        if let Some(q) = chars.next_if(|c| *c == '"' || *c == '\'') {
            for c in chars.by_ref() {
                if c == q {
                    break;
                }
                val.push(c);
            }
        } else {
            val = std::iter::from_fn(|| chars.next_if(|c| !c.is_whitespace())).collect();
        }
        match key.as_str() {
            "db" => t.db = val,
            "schema" => t.schema = val,
            "coll" | "collection" => t.coll = val,
            _ => {}
        }
    }
    t
}

/// The kind of script a file extension holds, or None for one Quarry does not read. `json` is decided by its content.
pub fn kind_of_ext(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "sql" => Some("sql"),
        "js" => Some("shell"),
        "json" => Some("filter"),
        _ => None,
    }
}

/// The lowercase extension of a path, without the dot.
pub fn ext_of(path: &std::path::Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// A .json file read as a script. `body` is the filter or pipeline as shell text, the way the editors show it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parsed {
    /// "filter" or "pipeline".
    pub kind: String,
    pub body: String,
    pub sort: String,
    pub project: String,
    pub target: Target,
}

/// A filter, sort or projection kept as a string holds the text as the person wrote it, query shortcuts and all.
fn doc_text(v: &Value) -> Option<String> {
    if let Value::String(t) = v {
        return crate::db::parse_doc(t).is_ok().then(|| t.clone());
    }
    match Bson::try_from(v.clone()).ok()? {
        Bson::Document(d) if d.is_empty() => Some(String::new()),
        b @ Bson::Document(_) => Some(crate::db::shell_text(&b)),
        _ => None,
    }
}

fn pipeline_text(v: &Value) -> Option<String> {
    if let Value::String(t) = v {
        return crate::db::parse_pipeline(t).is_ok().then(|| t.clone());
    }
    let Bson::Array(a) = Bson::try_from(v.clone()).ok()? else { return None };
    a.iter().all(|s| matches!(s, Bson::Document(_))).then(|| crate::db::shell_text(&Bson::Array(a)))
}

/// A .json file as a filter or a pipeline, or None for any other JSON, which Quarry leaves alone.
///   `{"collection": "orders", "db": "shop", "filter": {...}, "sort": {...}, "project": {...}}` is a filter.
///   `{"collection": "orders", "pipeline": [...]}` or a top-level array is a pipeline.
/// Values are Extended JSON, so `{"$oid": ...}` and `{"$date": ...}` become shell values, while a plain string stays one.
pub fn parse_json(text: &str) -> Option<Parsed> {
    let v: Value = json5::from_str(text).ok()?;
    let mut p = Parsed::default();
    match &v {
        Value::Array(_) => {
            p.kind = "pipeline".into();
            p.body = pipeline_text(&v)?;
        }
        Value::Object(o) => {
            let s = |k: &str| o.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            p.target = Target { db: s("db"), schema: s("schema"), coll: s("collection") };
            if let Some(pl) = o.get("pipeline") {
                p.kind = "pipeline".into();
                p.body = pipeline_text(pl)?;
            } else if o.contains_key("filter") || o.contains_key("collection") {
                p.kind = "filter".into();
                p.body = o.get("filter").map_or(Some(String::new()), doc_text)?;
                p.sort = o.get("sort").map_or(Some(String::new()), doc_text)?;
                p.project = o.get("project").map_or(Some(String::new()), doc_text)?;
            } else {
                return None;
            }
        }
        _ => return None,
    }
    Some(p)
}

/// Whether text may hold a query shortcut (`#today`, `@2024-01-02`, `~"text"`). Parsing expands those to a fixed date or a
/// regex, so text with one is kept as written. A false hit, such as a `#` inside a string, only keeps the text too.
fn has_shortcut(text: &str) -> bool {
    text.contains(['~', '#', '@'])
}

/// Shell text as Extended JSON, in the relaxed form people write by hand. Text with a query shortcut is checked, then kept
/// as a string, which `parse_json` reads back as the same text.
fn to_value(text: &str, pipeline: bool) -> Result<Value, String> {
    if pipeline {
        let stages: Vec<Bson> = crate::db::parse_pipeline(text)?.into_iter().map(Bson::Document).collect();
        if has_shortcut(text) {
            return Ok(Value::String(text.trim().to_string()));
        }
        Ok(Bson::Array(stages).into_relaxed_extjson())
    } else {
        let d: Document = crate::db::parse_doc(text)?;
        if has_shortcut(text) {
            return Ok(Value::String(text.trim().to_string()));
        }
        Ok(Bson::Document(d).into_relaxed_extjson())
    }
}

/// The text of a .json file for a script of `p`. `old` is what the file holds now: its other keys (a "note", say) and its
/// collection and db stay as they are, and only the filter, sort, project or pipeline change. Comments in the old file
/// are lost, since JSON5 is read into plain values.
pub fn to_json(p: &Parsed, old: Option<&str>) -> Result<String, String> {
    let pipeline = p.kind == "pipeline";
    let mut obj = match old.and_then(|t| json5::from_str::<Value>(t).ok()) {
        Some(Value::Object(o)) => o,
        _ => serde_json::Map::new(),
    };
    if !p.target.coll.is_empty() {
        obj.insert("collection".into(), p.target.coll.clone().into());
    }
    if !p.target.db.is_empty() {
        obj.insert("db".into(), p.target.db.clone().into());
    }
    if pipeline {
        obj.insert("pipeline".into(), to_value(&p.body, true)?);
    } else {
        obj.insert("filter".into(), to_value(&p.body, false)?);
        for (k, t) in [("sort", &p.sort), ("project", &p.project)] {
            if t.trim().is_empty() {
                obj.remove(k);
            } else {
                obj.insert(k.into(), to_value(t, false)?);
            }
        }
    }
    let mut s = serde_json::to_string_pretty(&Value::Object(obj)).map_err(|e| e.to_string())?;
    s.push('\n');
    Ok(s)
}

/// The engine word a link's `engine` field holds.
pub fn engine_key(e: Engine) -> &'static str {
    match e {
        Engine::Mongo => "mongo",
        Engine::MySql => "mysql",
        Engine::Postgres => "postgres",
    }
}

/// The engine of an `engine_key` word; MongoDB for one this build does not know.
pub fn engine_of_key(key: &str) -> Engine {
    match key {
        "mysql" => Engine::MySql,
        "postgres" => Engine::Postgres,
        _ => Engine::Mongo,
    }
}

/// Whether a script of `kind` runs on `engine`: SQL on SQL connections, a filter or pipeline on MongoDB and a Shell
/// script on any. None for a kind this build does not know, which a saved script refuses and a file allows.
pub fn kind_runs_on(kind: &str, engine: Engine) -> Option<bool> {
    match kind {
        "sql" => Some(engine.is_sql()),
        "filter" | "pipeline" => Some(engine == Engine::Mongo),
        "shell" => Some(true),
        _ => None,
    }
}

/// Whether a file of `kind` in `link` shows for connection `conn` of `engine`. `.sql` belongs to SQL connections, a
/// filter or pipeline (.json) to Mongo ones and a Shell script (.js) to any. `engine` "mysql" covers MySQL and MariaDB,
/// which are one `Engine`.
pub fn fits(link: &Link, kind: &str, conn: u64, engine: Engine) -> bool {
    kind_runs_on(kind, engine).unwrap_or(true) && used_on(link, conn, Some(engine))
}

/// Whether a link is used on connection `conn` of `engine` (None when not known yet). An `on` this build does not know is
/// read as chosen connections, so a scope from a newer build never widens to every connection here. "engine" with no
/// engine named, which no dialog writes, is every connection rather than none.
pub fn used_on(link: &Link, conn: u64, engine: Option<Engine>) -> bool {
    match link.on.as_str() {
        "engine" if link.engine.is_empty() => true,
        "engine" => engine.is_none_or(|e| link.engine == engine_key(e)),
        "all" => true,
        _ => link.conns.contains(&conn),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(db: &str, schema: &str, coll: &str) -> Target {
        Target { db: db.into(), schema: schema.into(), coll: coll.into() }
    }

    #[test]
    fn a_header_on_the_first_line_names_the_target() {
        assert_eq!(header("-- quarry: db=shop schema=x\nselect 1"), t("shop", "x", ""));
        assert_eq!(header("// quarry: db=\"my db\" coll=orders\nlet a"), t("my db", "", "orders"));
        assert_eq!(header("--quarry:db=shop"), t("shop", "", ""));
    }

    #[test]
    fn other_first_lines_are_no_header() {
        assert_eq!(header("select 1\n-- quarry: db=shop"), Target::default());
        assert_eq!(header("-- a comment about quarry: db=x"), Target::default());
        assert_eq!(header(""), Target::default());
        assert_eq!(header("-- quarry: nonsense"), Target::default());
    }

    #[test]
    fn a_filter_file_is_read_with_its_sort_and_target() {
        let p = parse_json("{ \"collection\": \"orders\", \"db\": \"shop\", \"filter\": { \"status\": \"paid\", \"n\": { \"$gt\": 3 } }, \"sort\": { \"n\": -1 }, // note\n \"project\": { \"n\": 1 } }").unwrap();
        assert_eq!((p.kind.as_str(), p.target.clone()), ("filter", t("shop", "", "orders")));
        assert_eq!((p.body.as_str(), p.sort.as_str(), p.project.as_str()), ("{ status: \"paid\", n: { $gt: 3 } }", "{ n: -1 }", "{ n: 1 }"));
    }

    #[test]
    fn a_pipeline_is_an_array_or_a_key() {
        let a = parse_json(r#"[{"$match": {"a": 1}}, {"$limit": 5}]"#).unwrap();
        assert_eq!((a.kind.as_str(), a.body.as_str()), ("pipeline", "[{ $match: { a: 1 } }, { $limit: 5 }]"));
        let b = parse_json(r#"{"collection": "orders", "pipeline": [{"$count": "n"}]}"#).unwrap();
        assert_eq!((b.kind.as_str(), b.target.coll.as_str(), b.body.as_str()), ("pipeline", "orders", "[{ $count: \"n\" }]"));
    }

    #[test]
    fn other_json_is_skipped() {
        for s in ["{\"name\": \"pkg\", \"version\": 1}", "[1, 2]", "42", "not json", "{\"filter\": 3}", "{\"pipeline\": [3]}"] {
            assert!(parse_json(s).is_none(), "{s}");
        }
    }

    #[test]
    fn extended_json_round_trips_and_strings_stay_strings() {
        let text = r#"{"collection":"c","filter":{"_id":{"$oid":"64b7f0a2c3d4e5f607182930"},"at":{"$date":"2024-01-02T03:04:05Z"},"code":"2024-01-02","s":"{\"$oid\": 1}"}}"#;
        let p = parse_json(text).unwrap();
        assert!(p.body.contains("ObjectId(\"64b7f0a2c3d4e5f607182930\")") && p.body.contains("ISODate("), "{}", p.body);
        assert!(p.body.contains("code: \"2024-01-02\""), "{}", p.body);
        let out = to_json(&p, Some(text)).unwrap();
        let again = parse_json(&out).unwrap();
        assert_eq!(again, p);
        // The string that looks like Extended JSON is still a string after the round trip.
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["filter"]["s"], Value::String("{\"$oid\": 1}".into()));
        assert_eq!(v["filter"]["code"], Value::String("2024-01-02".into()));
    }

    #[test]
    fn query_shortcuts_are_kept_as_written() {
        // Expanding #today into a date when the file is written would freeze the filter to the day it was saved.
        let p = Parsed { kind: "filter".into(), body: "{ created: #today, name: ~\"x\" }".into(), sort: "{ created: -1 }".into(), target: Target { coll: "orders".into(), ..Default::default() }, ..Default::default() };
        let out = to_json(&p, None).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["filter"], Value::String(p.body.clone()), "{out}");
        assert_eq!(v["sort"]["created"], -1, "text without a shortcut stays an object");
        assert_eq!(parse_json(&out).unwrap(), p);
        let q = Parsed { kind: "pipeline".into(), body: "[{ $match: { at: #last7days } }]".into(), ..Default::default() };
        let out = to_json(&q, None).unwrap();
        assert_eq!(parse_json(&out).unwrap().body, q.body);
        // A string that is no filter is still not a script.
        assert!(parse_json(r#"{"collection": "c", "filter": "{ a: "}"#).is_none());
    }

    #[test]
    fn writing_keeps_unknown_keys_and_drops_emptied_ones() {
        let old = r#"{"note": "keep me", "collection": "orders", "filter": {}, "sort": {"a": 1}}"#;
        let mut p = parse_json(old).unwrap();
        p.body = "{ a: 2 }".into();
        p.sort.clear();
        let v: Value = serde_json::from_str(&to_json(&p, Some(old)).unwrap()).unwrap();
        assert_eq!((v["note"].as_str(), v["collection"].as_str(), v.get("sort")), (Some("keep me"), Some("orders"), None));
        assert_eq!(v["filter"]["a"], 2);
        assert!(to_json(&Parsed { kind: "filter".into(), body: "{ a: ".into(), ..Default::default() }, None).is_err());
    }

    #[test]
    fn a_link_fits_the_connections_it_is_used_on() {
        let link = |on: &str, engine: &str, conns: &[u64]| Link { on: on.into(), engine: engine.into(), conns: conns.to_vec(), ..Default::default() };
        let chosen = link("conns", "", &[7]);
        assert!(fits(&chosen, "sql", 7, Engine::MySql) && !fits(&chosen, "sql", 8, Engine::MySql));
        assert!(!fits(&chosen, "sql", 7, Engine::Mongo) && fits(&chosen, "filter", 7, Engine::Mongo) && !fits(&chosen, "pipeline", 7, Engine::Postgres));
        assert!(fits(&chosen, "shell", 7, Engine::Mongo) && fits(&chosen, "shell", 7, Engine::Postgres));
        let my = link("engine", "mysql", &[]);
        assert!(fits(&my, "sql", 1, Engine::MySql) && !fits(&my, "sql", 1, Engine::Postgres) && fits(&my, "shell", 1, Engine::MySql));
        assert!(fits(&link("all", "", &[]), "sql", 1, Engine::Postgres) && !fits(&link("all", "", &[]), "sql", 1, Engine::Mongo));
    }

    #[test]
    fn odd_scopes_read_from_disk_stay_narrow() {
        let link = |on: &str, engine: &str, conns: &[u64]| Link { on: on.into(), engine: engine.into(), conns: conns.to_vec(), ..Default::default() };
        // A scope from a newer build is read as chosen connections, never as every connection.
        let future = link("team", "", &[7]);
        assert!(fits(&future, "sql", 7, Engine::MySql) && !fits(&future, "sql", 8, Engine::MySql));
        // "engine" with no engine named is every connection; a removed connection's id matches nobody else.
        assert!(fits(&link("engine", "", &[]), "sql", 3, Engine::Postgres));
        let gone = link("conns", "", &[99]);
        assert!(!fits(&gone, "sql", 1, Engine::MySql) && !fits(&gone, "shell", 2, Engine::Mongo));
        assert!(used_on(&link("engine", "mysql", &[]), 1, None));
    }
}
