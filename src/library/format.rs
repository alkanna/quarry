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

fn doc_text(v: &Value) -> Option<String> {
    match Bson::try_from(v.clone()).ok()? {
        Bson::Document(d) if d.is_empty() => Some(String::new()),
        b @ Bson::Document(_) => Some(crate::db::shell_text(&b)),
        _ => None,
    }
}

fn pipeline_text(v: &Value) -> Option<String> {
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

/// Shell text as Extended JSON, in the relaxed form people write by hand.
fn to_value(text: &str, pipeline: bool) -> Result<Value, String> {
    if pipeline {
        let stages: Vec<Bson> = crate::db::parse_pipeline(text)?.into_iter().map(Bson::Document).collect();
        Ok(Bson::Array(stages).into_relaxed_extjson())
    } else {
        let d: Document = crate::db::parse_doc(text)?;
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

/// Whether a file of `kind` in `link` shows for connection `conn` of `engine`. `.sql` belongs to SQL connections, a
/// filter or pipeline (.json) to Mongo ones and a Shell script (.js) to any. `engine` "mysql" covers MySQL and MariaDB,
/// which are one `Engine`.
pub fn fits(link: &Link, kind: &str, conn: u64, engine: Engine) -> bool {
    let kind_ok = match kind {
        "sql" => engine.is_sql(),
        "filter" | "pipeline" => engine == Engine::Mongo,
        _ => true,
    };
    kind_ok
        && match link.on.as_str() {
            "engine" => link.engine == engine_key(engine),
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
}
