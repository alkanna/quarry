//! State behind src/app/sql/filter_panel.rs, which draws it.

use crate::db::sql::facts::{FilterCount, FilterErr};
use std::collections::HashMap;

pub type Counted = Result<FilterCount, FilterErr>;

/// One count the panel shows: the text last asked about under which request, and the answer with the text it is for.
#[derive(Default, Clone)]
pub struct Count {
    pub req: u64,
    pub asked: Option<String>,
    /// The text seen last and since when, for the pause before asking.
    pub seen: String,
    pub since: f64,
    pub res: Option<(String, Counted)>,
}

impl Count {
    /// Whether to ask about `text` now: once it has rested PAUSE seconds and was not asked yet.
    pub fn tick(&mut self, text: &str, now: f64) -> Tick {
        if text != self.seen {
            self.seen = text.to_string();
            self.since = now;
        }
        if self.asked.as_deref() == Some(text) {
            return Tick::Idle;
        }
        let left = PAUSE - (now - self.since);
        if left > 0.0 { Tick::Wait(left as f32) } else { Tick::Ask }
    }

    pub fn ask(&mut self, req: u64, text: &str) {
        self.req = req;
        self.asked = Some(text.to_string());
    }

    /// Keeps the answer to request `req` when it is this slot's latest, and says whether it was.
    pub fn answer(&mut self, req: u64, res: &Counted) -> bool {
        match &self.asked {
            Some(text) if req == self.req => {
                self.res = Some((text.clone(), res.clone()));
                true
            }
            _ => false,
        }
    }

    /// The answer for `text`, never one for an older text.
    pub fn shown(&self, text: &str) -> Option<&Counted> {
        self.res.as_ref().filter(|(t, _)| t == text).map(|(_, r)| r)
    }
}

#[derive(Default, Clone)]
pub struct Panel {
    pub open: bool,
    /// The selected row or group.
    pub sel: Option<u32>,
    /// A row just added, whose column picker opens on its first frame.
    pub pick: Option<u32>,
    /// A row whose operator menu opens on its first frame; only the demo of the menu sets it.
    pub ops: Option<u32>,
    /// How many rows the whole WHERE matches.
    pub count: Count,
    /// The row under the pointer, whose own count shows at its end.
    pub hover: Option<u32>,
    pub hover_count: Count,
    pub hover_cache: HashMap<String, Counted>,
    /// Where the counts are from (connection, database, schema and table), since a tab can move.
    pub place: (u64, String, String, String),
}

impl Panel {
    /// Keeps the answer to request `req` in the slot that asked. False when neither did.
    pub fn counted(&mut self, req: u64, res: &Counted) -> bool {
        if self.count.answer(req, res) {
            return true;
        }
        match self.hover_count.asked.clone().filter(|_| self.hover_count.req == req) {
            Some(text) => {
                if self.hover_cache.len() >= HOVER_CACHE {
                    self.hover_cache.clear();
                }
                self.hover_cache.insert(text, res.clone());
                true
            }
            None => false,
        }
    }
}

impl Panel {
    pub fn forget_counts(&mut self) {
        (self.count, self.hover_count) = (Count::default(), Count::default());
        self.hover_cache.clear();
    }
}

/// The pause after the last edit before a count is asked again, as long as the Query editor's live check waits.
pub const PAUSE: f64 = 0.6;

/// Counts of single rows kept while the panel is used, so hovering back and forth asks once per row.
pub const HOVER_CACHE: usize = 64;

/// What a count slot should do this frame.
pub enum Tick {
    Ask,
    /// Look again after this many seconds.
    Wait(f32),
    Idle,
}
