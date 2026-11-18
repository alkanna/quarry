//! The findings strip and the node panel of the explain dialog. A finding is a card like a toast, outlined in its
//! severity color, that selects its node; the panel shows the selected node's own numbers, its filter laid out one
//! condition per line, its findings and, when one has a fix, the SQL to open as a draft in a Query tab or to copy.

use super::explain::{SqlExplain, XAct};
use crate::app::*;
use crate::db::sql::plan::{cond, findings, Access, Finding, FixKind, PlanNode, Severity};
use crate::db::sql::Dialect;

/// The findings shown before "N more".
const SHOWN: usize = 2;
/// A glyph's box beside a title, and a stat tile's height.
const GLYPH_BOX: f32 = 16.0;
const TILE_H: f32 = 54.0;

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
        // What the node is.
        ui.horizontal(|ui| {
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
            let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 10.0), Sense::hover());
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
        for (title, text, expr) in [("INDEX CONDITION", &n.index_cond, &n.index_cond_expr), (if x.dialect == Dialect::Postgres { "JOIN CONDITION" } else { "LOOKUP" }, &n.join_cond, &n.join_cond_expr)] {
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
            if let Some(fix) = f.fix.as_ref().filter(|fx| fx.kind == FixKind::Index) {
                candidate_box(ui, x, &fix.note, &fix.sql, acts);
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
fn candidate_box(ui: &mut egui::Ui, x: &SqlExplain, note: &str, sql: &str, acts: &mut Vec<XAct>) {
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
    super::explain_hypo::hypo_row(ui, x, sql, acts);
}

/// Detail lines the panel already shows in a better form.
const SHOWN_ELSEWHERE: &[&str] = &["Filter:", "Index Cond:", "Recheck Cond:", "Hash Cond:", "Merge Cond:", "Rows removed by filter:", "Buffers:", "Index condition:", "Lookup:"];

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
            let (rect, _) = ui.allocate_exact_size(egui::vec2(w, TILE_H), Sense::hover());
            let pt = ui.painter();
            pt.rect(rect, R_BAR, PANEL(), Stroke::new(1.0, LINE()), egui::StrokeKind::Inside);
            let g = pt.layout_job(egui::text::LayoutJob::single_section(label.to_string(), ui_fmt(TXT_SECTION, FAINT(), W_SEMIBOLD, 0.6)));
            galley_at(pt, rect.left() + 12.0, rect.top() + 16.0, g, FAINT());
            let g = pt.layout_job(egui::text::LayoutJob::single_section(value, mono_fmt(TXT_SUBHEAD, color, 600.0)));
            let vw = g.size().x;
            galley_at(pt, rect.left() + 12.0, rect.top() + 37.0, g, color);
            if !note.is_empty() {
                let g = pt.layout_job(egui::text::LayoutJob::single_section(note, ui_fmt(TXT_SMALL, MUTED(), 400.0, 0.0)));
                galley_at(pt, rect.left() + 18.0 + vw, rect.top() + 38.0, g, MUTED());
            }
        }
    });
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
        let pl = parent.label.as_str();
        let first = parent.kids.first() == Some(&i);
        let what = if n.role.starts_with("SubPlan") || n.role.starts_with("InitPlan") || n.role.starts_with("CTE") {
            format!("{}, runs for {pl}", n.role)
        } else if pl == "Hash" {
            "builds the join's hash table".to_string()
        } else if pl.to_ascii_lowercase().starts_with("nested loop") {
            if first { "drives the nested loop".into() } else { "runs once per row of the side above".into() }
        } else if pl.contains("Join") {
            if first { "the join's outer side".into() } else { "the join's inner side".into() }
        } else if pl.starts_with("Gather") {
            "run by parallel workers".into()
        } else if pl == "Limit" {
            "read until the limit is reached".into()
        } else if pl.starts_with("Sort") || pl == "Filesort" {
            "feeds the sort".into()
        } else if pl.starts_with("Aggregate") || pl == "Group" || pl.starts_with("Group") {
            "feeds the aggregate".into()
        } else {
            format!("under {pl}")
        };
        parts.push(what);
    }
    parts.join("  ·  ")
}
