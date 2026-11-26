//! The memory readout's tooltip: estimates by holder, beside egui's textures and what the system reports.

use super::*;

/// The tooltip's lines and the notes under them.
type MemLines = (Vec<(&'static str, usize)>, String);

impl App {
    /// Where the memory goes, by what holds it. Empty lines are left out.
    pub(super) fn mem_parts(&self) -> Vec<(&'static str, usize)> {
        let sizes: Vec<usize> = self.core.tabs.iter().map(Tab::mem).collect();
        let cur = sizes.get(self.core.cur).copied().unwrap_or(0);
        let open: usize = sizes.iter().sum();
        let log: usize = db::log().iter().map(db::LogEntry::mem).sum();
        let kept = self.core.tabs.iter().map(Tab::kept).reduce(|mut a, b| {
            a.iter_mut().zip(b).for_each(|(x, y)| x.1 += y.1);
            a
        });
        let closed = self.core.closed.iter().map(|t| t.mem() + t.kept().iter().map(|k| k.1).sum::<usize>()).sum();
        // Sizing an entry encodes its documents, so each is measured once, the first time the tooltip asks, rather than
        // on every write while the readout is off. Entries gone from the list are forgotten.
        let mut undo_sizes = self.core.undo_bytes.borrow_mut();
        undo_sizes.retain(|at, _| self.core.undo.iter().any(|a| a.at == *at));
        // Agent undo is a separate store that counts the encoded bytes of its decoded documents.
        let undo = self.core.undo.iter().map(|a| *undo_sizes.entry(a.at).or_insert_with(|| a.held())).sum::<usize>() + self.core.agents.undo.total() as usize * db::DECODED;
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
