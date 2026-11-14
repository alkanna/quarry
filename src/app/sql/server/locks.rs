//! The Locks tab of the SQL Server window: blocker chains from the server's lock tables, with the same Kill query and Kill
//! connection steps as the Processes tab. A thread idle in a transaction is the usual blocker, and only Kill connection
//! frees it, so that button is the one that stays on for it.

use super::*;
use db::sql::server::locks::{chains, LockWait, Locks};
use db::sql::server::{one_line, Proc};
use std::sync::Arc;

const LOCK_COLS: [(&str, f32); 5] = [("Id", ID_W), ("User", 110.0), ("Db", 100.0), ("Lock", 150.0), ("On", 210.0)];
const WAIT_W: f32 = 76.0;
const HEAD_H: f32 = 50.0;
/// The Kill buttons of a blocker row, and the same width once the second click is asked for.
const BTNS_W: f32 = 330.0;

/// A span as the tab shows it: "12 s", "3 m 05 s", "1 h 02 m".
pub(super) fn dur(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} m {:02} s", s / 60, s % 60)
    } else {
        format!("{} h {:02} m", s / 3600, s % 3600 / 60)
    }
}

/// Whether the armed Kill still points at the same statement of the same connection on the same server.
pub(super) fn confirm_still_ok(c: &Confirm, l: &Locks) -> bool {
    c.identity == l.identity && l.procs.get(&c.p.id).is_some_and(|p| c.p.same_thread(p))
}

/// The statement a thread shows: what it runs now, or the one its transaction is stuck on when the process list has none.
fn statement(l: &Locks, id: u64) -> String {
    match l.procs.get(&id) {
        Some(p) if !p.line.is_empty() => p.line.clone(),
        _ => l.trx.get(&id).map(|t| one_line(&t.query, 300)).unwrap_or_default(),
    }
}

pub(super) fn locks_view(ui: &mut egui::Ui, s: &mut SqlServerWin, max_h: f32, kill: &mut Option<Confirm>) {
    if !s.writable {
        s.unlock = banner(ui, Notice::Locked, "Read-only.", "Unlock writes to kill queries or connections.", Some("Unlock…"));
    }
    ui.horizontal(|ui| {
        if chip(ui, "Refresh every 2 s", PRIMARY(), s.auto).clicked() {
            s.auto = !s.auto;
        }
        if !s.auto && quiet(ui, "Refresh").clicked() {
            s.refresh = true;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(Ok(l)) = &s.locks {
                ui.label(RichText::new(format!("From {}", l.source)).color(FAINT()));
            }
        });
    });
    ui.add_space(8.0);
    let l: Arc<Locks> = match &s.locks {
        None => return spin(ui),
        Some(Err(e)) => {
            if error_row(ui, format!("Could not read the locks: {e}")) {
                s.refresh = true;
            }
            return;
        }
        Some(Ok(l)) => l.clone(),
    };
    let ch = chains(&l.waits);
    // The thread that read the list is Quarry's too.
    let mut own = s.own.clone();
    own.entry(l.me).or_insert_with(|| "Server window".to_string());
    if s.confirm.as_ref().is_some_and(|c| !confirm_still_ok(c, &l) || !ch.iter().any(|x| x.head == c.p.id)) {
        s.confirm = None;
    }
    let w = ui.available_width();
    let query_w = (w - LOCK_COLS.iter().map(|c| c.1).sum::<f32>() - WAIT_W).max(140.0);
    let mut cols: Vec<(&str, f32, bool)> = LOCK_COLS.iter().map(|(l, w)| (*l, *w, false)).collect();
    cols.push(("Waiting", WAIT_W, true));
    cols.push(("Statement", query_w, false));
    header(ui, &cols);
    if ch.is_empty() {
        ui.add_space(8.0);
        ui.label(RichText::new("No thread waits for a lock.").color(MUTED()));
    } else {
        let mut deeper: Option<(u64, u64, u64)> = None;
        egui::ScrollArea::vertical().id_salt("sql-locks").max_height(max_h - 30.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for c in &ch {
                blocker_row(ui, s, &l, &own, c.head, w, kill);
                let mut parents: Vec<u64> = vec![c.head];
                for (id, depth) in &c.waiting {
                    parents.truncate(*depth);
                    let parent = parents[depth - 1];
                    if *depth > 1 && deeper.is_none() {
                        deeper = Some((*id, parent, c.head));
                    }
                    let edge = l.waits.iter().find(|x| x.waiter == *id && x.blocker == parent).or_else(|| l.waits.iter().find(|x| x.waiter == *id));
                    waiter_row(ui, &l, *id, edge, *depth, w, query_w);
                    parents.push(*id);
                }
            }
        });
        ui.add_space(8.0);
        let lead = deeper.map(|(p, q, h)| format!("{p} queues behind {q}, not behind {h} directly. ")).unwrap_or_default();
        ui.label(RichText::new(format!("{lead}Killing a blocker's connection rolls back its transaction and frees every thread under it.")).size(TXT_SMALL).color(FAINT()));
    }
    for n in &l.notes {
        ui.add_space(4.0);
        ui.label(RichText::new(n).size(TXT_SMALL).color(MUTED()));
    }
}

fn blocker_row(ui: &mut egui::Ui, s: &mut SqlServerWin, l: &Locks, own: &HashMap<u64, String>, id: u64, w: f32, kill: &mut Option<Confirm>) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, HEAD_H), Sense::hover());
    let proc = l.procs.get(&id);
    let trx = l.trx.get(&id);
    // Open and doing nothing: the usual reason a lock is never let go.
    let idle = proc.is_some_and(|p| p.sleeping());
    let p = ui.painter();
    p.rect_filled(rect, 0.0, if idle { tint(WARNING(), 20) } else { PANEL() });
    if idle {
        p.rect_filled(egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())), 0.0, WARNING());
    }
    let (l1, l2) = (egui::Rect::from_min_max(egui::pos2(rect.left(), rect.top() + 6.0), egui::pos2(rect.right(), rect.top() + 26.0)), egui::Rect::from_min_max(egui::pos2(rect.left(), rect.top() + 26.0), egui::pos2(rect.right(), rect.bottom() - 4.0)));
    let mine = own.contains_key(&id);
    paint_cell(ui, l1, rect.left(), LOCK_COLS[0].1, &id.to_string(), C_NUMBER(), true, false);
    let mut x = rect.left() + LOCK_COLS[0].1;
    paint_cell(ui, l1, x, LOCK_COLS[1].1, &proc.map(|p| p.user.clone()).unwrap_or_default(), BRIGHT(), true, false);
    let qx = x;
    x += LOCK_COLS[1].1;
    paint_cell(ui, l1, x, LOCK_COLS[2].1, &proc.map(|p| p.db.clone()).unwrap_or_default(), BRIGHT(), true, false);
    x += LOCK_COLS[2].1;
    let g = ui.painter().layout_no_wrap("Blocker".into(), FontId::proportional(TXT_SMALL), ALERT());
    let bw = g.size().x;
    ui.painter().galley(egui::pos2(x + PAD, l1.center().y - g.size().y / 2.0 + optical_dy(&g)), g, ALERT());
    let open = trx.and_then(|t| t.open_secs).or(proc.map(|p| p.secs));
    let (what, color) = match proc {
        None => ("not in the process list".to_string(), MUTED()),
        Some(_) if idle => (format!("idle in a transaction{}", open.map(|o| format!(" for {}", dur(o))).unwrap_or_default()), WARNING()),
        Some(p) if p.running() => (format!("running for {}", dur(p.secs)), SUCCESS()),
        Some(p) => (p.command.to_lowercase(), MUTED()),
    };
    paint_cell(ui, l1, x + bw + 4.0, (rect.right() - BTNS_W - x - bw).max(40.0), &what, color, false, false);
    let line = statement(l, id);
    let (line, line_color) = if line.is_empty() { (if idle { "No statement is running; the transaction is only open.".to_string() } else { String::new() }, FAINT()) } else { (line, MUTED()) };
    paint_cell(ui, l2, qx, (rect.right() - BTNS_W - qx).max(60.0), &line, line_color, true, false);
    if mine && resp.hovered() {
        resp.clone().on_hover_text("Quarry's own session. Stop it where it runs: Kill is not offered for it.");
    }
    let area = egui::Rect::from_min_max(egui::pos2(rect.right() - BTNS_W, rect.top() + 9.0), egui::pos2(rect.right() - 8.0, rect.bottom() - 9.0));
    let mut k = ui.new_child(egui::UiBuilder::new().max_rect(area).layout(egui::Layout::right_to_left(egui::Align::Center)));
    let Some(p) = proc else { return };
    match s.confirm.clone().filter(|c| c.p.id == id) {
        Some(c) => {
            // Drawn where the button was, so the guard keeps a double click from sending it.
            let b = danger(&mut k, if c.whole { "Kill connection" } else { "Kill it" });
            if confirmed(&k, &b, c.at) {
                *kill = Some(c.clone());
                s.confirm = None;
            }
            if quiet(&mut k, "Keep").clicked() {
                s.confirm = None;
            }
            k.label(RichText::new(if c.whole { format!("End connection {id}?") } else { format!("Stop the statement of {id}?") }).size(TXT_SMALL).color(ALERT()));
        }
        None => {
            let whole = k.add_enabled_ui(can_kill(s.writable, own, p, true), |ui| danger(ui, "Kill connection")).inner;
            let query = k.add_enabled(can_kill(s.writable, own, p, false), egui::Button::new("Kill query"));
            for (resp, whole_conn) in [(&whole, true), (&query, false)] {
                if resp.clicked() {
                    s.confirm = Some(Confirm { p: p.clone(), identity: l.identity.clone(), whole: whole_conn, at: arm_time(&k) });
                    s.sel = Some(id);
                }
            }
        }
    }
}

fn waiter_row(ui: &mut egui::Ui, l: &Locks, id: u64, edge: Option<&LockWait>, depth: usize, w: f32, query_w: f32) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::hover());
    let proc: Option<&Proc> = l.procs.get(&id);
    ui.painter().rect_filled(rect, 0.0, BG());
    // The tree line: a stem from the row above and a tick into this one, one step in per level.
    let indent = 14.0 * depth as f32;
    let tx = rect.left() + PAD + indent - 8.0;
    let stroke = Stroke::new(1.0, FAINT());
    ui.painter().line_segment([egui::pos2(tx, rect.top()), egui::pos2(tx, rect.center().y)], stroke);
    ui.painter().line_segment([egui::pos2(tx, rect.center().y), egui::pos2(tx + 6.0, rect.center().y)], stroke);
    paint_cell(ui, rect, rect.left() + indent, LOCK_COLS[0].1 - indent, &id.to_string(), C_NUMBER(), true, false);
    let mut x = rect.left() + LOCK_COLS[0].1;
    paint_cell(ui, rect, x, LOCK_COLS[1].1, &proc.map(|p| p.user.clone()).unwrap_or_default(), BRIGHT(), true, false);
    x += LOCK_COLS[1].1;
    paint_cell(ui, rect, x, LOCK_COLS[2].1, &proc.map(|p| p.db.clone()).unwrap_or_default(), BRIGHT(), true, false);
    x += LOCK_COLS[2].1;
    let (mode, on) = edge.map(|e| (if e.mode.is_empty() { e.kind.clone() } else { format!("{} {}", e.kind, e.mode) }, e.on.clone())).unwrap_or_default();
    let strong = edge.is_some_and(|e| e.mode.starts_with("X") || e.mode == "EXCLUSIVE");
    paint_cell(ui, rect, x, LOCK_COLS[3].1, &mode, if strong { WARNING() } else { TEXT() }, true, false);
    x += LOCK_COLS[3].1;
    paint_cell(ui, rect, x, LOCK_COLS[4].1, &on, TEXT(), true, false);
    x += LOCK_COLS[4].1;
    let waiting = l.trx.get(&id).and_then(|t| t.wait_secs).or(proc.map(|p| p.secs));
    paint_cell(ui, rect, x, WAIT_W, &waiting.map(dur).unwrap_or_default(), C_NUMBER(), true, true);
    x += WAIT_W;
    let line = statement(l, id);
    paint_cell(ui, rect, x, query_w, &line, MUTED(), true, false);
    if resp.hovered() && resp.hover_pos().is_some_and(|h| h.x >= x) {
        let long: String = proc.map(|p| p.info.clone()).filter(|i| !i.is_empty()).or_else(|| l.trx.get(&id).map(|t| t.query.clone())).unwrap_or_default().chars().take(1200).collect();
        if !long.is_empty() {
            resp.on_hover_text(long);
        }
    }
}
