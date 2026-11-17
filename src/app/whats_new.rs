//! The release notes shown after an update and from Settings: each release's entries grouped by
//! kind, with the commit scope, inline code in mono and the PR number.

use super::*;
use crate::update::{Entry, Kind, Release};

pub(super) struct WhatsNew {
    pub releases: Vec<Release>,
    /// The version Quarry was updated from, empty when the notes were opened by hand.
    pub from: String,
    /// The versions unfolded when several releases show. The newest starts open.
    pub open: HashSet<String>,
}

impl WhatsNew {
    pub fn new(releases: Vec<Release>, from: &str) -> Self {
        let open = releases.first().map(|r| r.version.clone()).into_iter().collect();
        Self { releases, from: from.to_string(), open }
    }

    fn title(&self) -> String {
        match self.releases.as_slice() {
            [one] => format!("What's new in Quarry {}", one.version),
            _ if !self.from.is_empty() => format!("What's new since Quarry {}", self.from),
            _ => "What's new in Quarry".into(),
        }
    }

    fn sub(&self) -> String {
        match self.releases.as_slice() {
            [one] => {
                let mut parts = Vec::new();
                if !one.date.is_empty() {
                    parts.push(format!("Released {}", nice_date(&one.date)));
                }
                if !self.from.is_empty() {
                    parts.push(format!("updated from {}", self.from));
                }
                parts.join("  ·  ")
            }
            many => {
                let (new, old) = (&many[0], &many[many.len() - 1]);
                format!("{} releases  ·  {} to {}", many.len(), old.version, new.version)
            }
        }
    }
}

/// "17 Nov 2026" from release-please's "2026-11-17".
fn nice_date(iso: &str) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let p: Vec<&str> = iso.split('-').collect();
    match (p.as_slice(), p.get(1).and_then(|m| m.parse::<usize>().ok())) {
        ([y, _, d], Some(m @ 1..=12)) => format!("{} {} {y}", d.trim_start_matches('0'), MONTHS[m - 1]),
        _ => iso.to_string(),
    }
}

fn kind_color(k: Kind) -> Color32 {
    match k {
        Kind::New => PRIMARY(),
        Kind::Fixed => SUCCESS(),
        Kind::Faster => WARNING(),
        Kind::Other => MUTED(),
    }
}

fn kind_label(k: Kind) -> &'static str {
    match k {
        Kind::New => "New",
        Kind::Fixed => "Fixed",
        Kind::Faster => "Faster",
        Kind::Other => "Changes",
    }
}

/// "2 new · 1 fixed", for a folded release.
fn counts(r: &Release) -> String {
    [(Kind::New, "new"), (Kind::Fixed, "fixed"), (Kind::Faster, "faster")]
        .iter()
        .filter_map(|&(k, w)| Some(r.count(k)).filter(|&n| n > 0).map(|n| format!("{n} {w}")))
        .collect::<Vec<_>>()
        .join("  ·  ")
}

/// "4 new features and 6 fixes", for the line over several releases.
fn summary(rs: &[Release]) -> String {
    let n = |k| rs.iter().map(|r| r.count(k)).sum::<usize>();
    let parts: Vec<String> = [(n(Kind::New), "new feature", "new features"), (n(Kind::Fixed), "fix", "fixes"), (n(Kind::Faster), "speedup", "speedups")]
        .into_iter()
        .filter(|(c, ..)| *c > 0)
        .map(|(c, one, many)| format!("{c} {}", if c == 1 { one } else { many }))
        .collect();
    match parts.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Words that read as code: operators, snake_case names and paths. The changelog has no
/// backticks, so this is a guess from the shape of the word.
fn looks_like_code(w: &str) -> bool {
    w.len() > 1 && (w.contains('$') || w.contains('_') || w.contains("::"))
}

/// The entry's text with code-like words in mono, after an optional "scope: " in muted mono.
fn entry_job(scope: Option<&str>, text: &str, width: f32) -> egui::text::LayoutJob {
    let plain = ui_fmt(TXT_BODY, TEXT(), 400.0, 0.0);
    let code = mono_fmt(TXT_SMALL, BRIGHT(), 400.0);
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = width;
    if let Some(sc) = scope {
        job.append(&format!("{sc}: "), 0.0, mono_fmt(TXT_SMALL, MUTED(), 400.0));
    }
    for (i, word) in text.split(' ').enumerate() {
        if i > 0 {
            job.append(" ", 0.0, plain.clone());
        }
        let core = word.trim_start_matches('(').trim_end_matches([',', '.', ';', ':', ')']);
        match word.find(core).filter(|_| looks_like_code(core)) {
            Some(at) => {
                job.append(&word[..at], 0.0, plain.clone());
                job.append(core, 0.0, code.clone());
                job.append(&word[at + core.len()..], 0.0, plain.clone());
            }
            None => job.append(word, 0.0, plain.clone()),
        }
    }
    job
}

/// A small tinted label in front of an entry: New, Fixed or Faster.
fn kind_tag(ui: &mut egui::Ui, k: Kind) {
    let c = kind_color(k);
    let g = ui.painter().layout_job(egui::text::LayoutJob::single_section(kind_label(k).into(), ui_fmt(TXT_SECTION, c, W_SEMIBOLD, 0.0)));
    let (rect, _) = ui.allocate_exact_size(egui::vec2(46.0, 18.0), Sense::hover());
    paint_badge(ui.painter(), rect, g, c, 40);
}

fn scope_chip(ui: &mut egui::Ui, scope: &str) {
    egui::Frame::new().fill(tint(TEXT(), 18)).corner_radius(R_SM).inner_margin(egui::Margin::symmetric(6, 1)).show(ui, |ui| {
        ui.label(RichText::new(scope).monospace().size(TXT_CAPTION).color(MUTED()));
    });
}

/// One entry: an optional kind tag or scope chip, the wrapping text, and the PR number at the right.
fn entry_row(ui: &mut egui::Ui, tag: Option<Kind>, e: &Entry, first: bool) {
    if !first {
        let y = ui.cursor().top();
        ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0, RAISED_LINE()));
    }
    ui.add_space(9.0);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        match (tag, &e.scope) {
            (Some(k), _) => kind_tag(ui, k),
            (None, Some(sc)) => scope_chip(ui, sc),
            (None, None) => {}
        }
        let pr = e.pr.map(|n| format!("#{n}"));
        let pr_w = pr.as_ref().map_or(0.0, |p| ui.painter().layout_no_wrap(p.clone(), egui::FontId::monospace(TXT_CAPTION), FAINT()).size().x + 12.0);
        let w = (ui.available_width() - pr_w).max(80.0);
        // With a kind tag in front, the scope moves into the text.
        let scope = tag.and(e.scope.as_deref());
        ui.allocate_ui(egui::vec2(w, 0.0), |ui| ui.add(egui::Label::new(entry_job(scope, &e.text, w)).wrap()));
        if let Some(p) = pr {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                ui.label(RichText::new(p).monospace().size(TXT_CAPTION).color(FAINT()));
            });
        }
    });
    ui.add_space(9.0);
}

fn entries_card<'a>(ui: &mut egui::Ui, rows: impl Iterator<Item = (Option<Kind>, &'a Entry)>) {
    card_frame().inner_margin(egui::Margin::symmetric(14, 0)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        for (i, (tag, e)) in rows.enumerate() {
            entry_row(ui, tag, e, i == 0);
        }
    });
}

/// A dot in the kind's color, the kind in caps and its count, over its card.
fn kind_caption(ui: &mut egui::Ui, k: Kind, title: &str, n: usize) {
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        let (r, _) = ui.allocate_exact_size(egui::vec2(6.0, 12.0), Sense::hover());
        ui.painter().circle_filled(r.center(), 3.0, kind_color(k));
        let label = if k == Kind::Other && !title.is_empty() { title } else { kind_label(k) };
        ui.label(caption_text(label.to_uppercase()));
        ui.label(RichText::new(n.to_string()).size(TXT_SECTION).color(FAINT()));
    });
    ui.add_space(6.0);
}

/// The head of an open release or the whole row of a folded one, which toggles it.
fn release_head(ui: &mut egui::Ui, r: &Release, open: bool) -> bool {
    let id = egui::Id::unique(("whats-new-release", &r.version));
    let hovered = ui.ctx().read_response(id).is_some_and(|x| x.hovered());
    let frame = if open { egui::Frame::new().inner_margin(egui::Margin { left: 4, right: 4, top: 4, bottom: 4 }).corner_radius(R_MD) } else { card_frame().inner_margin(egui::Margin::symmetric(14, 10)) };
    let fill = match (open, hovered) {
        (true, true) => HOVER(),
        (true, false) => Color32::TRANSPARENT,
        (false, true) => MENU_HOVER(),
        (false, false) => RAISED(),
    };
    let shown = frame.fill(fill).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let (c, _) = ui.allocate_exact_size(egui::vec2(10.0, 14.0), Sense::hover());
            chevron(ui.painter(), c.center(), open, MUTED());
            ui.label(semibold(RichText::new(&r.version).size(TXT_TITLE).color(BRIGHT())));
            if !r.date.is_empty() {
                ui.label(RichText::new(nice_date(&r.date)).size(TXT_SMALL).color(FAINT()));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if r.version == env!("CARGO_PKG_VERSION") {
                    let g = ui.painter().layout_job(egui::text::LayoutJob::single_section("Running now".into(), ui_fmt(TXT_CAPTION, PRIMARY(), W_MEDIUM, 0.0)));
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(g.size().x + 16.0, 20.0), Sense::hover());
                    ui.painter().rect_filled(rect, 10, tint(PRIMARY(), 40));
                    let dy = optical_dy(&g);
                    ui.painter().galley(rect.center() - g.size() / 2.0 + egui::vec2(0.0, dy), g, PRIMARY());
                } else if !open {
                    ui.label(RichText::new(counts(r)).size(TXT_SMALL).color(MUTED()));
                }
            });
        });
    });
    let resp = ui.interact(shown.response.rect, id, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, open, format!("Quarry {}", r.version)));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

impl App {
    pub(super) fn whats_new_dialog(&mut self, ui: &mut egui::Ui, w: &mut WhatsNew) -> bool {
        let mut keep = true;
        ui.set_width(fit_width(ui.ctx(), 640.0));
        dialog_head(ui, &Head::new(w.title()).sub(w.sub()).glyph(glyphs::sparkle));
        dialog_body(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if let [one] = w.releases.as_slice() {
                for (i, s) in one.sections.iter().enumerate() {
                    if i > 0 {
                        ui.add_space(14.0);
                    }
                    kind_caption(ui, s.kind, &s.title, s.entries.len());
                    entries_card(ui, s.entries.iter().map(|e| (None, e)));
                }
                return;
            }
            ui.add(egui::Label::new(RichText::new(format!("{} across {} releases.", summary(&w.releases), w.releases.len())).size(TXT_BODY).color(TEXT())).wrap());
            ui.add_space(12.0);
            for r in &w.releases {
                let open = w.open.contains(&r.version);
                if release_head(ui, r, open) && !w.open.remove(&r.version) {
                    w.open.insert(r.version.clone());
                }
                if open {
                    ui.add_space(6.0);
                    entries_card(ui, r.sections.iter().flat_map(|s| s.entries.iter().map(move |e| (Some(s.kind), e))));
                }
                ui.add_space(10.0);
            }
        });
        dialog_foot(ui, |ui| {
            if primary(ui, "Close", PRIMARY()).on_hover_text("Esc").clicked() {
                keep = false;
            }
            foot_left(ui, |ui| {
                if ui.button("Copy").clicked() {
                    ui.ctx().copy_text(crate::update::notes_text(&w.releases));
                }
                let url = format!("https://github.com/{}/releases", crate::update::REPO);
                if quiet(ui, "All releases").on_hover_text(&url).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                }
            });
        });
        keep
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(v: &str, new: usize, fixed: usize) -> Release {
        let e = || Entry { scope: None, text: "x".into(), pr: None };
        let s = |kind, n| crate::update::Section { kind, title: String::new(), entries: (0..n).map(|_| e()).collect() };
        Release { version: v.into(), date: "2026-11-17".into(), sections: vec![s(Kind::New, new), s(Kind::Fixed, fixed)] }
    }

    #[test]
    fn notes_read_as_sentences() {
        assert_eq!(nice_date("2026-01-07"), "7 Jan 2026");
        assert_eq!(nice_date("soon"), "soon");
        assert_eq!(counts(&rel("1.0.0", 2, 1)), "2 new  ·  1 fixed");
        assert_eq!(counts(&rel("1.0.0", 0, 3)), "3 fixed");
        assert_eq!(summary(&[rel("1.0.1", 1, 1), rel("1.0.0", 3, 5)]), "4 new features and 6 fixes");
        assert_eq!(summary(&[rel("1.0.0", 1, 0)]), "1 new feature");
        let w = WhatsNew::new(vec![rel("0.54.0", 1, 0), rel("0.53.0", 1, 0)], "0.52.1");
        assert_eq!((w.title(), w.sub()), ("What's new since Quarry 0.52.1".into(), "2 releases  ·  0.53.0 to 0.54.0".into()));
        assert!(w.open.contains("0.54.0") && !w.open.contains("0.53.0"));
        let w = WhatsNew::new(vec![rel("0.54.0", 1, 0)], "0.53.1");
        assert_eq!(w.sub(), "Released 17 Nov 2026  ·  updated from 0.53.1");
    }

    #[test]
    fn code_words_are_mono() {
        assert!(looks_like_code("$getField") && looks_like_code("select_rows") && looks_like_code("db::Msg"));
        assert!(!looks_like_code("UTF-8") && !looks_like_code("0.102") && !looks_like_code("_"));
        let job = entry_job(None, "Fenced $objectToArray, OR groups", 400.0);
        let mono = job.sections.iter().filter(|s| s.format.font_id.family == egui::FontFamily::Monospace).count();
        assert_eq!(mono, 1, "only $objectToArray is code, the comma after it is not");
        assert_eq!(job.text, "Fenced $objectToArray, OR groups");
    }
}
