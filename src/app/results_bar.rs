//! The line above a table of rows or a list of documents: where the page is, how long it took, the pager,
//! and the tools on the right. One drawing for SQL tables and Mongo collections.

use super::*;
use egui::containers::menu::MenuButton;

/// What the left side of the bar says. `total` is the number of rows, documents or groups the query matches.
pub(super) struct BarInfo {
    pub page: u64,
    pub size: u64,
    pub total: u64,
    pub unit: &'static str,
    /// "~" when the total is an estimate.
    pub approx: &'static str,
    pub ms: u64,
    pub loading: bool,
    /// Plain facts after the time, such as a collection's size.
    pub stats: Vec<String>,
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
pub(super) fn range_text(page: u64, size: u64, total: u64, unit: &str, approx: &str) -> String {
    if total == 0 {
        return format!("0 {unit}");
    }
    let first = page.saturating_mul(size).saturating_add(1).min(total);
    let last = first.saturating_add(size.max(1) - 1).min(total);
    let unit = if total == 1 { unit.strip_suffix('s').unwrap_or(unit) } else { unit };
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

impl App {
    /// Draws the bar. `right` adds the tools right to left, so its first item is the rightmost.
    pub(super) fn results_bar(&mut self, ui: &mut egui::Ui, info: &BarInfo, right: impl FnOnce(&mut App, &mut egui::Ui)) -> BarOut {
        let mut out = BarOut::default();
        let pages = info.total.div_ceil(info.size.max(1)).max(1);
        let mono = |s: String, c: Color32| RichText::new(s).font(FontId::monospace(TXT_SMALL)).color(c);
        ui.horizontal(|ui| {
            // The count is also the page-size control, so the bar has one control fewer.
            let mut size = self.page_size();
            let text = semibold(mono(range_text(info.page, info.size, info.total, info.unit, info.approx), BRIGHT()));
            let (r, _) = ui
                .scope(|ui| {
                    quiet_look(ui);
                    MenuButton::from_button(egui::Button::new(text).min_size(egui::vec2(0.0, BUTTON_H))).ui(ui, |ui| {
                        menu_look(ui);
                        for n in [db::PAGE_SIZE, 100, 200, db::PAGE_SIZE_MAX] {
                            if ui.selectable_label(size == n, format!("{n} per page")).clicked() {
                                size = n;
                                ui.close();
                            }
                        }
                    })
                })
                .inner;
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Page size"));
            let (rc, _) = ui.allocate_exact_size(egui::vec2(10.0, BUTTON_H), Sense::hover());
            super::fold::chevron(ui.painter(), rc.center(), 0.0, MUTED());
            ui.label(mono(format!("{} ms", info.ms), SUCCESS()));
            for s in &info.stats {
                ui.label(mono(s.clone(), FAINT()));
            }
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
        assert_eq!(range_text(0, 50, 4410, "rows", ""), "1-50 of 4,410 rows");
    }

    #[test]
    fn the_last_page_stops_at_the_total() {
        assert_eq!(range_text(88, 50, 4410, "rows", ""), "4401-4410 of 4,410 rows");
        assert_eq!(range_text(1, 50, 60, "docs", ""), "51-60 of 60 docs");
    }

    #[test]
    fn a_single_row_and_an_empty_result_have_no_range() {
        assert_eq!(range_text(0, 50, 1, "rows", ""), "1 of 1 row");
        assert_eq!(range_text(0, 50, 0, "rows", ""), "0 rows");
    }

    #[test]
    fn an_estimate_marks_the_total() {
        assert_eq!(range_text(0, 50, 1_200_000, "rows", "~"), "1-50 of ~1,200,000 rows");
    }

    #[test]
    fn the_grouped_note_counts_source_rows() {
        assert_eq!(grouped_text(1234), "from 1,234 rows, read-only");
        assert_eq!(range_text(0, 50, 3, "groups", ""), "1-3 of 3 groups");
    }
}
