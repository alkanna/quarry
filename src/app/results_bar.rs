//! The line above a table of rows or a list of documents: where the page is, how long it took, the pager,
//! and the tools on the right. One drawing for SQL tables and Mongo collections.

use super::*;
use egui::containers::menu::MenuButton;

/// What the left side of the bar says. `total` is the number of rows, documents or groups the query matches.
pub(super) struct BarInfo<'a> {
    pub page: u64,
    pub size: u64,
    pub total: u64,
    pub unit: &'static str,
    /// Whether the total is an estimate.
    pub approx: bool,
    /// Whether the total is only a lower bound, because counting ran out of time.
    pub at_least: bool,
    pub ms: u128,
    pub loading: bool,
    /// Plain facts after the time, such as a collection's size.
    pub stats: &'a [String],
    /// Source rows of a grouped SQL answer, which adds the read-only note and Ungroup.
    pub grouped: Option<u64>,
}

/// What the person did in the bar.
#[derive(Default)]
pub(super) struct BarOut {
    pub go: Option<u64>,
    pub ungroup: bool,
}

/// "1-50 of 4,410 rows". A hyphen, not a dash, so the text stays plain in every font. The last page is cut at the
/// total, and an empty result has no range at all.
pub(super) fn range_text(page: u64, size: u64, total: u64, unit: &str, approx: bool) -> String {
    if total == 0 {
        return format!("0 {unit}");
    }
    let first = page.saturating_mul(size).saturating_add(1).min(total);
    let last = first.saturating_add(size.max(1) - 1).min(total);
    let unit = if total == 1 { unit.strip_suffix('s').unwrap_or(unit) } else { unit };
    let approx = if approx { "~" } else { "" };
    if first == last {
        format!("{first} of {approx}{} {unit}", group(&total.to_string()))
    } else {
        format!("{first}-{last} of {approx}{} {unit}", group(&total.to_string()))
    }
}

/// The note that replaces the grouped banner: a grouped answer holds summaries, not table rows.
pub(super) fn grouped_text(rows: u64) -> String {
    format!("from {} rows, read-only", group(&rows.to_string()))
}

/// "14 ms · 412 KB · 6 indexes": the time in green, then the plain facts, one line with a middle dot between them.
fn stats_job(ms: u128, stats: &[String]) -> LayoutJob {
    let mut job = LayoutJob::single_section(format!("{ms} ms"), mono_fmt(TXT_SMALL, SUCCESS(), 400.0));
    for s in stats {
        job.append(&format!(" \u{b7} {s}"), 0.0, mono_fmt(TXT_SMALL, FAINT(), 400.0));
    }
    job
}

impl App {
    /// Draws the bar. `right` adds the tools right to left, so its first item is the rightmost.
    pub(super) fn results_bar(&mut self, ui: &mut egui::Ui, info: &BarInfo<'_>, right: impl FnOnce(&mut App, &mut egui::Ui)) -> BarOut {
        let mut out = BarOut::default();
        let pages = info.total.div_ceil(info.size.max(1)).max(1);
        let mono = |s: String, c: Color32| RichText::new(s).font(FontId::monospace(TXT_SMALL)).color(c);
        ui.horizontal(|ui| {
            // The count is also the page-size control, so the bar has one control fewer.
            let mut size = self.page_size();
            let mut range = range_text(info.page, info.size, info.total, info.unit, info.approx);
            if info.at_least {
                range = range.replacen(" of ", " of at least ", 1);
            }
            let text = semibold(mono(range, BRIGHT()));
            let (r, _) = ui
                .scope(|ui| {
                    quiet_look(ui);
                    // A painted down chevron right after the unit says the count opens a menu; a glyph would sit off center.
                    let down = egui::Atom::paint(egui::vec2(8.0, 16.0), |ui, a| chevron(ui.painter(), a.rect.center(), true, MUTED()));
                    MenuButton::from_button(egui::Button::new((text, down)).min_size(egui::vec2(0.0, BUTTON_H))).ui(ui, |ui| {
                        menu_list(ui);
                        let mut picked = Vec::new();
                        for n in db::PAGE_SIZES {
                            menu_act(ui, &mut picked, n, |ui| ui.selectable_label(size == n, format!("{n} per page")));
                        }
                        size = picked.pop().unwrap_or(size);
                    })
                })
                .inner;
            a11y(&r, egui::Role::Button, "Page size");
            ui.label(stats_job(info.ms, info.stats));
            if let Some(rows) = info.grouped {
                ui.label(RichText::new(grouped_text(rows)).size(TXT_SMALL).color(MUTED())).on_hover_text("Double-click a group to see its rows");
                if quiet(ui, RichText::new("Ungroup").color(PRIMARY())).clicked() {
                    out.ungroup = true;
                }
            }
            if info.loading {
                spin(ui);
            }
            if let Some(p) = pager(ui, info.page as usize, pages as usize) {
                out.go = Some(p as u64);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                right(self, ui);
            });
            // Remembered for every tab; the others reload when shown, see the top of `documents`.
            if size != self.page_size() {
                self.prefs.page_size = size;
                self.save_prefs();
                out.go = Some(0);
            }
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_page_reads_one_to_fifty() {
        assert_eq!(range_text(0, 50, 4410, "rows", false), "1-50 of 4,410 rows");
    }

    #[test]
    fn the_last_page_stops_at_the_total() {
        assert_eq!(range_text(88, 50, 4410, "rows", false), "4401-4410 of 4,410 rows");
        assert_eq!(range_text(1, 50, 60, "docs", false), "51-60 of 60 docs");
    }

    #[test]
    fn a_single_row_and_an_empty_result_have_no_range() {
        assert_eq!(range_text(0, 50, 1, "rows", false), "1 of 1 row");
        assert_eq!(range_text(0, 50, 0, "rows", false), "0 rows");
    }

    #[test]
    fn an_estimate_marks_the_total() {
        assert_eq!(range_text(0, 50, 1_200_000, "rows", true), "1-50 of ~1,200,000 rows");
    }

    #[test]
    fn the_stats_read_with_middle_dots() {
        assert_eq!(stats_job(14, &["412 KB".into(), "6 indexes".into()]).text, "14 ms \u{b7} 412 KB \u{b7} 6 indexes");
        assert_eq!(stats_job(18, &[]).text, "18 ms");
    }

    #[test]
    fn the_grouped_note_counts_source_rows() {
        assert_eq!(grouped_text(1234), "from 1,234 rows, read-only");
        assert_eq!(range_text(0, 50, 3, "groups", false), "1-3 of 3 groups");
    }
}
