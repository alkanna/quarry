//! The memory readout's tooltip: estimates by holder, beside egui's textures and what the system reports.

use super::*;

/// Guessed bytes of one decoded document of an Import or Compare preview, which keep no raw bytes to measure.
const DOC_GUESS: usize = 1024;
/// Guessed bytes of one per-field row of a Compare result: an id, a field name and two values.
const DIFF_GUESS: usize = 100;
/// Guessed bytes of one sample value kept for Query suggestions.
const VALUE_GUESS: usize = 32;

impl Tab {
    /// About the bytes of the rows and texts this tab holds.
    pub(super) fn mem(&self) -> usize {
        let rows = |v: &[db::Row]| v.iter().map(db::Row::mem).sum::<usize>();
        let sql = self.sql.as_ref().map_or(0, |s| s.sets.iter().map(|r| rows(&r.rows)).sum());
        let logs: usize = self.shell.logs.iter().map(|(a, b)| a.len() + b.len()).sum();
        let texts = self.shell.raw_text.as_ref().map_or(0, String::len) + self.import.text.len();
        let parsed = self.import.parsed.as_ref().and_then(|p| p.as_ref().ok()).map_or(0, Vec::len) * DOC_GUESS;
        let cmp = self.compare.result.as_ref().map_or(0, |c| (c.only_src.len() + c.only_dst.len() + 2 * c.changed.len()) * DOC_GUESS + c.diffs.len() * DIFF_GUESS);
        rows(&self.docs) + rows(&self.agg_docs) + rows(&self.shell.docs) + rows(&self.watch.rows) + sql + logs + texts + parsed + cmp
    }

    /// About the bytes of what this tab keeps besides its rows, by line. A closed tab's share goes to its reopen line.
    fn kept(&self) -> [(&'static str, usize); 4] {
        let strs = |m: &HashMap<String, String>| m.iter().map(|(a, b)| a.len() + b.len()).sum::<usize>();
        let samples = self.stages.iter().chain([&self.agg_input]).flat_map(|s| &s.sample).map(decoded).sum();
        let schema = self.schema.stats.iter().map(|f| size_of::<db::FieldStat>() + f.path.len() + f.examples.iter().map(String::len).sum::<usize>()).sum();
        let indexes = self.indexes.iter().chain(&self.idx_rows).map(decoded).sum::<usize>() + strs(&self.indexed);
        let hints = self.paths.iter().map(String::len).sum::<usize>() + self.field_info.iter().map(|(k, f)| k.len() + f.values.len() * VALUE_GUESS).sum::<usize>() + strs(&self.related);
        [("Pipeline samples", samples), ("Schema analysis", schema), ("Indexes", indexes), ("Suggestions and previews", hints)]
    }
}

fn decoded(d: &Document) -> usize {
    db::doc_bytes(d) as usize * db::DECODED
}

/// The tooltip's lines and the notes under them.
type MemLines = (Vec<(&'static str, usize)>, String);

impl App {
    /// Where the memory goes, by what holds it. Empty lines are left out.
    pub(super) fn mem_parts(&self) -> Vec<(&'static str, usize)> {
        let sizes: Vec<usize> = self.tabs.iter().map(Tab::mem).collect();
        let cur = sizes.get(self.cur).copied().unwrap_or(0);
        let open: usize = sizes.iter().sum();
        let log: usize = db::log().iter().map(db::LogEntry::mem).sum();
        let kept = self.tabs.iter().map(Tab::kept).reduce(|mut a, b| {
            a.iter_mut().zip(b).for_each(|(x, y)| x.1 += y.1);
            a
        });
        let closed = self.closed.iter().map(|t| t.mem() + t.kept().iter().map(|k| k.1).sum::<usize>()).sum();
        // Sizing an entry encodes its documents, so each is measured once, the first time the tooltip asks, rather than
        // on every write while the readout is off. Entries gone from the list are forgotten.
        let mut undo_sizes = self.undo_bytes.borrow_mut();
        undo_sizes.retain(|at, _| self.undo.iter().any(|a| a.at == *at));
        // Agent undo is a separate store that counts the encoded bytes of its decoded documents.
        let undo = self.undo.iter().map(|a| *undo_sizes.entry(a.at).or_insert_with(|| a.held())).sum::<usize>() + self.agents.undo.total() as usize * db::DECODED;
        let parts = [("Current tab", cur), ("Other open tabs", open - cur), ("Closed tabs kept to reopen", closed), ("Query log", log)];
        parts.into_iter().chain(kept.into_iter().flatten()).chain([("Undo history", undo)]).filter(|p| p.1 > 0).collect()
    }

    /// The tooltip's lines, the estimates by holder and then what the system tells about the rest of the total, with
    /// the notes that explain them. None until the first system reading arrives from its thread.
    fn mem_lines(&self, ctx: &egui::Context, total: Option<u64>) -> Option<MemLines> {
        let mut parts = self.mem_parts();
        let known: usize = parts.iter().map(|p| p.1).sum();
        let sys = crate::sysmem::latest({ let ctx = ctx.clone(); move || ctx.request_repaint() })?;
        // Textures get a line only where the system's window figure holds them (macOS); elsewhere they sit in the heap,
        // the GPU mappings or graphics memory outside the total.
        let tex = sys.window_has_textures.then(|| ctx.tex_manager().read().allocated().map(|(_, m)| m.bytes_used()).sum::<usize>());
        // Some figures count bytes asked for rather than resident, so each line takes at most what the total has left.
        let mut room = total.map_or(usize::MAX, |t| t as usize);
        let mut take = |n: usize| {
            let n = n.min(room);
            room -= n;
            n
        };
        // Figures measured from resident pages go first; the heap figures can be committed bytes that are not resident.
        let buffers = sys.window.map(|w| take(w.saturating_sub(tex.unwrap_or(0))));
        let tex = tex.map(&mut take);
        let lib_data = sys.lib_data.map(&mut take);
        let code = sys.code.map(&mut take);
        let heap = take(sys.malloc.unwrap_or(0).max(known));
        // Estimates can pass the heap, and a zero line would read as exact, so it is left out.
        let other = sys.malloc.map(|_| heap.saturating_sub(known));
        let reuse = sys.reuse.map(&mut take);
        let rest = total.map(|_| room);
        let lines = [("Other heap data", other), ("Freed heap kept for reuse", reuse), ("Fonts and images", tex), ("Window and GPU buffers", buffers), ("Libraries' written data", lib_data), ("Code and mapped files", code), ("Everything else", rest)];
        parts.extend(lines.into_iter().filter_map(|(what, n)| Some((what, n?))).filter(|p| p.1 > 0));
        let notes = [
            other.filter(|&n| n > 0).map(|_| "Other heap data is memory Quarry and the libraries it loads hold that the lines above do not name."),
            rest.filter(|&n| n > 0).map(|_| "Everything else is thread stacks, driver memory and other memory the system does not break down here."),
        ];
        Some((parts, notes.into_iter().flatten().collect::<Vec<_>>().join(" ")))
    }

    pub(super) fn mem_tip(&self, ui: &mut egui::Ui, total: Option<u64>) {
        ui.label("Memory Quarry uses now, updated every second.");
        ui.add_space(6.0);
        // The lines are worked out once a second like the total, from one snapshot, and only while this tooltip is open.
        let (id, now) = (egui::Id::unique("mem-parts"), ui.input(|i| i.time));
        let cached = ui.data(|d| d.get_temp::<(f64, MemLines)>(id));
        let (parts, notes) = match cached {
            Some((at, p)) if now - at < 1.0 => p,
            _ => match self.mem_lines(ui.ctx(), total) {
                Some(p) => {
                    ui.data_mut(|d| d.insert_temp(id, (now, p.clone())));
                    p
                }
                None => cached.map(|c| c.1).unwrap_or_default(),
            },
        };
        egui::Grid::new("mem-tip").num_columns(2).spacing([16.0, 2.0]).show(ui, |ui| {
            for (what, n) in parts {
                ui.label(RichText::new(what).size(TXT_CAPTION).color(MUTED()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(RichText::new(format!("~{}", human_bytes(n))).font(FontId::monospace(TXT_CAPTION))));
                ui.end_row();
            }
        });
        if !notes.is_empty() {
            ui.add_space(4.0);
            ui.label(RichText::new(notes).size(TXT_CAPTION).color(FAINT()));
        }
    }
}
