//! The grid of a Documents, SQL Content or results view: its saved columns (shown, order, pins, widths, alignment,
//! highlights, labels), the header popover's sort and whole-query reads, and a page read again when the page size
//! changed in another tab. src/app/table.rs and column_menu.rs draw them.

use super::{Refusal, Reply};
use crate::core::Core;
use crate::db::{self, ColOp};
use crate::state::config::ColPrefs;
use crate::state::*;
use mongodb::bson::Document;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum GridA {
    /// Column changes of the grid saved under `key` (`col_key`): its results (`agg`) or the Documents rows.
    Cols { key: String, agg: bool, acts: Vec<ColAct> },
    /// Column widths of grid `key`: px, or None for the measured width. A drag sets them unsaved each frame and
    /// saves where it stops.
    Widths { key: String, set: Vec<(String, Option<f32>)>, save: bool },
    /// A header's sort: the bar text it wrote and, for MongoDB, the sort document.
    Sort { tab: u64, text: String, sort: Option<Document> },
    /// The popover's switch between the page and the whole query for data actions.
    ScopeQuery(bool),
    /// A data action of a MongoDB column over every document the query matches, read on the server.
    Values { tab: u64, col: String, op: ColOp },
}

/// Applies column actions to one set of column preferences. `n` is how many columns show before any were picked.
pub fn col_acts_on(p: &mut ColPrefs, n: usize, fields: &[String], acts: Vec<ColAct>) {
    for a in acts {
        let mut shown = visible_cols(Some(p), fields, n);
        match a {
            ColAct::Highlight(c) => {
                let c = db::strip_indices(&c);
                if !p.highlight.contains(&c) {
                    p.highlight.push(c);
                } else {
                    p.highlight.retain(|x| *x != c);
                }
                continue;
            }
            ColAct::Pin(c) => {
                if !p.pinned.contains(&c) {
                    p.pinned.push(c);
                } else {
                    p.pinned.retain(|x| *x != c);
                }
                continue;
            }
            ColAct::Hide(c) => {
                shown.retain(|x| *x != c);
                p.pinned.retain(|x| *x != c);
            }
            ColAct::Move(c, d) => {
                if let Some(i) = shown.iter().position(|x| *x == c) {
                    let j = (i as i32 + d).clamp(0, shown.len() as i32 - 1) as usize;
                    let pinned_n = p.pinned.iter().filter(|x| shown.contains(x)).count();
                    shown.swap(i, j);
                    // Moving across the edge of the pinned block changes which columns are pinned.
                    p.pinned = shown[..pinned_n].to_vec();
                }
            }
            ColAct::MoveTo(c, to) => {
                if let Some(i) = shown.iter().position(|x| *x == c) {
                    let pinned_n = p.pinned.iter().filter(|x| shown.contains(x)).count();
                    let c = shown.remove(i);
                    shown.insert(if to > i { to - 1 } else { to }.min(shown.len()), c);
                    p.pinned = shown[..pinned_n].to_vec();
                }
            }
            ColAct::Show(c) => {
                if !shown.contains(&c) {
                    shown.push(c);
                }
            }
            ColAct::Align(c, a) => {
                if a == 0 {
                    p.align.remove(&c);
                } else {
                    p.align.insert(c, a);
                }
                continue;
            }
            ColAct::Customize { col, label, path } => {
                if path != col {
                    if shown.contains(&path) {
                        shown.retain(|x| *x != col);
                        p.pinned.retain(|x| *x != col);
                    } else {
                        shown.iter_mut().chain(p.pinned.iter_mut()).filter(|x| **x == col).for_each(|x| *x = path.clone());
                    }
                    // A column already at that path keeps its own width and alignment.
                    if let Some(w) = p.widths.remove(&col) {
                        p.widths.entry(path.clone()).or_insert(w);
                    }
                    if let Some(a) = p.align.remove(&col) {
                        p.align.entry(path.clone()).or_insert(a);
                    }
                    p.labels.remove(&col);
                    let (from, to) = (db::strip_indices(&col), db::strip_indices(&path));
                    if p.highlight.contains(&from) {
                        p.highlight.retain(|h| *h != from && *h != to);
                        p.highlight.push(to);
                    }
                }
                if label.trim().is_empty() {
                    p.labels.remove(&path);
                } else {
                    p.labels.insert(path, label.trim().to_string());
                }
            }
            ColAct::Reset => {
                *p = ColPrefs::default();
                continue;
            }
        }
        p.shown = Some(shown);
    }
}

pub fn visible_cols(p: Option<&ColPrefs>, fields: &[String], n: usize) -> Vec<String> {
    let base: Vec<String> = match p.and_then(|p| p.shown.clone()) {
        Some(v) => v,
        None => fields.iter().take(n).cloned().collect(),
    };
    let pinned: Vec<String> = p.map(|p| p.pinned.clone()).unwrap_or_default();
    let mut out: Vec<String> = pinned.iter().filter(|c| base.contains(c)).cloned().collect();
    out.extend(base.into_iter().filter(|c| !pinned.contains(c)));
    out
}

/// How many columns show before the user picks some. SQL tables are narrow enough to show whole,
/// while Mongo documents can have hundreds of fields.
pub fn default_n(sql: bool, setting: usize) -> usize {
    if sql { usize::MAX } else { setting }
}

impl Core {
    pub(super) fn grid(&mut self, a: GridA) -> Result<Reply, Refusal> {
        match a {
            GridA::Cols { key, agg, acts } => {
                let fields = self.t().map(|t| if agg { t.agg_source().1.to_vec() } else { t.fields.clone() }).unwrap_or_default();
                self.apply_col_acts(&key, &fields, acts);
            }
            GridA::Widths { key, set, save } => {
                let widths = &mut self.prefs.cols.entry(key).or_default().widths;
                for (c, w) in set {
                    match w {
                        Some(w) => widths.insert(c, w),
                        None => widths.remove(&c),
                    };
                }
                if save {
                    self.save_prefs();
                }
            }
            GridA::Sort { tab, text, sort } => {
                self.cur_tab(tab)?;
                let t = self.tm().unwrap();
                let sql = t.sql.is_some();
                t.sort_txt = text;
                // Only Mongo keeps an applied sort besides the text; ran_sort reads the text, so it comes after.
                if !sql {
                    t.query.sort = sort;
                    t.ran_sort();
                }
                t.page = 0;
                self.reload();
            }
            GridA::ScopeQuery(on) => self.col_scope_query = on,
            GridA::Values { tab, col, op } => {
                self.cur_tab(tab)?;
                let t = &self.tabs[self.cur];
                let (conn, matched, sort) = (t.sel.conn, t.query.scope(), t.query.sort.clone());
                let Some(target) = self.target() else {
                    self.warn_offline();
                    return Err(Refusal { kind: super::RefusalKind::Offline, msg: self.last_warning() });
                };
                self.ok(format!("Reading {col} from every matching document…"));
                self.worker.column_values(conn, target, op, col, matched, sort);
            }
        }
        Ok(Reply::Done)
    }

    /// How many columns the current tab's grid shows before any were picked.
    pub fn default_cols(&self) -> usize {
        default_n(self.t().is_some_and(|t| t.sql.is_some()), self.prefs.default_cols)
    }

    pub fn apply_col_acts(&mut self, key: &str, fields: &[String], acts: Vec<ColAct>) {
        let n = self.default_cols();
        let p = self.prefs.cols.entry(key.to_string()).or_default();
        col_acts_on(p, n, fields, acts);
        self.save_prefs();
    }

    /// A page read with another page size, set from another tab, no longer lines up with the page numbers: the
    /// current tab goes back to its first page. Run each frame the Documents view shows.
    pub fn follow_page_size(&mut self) {
        let size = self.page_size();
        if let Some(t) = self.tm().filter(|t| t.size != 0 && t.size != size && !t.loading) {
            t.page = 0;
            self.reload();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_shows_every_column_by_default() {
        let fields: Vec<String> = (0..30).map(|i| format!("f{i}")).collect();
        assert_eq!(visible_cols(None, &fields, default_n(true, DEFAULT_COLS)).len(), 30);
        assert_eq!(visible_cols(None, &fields, default_n(false, DEFAULT_COLS)).len(), DEFAULT_COLS);
    }

    #[test]
    fn columns_default_pin_and_move() {
        let fields: Vec<String> = (0..12).map(|i| format!("f{i}")).collect();
        assert_eq!(visible_cols(None, &fields, DEFAULT_COLS).len(), DEFAULT_COLS);
        let p = ColPrefs { pinned: vec!["f3".into()], ..Default::default() };
        assert_eq!(visible_cols(Some(&p), &fields, DEFAULT_COLS)[0], "f3");
        let p = ColPrefs { shown: Some(vec!["a".into(), "b".into()]), pinned: vec!["b".into(), "zz".into()], ..Default::default() };
        assert_eq!(visible_cols(Some(&p), &fields, DEFAULT_COLS), ["b", "a"]);
    }
}
