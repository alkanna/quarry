//! The memory readout's tooltip: where the memory goes, estimated from the rows and texts each part of the app keeps.

use super::*;

/// Guessed bytes of one decoded document of an Import or Compare preview, which keep no raw bytes to measure.
const DOC_GUESS: usize = 1024;
/// Guessed bytes of one per-field row of a Compare result: an id, a field name and two values.
const DIFF_GUESS: usize = 100;

impl Tab {
    /// About the bytes of the rows and texts this tab holds.
    // ponytail: counts the row holders only; pipeline samples, indexes and schema analysis go to "Everything else".
    pub(super) fn mem(&self) -> usize {
        let rows = |v: &[db::Row]| v.iter().map(db::Row::mem).sum::<usize>();
        let sql = self.sql.as_ref().map_or(0, |s| s.sets.iter().map(|r| rows(&r.rows)).sum());
        let logs: usize = self.shell.logs.iter().map(|(a, b)| a.len() + b.len()).sum();
        let texts = self.shell.raw_text.as_ref().map_or(0, String::len) + self.import.text.len();
        let parsed = self.import.parsed.as_ref().and_then(|p| p.as_ref().ok()).map_or(0, Vec::len) * DOC_GUESS;
        let cmp = self.compare.result.as_ref().map_or(0, |c| (c.only_src.len() + c.only_dst.len() + 2 * c.changed.len()) * DOC_GUESS + c.diffs.len() * DIFF_GUESS);
        rows(&self.docs) + rows(&self.agg_docs) + rows(&self.shell.docs) + rows(&self.watch.rows) + sql + logs + texts + parsed + cmp
    }
}

impl App {
    /// Where the memory goes, by what holds it. Empty lines are left out.
    // ponytail: undo history and suggestion caches are left out as small; add a line if one grows.
    pub(super) fn mem_parts(&self) -> Vec<(&'static str, usize)> {
        let sizes: Vec<usize> = self.tabs.iter().map(Tab::mem).collect();
        let cur = sizes.get(self.cur).copied().unwrap_or(0);
        let open: usize = sizes.iter().sum();
        let log: usize = db::log().iter().map(db::LogEntry::mem).sum();
        let parts = [("Current tab", cur), ("Other open tabs", open - cur), ("Closed tabs kept to reopen", self.closed.iter().map(Tab::mem).sum()), ("Query log", log)];
        parts.into_iter().filter(|p| p.1 > 0).collect()
    }

    pub(super) fn mem_tip(&self, ui: &mut egui::Ui, total: Option<u64>) {
        ui.label("Memory Quarry uses now, updated every second.");
        ui.add_space(6.0);
        let mut parts = self.mem_parts();
        let known: usize = parts.iter().map(|p| p.1).sum();
        // The lines are estimates of heap bytes and the total is resident memory, so they can pass it; a remainder of
        // zero would then read as exact, so it is left out.
        let rest = total.map(|t| t as usize).filter(|&t| t > known).map(|t| t - known);
        parts.extend(rest.map(|r| ("Everything else", r)));
        egui::Grid::new("mem-tip").num_columns(2).spacing([16.0, 2.0]).show(ui, |ui| {
            for (what, n) in parts {
                ui.label(RichText::new(what).size(TXT_CAPTION).color(MUTED()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(RichText::new(format!("~{}", human_bytes(n))).font(FontId::monospace(TXT_CAPTION))));
                ui.end_row();
            }
        });
        if rest.is_some() {
            ui.add_space(4.0);
            ui.label(RichText::new("Everything else is the app itself, fonts, GPU textures, data not listed above, and memory already freed that the system has not taken back yet.").size(TXT_CAPTION).color(FAINT()));
        }
    }
}
