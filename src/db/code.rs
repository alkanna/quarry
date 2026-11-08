//! Driver code for a query or pipeline in other languages.

use super::*;

/// What to turn into driver code: a find with its options, or a pipeline.
pub enum CodeOf {
    Find(Box<Query>),
    Aggregate(Vec<Document>),
}

#[derive(Clone, Copy, PartialEq)]
pub enum Lang {
    Mongosh,
    Node,
    Python,
    Java,
    Go,
    Rust,
}

impl Lang {
    pub const ALL: [Lang; 6] = [Lang::Mongosh, Lang::Node, Lang::Python, Lang::Java, Lang::Go, Lang::Rust];

    pub fn name(self) -> &'static str {
        match self {
            Lang::Mongosh => "mongosh",
            Lang::Node => "Node.js",
            Lang::Python => "Python",
            Lang::Java => "Java",
            Lang::Go => "Go",
            Lang::Rust => "Rust",
        }
    }
}

/// A runnable snippet for the query. Values travel as Extended JSON in a raw string literal and are
/// read back by each driver's own parser, so every BSON type survives without a per-language converter.
pub fn driver_code(lang: Lang, db: &str, coll: &str, what: &CodeOf) -> String {
    let q = |s: &str| serde_json::Value::String(s.into()).to_string();
    let uri = "mongodb://localhost:27017";
    let (filter, sort, proj, skip, limit, hint, collation) = match what {
        CodeOf::Find(f) => (Some(to_edit_json(&f.filter)), f.sort.as_ref().map(to_edit_json), f.projection.as_ref().map(to_edit_json), f.skip, f.limit, f.hint.as_ref(), f.collation.as_ref()),
        CodeOf::Aggregate(_) => (None, None, None, 0, None, None, None),
    };
    // An index name stays a plain string; index keys and collations travel as Extended JSON like the rest.
    let hint_name = hint.and_then(|h| h.as_str());
    let hint_keys = hint.filter(|h| h.as_str().is_none()).map(to_edit_value);
    let coll_json = collation.map(to_edit_json);
    let pipe = match what {
        CodeOf::Aggregate(p) => Some(to_edit_value(&Bson::Array(p.iter().cloned().map(Bson::Document).collect()))),
        CodeOf::Find(_) => None,
    };
    // Each literal escapes only what its delimiter cannot hold.
    let js = |s: &str| format!("String.raw`{}`", s.replace('`', "` + \"`\" + String.raw`").replace("${", "${'${'}"));
    let py = |s: &str| format!("r\"\"\"{}\"\"\"", s.replace("\"\"\"", "\"\"\" + '\"\"\"' + r\"\"\""));
    let java = |s: &str| format!("Document.parse(\"\"\"\n{}\n\"\"\")", s.replace('\\', "\\\\").replace("\"\"\"", "\\\"\"\""));
    // Go reads only canonical numbers when canonical is true, and the editor text is relaxed, so it passes false.
    let go = |s: &str| format!("[]byte(`{}`)", s.replace('`', "` + \"`\" + `"));
    let rs = |s: &str| {
        let hashes = "#".repeat(1 + s.matches("\"#").count().min(8));
        format!("r{hashes}\"{s}\"{hashes}")
    };
    let c = crate::app::collection_ref(coll);
    let c = if c.starts_with("db.") { c.replacen("collection", "getCollection", 1) } else { format!("db.{c}") };
    let sh = |d: &Document| shell_text(&Bson::Document(d.clone()));
    match lang {
        Lang::Mongosh => match what {
            CodeOf::Find(f) => {
                let mut s = format!("{c}.find({}", sh(&f.filter));
                if let Some(p) = &f.projection {
                    s += &format!(", {}", sh(p));
                }
                s.push(')');
                if let Some(o) = &f.sort {
                    s += &format!(".sort({})", sh(o));
                }
                if skip > 0 {
                    s += &format!(".skip({skip})");
                }
                if let Some(n) = limit {
                    s += &format!(".limit({n})");
                }
                if let Some(h) = hint {
                    s += &format!(".hint({})", shell_text(h));
                }
                if let Some(co) = collation {
                    s += &format!(".collation({})", sh(co));
                }
                s
            }
            CodeOf::Aggregate(p) => format!("{c}.aggregate({})", shell_text(&Bson::Array(p.iter().cloned().map(Bson::Document).collect()))),
        },
        Lang::Node => {
            let head = format!("import {{ MongoClient }} from \"mongodb\";\nimport {{ EJSON }} from \"bson\";\n\nconst client = new MongoClient({});\nconst coll = client.db({}).collection({});\n\n", q(uri), q(db), q(coll));
            let body = match (&filter, &pipe) {
                (Some(f), _) => {
                    let mut s = format!("const filter = EJSON.parse({});\n", js(f));
                    let mut opts = Vec::new();
                    for n in ["sort", "projection"] {
                        if let Some(v) = if n == "sort" { &sort } else { &proj } {
                            s += &format!("const {n} = EJSON.parse({});\n", js(v));
                            opts.push(n.to_string());
                        }
                    }
                    if skip > 0 {
                        opts.push(format!("skip: {skip}"));
                    }
                    if let Some(n) = limit {
                        opts.push(format!("limit: {n}"));
                    }
                    if let Some(h) = hint_name {
                        opts.push(format!("hint: {}", q(h)));
                    }
                    if let Some(h) = &hint_keys {
                        opts.push(format!("hint: EJSON.parse({})", js(h)));
                    }
                    if let Some(co) = &coll_json {
                        opts.push(format!("collation: EJSON.parse({})", js(co)));
                    }
                    format!("{s}const docs = await coll.find(filter, {{ {} }}).toArray();\n", opts.join(", "))
                }
                (_, Some(p)) => format!("const pipeline = EJSON.parse({});\nconst docs = await coll.aggregate(pipeline).toArray();\n", js(p)),
                _ => String::new(),
            };
            format!("{head}{body}console.log(docs);\nawait client.close();\n")
        }
        Lang::Python => {
            let head = format!("from bson import json_util\nfrom pymongo import MongoClient\n\nclient = MongoClient({})\ncoll = client[{}][{}]\n\n", q(uri), q(db), q(coll));
            let body = match (&filter, &pipe) {
                (Some(f), _) => {
                    let mut s = format!("query = json_util.loads({})\n", py(f));
                    let mut call = "coll.find(query".to_string();
                    if let Some(v) = &proj {
                        s += &format!("projection = json_util.loads({})\n", py(v));
                        call += ", projection";
                    }
                    call.push(')');
                    if let Some(v) = &sort {
                        s += &format!("sort = json_util.loads({})\n", py(v));
                        call += ".sort(list(sort.items()))";
                    }
                    if let Some(h) = hint_name {
                        call += &format!(".hint({})", q(h));
                    }
                    if let Some(h) = &hint_keys {
                        s += &format!("hint = json_util.loads({})\n", py(h));
                        call += ".hint(list(hint.items()))";
                    }
                    if let Some(co) = &coll_json {
                        s += &format!("collation = json_util.loads({})\n", py(co));
                        call += ".collation(collation)";
                    }
                    if skip > 0 {
                        call += &format!(".skip({skip})");
                    }
                    if let Some(n) = limit {
                        call += &format!(".limit({n})");
                    }
                    format!("{s}for doc in {call}:\n    print(doc)\n")
                }
                (_, Some(p)) => format!("pipeline = json_util.loads({})\nfor doc in coll.aggregate(pipeline):\n    print(doc)\n", py(p)),
                _ => String::new(),
            };
            format!("{head}{body}")
        }
        Lang::Java => {
            let head = format!("import com.mongodb.client.*;\nimport java.util.List;\nimport org.bson.Document;\n\ntry (MongoClient client = MongoClients.create({})) {{\n    MongoCollection<Document> coll = client.getDatabase({}).getCollection({});\n\n", q(uri), q(db), q(coll));
            let body = match (&filter, &pipe) {
                (Some(f), _) => {
                    let mut call = "coll.find(filter)".to_string();
                    let mut s = format!("    Document filter = {};\n", java(f));
                    if let Some(v) = &proj {
                        s += &format!("    Document projection = {};\n", java(v));
                        call += ".projection(projection)";
                    }
                    if let Some(v) = &sort {
                        s += &format!("    Document sort = {};\n", java(v));
                        call += ".sort(sort)";
                    }
                    if let Some(h) = hint_name {
                        call += &format!(".hintString({})", q(h));
                    }
                    if let Some(h) = &hint_keys {
                        s += &format!("    Document hint = {};\n", java(h));
                        call += ".hint(hint)";
                    }
                    if let Some(co) = collation {
                        call += &format!(".collation({})", java_collation(co));
                    }
                    if skip > 0 {
                        call += &format!(".skip({skip})");
                    }
                    if let Some(n) = limit {
                        call += &format!(".limit({n})");
                    }
                    format!("{s}    for (Document doc : {call}) {{\n        System.out.println(doc.toJson());\n    }}\n")
                }
                // Document.parse only reads objects, so the array rides inside one.
                (_, Some(p)) => {
                    let wrapped = java(&format!("{{ \"pipeline\": {p} }}"));
                    format!("    List<Document> pipeline = {wrapped}.getList(\"pipeline\", Document.class);\n    for (Document doc : coll.aggregate(pipeline)) {{\n        System.out.println(doc.toJson());\n    }}\n")
                }
                _ => String::new(),
            };
            format!("{head}{body}}}\n")
        }
        Lang::Go => {
            let head = format!("package main\n\nimport (\n\t\"context\"\n\t\"fmt\"\n\n\t\"go.mongodb.org/mongo-driver/v2/bson\"\n\t\"go.mongodb.org/mongo-driver/v2/mongo\"\n\t\"go.mongodb.org/mongo-driver/v2/mongo/options\"\n)\n\nfunc main() {{\n\tctx := context.TODO()\n\tclient, err := mongo.Connect(options.Client().ApplyURI({}))\n\tif err != nil {{\n\t\tpanic(err)\n\t}}\n\tdefer client.Disconnect(ctx)\n\tcoll := client.Database({}).Collection({})\n\n\tparse := func(js []byte, v any) {{\n\t\tif err := bson.UnmarshalExtJSON(js, false, v); err != nil {{\n\t\t\tpanic(err)\n\t\t}}\n\t}}\n", q(uri), q(db), q(coll));
            let body = match (&filter, &pipe) {
                (Some(f), _) => {
                    let mut s = format!("\tvar filter bson.D\n\tparse({}, &filter)\n", go(f));
                    let mut opts = Vec::new();
                    for (n, v, set) in [("sort", &sort, "SetSort"), ("projection", &proj, "SetProjection")] {
                        if let Some(v) = v {
                            s += &format!("\tvar {n} bson.D\n\tparse({}, &{n})\n", go(v));
                            opts.push(format!(".{set}({n})"));
                        }
                    }
                    if skip > 0 {
                        opts.push(format!(".SetSkip({skip})"));
                    }
                    if let Some(n) = limit {
                        opts.push(format!(".SetLimit({n})"));
                    }
                    if let Some(h) = hint_name {
                        opts.push(format!(".SetHint({})", q(h)));
                    }
                    if let Some(h) = &hint_keys {
                        s += &format!("\tvar hint bson.D\n\tparse({}, &hint)\n", go(h));
                        opts.push(".SetHint(hint)".into());
                    }
                    if let Some(co) = collation {
                        opts.push(format!(".SetCollation({})", go_collation(co)));
                    }
                    format!("{s}\tcur, err := coll.Find(ctx, filter, options.Find(){})\n", opts.concat())
                }
                // UnmarshalExtJSON needs a document at the top, so the array rides inside one.
                (_, Some(p)) => format!("\tvar wrapped struct{{ Pipeline mongo.Pipeline }}\n\tparse({}, &wrapped)\n\tcur, err := coll.Aggregate(ctx, wrapped.Pipeline)\n", go(&format!("{{ \"pipeline\": {p} }}"))),
                _ => String::new(),
            };
            format!("{head}{body}\tif err != nil {{\n\t\tpanic(err)\n\t}}\n\tvar docs []bson.M\n\tif err := cur.All(ctx, &docs); err != nil {{\n\t\tpanic(err)\n\t}}\n\tfmt.Println(docs)\n}}\n")
        }
        Lang::Rust => {
            let head = format!("use futures_util::TryStreamExt;\nuse mongodb::{{bson::Document, Client}};\n\n#[tokio::main]\nasync fn main() -> mongodb::error::Result<()> {{\n    let client = Client::with_uri_str({}).await?;\n    let coll = client.database({}).collection::<Document>({});\n\n", q(uri), q(db), q(coll));
            let body = match (&filter, &pipe) {
                (Some(f), _) => {
                    let mut s = format!("    let filter: Document = serde_json::from_str({}).unwrap();\n", rs(f));
                    let mut call = "coll.find(filter)".to_string();
                    for (n, v) in [("sort", &sort), ("projection", &proj)] {
                        if let Some(v) = v {
                            s += &format!("    let {n}: Document = serde_json::from_str({}).unwrap();\n", rs(v));
                            call += &format!(".{n}({n})");
                        }
                    }
                    if skip > 0 {
                        call += &format!(".skip({skip})");
                    }
                    if let Some(n) = limit {
                        call += &format!(".limit({n})");
                    }
                    if let Some(h) = hint_name {
                        call += &format!(".hint(mongodb::options::Hint::Name({}.into()))", q(h));
                    }
                    if let Some(h) = &hint_keys {
                        s += &format!("    let hint: Document = serde_json::from_str({}).unwrap();\n", rs(h));
                        call += ".hint(mongodb::options::Hint::Keys(hint))";
                    }
                    if let Some(co) = &coll_json {
                        s += &format!("    let collation: Document = serde_json::from_str({}).unwrap();\n", rs(co));
                        call += ".collation(mongodb::bson::from_document::<mongodb::options::Collation>(collation).unwrap())";
                    }
                    format!("{s}    let mut cursor = {call}.await?;\n")
                }
                (_, Some(p)) => format!("    let pipeline: Vec<Document> = serde_json::from_str({}).unwrap();\n    let mut cursor = coll.aggregate(pipeline).await?;\n", rs(p)),
                _ => String::new(),
            };
            format!("{head}{body}    while let Some(doc) = cursor.try_next().await? {{\n        println!(\"{{doc}}\");\n    }}\n    Ok(())\n}}\n")
        }
    }
}

/// A collation option as a literal both Java and Go read: a quoted string, a bool or an integer.
fn collation_lit(v: &Bson) -> Option<String> {
    match v {
        Bson::String(s) => Some(serde_json::Value::String(s.clone()).to_string()),
        Bson::Boolean(b) => Some(b.to_string()),
        Bson::Int32(n) => Some(n.to_string()),
        Bson::Int64(n) => Some(n.to_string()),
        Bson::Double(n) => Some((*n as i64).to_string()),
        _ => None,
    }
}

/// Java has no parser for a collation, so it is spelled out with the builder.
fn java_collation(d: &Document) -> String {
    let m = "com.mongodb.client.model.";
    let mut s = format!("{m}Collation.builder()");
    for (k, v) in d {
        let Some(v) = collation_lit(v) else { continue };
        s += &match k.as_str() {
            "strength" => format!(".collationStrength({m}CollationStrength.fromInt({v}))"),
            "caseFirst" => format!(".collationCaseFirst({m}CollationCaseFirst.fromString({v}))"),
            "alternate" => format!(".collationAlternate({m}CollationAlternate.fromString({v}))"),
            "maxVariable" => format!(".collationMaxVariable({m}CollationMaxVariable.fromString({v}))"),
            "locale" | "caseLevel" | "numericOrdering" | "normalization" | "backwards" => format!(".{k}({v})"),
            _ => continue,
        };
    }
    s + ".build()"
}

/// Go's Collation fields carry no bson names that match the server's, so it is a struct literal.
fn go_collation(d: &Document) -> String {
    let fields: Vec<String> = d
        .iter()
        .filter_map(|(k, v)| {
            let name = match k.as_str() {
                "locale" => "Locale",
                "caseLevel" => "CaseLevel",
                "caseFirst" => "CaseFirst",
                "strength" => "Strength",
                "numericOrdering" => "NumericOrdering",
                "alternate" => "Alternate",
                "maxVariable" => "MaxVariable",
                "normalization" => "Normalization",
                "backwards" => "Backwards",
                _ => return None,
            };
            Some(format!("{name}: {}", collation_lit(v)?))
        })
        .collect();
    format!("&options.Collation{{{}}}", fields.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code_query() -> Query {
        let id = mongodb::bson::oid::ObjectId::parse_str("64b7f0c2a1b2c3d4e5f60718").unwrap();
        let at = mongodb::bson::DateTime::parse_rfc3339_str("2024-01-02T03:04:05Z").unwrap();
        Query { filter: doc! { "_id": id, "at": { "$gt": at }, "n": 5i64 }, sort: Some(doc! { "at": -1 }), projection: Some(doc! { "n": 1 }), skip: 10, limit: Some(20), hint: Some(Bson::Document(doc! { "at": -1 })), collation: Some(doc! { "locale": "fr", "strength": 2 }) }
    }

    #[test]
    fn driver_code_embeds_extended_json() {
        let what = CodeOf::Find(Box::new(code_query()));
        for (lang, parser) in [(Lang::Node, "EJSON.parse("), (Lang::Python, "json_util.loads("), (Lang::Java, "Document.parse("), (Lang::Go, "bson.UnmarshalExtJSON(js, false, v)"), (Lang::Rust, "serde_json::from_str(")] {
            let code = driver_code(lang, "shop", "orders", &what);
            assert!(code.contains(parser), "{}", lang.name());
            assert!(code.contains("\"$oid\": \"64b7f0c2a1b2c3d4e5f60718\""), "{}", lang.name());
            assert!(code.contains("\"$date\": \"2024-01-02T03:04:05Z\""), "{}", lang.name());
            assert!(code.contains("20") && code.contains("10"), "{}", lang.name());
            assert!(code.contains("hint") || code.contains("Hint"), "{}", lang.name());
            assert!(code.contains("\"fr\""), "{}", lang.name());
        }
        assert!(driver_code(Lang::Java, "shop", "orders", &what).contains(".collation(com.mongodb.client.model.Collation.builder().locale(\"fr\").collationStrength(com.mongodb.client.model.CollationStrength.fromInt(2)).build())"));
        assert!(driver_code(Lang::Go, "shop", "orders", &what).contains(".SetCollation(&options.Collation{Locale: \"fr\", Strength: 2})"));
        let sh = driver_code(Lang::Mongosh, "shop", "orders", &what);
        assert!(sh.starts_with("db.orders.find({") && sh.ends_with(".sort({ at: -1 }).skip(10).limit(20).hint({ at: -1 }).collation({ locale: \"fr\", strength: 2 })"), "{sh}");
        assert!(driver_code(Lang::Mongosh, "shop", "my-coll", &what).starts_with("db.getCollection(\"my-coll\").find("));
    }

    #[test]
    fn driver_code_pipeline_and_rust_json_reads_back() {
        let p = vec![doc! { "$match": { "_id": mongodb::bson::oid::ObjectId::parse_str("64b7f0c2a1b2c3d4e5f60718").unwrap() } }, doc! { "$limit": 3 }];
        let what = CodeOf::Aggregate(p.clone());
        assert!(driver_code(Lang::Node, "shop", "orders", &what).contains("coll.aggregate(pipeline)"));
        assert!(driver_code(Lang::Python, "shop", "orders", &what).contains("json_util.loads(r\"\"\"["));
        assert!(driver_code(Lang::Java, "shop", "orders", &what).contains("{ \"pipeline\": ["));
        assert!(driver_code(Lang::Go, "shop", "orders", &what).contains("coll.Aggregate(ctx, wrapped.Pipeline)"));
        assert!(driver_code(Lang::Mongosh, "shop", "orders", &what).starts_with("db.orders.aggregate([{ $match:"));
        // The literal the Rust snippet embeds must parse with the Rust driver's own types.
        let rust = driver_code(Lang::Rust, "shop", "orders", &what);
        let json = rust.split("r#\"").nth(1).unwrap().split("\"#").next().unwrap();
        assert_eq!(serde_json::from_str::<Vec<Document>>(json).unwrap(), p);
        let q = code_query();
        let json = to_edit_json(&q.filter);
        assert_eq!(serde_json::from_str::<Document>(&json).unwrap(), q.filter);
    }
}
