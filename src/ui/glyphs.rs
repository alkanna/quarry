//! Small line icons for dialog headers and buttons, painted as shapes because glyphs from fallback fonts sit off
//! center. Each one fits a 16 px box centered on `c`, so they share the `fn(&Painter, Pos2, Color32)` signature of
//! `table_glyph` and can be passed around as values.

use eframe::egui::{self, Color32, Pos2, Stroke};

pub(crate) type Glyph = fn(&egui::Painter, Pos2, Color32);

const W: f32 = 1.4;

/// A point given in the 16 px design box, moved onto `c`.
fn at(c: Pos2, x: f32, y: f32) -> Pos2 {
    c + egui::vec2(x - 8.0, y - 8.0)
}

fn line(p: &egui::Painter, c: Pos2, col: Color32, pts: &[(f32, f32)]) {
    let pts: Vec<Pos2> = pts.iter().map(|&(x, y)| at(c, x, y)).collect();
    p.add(egui::Shape::line(pts, Stroke::new(W, col)));
}

fn closed(p: &egui::Painter, c: Pos2, col: Color32, pts: &[(f32, f32)]) {
    let pts: Vec<Pos2> = pts.iter().map(|&(x, y)| at(c, x, y)).collect();
    p.add(egui::Shape::closed_line(pts, Stroke::new(W, col)));
}

fn rect(p: &egui::Painter, c: Pos2, col: Color32, x: f32, y: f32, w: f32, h: f32, r: f32) {
    let rr = egui::Rect::from_min_size(at(c, x, y), egui::vec2(w, h));
    p.rect_stroke(rr, r, Stroke::new(W, col), egui::StrokeKind::Middle);
}

pub(crate) fn user(p: &egui::Painter, c: Pos2, col: Color32) {
    p.circle_stroke(at(c, 8.0, 5.5), 2.8, Stroke::new(W, col));
    line(p, c, col, &[(2.5, 14.0), (3.6, 11.6), (5.6, 10.3), (8.0, 9.9), (10.4, 10.3), (12.4, 11.6), (13.5, 14.0)]);
}

pub(crate) fn server(p: &egui::Painter, c: Pos2, col: Color32) {
    rect(p, c, col, 2.0, 2.5, 12.0, 4.5, 1.2);
    rect(p, c, col, 2.0, 9.0, 12.0, 4.5, 1.2);
    p.circle_filled(at(c, 4.8, 4.75), 0.9, col);
    p.circle_filled(at(c, 4.8, 11.25), 0.9, col);
}

pub(crate) fn download(p: &egui::Painter, c: Pos2, col: Color32) {
    line(p, c, col, &[(8.0, 2.0), (8.0, 10.5)]);
    line(p, c, col, &[(4.5, 7.0), (8.0, 10.5), (11.5, 7.0)]);
    line(p, c, col, &[(3.0, 13.5), (13.0, 13.5)]);
}

pub(crate) fn upload(p: &egui::Painter, c: Pos2, col: Color32) {
    line(p, c, col, &[(8.0, 11.0), (8.0, 2.5)]);
    line(p, c, col, &[(4.5, 6.0), (8.0, 2.5), (11.5, 6.0)]);
    line(p, c, col, &[(3.0, 13.5), (13.0, 13.5)]);
}

pub(crate) fn table(p: &egui::Painter, c: Pos2, col: Color32) {
    rect(p, c, col, 2.5, 3.0, 11.0, 10.0, 1.2);
    line(p, c, col, &[(2.5, 6.5), (13.5, 6.5)]);
    line(p, c, col, &[(6.5, 6.5), (6.5, 13.0)]);
}

pub(crate) fn plug(p: &egui::Painter, c: Pos2, col: Color32) {
    line(p, c, col, &[(6.0, 2.0), (6.0, 5.0)]);
    line(p, c, col, &[(10.0, 2.0), (10.0, 5.0)]);
    line(p, c, col, &[(4.5, 5.0), (11.5, 5.0), (11.5, 7.5), (10.6, 9.6), (8.0, 11.0), (5.4, 9.6), (4.5, 7.5), (4.5, 5.0)]);
    line(p, c, col, &[(8.0, 11.0), (8.0, 14.0)]);
}

pub(crate) fn warn(p: &egui::Painter, c: Pos2, col: Color32) {
    closed(p, c, col, &[(8.0, 2.5), (14.0, 13.0), (2.0, 13.0)]);
    line(p, c, col, &[(8.0, 6.5), (8.0, 9.3)]);
    p.circle_filled(at(c, 8.0, 11.2), 0.8, col);
}

pub(crate) fn trash(p: &egui::Painter, c: Pos2, col: Color32) {
    line(p, c, col, &[(3.0, 4.5), (13.0, 4.5)]);
    line(p, c, col, &[(6.5, 4.5), (6.5, 3.0), (9.5, 3.0), (9.5, 4.5)]);
    line(p, c, col, &[(4.5, 4.5), (5.2, 13.0), (10.8, 13.0), (11.5, 4.5)]);
}

pub(crate) fn plus(p: &egui::Painter, c: Pos2, col: Color32) {
    line(p, c, col, &[(8.0, 3.5), (8.0, 12.5)]);
    line(p, c, col, &[(3.5, 8.0), (12.5, 8.0)]);
}

pub(crate) fn sliders(p: &egui::Painter, c: Pos2, col: Color32) {
    line(p, c, col, &[(3.0, 4.5), (13.0, 4.5)]);
    line(p, c, col, &[(3.0, 11.5), (13.0, 11.5)]);
    line(p, c, col, &[(6.0, 3.0), (6.0, 6.0)]);
    line(p, c, col, &[(10.5, 10.0), (10.5, 13.0)]);
}

pub(crate) fn key(p: &egui::Painter, c: Pos2, col: Color32) {
    p.circle_stroke(at(c, 5.5, 10.0), 3.0, Stroke::new(W, col));
    line(p, c, col, &[(7.6, 7.9), (13.0, 2.5)]);
    line(p, c, col, &[(11.0, 4.5), (12.5, 6.0)]);
}

pub(crate) fn lock(p: &egui::Painter, c: Pos2, col: Color32) {
    rect(p, c, col, 3.0, 7.5, 10.0, 6.5, 1.2);
    line(p, c, col, &[(5.5, 7.5), (5.5, 5.0), (6.2, 3.2), (8.0, 2.5), (9.8, 3.2), (10.5, 5.0), (10.5, 7.5)]);
}

pub(crate) fn copy(p: &egui::Painter, c: Pos2, col: Color32) {
    rect(p, c, col, 6.0, 5.5, 7.0, 8.0, 1.0);
    line(p, c, col, &[(4.0, 10.5), (3.5, 10.5), (3.5, 3.0), (10.0, 3.0), (10.0, 3.5)]);
}

pub(crate) fn doc(p: &egui::Painter, c: Pos2, col: Color32) {
    line(p, c, col, &[(4.0, 2.0), (9.0, 2.0), (12.0, 5.0), (12.0, 14.0), (4.0, 14.0), (4.0, 2.0)]);
    line(p, c, col, &[(9.0, 2.0), (9.0, 5.0), (12.0, 5.0)]);
    line(p, c, col, &[(6.0, 8.5), (10.0, 8.5)]);
    line(p, c, col, &[(6.0, 11.0), (10.0, 11.0)]);
}

pub(crate) fn code(p: &egui::Painter, c: Pos2, col: Color32) {
    line(p, c, col, &[(5.5, 4.5), (2.5, 8.0), (5.5, 11.5)]);
    line(p, c, col, &[(10.5, 4.5), (13.5, 8.0), (10.5, 11.5)]);
}

pub(crate) fn link(p: &egui::Painter, c: Pos2, col: Color32) {
    line(p, c, col, &[(7.0, 5.0), (8.5, 3.5), (10.5, 2.8), (12.5, 3.5), (13.2, 5.5), (12.5, 7.5), (11.0, 9.0)]);
    line(p, c, col, &[(9.0, 11.0), (7.5, 12.5), (5.5, 13.2), (3.5, 12.5), (2.8, 10.5), (3.5, 8.5), (5.0, 7.0)]);
    line(p, c, col, &[(6.0, 10.0), (10.0, 6.0)]);
}

pub(crate) fn shield(p: &egui::Painter, c: Pos2, col: Color32) {
    closed(p, c, col, &[(8.0, 2.0), (13.0, 4.0), (13.0, 8.0), (11.7, 11.2), (8.0, 14.0), (4.3, 11.2), (3.0, 8.0), (3.0, 4.0)]);
}

pub(crate) fn check(p: &egui::Painter, c: Pos2, col: Color32) {
    line(p, c, col, &[(3.5, 8.5), (6.5, 11.5), (12.5, 4.5)]);
}

pub(crate) fn info(p: &egui::Painter, c: Pos2, col: Color32) {
    p.circle_stroke(at(c, 8.0, 8.0), 6.0, Stroke::new(W, col));
    line(p, c, col, &[(8.0, 7.5), (8.0, 11.0)]);
    p.circle_filled(at(c, 8.0, 5.2), 0.8, col);
}

pub(crate) fn list(p: &egui::Painter, c: Pos2, col: Color32) {
    for y in [4.0, 8.0, 12.0] {
        p.circle_filled(at(c, 3.5, y), 0.9, col);
        line(p, c, col, &[(6.0, y), (13.0, y)]);
    }
}

pub(crate) fn undo(p: &egui::Painter, c: Pos2, col: Color32) {
    line(p, c, col, &[(5.5, 3.5), (2.5, 6.5), (5.5, 9.5)]);
    line(p, c, col, &[(2.5, 6.5), (10.0, 6.5), (12.2, 7.4), (13.2, 9.5), (12.2, 11.6), (10.0, 12.5), (6.0, 12.5)]);
}

pub(crate) fn pencil(p: &egui::Painter, c: Pos2, col: Color32) {
    closed(p, c, col, &[(10.5, 3.0), (13.0, 5.5), (6.0, 12.5), (3.0, 13.0), (3.5, 10.0)]);
    line(p, c, col, &[(9.0, 4.5), (11.5, 7.0)]);
}

/// The kind glyphs of the Library panel. The folder is a tab and a body; open, its front edge slants forward.
pub(crate) fn folder(p: &egui::Painter, c: Pos2, col: Color32) {
    closed(p, c, col, &[(2.0, 3.5), (6.5, 3.5), (7.8, 5.5), (14.0, 5.5), (14.0, 12.5), (2.0, 12.5)]);
}

/// `folder` filled with a light tint of `col`, for the colored group folders. The outline is not convex, so the fill
/// is its two convex parts, the tab and the body.
pub(crate) fn folder_filled(p: &egui::Painter, c: Pos2, col: Color32) {
    let fill = super::theme::tint(col, 90);
    let poly = |pts: &[(f32, f32)]| egui::Shape::convex_polygon(pts.iter().map(|&(x, y)| at(c, x, y)).collect(), fill, Stroke::NONE);
    p.add(poly(&[(2.0, 3.5), (6.5, 3.5), (7.8, 5.5), (2.0, 5.5)]));
    p.add(poly(&[(2.0, 5.5), (14.0, 5.5), (14.0, 12.5), (2.0, 12.5)]));
    folder(p, c, col);
}

pub(crate) fn folder_open(p: &egui::Painter, c: Pos2, col: Color32) {
    line(p, c, col, &[(2.0, 12.5), (2.0, 3.5), (6.5, 3.5), (7.8, 5.5), (13.0, 5.5), (13.0, 7.0)]);
    closed(p, c, col, &[(2.0, 12.5), (4.0, 7.0), (15.0, 7.0), (13.0, 12.5)]);
}

/// A folder with a plus, for New folder.
pub(crate) fn folder_plus(p: &egui::Painter, c: Pos2, col: Color32) {
    line(p, c, col, &[(9.5, 12.5), (2.0, 12.5), (2.0, 3.5), (6.5, 3.5), (7.8, 5.5), (14.0, 5.5), (14.0, 8.0)]);
    line(p, c, col, &[(12.0, 9.5), (12.0, 14.0)]);
    line(p, c, col, &[(9.75, 11.75), (14.25, 11.75)]);
}

/// A page with a folded corner and two lines: a SQL file.
pub(crate) fn file_sql(p: &egui::Painter, c: Pos2, col: Color32) {
    doc(p, c, col);
}

/// A page with braces: a shell script.
pub(crate) fn file_braces(p: &egui::Painter, c: Pos2, col: Color32) {
    line(p, c, col, &[(4.0, 2.0), (9.0, 2.0), (12.0, 5.0), (12.0, 14.0), (4.0, 14.0), (4.0, 2.0)]);
    line(p, c, col, &[(9.0, 2.0), (9.0, 5.0), (12.0, 5.0)]);
    line(p, c, col, &[(7.2, 8.0), (6.2, 8.6), (6.2, 9.4), (5.6, 10.0), (6.2, 10.6), (6.2, 11.4), (7.2, 12.0)]);
    line(p, c, col, &[(8.8, 8.0), (9.8, 8.6), (9.8, 9.4), (10.4, 10.0), (9.8, 10.6), (9.8, 11.4), (8.8, 12.0)]);
}

/// A funnel: a filter.
pub(crate) fn funnel(p: &egui::Painter, c: Pos2, col: Color32) {
    closed(p, c, col, &[(2.5, 3.5), (13.5, 3.5), (9.2, 8.4), (9.2, 13.0), (6.8, 11.8), (6.8, 8.4)]);
}

/// Stacked bars: an aggregation pipeline.
pub(crate) fn stack(p: &egui::Painter, c: Pos2, col: Color32) {
    rect(p, c, col, 2.5, 3.0, 11.0, 3.0, 0.8);
    rect(p, c, col, 4.5, 6.5, 9.0, 3.0, 0.8);
    rect(p, c, col, 6.5, 10.0, 7.0, 3.0, 0.8);
}

/// A filled triangle pointing right, for Run.
pub(crate) fn play(p: &egui::Painter, c: Pos2, col: Color32) {
    p.add(egui::Shape::convex_polygon(vec![at(c, 5.0, 3.0), at(c, 12.5, 8.0), at(c, 5.0, 13.0)], col, Stroke::NONE));
}
