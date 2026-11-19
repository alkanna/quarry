//! A Query tab whose text has `{{...}}` values: a strip above the editor holds where it runs and the values, Template
//! shows the statement as written with its placeholders tinted, and Filled shows what Run sends. Run fills the values in
//! `sql_run_text`. Make it a value... turns a selected literal into a placeholder.
use crate::app::*;
use super::query::{run_db, run_schema_of};
use crate::library::snippet::{self, Ty};
use std::ops::Range;

pub(in crate::app) struct MakeValue {
    pub uid: u64,
    pub range: Range<usize>,
    pub literal: String,
    pub name: String,
    pub ty: String,
    pub default: Option<String>,
    pub keep: bool,
}

impl App {
    /// The strip, drawn when the current tab's Query text has values. Returns whether the editor shows them filled in.
    pub(in crate::app) fn snippet_strip(&mut self, ui: &mut egui::Ui) -> bool {
        let d = self.sql_dialect();
        let Some(t) = self.t() else { return false };
        let Some(s) = t.sql.as_ref() else { return false };
        // Plain SQL, most tabs, costs one scan for `{{` per frame.
        if s.snip_off || !s.query.contains("{{") {
            return false;
        }
        let p = snippet::parse(&s.query, d);
        if !p.is_snippet() {
            return false;
        }
        let conn = t.sel.conn;
        let db = run_db(t);
        let schema = run_schema_of(t);
        let target = if db.is_empty() { self.conn_name(conn) } else { format!("{} \u{b7} {db}", self.conn_name(conn)) };
        // A tab of a saved script offers to make where it runs now that script's default.
        let saved = t.script.as_ref().and_then(|b| match b.r {
            crate::library::ScriptRef::Saved(id) => self.library.scripts.iter().find(|s| s.id == id),
            _ => None,
        });
        let make_default = saved.filter(|s| !self.runs_at(s, conn, &db, &schema, &t.sel.schema)).map(|s| s.id);
        let mut set_default = None;
        let Some(s) = self.tabs.get_mut(self.cur).and_then(|t| t.sql.as_mut()) else { return false };
        egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(RichText::new("Run on").color(MUTED()));
                ui.label(RichText::new(&target).color(BRIGHT()));
                if let Some(id) = make_default
                    && quiet(ui, "Make default").on_hover_text("Run this snippet here from Cmd+K from now on").clicked()
                {
                    set_default = Some(id);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    segmented(ui, &mut s.snip_filled, &[(true, "Filled"), (false, "Template")]);
                });
            });
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let mono = FontId::monospace(TXT_SMALL);
                let digit = ui.fonts_mut(|f| f.glyph_width(&mono, '0'));
                for v in &p.vars {
                    let val = s.snip_vals.entry(v.name.clone()).or_insert_with(|| v.default.clone().unwrap_or_default());
                    // As wide as the value or its hint, never so short that typing scrolls it at once.
                    let w = match &v.ty {
                        Ty::Choice(c) => c.iter().map(|c| c.chars().count()).max().unwrap_or(0).max(5) as f32 * digit + BUTTON_H,
                        _ => (val.chars().count().max(v.ty.hint().chars().count()).max(16) as f32 * digit + BUTTON_H / 2.0).min(VALUE_FIELD_MAX_W),
                    };
                    // A name and its field wrap together, so many values flow onto more lines without splitting.
                    let name_w = v.name.chars().count() as f32 * digit;
                    if ui.available_rect_before_wrap().width() < name_w + w + 3.0 * ui.spacing().item_spacing.x {
                        ui.end_row();
                    }
                    ui.label(RichText::new(&v.name).font(mono.clone()).color(MUTED())).on_hover_text(v.ty.spec());
                    match &v.ty {
                        Ty::Choice(c) => {
                            let shown = if val.is_empty() { "pick\u{2026}".to_string() } else { val.clone() };
                            let mut picks = Vec::new();
                            ui.menu_button(RichText::new(shown).font(mono.clone()).color(C_STRING()), |ui| {
                                menu_list(ui);
                                for c in c {
                                    menu_act(ui, &mut picks, c.clone(), |ui| ui.selectable_label(val == c, c));
                                }
                            });
                            if let Some(c) = picks.pop() {
                                *val = c;
                            }
                        }
                        _ => {
                            ui.add(egui::TextEdit::singleline(val).font(mono.clone()).desired_width(w).hint_text(RichText::new(v.ty.hint()).color(FAINT())));
                        }
                    }
                    ui.add_space(8.0);
                }
            });
            if let Some(e) = &p.err {
                ui.label(RichText::new(&e.msg).size(TXT_SMALL).color(ALERT()));
            }
        });
        ui.add_space(6.0);
        let filled = s.snip_filled;
        if let Some(id) = set_default {
            self.set_snippet_default(id, conn, &db, &schema);
        }
        filled
    }

    /// The Filled view: the statement Run would send, read-only, in place of the editor.
    pub(in crate::app) fn snippet_preview(&mut self, ui: &mut egui::Ui) {
        let d = self.sql_dialect();
        let Some(s) = self.t().and_then(|t| t.sql.as_ref()) else { return };
        let p = snippet::parse(&s.query, d);
        let bs = self.t().is_none_or(|t| self.sql_bs_of(t.sel.conn));
        let out = snippet::fill_named(&s.query, &p, &s.snip_vals, d, bs);
        egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD).inner_margin(12).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().id_salt("snip-filled").auto_shrink([false, true]).max_height((ui.available_height() * 0.5).max(96.0)).show(ui, |ui| match out {
                Ok(t) => {
                    // Laid out once per text, not every frame.
                    ui.label(crate::ui::editor::memo_galley(ui, 10, &t, ui.available_width(), || crate::ui::editor::sql_job_in(&t, TXT_SMALL, d)));
                }
                Err(e) => {
                    ui.label(RichText::new(e).color(ALERT()));
                }
            });
        });
    }

    /// The text a Query run sends: `text` with the tab's values filled in, or why it can't be.
    pub(in crate::app) fn snippet_fill_run(&self, text: String) -> Result<String, String> {
        if !text.contains("{{") {
            return Ok(text);
        }
        let d = self.sql_dialect();
        let Some(t) = self.t() else { return Ok(text) };
        let Some(s) = t.sql.as_ref().filter(|s| !s.snip_off) else { return Ok(text) };
        snippet::fill_named(&text, &snippet::parse(&s.query, d), &s.snip_vals, d, self.sql_bs_of(t.sel.conn))
    }

    /// Make it a value... on the selected literal of the Query editor, from its context menu.
    pub(in crate::app) fn ask_make_value(&mut self, range: Range<usize>) {
        let d = self.sql_dialect();
        let Some(t) = self.t() else { return };
        let Some(s) = t.sql.as_ref() else { return };
        let literal = s.query[range.clone()].to_string();
        let (ty, default) = snippet::infer(&literal);
        let taken = snippet::parse(&s.query, d).vars;
        let base = match ty {
            Ty::Date | Ty::DateTime => "day",
            Ty::Int | Ty::Number => "n",
            _ => "value",
        };
        let name = std::iter::once(base.to_string()).chain((2..).map(|i| format!("{base}{i}"))).find(|n| !taken.iter().any(|v| &v.name == n)).unwrap_or_default();
        self.modal = Some(Modal::MakeValue(Box::new(MakeValue { uid: t.uid, range, literal, name, ty: ty.spec(), keep: default.is_some(), default })));
    }

    pub(in crate::app) fn make_value_dialog(&mut self, ui: &mut egui::Ui, f: &mut MakeValue, keep: &mut bool) {
        ui.set_width(fit_width(ui.ctx(), 460.0));
        dialog_head(ui, &Head::new("Make it a value").sub(format!("Replaces {}", f.literal)).glyph(glyphs::code));
        let ty = Ty::read(f.ty.trim());
        let name_ok = snippet::valid_name(f.name.trim());
        dialog_body(ui, |ui| {
            stacked_field(ui, "Name", "Letters, digits and _", |ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut f.name).font(FontId::monospace(TXT_BODY)).desired_width(f32::INFINITY));
                // Only while nothing has focus, so a click into Type keeps it there.
                if !ui.memory(|m| m.focused().is_some()) {
                    r.request_focus();
                }
            });
            ui.add_space(10.0);
            stacked_field(ui, "Type", "date, datetime, text, int, number, int list, identifier, or choices like paid|pending", |ui| {
                ui.add(egui::TextEdit::singleline(&mut f.ty).font(FontId::monospace(TXT_BODY)).desired_width(f32::INFINITY));
            });
            if let Some(d) = &f.default {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    toggle(ui, &mut f.keep);
                    ui.label(RichText::new(format!("Keep {d} as its default")).color(TEXT()));
                });
            }
            if ty.is_none() {
                field_note(ui, "That type is not one Quarry knows.", ALERT());
            }
        });
        let ph = ty.as_ref().map(|t| snippet::placeholder(f.name.trim(), t, f.default.as_deref().filter(|_| f.keep)));
        let mut apply = false;
        dialog_foot(ui, |ui| {
            apply = ui.add_enabled(name_ok && ty.is_some(), primary_button(ui, "Make value", PRIMARY())).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) && name_ok && ty.is_some();
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
            if let Some(ph) = &ph {
                foot_left(ui, |ui| {
                    ui.label(RichText::new(ph).font(FontId::monospace(TXT_SMALL)).color(PRIMARY()));
                });
            }
        });
        if apply && let Some(ph) = ph {
            *keep = false;
            let Some(s) = self.tabs.iter_mut().find(|t| t.uid == f.uid).and_then(|t| t.sql.as_mut()) else { return };
            // The text may have changed under the dialog; only the literal it was opened on is replaced.
            if s.query.get(f.range.clone()) == Some(f.literal.as_str()) {
                s.query.replace_range(f.range.clone(), &ph);
                s.snip_filled = false;
            } else {
                self.warn("The statement changed, so the literal was not replaced.");
            }
        }
    }
}

/// `job` with the bytes of `holes` in the accent on a tint, for the placeholders of the Query editor.
pub(in crate::app) fn tint_holes(job: &mut egui::text::LayoutJob, holes: &[Range<usize>]) {
    crate::ui::editor::mark_ranges(job, holes, |_, f| {
        f.color = PRIMARY();
        f.background = tint(PRIMARY(), 32);
    });
}

/// The byte range of the one literal the selection covers, string or number, spaces around it left out.
pub(in crate::app) fn literal_at(text: &str, sel: Range<usize>, d: db::sql::Dialect) -> Option<Range<usize>> {
    use db::sql::lex::{tokens, Tok};
    let picked: Vec<(Tok, Range<usize>)> = tokens(text, d).into_iter().filter(|(t, r)| *t != Tok::Space && r.start < sel.end && r.end > sel.start).collect();
    let [(t, r)] = &picked[..] else {
        // A negative or decimal number is several tokens side by side: a minus, digits, a dot.
        let (first, last) = (picked.first()?, picked.last()?);
        let span = first.1.start..last.1.end;
        let joined = picked.windows(2).all(|w| w[0].1.end == w[1].1.start);
        return (joined && snippet::number(&text[span.clone()]).is_some()).then_some(span);
    };
    let lit = match t {
        Tok::Str => true,
        Tok::Word => text[r.clone()].starts_with(|c: char| c.is_ascii_digit()),
        _ => false,
    };
    lit.then(|| r.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_single_literal_is_picked() {
        for d in [db::sql::Dialect::MySql, db::sql::Dialect::Postgres] {
            let t = "SELECT 1 WHERE a > 2.5 AND b < -0.25 AND c = t.x";
            let at = |s: &str| t.find(s).map(|i| i..i + s.len()).unwrap();
            assert_eq!(literal_at(t, at("2.5"), d), Some(at("2.5")));
            assert_eq!(literal_at(t, at("-0.25"), d), Some(at("-0.25")));
            assert_eq!(literal_at(t, at("t.x"), d), None);
        }
        let t = "SELECT * FROM o WHERE d = '2026-11-03' AND n = -5 AND x = y";
        let d = db::sql::Dialect::MySql;
        let at = |s: &str| t.find(s).map(|i| i..i + s.len()).unwrap();
        assert_eq!(literal_at(t, at("'2026-11-03'"), d), Some(at("'2026-11-03'")));
        assert_eq!(literal_at(t, at("2026-11"), d), Some(at("'2026-11-03'")));
        assert_eq!(literal_at(t, at("-5"), d), Some(at("-5")));
        assert_eq!(literal_at(t, at("x = y"), d), None);
        assert_eq!(literal_at(t, at("SELECT"), d), None);
    }
}
