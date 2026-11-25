//! A script whose text has `{{...}}` values, in a SQL Query tab, the Shell or the Documents bar: a strip above the editor
//! holds where it runs and the values, Template shows the text as written with its placeholders tinted, and Filled shows
//! what Run sends. Runs fill the values through `fill_tab`. Make it a value... turns a selected literal into a placeholder.
use crate::app::*;
use crate::app::sql::query::{run_db, run_schema_of};
use crate::library::snippet::{self, Lang, Parsed, Ty};
use std::ops::Range;
pub(crate) use quarry_core::state::library::snippet_strip::*;

/// What a strip shows: the values, where they run, and the script whose default target Make default would change.
pub(in crate::app) struct Strip {
    pub p: Parsed,
    pub target: String,
    pub make_default: Option<(u64, u64, String, String)>,
}

/// The strip: where the script runs, Make default, Filled or Template unless `toggle` is off, and a field per value.
/// Returns whether Make default was clicked.
pub(in crate::app) fn strip_ui(ui: &mut egui::Ui, s: &Strip, snip: &mut Snip, toggle: bool) -> bool {
    let mut set_default = false;
    egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(RichText::new("Run on").color(MUTED()));
            ui.label(RichText::new(&s.target).color(BRIGHT()));
            if s.make_default.is_some() && quiet(ui, "Make default").on_hover_text("Run this snippet here from Cmd+K from now on").clicked() {
                set_default = true;
            }
            if toggle {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    segmented(ui, &mut snip.filled, &[(true, "Filled"), (false, "Template")]);
                });
            }
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let mono = FontId::monospace(TXT_SMALL);
            let digit = ui.fonts_mut(|f| f.glyph_width(&mono, '0'));
            for v in &s.p.vars {
                let val = snip.vals.entry(v.name.clone()).or_insert_with(|| v.default.clone().unwrap_or_default());
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
        if let Some(e) = &s.p.err {
            ui.label(RichText::new(&e.msg).size(TXT_SMALL).color(ALERT()));
        }
    });
    ui.add_space(6.0);
    set_default
}

/// The Filled view: what Run would send, read-only, in place of the editor. `job` colours it.
pub(in crate::app) fn preview_ui(ui: &mut egui::Ui, out: Result<String, String>, kind: u8, job: impl Fn(&str) -> egui::text::LayoutJob) {
    egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD).inner_margin(12).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical().id_salt(("snip-filled", kind)).auto_shrink([false, true]).max_height((ui.available_height() * 0.5).max(96.0)).show(ui, |ui| match out {
            Ok(t) => {
                // Laid out once per text, not every frame.
                ui.label(crate::ui::editor::memo_galley(ui, kind, &t, ui.available_width(), || job(&t)));
            }
            Err(e) => {
                ui.label(RichText::new(e).color(ALERT()));
            }
        });
    });
}

impl App {
    pub(in crate::app) fn snip_lang(&self, src: Src) -> Lang {
        match src {
            Src::Query => self.sql_dialect().into(),
            Src::Shell => Lang::Shell,
            Src::Bar => Lang::Query,
        }
    }

    /// The strip of `src` on the current tab, or None when its text has no values.
    pub(in crate::app) fn strip_of(&self, src: Src) -> Option<Strip> {
        let t = self.t()?;
        let p = parsed(t, src, self.snip_lang(src))?;
        let conn = t.sel.conn;
        let (db, schema) = if src == Src::Query { (run_db(t), run_schema_of(t)) } else { (t.sel.db.clone(), t.sel.schema.clone()) };
        let target = if db.is_empty() { self.conn_name(conn) } else { format!("{} \u{b7} {db}", self.conn_name(conn)) };
        // A tab of a saved script offers to make where it runs now that script's default.
        let saved = t.script.as_ref().and_then(|b| match b.r {
            crate::library::ScriptRef::Saved(id) => self.core.library.scripts.iter().find(|s| s.id == id),
            _ => None,
        });
        let make_default = saved.filter(|s| !self.runs_at(s, conn, &db, &schema, &t.sel.schema)).map(|s| (s.id, conn, db, schema));
        Some(Strip { p, target, make_default })
    }

    /// The strip, drawn when `src` on the current tab has values. Returns whether the editor shows them filled in.
    pub(in crate::app) fn snippet_strip(&mut self, ui: &mut egui::Ui, src: Src) -> bool {
        let Some(strip) = self.strip_of(src) else { return false };
        let Some(t) = self.core.tabs.get_mut(self.core.cur) else { return false };
        let toggle = src != Src::Bar;
        if strip_ui(ui, &strip, &mut t.snip, toggle) {
            self.make_snippet_default(&strip);
        }
        toggle && self.core.tabs[self.core.cur].snip.filled
    }

    pub(in crate::app) fn make_snippet_default(&mut self, s: &Strip) {
        if let Some((id, conn, db, schema)) = &s.make_default {
            self.set_snippet_default(*id, *conn, db, schema);
        }
    }

    /// The Filled view of the SQL Query editor.
    pub(in crate::app) fn snippet_preview(&mut self, ui: &mut egui::Ui) {
        let d = self.sql_dialect();
        let Some(t) = self.t() else { return };
        let Some(s) = t.sql.as_ref() else { return };
        let out = fill_tab(t, &s.query, Src::Query, d.into(), self.sql_bs_of(t.sel.conn));
        preview_ui(ui, out, 10, |t| crate::ui::editor::sql_job_in(t, TXT_SMALL, d));
    }

    pub(in crate::app) fn snippet_fill_run(&self, text: String) -> Result<String, String> {
        self.core.snippet_fill_run(text)
    }

    /// Make it a value... on the selected literal of the Query or Shell editor, from its context menu.
    pub(in crate::app) fn ask_make_value(&mut self, range: Range<usize>, src: Src) {
        let l = self.snip_lang(src);
        let Some(t) = self.t() else { return };
        let Some(text) = texts(t, src).first().map(|s| s.to_string()) else { return };
        let literal = text[range.clone()].to_string();
        let (ty, default) = snippet::infer(&literal);
        let taken = snippet::parse(&text, l).vars;
        let base = match ty {
            Ty::Date | Ty::DateTime => "day",
            Ty::Int | Ty::Number => "n",
            _ => "value",
        };
        let name = std::iter::once(base.to_string()).chain((2..).map(|i| format!("{base}{i}"))).find(|n| !taken.iter().any(|v| &v.name == n)).unwrap_or_default();
        self.modal = Some(Modal::MakeValue(Box::new(MakeValue { uid: t.uid, src, range, literal, name, ty: ty.spec(), keep: default.is_some(), default })));
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
            let Some(t) = self.core.tabs.iter_mut().find(|t| t.uid == f.uid) else { return };
            t.snip.filled = false;
            let Some(text) = text_mut(t, f.src) else { return };
            // The text may have changed under the dialog; only the literal it was opened on is replaced.
            if text.get(f.range.clone()) == Some(f.literal.as_str()) {
                text.replace_range(f.range.clone(), &ph);
            } else {
                self.warn("The statement changed, so the literal was not replaced.");
            }
        }
    }
}

/// The byte range of the one literal the selection covers, string or number, spaces around it left out.
pub(in crate::app) fn literal_at(text: &str, sel: Range<usize>, l: Lang) -> Option<Range<usize>> {
    use db::sql::lex::{tokens, Tok};
    let Lang::Sql(d) = l else { return js_literal_at(text, sel) };
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

/// `literal_at` for the Shell: the selection, spaces left out, when it is one string, number or ISODate.
fn js_literal_at(text: &str, sel: Range<usize>) -> Option<Range<usize>> {
    let s = text.get(sel.clone())?;
    let start = sel.start + (s.len() - s.trim_start().len());
    let r = start..start + s.trim().len();
    let t = &text[r.clone()];
    let quoted = |q: char| t.len() >= 2 && t.starts_with(q) && t.ends_with(q) && !t[1..t.len() - 1].contains([q, '\\']);
    (quoted('\'') || quoted('"') || snippet::number(t).is_some() || t.starts_with("ISODate(") && t.ends_with(')')).then_some(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_single_literal_is_picked() {
        for d in [db::sql::Dialect::MySql, db::sql::Dialect::Postgres] {
            let t = "SELECT 1 WHERE a > 2.5 AND b < -0.25 AND c = t.x";
            let at = |s: &str| t.find(s).map(|i| i..i + s.len()).unwrap();
            assert_eq!(literal_at(t, at("2.5"), d.into()), Some(at("2.5")));
            assert_eq!(literal_at(t, at("-0.25"), d.into()), Some(at("-0.25")));
            assert_eq!(literal_at(t, at("t.x"), d.into()), None);
        }
        let t = "SELECT * FROM o WHERE d = '2026-11-03' AND n = -5 AND x = y";
        let d = db::sql::Dialect::MySql;
        let at = |s: &str| t.find(s).map(|i| i..i + s.len()).unwrap();
        assert_eq!(literal_at(t, at("'2026-11-03'"), d.into()), Some(at("'2026-11-03'")));
        assert_eq!(literal_at(t, at("2026-11"), d.into()), Some(at("'2026-11-03'")));
        assert_eq!(literal_at(t, at("-5"), d.into()), Some(at("-5")));
        assert_eq!(literal_at(t, at("x = y"), d.into()), None);
        assert_eq!(literal_at(t, at("SELECT"), d.into()), None);
        let t = "f({ day: ISODate('2026-11-03T00:00:00Z'), s: 'paid', n: -5 })";
        let at = |s: &str| t.find(s).map(|i| i..i + s.len()).unwrap();
        assert_eq!(literal_at(t, at("'paid'"), Lang::Shell), Some(at("'paid'")));
        assert_eq!(literal_at(t, at(" -5"), Lang::Shell), Some(at("-5")));
        assert_eq!(literal_at(t, at("ISODate('2026-11-03T00:00:00Z')"), Lang::Shell), Some(at("ISODate('2026-11-03T00:00:00Z')")));
        assert_eq!(literal_at(t, at("s: 'paid'"), Lang::Shell), None);
    }
}
