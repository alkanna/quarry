//! The Import view (pasted or opened JSON, JSON Lines or CSV, inserted or upserted) and the Documents menu's Export.
//! The file pickers stay in the app, which sends the path it got.

use super::super::{Dialog, Refusal, RefusalKind, Reply, UiFx};
use crate::core::Core;
use crate::db;
use crate::state::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ImportA {
    /// The format, the CSV options and Insert or Upsert, as the toolbar shows them after a change.
    Opts { tab: u64, kind: db::ImportKind, separator: Option<char>, header: bool, camel: bool, upsert: bool },
    /// Open file: the file picked, read here.
    File { tab: u64, path: PathBuf },
    /// Import or Upsert: asks first.
    Start { tab: u64 },
    /// The Upsert by field's text.
    UpsertBy { tab: u64, field: String },
    /// The insert confirmation's Import.
    Insert { tab: u64 },
    /// The Documents menu's Export of the query's documents, to the file picked; its extension picks JSON, CSV or TSV.
    Export { tab: u64, path: PathBuf },
}

impl Core {
    pub(super) fn import(&mut self, a: ImportA) -> Result<Reply, Refusal> {
        let tab = match &a {
            ImportA::Opts { tab, .. } | ImportA::UpsertBy { tab, .. } | ImportA::File { tab, .. } | ImportA::Start { tab } | ImportA::Insert { tab } | ImportA::Export { tab, .. } => *tab,
        };
        self.cur_tab(tab)?;
        match a {
            ImportA::Opts { kind, separator, header, camel, upsert, .. } => {
                let im = &mut self.tabs[self.cur].import;
                (im.kind, im.separator, im.header, im.camel, im.upsert) = (kind, separator, header, camel, upsert);
                self.import_parse();
            }
            ImportA::UpsertBy { field, .. } => self.tabs[self.cur].import.upsert_by = field,
            ImportA::File { path, .. } => {
                match std::fs::read_to_string(&path) {
                    Ok(text) => {
                        let im = &mut self.tabs[self.cur].import;
                        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
                        im.kind = match ext.as_str() {
                            "csv" | "tsv" => db::ImportKind::Csv,
                            "jsonl" | "ndjson" => db::ImportKind::JsonLines,
                            _ if text.trim_start().starts_with('{') && text.lines().filter(|l| !l.trim().is_empty()).count() > 1 && db::parse_doc(text.lines().next().unwrap_or_default()).is_ok() => db::ImportKind::JsonLines,
                            _ => db::ImportKind::Json,
                        };
                        if ext == "tsv" {
                            im.separator = Some('\t');
                        }
                        im.file = path.file_name().map(|f| f.to_string_lossy().to_string());
                        im.text = text;
                    }
                    Err(e) => self.err(format!("Could not read the file: {e}")),
                }
                self.import_parse();
            }
            ImportA::Start { .. } => {
                self.import_parse();
                let t = &self.tabs[self.cur];
                let n = match &t.import.parsed {
                    Some(Ok(d)) if !d.is_empty() => d.len(),
                    _ => return Err(Self::mongo_quiet()),
                };
                if !self.sel_writable() {
                    // Unlock with nothing to run after it, as the view always asked.
                    self.ui_fx.push(UiFx::Open(Dialog::Unlock(t.sel.conn)));
                    return Ok(Reply::Done);
                }
                if t.import.upsert {
                    let key = t.import.upsert_by.trim().to_string();
                    let coll = t.sel.coll.clone();
                    let title = format!("Upsert {n} documents into {coll} by {key}? Matching documents are updated with the imported fields.");
                    let action = self.mongo_pinned(Danger::Upsert { key });
                    self.ui_fx.push(UiFx::Open(Dialog::Typed { title, expect: coll, action }));
                } else {
                    self.ui_fx.push(UiFx::Open(Dialog::ImportInsert(n)));
                }
            }
            ImportA::Insert { .. } => {
                if let Some((conn, target)) = self.writable_target() && let Some(Ok(docs)) = self.tabs[self.cur].import.parsed.clone() {
                    self.worker.import(conn, target, docs, None);
                    self.ok("Importing…");
                }
            }
            ImportA::Export { path, .. } => {
                let Some(target) = self.target() else {
                    self.warn_offline();
                    return Err(Refusal { kind: RefusalKind::Offline, msg: self.last_warning() });
                };
                self.ok(format!("Exporting to {}…", path.display()));
                let q = self.tabs[self.cur].query.clone();
                self.worker.export(target, q, path);
            }
        }
        Ok(Reply::Done)
    }

    /// Parses the current tab's import input again when it or its options changed since the last parse. Run each
    /// frame the view shows and before Import, so typing in the box is picked up.
    pub fn import_tick(&mut self) {
        if self.t().is_some_and(|t| t.view == crate::state::View::Import) {
            self.import_parse();
        }
    }

    pub fn import_parse(&mut self) {
        let Some(t) = self.tabs.get_mut(self.cur) else { return };
        let im = &mut t.import;
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (&im.text, im.kind as u8, im.separator, im.header, im.camel).hash(&mut h);
            h.finish()
        };
        if key != im.parsed_key {
            im.parsed_key = key;
            im.parsed = (!im.text.trim().is_empty()).then(|| db::parse_import(&im.text, im.kind, &db::CsvOptions { separator: im.separator, header: im.header, camel_case: im.camel }));
            im.fields = match &im.parsed {
                Some(Ok(d)) => auto_fields(&d[..d.len().min(500)]),
                _ => Vec::new(),
            };
            t.selected = None;
        }
    }
}
