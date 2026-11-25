//! State behind src/app/indexes.rs, which draws it.

use crate::db;
use mongodb::bson::{Bson, Document, doc};

/// What the New index dialog holds.
#[derive(Default)]
pub struct IndexForm {
    pub keys: String,
    pub name: String,
    pub unique: bool,
    pub sparse: bool,
    pub hidden: bool,
    pub ttl: String,
    pub partial: String,
    pub locale: String,
    pub strength: i32,
}

impl IndexForm {
    /// The index entry of a createIndexes command, or what is wrong with the form.
    pub fn spec(&self) -> Result<Document, String> {
        let keys = db::parse_doc(&self.keys).map_err(|e| format!("Keys: {e}"))?;
        if keys.is_empty() {
            return Err("Give at least one key, like { email: 1 }.".into());
        }
        // The server refuses these combinations; saying so here keeps the form open to fix them.
        if self.sparse && !self.partial.trim().is_empty() {
            return Err("Sparse and a partial filter cannot be combined; a partial filter like { field: { $exists: true } } does what sparse does.".into());
        }
        let single = keys.len() == 1;
        let name = if self.name.trim().is_empty() { default_index_name(&keys) } else { self.name.trim().to_string() };
        let mut s = doc! { "key": keys, "name": name };
        for (on, k) in [(self.unique, "unique"), (self.sparse, "sparse"), (self.hidden, "hidden")] {
            if on {
                s.insert(k, true);
            }
        }
        if !self.ttl.trim().is_empty() {
            let n: i64 = self.ttl.trim().parse().ok().filter(|n| *n >= 0).ok_or("Expire after: give a whole number of seconds.")?;
            if !single {
                return Err("Expire after works only on an index with a single key, a date field.".into());
            }
            s.insert("expireAfterSeconds", n);
        }
        if !self.partial.trim().is_empty() {
            s.insert("partialFilterExpression", db::parse_doc(&self.partial).map_err(|e| format!("Partial filter: {e}"))?);
        }
        if !self.locale.trim().is_empty() {
            s.insert("collation", doc! { "locale": self.locale.trim(), "strength": self.strength.clamp(1, 5) });
        }
        Ok(s)
    }
}

/// Columns of the Indexes table.
pub static IDX_FIELDS: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| ["name", "keys", "options", "status", "ops", "since", "size"].map(String::from).to_vec());

/// The name the server would give an index with these keys, like `email_1_createdAt_-1`.
pub fn default_index_name(keys: &Document) -> String {
    let part = |v: &Bson| match v {
        Bson::String(s) => s.clone(),
        Bson::Double(f) => f.to_string(),
        other => db::compact(other),
    };
    keys.iter().map(|(k, v)| format!("{k}_{}", part(v))).collect::<Vec<_>>().join("_")
}
