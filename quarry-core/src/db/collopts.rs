//! What the New database and New collection dialogs send: name checks and the `create` command built from the form.
//! Egui-free so every option combination is tested offline.

use mongodb::bson::{Bson, Document, doc};

/// Major and minor of the server, from "7.0.12". None until the server has said, which allows everything.
pub type Ver = Option<(u32, u32)>;

pub fn parse_ver(s: &str) -> Ver {
    let mut p = s.split(['.', '-']).map(|x| x.parse::<u32>().ok());
    Some((p.next()??, p.next()??))
}

pub fn at_least(v: Ver, major: u32, minor: u32) -> bool {
    v.is_none_or(|v| v >= (major, minor))
}

/// What the collection is. One choice, so a capped time series or a capped clustered collection cannot be asked for.
#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub enum Kind {
    #[default]
    Plain,
    Capped,
    TimeSeries,
    Clustered,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Plain, Kind::Capped, Kind::TimeSeries, Kind::Clustered];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Plain => "Standard",
            Kind::Capped => "Capped",
            Kind::TimeSeries => "Time series",
            Kind::Clustered => "Clustered",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Kind::Plain => "An ordinary collection.",
            Kind::Capped => "A fixed size; the oldest documents make room for new ones.",
            Kind::TimeSeries => "Measurements over time, stored in buckets.",
            Kind::Clustered => "Documents stored in _id order, so _id lookups and ranges are cheaper.",
        }
    }

    /// Whether the server can create this kind.
    pub fn supported(self, v: Ver) -> bool {
        match self {
            Kind::Plain | Kind::Capped => true,
            Kind::TimeSeries => at_least(v, 5, 0),
            Kind::Clustered => at_least(v, 5, 3),
        }
    }

    /// Time series collections take neither a validator nor a collation.
    pub fn takes_rules(self) -> bool {
        self != Kind::TimeSeries
    }
}

#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub enum Granularity {
    #[default]
    Seconds,
    Minutes,
    Hours,
}

impl Granularity {
    pub const ALL: [Granularity; 3] = [Granularity::Seconds, Granularity::Minutes, Granularity::Hours];

    pub fn word(self) -> &'static str {
        match self {
            Granularity::Seconds => "seconds",
            Granularity::Minutes => "minutes",
            Granularity::Hours => "hours",
        }
    }
}

/// The options of the New collection dialog, as typed.
#[derive(Clone, PartialEq, Debug)]
pub struct CollForm {
    pub kind: Kind,
    pub capped_size: String,
    pub capped_max: String,
    pub ts_time: String,
    pub ts_meta: String,
    pub ts_gran: Granularity,
    /// Bucket spans in seconds instead of a granularity (6.3 and later).
    pub ts_custom: bool,
    pub ts_span: String,
    pub ts_round: String,
    pub ts_expire: String,
    pub cl_expire: String,
    pub collate: bool,
    pub locale: String,
    pub strength: u8,
    pub validate: bool,
    pub validator: String,
    pub level: String,
    pub action: String,
}

impl Default for CollForm {
    fn default() -> Self {
        CollForm {
            kind: Kind::Plain,
            capped_size: String::new(),
            capped_max: String::new(),
            ts_time: String::new(),
            ts_meta: String::new(),
            ts_gran: Granularity::Seconds,
            ts_custom: false,
            ts_span: String::new(),
            ts_round: String::new(),
            ts_expire: String::new(),
            cl_expire: String::new(),
            collate: false,
            locale: "en".into(),
            strength: 3,
            validate: false,
            validator: "{\n  $jsonSchema: {\n    bsonType: 'object',\n    required: [],\n    properties: {}\n  }\n}".into(),
            level: "strict".into(),
            action: "error".into(),
        }
    }
}

impl CollForm {
    /// Whether every option is still at its default, which the collapsed section says.
    pub fn is_plain(&self) -> bool {
        self.kind == Kind::Plain && !self.collate && !self.validate
    }
}

/// Why a database name is refused, or None. MongoDB bars `/\. "$` and the null character; `*<>:|?` are barred on Windows
/// servers and refused everywhere here so a database made on one host opens on another.
pub fn db_name_problem(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("Name the database.".into());
    }
    if name.len() > 63 {
        return Some("A database name is at most 63 bytes.".into());
    }
    if let Some(c) = name.chars().find(|c| matches!(c, '/' | '\\' | '.' | ' ' | '"' | '$' | '*' | '<' | '>' | ':' | '|' | '?' | '\0')) {
        let what = match c {
            '\0' => "a null character".to_string(),
            ' ' => "a space".to_string(),
            c => format!("\"{c}\""),
        };
        return Some(format!("A database name cannot contain {what}."));
    }
    if matches!(name, "admin" | "local" | "config") {
        return Some(format!("{name} is a reserved database."));
    }
    None
}

/// Why a collection name is refused, or None. `db` only counts toward the 255 byte namespace limit.
pub fn coll_name_problem(name: &str, db: &str) -> Option<String> {
    if name.is_empty() {
        return Some("Name the collection.".into());
    }
    if name.contains('\0') {
        return Some("A collection name cannot contain a null character.".into());
    }
    if name.contains('$') {
        return Some("A collection name cannot contain \"$\".".into());
    }
    if name.starts_with("system.") {
        return Some("Names starting with system. are reserved.".into());
    }
    if name.starts_with('.') || name.ends_with('.') {
        return Some("A collection name cannot start or end with a dot.".into());
    }
    if db.len() + 1 + name.len() > 255 {
        return Some("The namespace (database.collection) is at most 255 bytes.".into());
    }
    None
}

fn whole(text: &str, what: &str, min: i64) -> Result<Option<i64>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    match t.parse::<i64>() {
        Ok(n) if n >= min => Ok(Some(n)),
        Ok(_) => Err(format!("{what} must be at least {min}.")),
        Err(_) => Err(format!("{what} must be a whole number.")),
    }
}

fn need(text: &str, what: &str, min: i64) -> Result<i64, String> {
    whole(text, what, min)?.ok_or_else(|| format!("{what} is required."))
}

/// The validator document: the text as typed, or a bare JSON schema (`bsonType`, `properties`, `required`) wrapped in `$jsonSchema`.
pub fn validator_doc(text: &str) -> Result<Document, String> {
    let d = crate::db::parse_doc(text)?;
    if d.is_empty() {
        return Err("The validator is empty.".into());
    }
    if d.keys().any(|k| matches!(k.as_str(), "bsonType" | "properties" | "required")) && !d.contains_key("$jsonSchema") {
        return Ok(doc! { "$jsonSchema": d });
    }
    Ok(d)
}

/// The `create` command for `name` from the form, checked against the server version.
pub fn create_command(name: &str, f: &CollForm, v: Ver) -> Result<Document, String> {
    let mut c = doc! { "create": name };
    if !f.kind.supported(v) {
        return Err(format!("This server cannot create a {} collection.", f.kind.label().to_lowercase()));
    }
    match f.kind {
        Kind::Plain => {}
        Kind::Capped => {
            c.insert("capped", true);
            c.insert("size", need(&f.capped_size, "The size in bytes", 1)?);
            if let Some(m) = whole(&f.capped_max, "The maximum number of documents", 1)? {
                c.insert("max", m);
            }
        }
        Kind::TimeSeries => {
            let time = f.ts_time.trim();
            if time.is_empty() {
                return Err("The time field is required.".into());
            }
            let mut ts = doc! { "timeField": time };
            let meta = f.ts_meta.trim();
            if !meta.is_empty() {
                ts.insert("metaField", meta);
            }
            if f.ts_custom && at_least(v, 6, 3) {
                let span = need(&f.ts_span, "The bucket span", 1)?;
                let round = need(&f.ts_round, "The bucket rounding", 1)?;
                // The server refuses unequal values, so say so before the round trip.
                if span != round {
                    return Err("The bucket span and the bucket rounding must be equal.".into());
                }
                ts.insert("bucketMaxSpanSeconds", span as i32);
                ts.insert("bucketRoundingSeconds", round as i32);
            } else {
                ts.insert("granularity", f.ts_gran.word());
            }
            c.insert("timeseries", ts);
            if let Some(e) = whole(&f.ts_expire, "Expire after", 0)? {
                c.insert("expireAfterSeconds", e);
            }
        }
        Kind::Clustered => {
            c.insert("clusteredIndex", doc! { "key": { "_id": 1 }, "unique": true });
            if let Some(e) = whole(&f.cl_expire, "Expire after", 0)? {
                c.insert("expireAfterSeconds", e);
            }
        }
    }
    if f.kind.takes_rules() {
        if f.collate {
            let locale = f.locale.trim();
            if locale.is_empty() {
                return Err("The collation locale is required.".into());
            }
            if !(1..=5).contains(&f.strength) {
                return Err("The collation strength is 1 to 5.".into());
            }
            c.insert("collation", doc! { "locale": locale, "strength": f.strength as i32 });
        }
        if f.validate {
            c.insert("validator", Bson::Document(validator_doc(&f.validator)?));
            c.insert("validationLevel", f.level.as_str());
            c.insert("validationAction", f.action.as_str());
        }
    }
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(f: impl FnOnce(&mut CollForm)) -> CollForm {
        let mut x = CollForm::default();
        f(&mut x);
        x
    }

    #[test]
    fn a_plain_form_sends_only_the_name() {
        assert_eq!(create_command("c", &CollForm::default(), None), Ok(doc! { "create": "c" }));
    }

    #[test]
    fn version_parsing() {
        assert_eq!(parse_ver("7.0.12"), Some((7, 0)));
        assert_eq!(parse_ver("8.0.0-rc1"), Some((8, 0)));
        assert_eq!(parse_ver(""), None);
        assert_eq!(parse_ver("x.y"), None);
    }

    #[test]
    fn capped_needs_a_size_and_takes_a_max() {
        let f = form(|f| f.kind = Kind::Capped);
        assert!(create_command("c", &f, None).unwrap_err().contains("size"));
        let f = form(|f| {
            f.kind = Kind::Capped;
            f.capped_size = "4096".into();
        });
        assert_eq!(create_command("c", &f, None), Ok(doc! { "create": "c", "capped": true, "size": 4096_i64 }));
        let f = form(|f| {
            f.kind = Kind::Capped;
            f.capped_size = "4096".into();
            f.capped_max = "10".into();
        });
        assert_eq!(create_command("c", &f, None), Ok(doc! { "create": "c", "capped": true, "size": 4096_i64, "max": 10_i64 }));
        let f = form(|f| {
            f.kind = Kind::Capped;
            f.capped_size = "-1".into();
        });
        assert!(create_command("c", &f, None).is_err());
        let f = form(|f| {
            f.kind = Kind::Capped;
            f.capped_size = "1.5".into();
        });
        assert!(create_command("c", &f, None).unwrap_err().contains("whole"));
    }

    #[test]
    fn time_series_with_granularity_or_bucket_spans() {
        let f = form(|f| f.kind = Kind::TimeSeries);
        assert!(create_command("c", &f, None).unwrap_err().contains("time field"));
        let f = form(|f| {
            f.kind = Kind::TimeSeries;
            f.ts_time = "ts".into();
            f.ts_meta = "sensor".into();
            f.ts_gran = Granularity::Hours;
            f.ts_expire = "3600".into();
        });
        assert_eq!(create_command("c", &f, Some((7, 0))), Ok(doc! { "create": "c", "timeseries": { "timeField": "ts", "metaField": "sensor", "granularity": "hours" }, "expireAfterSeconds": 3600_i64 }));
        let spans = form(|f| {
            f.kind = Kind::TimeSeries;
            f.ts_time = "ts".into();
            f.ts_custom = true;
            f.ts_span = "86400".into();
            f.ts_round = "86400".into();
        });
        assert_eq!(create_command("c", &spans, Some((7, 0))), Ok(doc! { "create": "c", "timeseries": { "timeField": "ts", "bucketMaxSpanSeconds": 86400, "bucketRoundingSeconds": 86400 } }));
        // Before 6.3 the spans are not sent, the granularity is.
        assert_eq!(create_command("c", &spans, Some((6, 0))).unwrap().get_document("timeseries").unwrap().get_str("granularity"), Ok("seconds"));
        let uneven = CollForm { ts_round: "60".into(), ..spans.clone() };
        assert!(create_command("c", &uneven, Some((7, 0))).unwrap_err().contains("equal"));
        assert!(create_command("c", &f, Some((4, 4))).unwrap_err().contains("cannot create"));
    }

    #[test]
    fn clustered_on_id_is_unique_with_optional_expiry() {
        let f = form(|f| f.kind = Kind::Clustered);
        assert_eq!(create_command("c", &f, Some((6, 0))), Ok(doc! { "create": "c", "clusteredIndex": { "key": { "_id": 1 }, "unique": true } }));
        let f = form(|f| {
            f.kind = Kind::Clustered;
            f.cl_expire = "0".into();
        });
        assert_eq!(create_command("c", &f, None).unwrap().get_i64("expireAfterSeconds"), Ok(0));
        assert!(create_command("c", &f, Some((5, 0))).is_err());
    }

    #[test]
    fn collation_and_validator_ride_along_except_on_time_series() {
        let f = form(|f| {
            f.kind = Kind::Clustered;
            f.collate = true;
            f.locale = "fr".into();
            f.strength = 2;
            f.validate = true;
            f.validator = "{ bsonType: 'object', required: ['a'] }".into();
            f.level = "moderate".into();
            f.action = "warn".into();
        });
        let c = create_command("c", &f, None).unwrap();
        assert_eq!(c.get_document("collation"), Ok(&doc! { "locale": "fr", "strength": 2 }));
        assert_eq!(c.get_document("validator"), Ok(&doc! { "$jsonSchema": { "bsonType": "object", "required": ["a"] } }));
        assert_eq!((c.get_str("validationLevel"), c.get_str("validationAction")), (Ok("moderate"), Ok("warn")));
        let ts = form(|g| {
            g.kind = Kind::TimeSeries;
            g.ts_time = "t".into();
            g.collate = true;
            g.validate = true;
        });
        let c = create_command("c", &ts, None).unwrap();
        assert!(!c.contains_key("collation") && !c.contains_key("validator"));
        let own = form(|f| {
            f.validate = true;
            f.validator = "{ a: { $exists: true } }".into();
        });
        assert_eq!(create_command("c", &own, None).unwrap().get_document("validator"), Ok(&doc! { "a": { "$exists": true } }));
        let bad = form(|f| {
            f.validate = true;
            f.validator = "{ nope".into();
        });
        assert!(create_command("c", &bad, None).is_err());
        let blank = form(|f| {
            f.collate = true;
            f.locale = " ".into();
        });
        assert!(create_command("c", &blank, None).is_err());
    }

    #[test]
    fn database_names() {
        assert_eq!(db_name_problem("shop_2"), None);
        assert_eq!(db_name_problem("a-b"), None);
        for bad in ["", "a b", "a.b", "a/b", "a\\b", "a\"b", "a$b", "a*b", "a<b", "a>b", "a:b", "a|b", "a?b", "a\0b", "admin", "local", "config"] {
            assert!(db_name_problem(bad).is_some(), "{bad:?}");
        }
        assert_eq!(db_name_problem(&"x".repeat(63)), None);
        assert!(db_name_problem(&"x".repeat(64)).is_some());
    }

    #[test]
    fn collection_names() {
        assert_eq!(coll_name_problem("orders.2024", "shop"), None);
        for bad in ["", "a$b", "a\0b", "system.users", ".a", "a."] {
            assert!(coll_name_problem(bad, "shop").is_some(), "{bad:?}");
        }
        assert!(coll_name_problem(&"x".repeat(252), "shop").is_some());
        assert_eq!(coll_name_problem(&"x".repeat(250), "shop"), None);
    }
}
