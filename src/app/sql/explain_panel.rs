//! The findings strip and the node panel of the explain dialog. A finding is a card like a toast, outlined in its
//! severity color, that selects its node; the panel shows the selected node's own numbers, its filter laid out one
//! condition per line, its findings and, when one has a fix, the SQL to open as a draft in a Query tab or to copy.

use super::explain::{SqlExplain, XAct};
use crate::app::*;
use crate::db::sql::plan::findings::{self, Fix};
use crate::db::sql::plan::{cond, rewrite, Access, Finding, FixKind, Op, PlanNode, Severity};
use crate::db::sql::Dialect;

/// The findings shown before "N more".
const SHOWN: usize = 2;
/// A glyph's box beside a title, and a stat tile's height.
pub(super) const GLYPH_BOX: f32 = 16.0;
const TILE_H: f32 = 54.0;
/// The tags painted in the plan's rows: a node's type, a change, how far an estimate was off.
pub(super) const TAG_H: f32 = 20.0;
/// The column captions above the tree and the Compare table.
pub(super) const HEAD_H: f32 = 24.0;
/// The bar of the rows a node read and kept.
const READ_BAR_H: f32 = 10.0;

/// A count or time for a cell: whole numbers grouped, small ones with two decimals.
pub(super) fn figure(n: f64) -> String {
    if n >= 100.0 { group(&format!("{n:.0}")) } else { format!("{n:.2}").trim_end_matches('0').trim_end_matches('.').to_string() }
}

pub(super) fn access_color(a: Access) -> Color32 {
    match a {
        Access::Scan => ALERT(),
        Access::Index => SUCCESS(),
        Access::Other => TEXT(),
    }
}

/// Red for a node that takes most of the plan, amber for a large part, quiet below.
pub(super) fn share_color(s: f64) -> Color32 {
    if s >= 0.4 {
        ALERT()
    } else if s >= 0.2 {
        WARNING()
    } else {
        FAINT()
    }
}

fn severity_color(s: Severity) -> Color32 {
    match s {
        Severity::Warn => WARNING(),
        Severity::Note => MUTED(),
    }
}

pub(super) fn findings_strip(ui: &mut egui::Ui, x: &mut SqlExplain, acts: &mut Vec<XAct>) {
    let Some(p) = &x.plan else { return };
    if p.findings.is_empty() {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(GLYPH_BOX, GLYPH_BOX), Sense::hover());
            glyphs::check(ui.painter(), r.center(), SUCCESS());
            ui.label(RichText::new("Nothing in this plan stands out.").color(MUTED()));
        });
        return;
    }
    ui.label(caption_text(format!("FINDINGS  ·  {}", p.findings.len())));
    ui.add_space(4.0);
    let w = ui.available_width();
    let per_row = if w >= 900.0 { 2 } else { 1 };
    let shown = if x.more { p.findings.len() } else { SHOWN.min(p.findings.len()) };
    let card_w = (w - 12.0 * (per_row - 1) as f32) / per_row as f32;
    for row in p.findings[..shown].chunks(per_row) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            for f in row {
                ui.allocate_ui_with_layout(egui::vec2(card_w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_width(card_w);
                    finding_card(ui, f, f.node.is_some() && f.node == x.sel, acts);
                });
            }
        });
        ui.add_space(8.0);
    }
    if p.findings.len() > SHOWN {
        let label = if x.more { "Fewer".to_string() } else { format!("{} more", p.findings.len() - SHOWN) };
        if quiet(ui, label).clicked() {
            x.more = !x.more;
        }
    }
}

/// One finding, outlined like a toast. Clicking it shows its node.
fn finding_card(ui: &mut egui::Ui, f: &Finding, selected: bool, acts: &mut Vec<XAct>) {
    let color = severity_color(f.severity);
    let id = ui.scope_id().with(("explain-finding", &f.title, f.node));
    let hovered = ui.ctx().read_response(id).is_some_and(|r| r.hovered());
    let fill = if selected || hovered { MENU_HOVER() } else { RAISED() };
    let out = egui::Frame::new().fill(fill).stroke(Stroke::new(1.0, color)).corner_radius(R_LG).inner_margin(egui::Margin { left: 14, right: 12, top: 10, bottom: 10 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(GLYPH_BOX, GLYPH_BOX), Sense::hover());
            match f.severity {
                Severity::Warn => glyphs::warn(ui.painter(), r.center(), color),
                Severity::Note => glyphs::info(ui.painter(), r.center(), color),
            }
            ui.label(semibold(RichText::new(&f.title).color(BRIGHT())));
            if !f.weight.is_empty() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(&f.weight).size(TXT_SMALL).color(color));
                });
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(24.0);
            ui.vertical(|ui| {
                for b in &f.body {
                    ui.label(RichText::new(b).size(TXT_SMALL).color(MUTED()));
                }
                // The fix is beside the plan once the card is clicked; a button here would sit under the card's own click.
                if let Some(fix) = &f.fix {
                    ui.label(RichText::new(format!("Fix: {}", fix.label)).size(TXT_SMALL).color(C_LINK()));
                }
            });
        });
    });
    let r = ui.interact(out.response.rect, id, Sense::click());
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &f.title));
    if r.clicked() {
        if let Some(n) = f.node {
            acts.push(XAct::Select(n));
        }
    }
}

/// The selected node: what it is, its own numbers, its conditions, its findings and their fixes.
pub(super) fn inspector(ui: &mut egui::Ui, x: &SqlExplain, acts: &mut Vec<XAct>) {
    let Some(p) = &x.plan else { return };
    let Some(i) = x.sel.filter(|&i| i < p.nodes.len()) else {
        ui.label(RichText::new("Select a node to see it here.").color(MUTED()));
        return;
    };
    let n = &p.nodes[i];
    egui::Frame::new().fill(RAISED()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_LG).inner_margin(egui::Margin::same(16)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        // What the node is. The panes zero the row spacing, so each row sets its own gap.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let col = access_color(n.access);
            let label = RichText::new(&n.label).font(egui::FontId::monospace(TXT_BODY));
            if col == TEXT() {
                ui.label(label.color(BRIGHT()));
            } else {
                egui::Frame::new().fill(tint(col, 36)).corner_radius(R_SM).inner_margin(egui::Margin::symmetric(6, 2)).show(ui, |ui| ui.label(label.color(col)));
            }
            let name = n.name();
            if !name.is_empty() {
                ui.label(semibold(RichText::new(name).font(egui::FontId::monospace(TXT_TITLE)).color(BRIGHT())));
            }
        });
        let context = context_line(p, i);
        if !context.is_empty() {
            ui.label(RichText::new(context).size(TXT_SMALL).color(MUTED()));
        }
        ui.add_space(8.0);
        tiles(ui, p, n);
        // How many rows the node read and kept, and from where.
        if let (Some(kept), Some(removed)) = (n.kept_rows(), n.removed) {
            let loops = n.loops.unwrap_or(1.0);
            let (kept, removed) = (kept * loops, removed * loops);
            let read = kept + removed;
            ui.add_space(10.0);
            ui.label(caption_text("ROWS READ"));
            let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), READ_BAR_H), Sense::hover());
            ui.painter().rect_filled(r, R_SM, TRACK());
            if read > 0.0 {
                let w = (r.width() * (kept / read) as f32).max(4.0);
                ui.painter().rect_filled(egui::Rect::from_min_size(r.min, egui::vec2(w, r.height())), R_SM, SUCCESS());
            }
            let pct = if read > 0.0 { kept / read * 100.0 } else { 0.0 };
            ui.label(RichText::new(format!("{} read  ·  {} kept ({})  ·  {} removed by the filter", figure(read), figure(kept), findings::pct_text(pct), figure(removed))).size(TXT_SMALL).color(TEXT()));
        }
        if let Some((hit, read)) = n.buffers {
            ui.label(RichText::new(format!("Buffers  {} hit  ·  {} read from disk", figure(hit), figure(read))).size(TXT_SMALL).color(if read > hit { WARNING() } else { MUTED() }));
        }
        // Conditions.
        if !n.filter.is_empty() {
            section(ui, "FILTER", Some(&n.filter), acts);
            match &n.filter_expr {
                Some(e) => condition_lines(ui, e),
                None => {
                    ui.label(crate::ui::editor::sql_job_in(&n.filter, TXT_SMALL, x.dialect));
                }
            }
        }
        for (title, text, expr) in [("INDEX CONDITION", &n.index_cond, &n.index_cond_expr), (if x.dialect == Dialect::Postgres { "JOIN CONDITION" } else { "LOOKUP" }, &n.join_cond, &n.join_cond_expr), ("JOIN FILTER", &n.join_filter, &n.join_filter_expr)] {
            if text.is_empty() {
                continue;
            }
            section(ui, title, Some(text), acts);
            match expr {
                Some(e) => condition_lines(ui, e),
                None => {
                    ui.label(crate::ui::editor::sql_job_in(text, TXT_SMALL, x.dialect));
                }
            }
        }
        // How many pairs the join's condition matched and its filter threw away, after ANALYZE.
        if let (Some(kept), Some(removed)) = (n.actual_rows, n.removed_join) {
            let loops = n.loops.unwrap_or(1.0);
            let (kept, removed) = (kept * loops, removed * loops);
            let pct = if kept + removed > 0.0 { kept / (kept + removed) * 100.0 } else { 0.0 };
            ui.label(RichText::new(format!("{} pairs matched  ·  {} kept ({})  ·  {} removed by the join filter", figure(kept + removed), figure(kept), findings::pct_text(pct), figure(removed))).size(TXT_SMALL).color(TEXT()));
        }
        let rest: Vec<&String> = n.detail.iter().filter(|d| !SHOWN_ELSEWHERE.iter().any(|k| d.starts_with(k))).collect();
        if !rest.is_empty() {
            section(ui, "DETAILS", None, acts);
            for d in rest {
                ui.label(RichText::new(d.as_str()).size(TXT_SMALL).font(egui::FontId::monospace(TXT_SMALL)).color(MUTED()));
            }
        }
        // Findings about this node and their fixes.
        for f in p.findings.iter().filter(|f| f.node == Some(i)) {
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let (r, _) = ui.allocate_exact_size(egui::vec2(GLYPH_BOX, GLYPH_BOX), Sense::hover());
                match f.severity {
                    Severity::Warn => glyphs::warn(ui.painter(), r.center(), WARNING()),
                    Severity::Note => glyphs::info(ui.painter(), r.center(), MUTED()),
                }
                ui.label(semibold(RichText::new(&f.title).color(BRIGHT())));
            });
            for b in &f.body {
                ui.label(RichText::new(b).size(TXT_SMALL).color(MUTED()));
            }
            if !f.evidence.is_empty() {
                section(ui, "FROM THE CATALOG", None, acts);
                for line in &f.evidence {
                    ui.label(RichText::new(line).size(TXT_SMALL).color(TEXT()));
                }
            }
            if let Some(why) = &f.no_fix {
                ui.add_space(6.0);
                ui.label(RichText::new(why).size(TXT_SMALL).color(MUTED()));
            }
            if x.ev_busy && f.fix.is_none() && f.no_fix.is_none() && n.access == Access::Scan && !n.filter.is_empty() {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Reading the table's statistics and indexes").size(TXT_SMALL).color(MUTED()));
                });
            }
            if let Some(fix) = f.fix.as_ref().filter(|fx| fx.kind == FixKind::Rewrite) {
                rewrite_box(ui, x, n, fix, acts);
                continue;
            }
            if let Some(fix) = f.fix.as_ref().filter(|fx| fx.kind == FixKind::Index) {
                candidate_box(ui, x, fix, acts);
                continue;
            }
            if let Some(fix) = f.fix.as_ref().filter(|fx| fx.kind == FixKind::Setting && x.db.pooler()) {
                ui.add_space(6.0);
                ui.label(RichText::new(format!("Behind a connection pooler a session setting does not last to the next statement, so {} is not offered here. Set it on the role or the server instead.", fix.label)).size(TXT_SMALL).color(MUTED()));
                continue;
            }
            if let Some(fix) = &f.fix {
                let sql = if fix.kind == FixKind::Setting { format!("{}\n{};", fix.sql, x.stmt.trim().trim_end_matches(';')) } else { fix.sql.clone() };
                ui.add_space(6.0);
                well_frame().inner_margin(egui::Margin::same(12)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if !fix.note.is_empty() {
                        ui.label(RichText::new(&fix.note).size(TXT_SMALL).color(MUTED()));
                        ui.add_space(4.0);
                    }
                    ui.add(egui::Label::new(crate::ui::editor::sql_job_in(&fix.sql, TXT_SMALL, x.dialect)).wrap());
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if primary(ui, "Open in Query tab", PRIMARY()).on_hover_text("Opens it as a draft in a new Query tab. Nothing runs.").clicked() {
                        acts.push(XAct::Draft(sql.clone()));
                    }
                    if quiet(ui, "Copy").clicked() {
                        acts.push(XAct::Copy(sql));
                    }
                });
            }
        }
    });
}

/// The index a finding suggests: why these columns in this order, the statement colored, and the two ways to take it.
/// Nothing here runs it; the draft opens in a Query tab for the user to run.
fn candidate_box(ui: &mut egui::Ui, x: &SqlExplain, fix: &Fix, acts: &mut Vec<XAct>) {
    let (note, sql) = (fix.note.as_str(), fix.sql.as_str());
    section(ui, "CANDIDATE INDEX", None, acts);
    well_frame().inner_margin(egui::Margin::same(12)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(note).size(TXT_SMALL).color(MUTED()));
        ui.add_space(6.0);
        ui.add(egui::Label::new(crate::ui::editor::sql_job_in(sql, TXT_SMALL, x.dialect)).wrap());
    });
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        if primary(ui, "Open in Query tab", PRIMARY()).on_hover_text("Opens it as a draft in a new Query tab. Nothing runs.").clicked() {
            acts.push(XAct::Draft(sql.to_string()));
        }
        if quiet(ui, "Copy").clicked() {
            acts.push(XAct::Copy(sql.to_string()));
        }
        ui.label(RichText::new("Then Explain again to compare.").size(TXT_SMALL).color(MUTED()));
    });
    super::explain_hypo::hypo_row(ui, x, fix, acts);
}

/// The filter written so an index serves it. The draft is the whole statement with the condition replaced when it can be
/// found there for certain, else the condition alone; Copy takes the condition.
fn rewrite_box(ui: &mut egui::Ui, x: &SqlExplain, n: &PlanNode, fix: &Fix, acts: &mut Vec<XAct>) {
    section(ui, "REWRITTEN FILTER", None, acts);
    well_frame().inner_margin(egui::Margin::same(12)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(&fix.note).size(TXT_SMALL).color(MUTED()));
        ui.add_space(6.0);
        ui.add(egui::Label::new(crate::ui::editor::sql_job_in(&fix.sql, TXT_SMALL, x.dialect)).wrap());
    });
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        if primary(ui, "Open in Query tab", PRIMARY()).on_hover_text("Opens the statement with this filter as a draft in a new Query tab. Nothing runs.").clicked() {
            acts.push(XAct::Draft(rewrite::statement(&x.stmt, x.dialect, n, fix).unwrap_or_else(|| fix.sql.clone())));
        }
        if quiet(ui, "Copy").clicked() {
            acts.push(XAct::Copy(fix.sql.clone()));
        }
        ui.label(RichText::new("Then Explain again to compare.").size(TXT_SMALL).color(MUTED()));
    });
}

/// Detail lines the panel already shows in a better form.
const SHOWN_ELSEWHERE: &[&str] = &["Filter:", "Index Cond:", "Recheck Cond:", "Hash Cond:", "Merge Cond:", "Join Filter:", "Rows removed by filter:", "Rows removed by join filter:", "Buffers:", "Index condition:", "Lookup:"];

fn section(ui: &mut egui::Ui, title: &str, copy: Option<&String>, acts: &mut Vec<XAct>) {
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.label(caption_text(title));
        if let Some(text) = copy {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if quiet(ui, "Copy").on_hover_text(format!("Copy the {} as the server wrote it", title.to_lowercase())).clicked() {
                    acts.push(XAct::Copy(text.clone()));
                }
            });
        }
    });
}

fn condition_lines(ui: &mut egui::Ui, e: &cond::Expr) {
    let space = text_w(ui, " ", egui::FontId::monospace(TXT_SMALL));
    for line in cond::pretty(e, 3) {
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = ui.available_width() - line.indent as f32 * space;
        for (role, text) in &line.parts {
            let color = match role {
                cond::Role::Col => TEXT(),
                cond::Role::Op | cond::Role::Punct => MUTED(),
                cond::Role::Kw => C_KEYWORD(),
                cond::Role::Val => C_STRING(),
                cond::Role::More => C_LINK(),
            };
            job.append(text, 0.0, mono_fmt(TXT_SMALL, color, 400.0));
        }
        ui.horizontal(|ui| {
            ui.add_space(line.indent as f32 * space);
            ui.label(job);
        });
    }
}

/// The node's own numbers as three tiles: cost or time, rows, and width or loops.
fn tiles(ui: &mut egui::Ui, p: &crate::db::sql::plan::Plan, n: &PlanNode) {
    let pct = |s: Option<f64>| s.map(|s| format!("{}%", (s * 100.0).round())).unwrap_or_default();
    let share = n.share.unwrap_or(0.0);
    let hot = |s: f64| if s >= 0.4 { ALERT() } else if s >= 0.2 { WARNING() } else { BRIGHT() };
    let items: Vec<(&str, String, String, Color32)> = if p.analyzed && n.self_ms.is_some() {
        let off = n.off().unwrap_or(1.0);
        vec![
            ("TIME", format!("{} ms", findings::ms_text(n.self_ms.unwrap_or(0.0))), pct(n.share), hot(share)),
            ("ROWS", n.actual_rows.map(figure).unwrap_or_else(|| "-".into()), n.rows.map(|r| format!("est. {}", figure(r))).unwrap_or_default(), if off >= findings::OFF_WARN { WARNING() } else { BRIGHT() }),
            ("LOOPS", n.loops.map(figure).unwrap_or_else(|| "-".into()), String::new(), BRIGHT()),
        ]
    } else {
        let third = match (n.filtered_pct, n.width) {
            (Some(f), _) => ("KEPT", format!("{f:.0}%"), "guess".to_string(), if f <= 20.0 { WARNING() } else { BRIGHT() }),
            (None, Some(w)) => ("WIDTH", format!("{w:.0} B"), String::new(), BRIGHT()),
            _ => ("COST", n.cost.map(figure).unwrap_or_else(|| "-".into()), "with below".to_string(), BRIGHT()),
        };
        vec![
            ("SELF COST", n.self_cost.map(figure).unwrap_or_else(|| "-".into()), pct(n.share), hot(share)),
            ("EST. ROWS", n.rows.map(figure).unwrap_or_else(|| "-".into()), String::new(), BRIGHT()),
            third,
        ]
    };
    let w = (ui.available_width() - 16.0) / 3.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for (label, value, note, color) in items {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, TILE_H), Sense::hover());
            let pt = ui.painter();
            pt.rect(rect, R_BAR, PANEL(), Stroke::new(1.0, LINE()), egui::StrokeKind::Inside);
            let lg = pt.layout_job(egui::text::LayoutJob::single_section(label.to_string(), ui_fmt(TXT_SECTION, FAINT(), W_SEMIBOLD, 0.6)));
            let vg = pt.layout_job(egui::text::LayoutJob::single_section(value, mono_fmt(TXT_SUBHEAD, color, 600.0)));
            let ng = (!note.is_empty()).then(|| pt.layout_job(egui::text::LayoutJob::single_section(note.clone(), ui_fmt(TXT_SMALL, MUTED(), 400.0, 0.0))));
            let place = note_place(w, lg.size().x, vg.size().x, ng.as_ref().map_or(0.0, |g| g.size().x));
            let vw = vg.size().x;
            galley_at(pt, rect.left() + TILE_PAD, rect.top() + 16.0, lg, FAINT());
            galley_at(pt, rect.left() + TILE_PAD, rect.top() + 37.0, vg, color);
            match (ng, place) {
                (Some(g), NotePlace::Beside) => galley_at(pt, rect.left() + TILE_PAD + vw + NOTE_GAP, rect.top() + 38.0, g, MUTED()),
                (Some(g), NotePlace::OnLabel) => galley_at(pt, rect.right() - TILE_PAD - g.size().x, rect.top() + 16.0, g, MUTED()),
                (Some(_), NotePlace::Hover) => {
                    resp.on_hover_text(note);
                }
                _ => {}
            }
        }
    });
}

/// A tile's text starts this far from its sides, and a note sits this far after the value.
const TILE_PAD: f32 = 12.0;
const NOTE_GAP: f32 = 6.0;

/// Where a tile's note goes so it never runs past the tile's edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NotePlace {
    /// After the value, as in the design.
    Beside,
    /// At the right of the label line, when the value leaves no room beside it.
    OnLabel,
    /// In the tile's hover text, when neither line has room.
    Hover,
}

fn note_place(tile_w: f32, label_w: f32, value_w: f32, note_w: f32) -> NotePlace {
    let room = tile_w - 2.0 * TILE_PAD;
    if value_w + NOTE_GAP + note_w <= room {
        NotePlace::Beside
    } else if label_w + NOTE_GAP + note_w <= room {
        NotePlace::OnLabel
    } else {
        NotePlace::Hover
    }
}

/// "public.bookings  ·  under Hash, builds the join's hash table": where the node sits and what it is for.
fn context_line(p: &crate::db::sql::plan::Plan, i: usize) -> String {
    let n = &p.nodes[i];
    let mut parts = Vec::new();
    if !n.target.is_empty() && n.target != n.name() {
        parts.push(n.target.clone());
    }
    if let Some(j) = n.parent {
        let parent = &p.nodes[j];
        let first = parent.kids.first() == Some(&i);
        let what = if n.role.starts_with("SubPlan") || n.role.starts_with("InitPlan") || n.role.starts_with("CTE") {
            format!("{}, runs for {}", n.role, parent.label)
        } else {
            match parent.op {
                Op::Hash => "builds the join's hash table".to_string(),
                Op::NestedLoop if first => "drives the nested loop".into(),
                Op::NestedLoop => "runs once per row of the side above".into(),
                o if o.is_join() && first => "the join's outer side".into(),
                o if o.is_join() => "the join's inner side".into(),
                Op::Gather => "run by parallel workers".into(),
                Op::Limit => "read until the limit is reached".into(),
                Op::Sort => "feeds the sort".into(),
                Op::Aggregate => "feeds the aggregate".into(),
                _ => format!("under {}", parent.label),
            }
        };
        parts.push(what);
    }
    parts.join("  ·  ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tile_note_never_runs_past_the_tile() {
        // A narrow panel: "13,885" and "est. 12,622" do not fit side by side, so the note moves up beside ROWS.
        assert_eq!(note_place(120.0, 30.0, 58.0, 58.0), NotePlace::OnLabel);
        assert_eq!(note_place(160.0, 30.0, 58.0, 58.0), NotePlace::Beside);
        // A long label leaves no room on either line, so the note goes to the hover text.
        assert_eq!(note_place(100.0, 60.0, 70.0, 58.0), NotePlace::Hover);
        let (label, value, note) = (30.0, 58.0, 58.0);
        for w in [80.0_f32, 116.0, 140.0, 200.0] {
            match note_place(w, label, value, note) {
                NotePlace::Beside => assert!(TILE_PAD + value + NOTE_GAP + note <= w - TILE_PAD, "{w}"),
                NotePlace::OnLabel => assert!(TILE_PAD + label + NOTE_GAP <= w - TILE_PAD - note, "{w}"),
                NotePlace::Hover => {}
            }
        }
    }
}
