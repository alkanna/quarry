//! Signature and docs cards for the shell editor: beside the suggestion list, on hover, and as
//! parameter hints while typing a call. Docs arrive as markdown from the language server.

use super::editor::js_job;
use super::js_elide::{elide_sig, first_paragraph};
use super::theme::*;
use crate::db::JsDoc;
use eframe::egui::{self, FontId, TextFormat, text::LayoutJob};

const DOC_W: f32 = 440.0;
const DOC_MAX_H: f32 = 280.0;
/// Characters of a signature kept when its long types are elided: about two lines of a hint.
const SIG_BUDGET: usize = 160;

/// The signature in syntax colors, with the active parameter picked out, wrapped at `w`.
fn sig_job(sig: &str, active: Option<&std::ops::Range<usize>>, w: f32) -> LayoutJob {
    let mut job = js_job(sig);
    if let Some(a) = active {
        for s in job.sections.iter_mut().filter(|s| s.byte_range.start.0 < a.end && a.start < s.byte_range.end.0) {
            s.format.color = BRIGHT();
            s.format.background = tint(PRIMARY(), 60);
        }
    }
    job.wrap.max_width = w;
    job
}

/// One paragraph: `code` spans in monospace, emphasis stars dropped, and a leading @tag colored.
fn para_job(text: &str, w: f32) -> LayoutJob {
    let body = TextFormat { font_id: FontId::proportional(TXT_BODY), color: TEXT(), ..Default::default() };
    let code = TextFormat { background: tint(TEXT(), 18), ..mono(BRIGHT()) };
    let mut job = LayoutJob::default();
    for (i, part) in text.split('`').enumerate() {
        if i % 2 == 1 {
            job.append(part, 0.0, code.clone());
            continue;
        }
        let part = part.replace('*', "");
        match part.strip_prefix('@').and_then(|r| r.split_once(' ')) {
            Some((tag, rest)) if i == 0 => {
                job.append(&format!("@{tag} "), 0.0, TextFormat { color: C_KEYWORD(), ..body.clone() });
                job.append(rest, 0.0, body.clone());
            }
            _ => job.append(&part, 0.0, body.clone()),
        }
    }
    job.wrap.max_width = w;
    job
}

/// The small markdown subset TypeScript docs use: paragraphs, code fences and inline code.
fn markdown(ui: &mut egui::Ui, md: &str, w: f32) {
    let mut para = String::new();
    let mut fence: Option<String> = None;
    let flush = |ui: &mut egui::Ui, para: &mut String| {
        if !para.is_empty() {
            ui.label(para_job(para.trim(), w));
            para.clear();
        }
    };
    let block = |ui: &mut egui::Ui, code: &str| {
        egui::Frame::new().fill(tint(TEXT(), 12)).corner_radius(R_SM).inner_margin(6).show(ui, |ui| {
            let mut job = js_job(code.trim_end());
            job.wrap.max_width = w - 12.0;
            ui.label(job);
        });
    };
    for line in md.lines() {
        if line.trim_start().starts_with("```") {
            flush(ui, &mut para);
            match fence.take() {
                Some(code) => block(ui, &code),
                None => fence = Some(String::new()),
            }
            continue;
        }
        if let Some(code) = &mut fence {
            code.push_str(line);
            code.push('\n');
            continue;
        }
        let t = line.trim();
        // Tags like *@param* start their own line in TypeScript docs, unlike wrapped prose.
        if t.is_empty() || t.trim_start_matches('*').starts_with('@') || t.starts_with("- ") {
            flush(ui, &mut para);
        }
        if !t.is_empty() {
            para.push_str(t);
            para.push(' ');
        }
    }
    flush(ui, &mut para);
    if let Some(code) = fence {
        block(ui, &code);
    }
}

/// A card beside the suggestion list with the signature (long types elided) on top and the docs
/// under it, scrolled when long. The popups of the editor have a Details toggle for the full text.
pub(crate) fn doc_card(ui: &mut egui::Ui, d: &JsDoc) {
    popup_card(ui).inner_margin(10).show(ui, |ui| {
        ui.set_max_width(DOC_W);
        ui.spacing_mut().item_spacing.y = 6.0;
        if !d.sig.is_empty() {
            let (sig, active) = elide_sig(&d.sig, d.active.as_ref(), SIG_BUDGET);
            ui.label(sig_job(&sig, active.as_ref(), DOC_W));
        }
        if !d.doc.is_empty() {
            if !d.sig.is_empty() {
                ui.separator();
            }
            egui::ScrollArea::vertical().id_salt("js-doc").max_height(DOC_MAX_H).show(ui, |ui| markdown(ui, &d.doc, DOC_W));
        }
    });
}

/// Whether Details has anything to add: a signature that was elided or docs past the first paragraph.
pub(crate) fn has_details(d: &JsDoc) -> bool {
    elide_sig(&d.sig, d.active.as_ref(), SIG_BUDGET).0 != d.sig || first_paragraph(&d.doc).1
}

/// A text link: primary colored, brighter with a line under it on hover.
fn link(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(TXT_CAPTION), PRIMARY());
    let (rect, r) = ui.allocate_exact_size(galley.size(), egui::Sense::click());
    let r = r.on_hover_cursor(egui::CursorIcon::PointingHand);
    let color = match crate::ui::widgets::state_of(&r) {
        2 => tint(PRIMARY(), 150),
        1 => BRIGHT(),
        _ => PRIMARY(),
    };
    ui.painter().galley(rect.min, galley, color);
    if r.hovered() {
        ui.painter().hline(rect.x_range(), rect.bottom() - 1.0, egui::Stroke::new(1.0, color));
    }
    r
}

/// What a hint popup reports back: where it is, and whether Details or Less was clicked.
pub(crate) struct HintOut {
    pub(crate) rect: egui::Rect,
    pub(crate) toggled: bool,
}

/// The editor's signature and hover popup at `pos`, placed by `pivot`, above everything. It takes
/// the pointer, so it can be hovered and scrolled. Collapsed it shows the elided signature and the
/// first paragraph of the docs; `open` shows everything, wrapped, in an area that scrolls past `max_h`.
pub(crate) fn hint_popup(ctx: &egui::Context, id: egui::Id, pos: egui::Pos2, pivot: egui::Align2, d: &JsDoc, open: &mut bool, max_w: f32, max_h: f32) -> HintOut {
    let w = max_w - 20.0;
    let more = has_details(d);
    let mut toggled = false;
    let r = egui::Area::new(id).order(egui::Order::Tooltip).pivot(pivot).fixed_pos(pos).show(ctx, |ui| {
        popup_card(ui).inner_margin(10).show(ui, |ui| {
            ui.set_max_width(w);
            ui.spacing_mut().item_spacing.y = 6.0;
            if *open {
                ui.set_width(w);
                egui::ScrollArea::vertical().id_salt("js-hint").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                    ui.set_width(w - 8.0);
                    if !d.sig.is_empty() {
                        ui.label(sig_job(&d.sig, d.active.as_ref(), w - 8.0));
                    }
                    if !d.doc.is_empty() {
                        if !d.sig.is_empty() {
                            ui.separator();
                        }
                        markdown(ui, &d.doc, w - 8.0);
                    }
                });
            } else {
                if !d.sig.is_empty() {
                    let (sig, active) = elide_sig(&d.sig, d.active.as_ref(), SIG_BUDGET);
                    ui.label(sig_job(&sig, active.as_ref(), w));
                }
                let head = first_paragraph(&d.doc).0;
                if !head.is_empty() {
                    if !d.sig.is_empty() {
                        ui.separator();
                    }
                    markdown(ui, head, w);
                }
            }
            if more {
                toggled = link(ui, if *open { "Less" } else { "Details" }).clicked();
            }
        });
    });
    if toggled {
        *open = !*open;
    }
    HintOut { rect: r.response.rect, toggled }
}
