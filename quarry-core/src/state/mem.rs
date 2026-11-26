//! State behind src/app/mem.rs, which draws it.

use crate::db;
use crate::state::Tab;
use mongodb::bson::Document;
use std::collections::HashMap;

impl Tab {
    /// About the bytes of the rows and texts this tab holds.
    pub fn mem(&self) -> usize {
        let rows = |v: &[db::Row]| v.iter().map(db::Row::mem).sum::<usize>();
        let sql = self.sql.as_ref().map_or(0, |s| s.sets.iter().map(|r| rows(&r.rows)).sum());
        let logs: usize = self.shell.logs.iter().map(|(a, b)| a.len() + b.len()).sum();
        let texts = self.shell.raw_text.as_ref().map_or(0, String::len) + self.import.text.len();
        let parsed = self.import.parsed.as_ref().and_then(|p| p.as_ref().ok()).map_or(0, Vec::len) * DOC_GUESS;
        let cmp = self.compare.result.as_ref().map_or(0, |c| (c.only_src.len() + c.only_dst.len() + 2 * c.changed.len()) * DOC_GUESS + c.diffs.len() * DIFF_GUESS);
        rows(&self.docs) + rows(&self.agg_docs) + rows(&self.shell.docs) + rows(&self.watch.rows) + sql + logs + texts + parsed + cmp
    }

    /// About the bytes of what this tab keeps besides its rows, by line. A closed tab's share goes to its reopen line.
    pub fn kept(&self) -> [(&'static str, usize); 4] {
        let strs = |m: &HashMap<String, String>| m.iter().map(|(a, b)| a.len() + b.len()).sum::<usize>();
        let samples = self.stages.iter().chain([&self.agg_input]).flat_map(|s| &s.sample).map(decoded).sum();
        let schema = self.schema.stats.iter().map(|f| size_of::<db::FieldStat>() + f.path.len() + f.examples.iter().map(String::len).sum::<usize>()).sum();
        let indexes = self.indexes.iter().chain(&self.idx_rows).map(decoded).sum::<usize>() + strs(&self.indexed);
        let hints = self.paths.iter().map(String::len).sum::<usize>() + self.field_info.iter().map(|(k, f)| k.len() + f.values.len() * VALUE_GUESS).sum::<usize>() + strs(&self.related);
        [("Pipeline samples", samples), ("Schema analysis", schema), ("Indexes", indexes), ("Suggestions and previews", hints)]
    }
}

/// Guessed bytes of one decoded document of an Import or Compare preview, which keep no raw bytes to measure.
pub const DOC_GUESS: usize = 1024;

/// Guessed bytes of one per-field row of a Compare result: an id, a field name and two values.
pub const DIFF_GUESS: usize = 100;

/// Guessed bytes of one sample value kept for Query suggestions.
pub const VALUE_GUESS: usize = 32;

pub fn decoded(d: &Document) -> usize {
    db::doc_bytes(d) as usize * db::DECODED
}
