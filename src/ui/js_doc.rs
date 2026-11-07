//! Signature and docs cards for the shell editor: beside the suggestion list, on hover, and as
//! parameter hints while typing a call. Docs arrive as markdown from the language server.

use super::editor::js_job;
use super::theme::*;
use crate::db::JsDoc;
use eframe::egui::{self, FontId, TextFormat, text::LayoutJob};

const DOC_W: f32 = 440.0;
const DOC_MAX_H: f32 = 280.0;

/// The signature in syntax colors, with the active parameter picked out.
fn sig_job(d: &JsDoc) -> LayoutJob {
    let mut job = js_job(&d.sig);
    if let Some(a) = &d.active {
        for s in job.sections.iter_mut().filter(|s| s.byte_range.start.0 < a.end && a.start < s.byte_range.end.0) {
            s.format.color = BRIGHT();
            s.format.background = tint(PRIMARY(), 60);
        }
    }
    job.wrap.max_width = DOC_W;
    job
}

/// One paragraph: `code` spans in monospace, emphasis stars dropped, and a leading @tag colored.
fn para_job(text: &str) -> LayoutJob {
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
    job.wrap.max_width = DOC_W;
    job
}

/// The small markdown subset TypeScript docs use: paragraphs, code fences and inline code.
fn markdown(ui: &mut egui::Ui, md: &str) {
    let mut para = String::new();
    let mut fence: Option<String> = None;
    let flush = |ui: &mut egui::Ui, para: &mut String| {
        if !para.is_empty() {
            ui.label(para_job(para.trim()));
            para.clear();
        }
    };
    let block = |ui: &mut egui::Ui, code: &str| {
        egui::Frame::new().fill(tint(TEXT(), 12)).corner_radius(R_SM).inner_margin(6).show(ui, |ui| {
            let mut job = js_job(code.trim_end());
            job.wrap.max_width = DOC_W - 12.0;
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

/// A card with the signature on top and the docs under it, scrolled when long.
pub(crate) fn doc_card(ui: &mut egui::Ui, d: &JsDoc) {
    popup_card(ui).inner_margin(10).show(ui, |ui| {
        ui.set_max_width(DOC_W);
        ui.spacing_mut().item_spacing.y = 6.0;
        if !d.sig.is_empty() {
            ui.label(sig_job(d));
        }
        if !d.doc.is_empty() {
            if !d.sig.is_empty() {
                ui.separator();
            }
            egui::ScrollArea::vertical().id_salt("js-doc").max_height(DOC_MAX_H).show(ui, |ui| markdown(ui, &d.doc));
        }
    });
}

/// A doc card floating at `pos`, placed by `pivot`, above everything and ignoring the pointer.
pub(crate) fn doc_popup(ctx: &egui::Context, id: egui::Id, pos: egui::Pos2, pivot: egui::Align2, d: &JsDoc) {
    egui::Area::new(id).order(egui::Order::Tooltip).interactable(false).pivot(pivot).fixed_pos(pos).show(ctx, |ui| doc_card(ui, d));
}
