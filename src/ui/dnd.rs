//! Drag and drop for rows: a row that can be dragged, the ghost that follows the pointer, and the marks a target shows.
//! Rows keep their own click, right-click and focus, so a press that moves less than egui's drag threshold is still a click.
//! The payload travels through `egui::DragAndDrop`; targets read it with `Response::dnd_hover_payload` and
//! `dnd_release_payload`. The `table.rs` grip pattern does not fit rows that are also clickable.
#![allow(dead_code)]

use super::theme::*;
use super::widgets::side_row_sense;
use eframe::egui::{self, FontId, Sense, Stroke};
use std::time::Duration;

/// How long a drag must rest on a closed folder before it opens.
const HOVER_OPEN_S: f64 = 0.6;

/// `side_row` that can also be dragged.
pub(crate) fn side_row_drag(ui: &mut egui::Ui, height: f32, indent: f32, active: bool, label: &str, paint: impl FnOnce(&egui::Painter, egui::Rect)) -> egui::Response {
    side_row_sense(ui, Sense::click_and_drag(), height, indent, active, label, paint)
}

/// Makes `r` the source of a drag: sets `payload` when the drag starts and, while it goes on, draws a ghost row with `label`
/// beside the pointer. The ghost is painted on the tooltip layer without a widget, so it never catches the pointer that
/// the targets below it need.
pub(crate) fn drag_source<T: std::any::Any + Send + Sync>(ui: &egui::Ui, r: &egui::Response, payload: T, label: &str) {
    if r.drag_started() {
        egui::DragAndDrop::set_payload(ui.ctx(), payload);
    }
    if !r.dragged() {
        return;
    }
    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    let Some(pos) = ui.ctx().pointer_interact_pos() else { return };
    let p = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("quarry-drag-ghost")));
    let galley = p.layout_no_wrap(label.to_string(), FontId::proportional(TXT_BODY), TEXT());
    let rect = egui::Rect::from_min_size(pos + egui::vec2(12.0, 8.0), galley.size() + egui::vec2(24.0, 10.0));
    p.rect(rect, R_SM, RAISED(), Stroke::new(1.0, PRIMARY()), egui::StrokeKind::Inside);
    p.galley(rect.min + egui::vec2(12.0, 5.0), galley, TEXT());
}

/// The insertion line between two rows: 2 px in the accent, across `x`.
pub(crate) fn drop_line(p: &egui::Painter, x: egui::Rangef, y: f32) {
    p.hline(x, y, Stroke::new(2.0, PRIMARY()));
}

/// The mark of a drop that lands inside a row or folder: a fill and an outline.
pub(crate) fn drop_fill(p: &egui::Painter, rect: egui::Rect) {
    p.rect(rect, R_SM, tint(PRIMARY(), 28), Stroke::new(1.0, PRIMARY()), egui::StrokeKind::Inside);
}

/// True once a drag has rested on the target `id` for 0.6 s of egui time, to open a closed folder under it. Call it every
/// frame with whether a drag is over the target; leaving the target starts the wait again. A repaint is scheduled for the
/// moment it is due, since nothing else redraws a held pointer.
pub(crate) fn hover_open(ctx: &egui::Context, id: egui::Id, hovering: bool) -> bool {
    if !hovering {
        ctx.data_mut(|d| d.remove_temp::<f64>(id));
        return false;
    }
    let now = ctx.input(|i| i.time);
    let since = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(id, || now));
    let left = since + HOVER_OPEN_S - now;
    if left > 0.0 {
        ctx.request_repaint_after(Duration::from_secs_f64(left));
    }
    left <= 0.0
}
