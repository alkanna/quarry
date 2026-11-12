//! Helpers drawn under an inline value editor: the date picker, the line that names a problem or what the text
//! saves as, and the lists for columns with a fixed set of values. They float on RAISED like menus. The editor
//! keeps the text; these read it and write it back, so typing and picking stay in sync.

use crate::db::dates::{DAY_MS, civil_from_days, days_from_civil, months_back};
use crate::db::dates::MONTHS;
use crate::ui::{theme::*, widgets::*};
use eframe::egui::{self, Color32, FontId, Key, RichText, Sense, Stroke};

/// A moment as a day number and the milliseconds into that day, in whatever zone the editor shows.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Civil {
    pub(crate) day: i64,
    pub(crate) ms: i64,
}

impl Civil {
    pub(crate) fn from_ms(ms: i64) -> Civil {
        Civil { day: ms.div_euclid(DAY_MS), ms: ms.rem_euclid(DAY_MS) }
    }

    pub(crate) fn to_ms(self) -> i64 {
        self.day * DAY_MS + self.ms
    }

    /// The same time of day on the same day `n` months later, the day clamped to the month's length.
    fn add_months(self, n: i64) -> Civil {
        Civil::from_ms(months_back(self.to_ms(), -n))
    }

    fn ymd(self) -> String {
        crate::db::dates::ymd(self.day)
    }

    fn hms(self) -> String {
        crate::db::dates::hms(self.ms / 1000)
    }
}

/// This computer's clock now, in its own zone, for `Now` and today's mark.
pub(crate) fn local_now() -> Civil {
    Civil::from_ms(utc_now_ms() + i64::from(crate::db::sql::advanced::local_offset_secs().unwrap_or(0)) * 1000)
}

pub(crate) fn utc_now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// `YYYY-MM-DD` or `YYYY-MM-DD HH:MM:SS`, the forms MySQL and PostgreSQL both read.
pub(crate) fn sql_text(c: Civil, time: bool) -> String {
    if time { format!("{} {}", c.ymd(), c.hms()) } else { c.ymd() }
}

/// `sql_text` that keeps what `old` had after its seconds (a fraction, a zone), so moving the day of a DATETIME(6)
/// or a timestamptz does not drop its microseconds or offset.
pub(crate) fn sql_text_like(c: Civil, time: bool, old: &str) -> String {
    let tail = old.trim().split_once([' ', 'T']).map(|x| x.1).filter(|t| t.len() > 8 && t.as_bytes()[2] == b':' && t.as_bytes()[5] == b':').map_or("", |t| &t[8..]);
    if time { sql_text(c, true) + tail } else { sql_text(c, false) }
}

/// The date and time of SQL text, ignoring a fraction or zone after the seconds. None when it is not one.
pub(crate) fn parse_sql(t: &str) -> Option<Civil> {
    let t = t.trim();
    let (date, time) = t.split_once([' ', 'T']).unwrap_or((t, ""));
    let p: Vec<i64> = date.split('-').map(|x| x.parse().ok()).collect::<Option<_>>()?;
    let [y, m, d] = p[..] else { return None };
    if !(1..=12).contains(&m) || d < 1 || d > crate::db::dates::month_len(y, m) || date.len() < 10 {
        return None;
    }
    let digits: String = time.chars().take_while(|c| c.is_ascii_digit() || *c == ':').collect();
    let hms: Vec<i64> = if digits.is_empty() { Vec::new() } else { digits.split(':').map(|x| x.parse().ok()).collect::<Option<_>>()? };
    let (h, mi, s) = (hms.first().copied().unwrap_or(0), hms.get(1).copied().unwrap_or(0), hms.get(2).copied().unwrap_or(0));
    (h < 24 && mi < 60 && s < 60).then(|| Civil { day: days_from_civil(y, m, d), ms: (h * 3600 + mi * 60 + s) * 1000 })
}

/// RFC 3339 with milliseconds, in UTC (`Z`) or at `offset` seconds east, the form a Mongo date is typed in.
pub(crate) fn mongo_text(utc_ms: i64, offset: Option<i32>) -> String {
    let off = i64::from(offset.unwrap_or(0));
    let c = Civil::from_ms(utc_ms + off * 1000);
    let zone = offset.map_or_else(|| "Z".to_string(), crate::db::sql::advanced::offset_text);
    format!("{}T{}.{:03}{zone}", c.ymd(), c.hms(), c.ms % 1000)
}

/// This computer's offset in milliseconds at the moment `utc_ms`, so a date across a daylight saving change shows its
/// own local time rather than today's.
pub(crate) fn local_shift(utc_ms: i64) -> i64 {
    i64::from(crate::db::sql::advanced::local_offset_at(utc_ms.div_euclid(1000)).unwrap_or(0)) * 1000
}

/// `local_shift` in seconds, the offset a Mongo date is written with in Local.
pub(crate) fn local_secs(utc_ms: i64) -> i32 {
    (local_shift(utc_ms) / 1000) as i32
}

/// The UTC moment of a local wall clock time. The offset is read at a first guess and again at the result, which
/// settles it everywhere but inside the hour a change skips or repeats.
pub(crate) fn from_local(local_ms: i64) -> i64 {
    let guess = local_ms - local_shift(local_ms);
    local_ms - local_shift(guess)
}

pub(crate) fn parse_mongo(t: &str) -> Option<i64> {
    mongodb::bson::DateTime::parse_rfc3339_str(t.trim()).ok().map(|d| d.timestamp_millis())
}

/// How the picker names the zone: a fixed line for SQL columns, or a UTC and Local switch for Mongo dates.
pub(crate) enum Zone<'a> {
    Note(String),
    Switch { local: &'a mut bool, offset: String },
}

pub(crate) struct DateOpts<'a> {
    pub(crate) time: bool,
    pub(crate) millis: bool,
    pub(crate) zone: Zone<'a>,
    pub(crate) nullable: bool,
    pub(crate) now: Civil,
    /// False when the clock of the column's zone is unknown, so Now would pick the wrong moment.
    pub(crate) now_button: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum PickOut {
    Apply,
    Cancel,
    Null,
}


/// Keys that move the picked day while the picker is open. Called before the text box, which would take the arrows.
/// Up and Down move a week, Alt with Left or Right a day, Page Up and Page Down a month.
/// The calendar's own fields (year, time) keep their arrows, so the day moves only while the editor's text box `text`
/// has focus or nothing does.
pub(crate) fn date_keys(ui: &egui::Ui, text: egui::Id, v: &mut Option<Civil>, now: Civil) -> bool {
    if !ui.ctx().memory(|m| m.focused()).is_none_or(|f| f == text) {
        return false;
    }
    let (none, alt) = (egui::Modifiers::NONE, egui::Modifiers::ALT);
    let keys = [(none, Key::ArrowUp, (0, -7)), (none, Key::ArrowDown, (0, 7)), (alt, Key::ArrowLeft, (0, -1)), (alt, Key::ArrowRight, (0, 1)), (none, Key::PageUp, (-1, 0)), (none, Key::PageDown, (1, 0))];
    let step = ui.input_mut(|i| keys.iter().find(|k| i.consume_key(k.0, k.1)).map(|k| k.2));
    let Some((months, days)) = step else { return false };
    let c = v.unwrap_or(now).add_months(months);
    *v = Some(Civil { day: c.day + days, ..c });
    true
}

/// A month as one number, `year * 12 + month - 1`, so paging is adding.
fn month_idx(y: i64, m: i64) -> i64 {
    y * 12 + m - 1
}

/// The year and month (1 to 12) of a `month_idx`.
fn month_of(i: i64) -> (i64, i64) {
    (i.div_euclid(12), i.rem_euclid(12) + 1)
}

/// The months the calendar can show, January of year 1 to December of 9999.
const MIN_MONTH: i64 = 12;
const MAX_MONTH: i64 = 9999 * 12 + 11;

/// What the calendar keeps between frames: the month shown, the picked day it was last centered on, whether the month
/// chooser is open, and the pass it was drawn in.
#[derive(Clone, Copy)]
struct CalState {
    month: i64,
    seen: Option<i64>,
    choosing: bool,
    pass: u64,
}

/// The calendar under the editor at `at`. `v` is the moment the text holds, None when it does not parse. Returns
/// whether `v` was changed here, what the footer asked for, and the rect it covers so a click in it is not a click
/// outside the editor.
pub(crate) fn date_picker(ctx: &egui::Context, id: egui::Id, at: egui::Pos2, v: &mut Option<Civil>, o: DateOpts) -> (bool, Option<PickOut>, egui::Rect) {
    let mid = id.with("month");
    // The month shown follows the value when the text changes, and the arrows page without touching the value. The
    // state is dropped when the calendar was not drawn last pass, so every cell opens on its own month and on days.
    let pass = ctx.cumulative_pass_nr();
    let kept = ctx.data(|d| d.get_temp::<CalState>(mid)).filter(|k| k.pass + 1 >= pass);
    let (mut month, mut choosing) = kept.map_or((i64::MIN, false), |k| (k.month, k.choosing));
    let cur = v.map(|c| c.day);
    if month == i64::MIN || (cur != kept.and_then(|k| k.seen) && cur.is_some()) {
        let (y, m, _) = civil_from_days(v.unwrap_or(o.now).day);
        month = month_idx(y, m);
    }
    let (mut changed, mut out) = (false, None);
    let rect = float(ctx, id, at, egui::Align2::LEFT_TOP, 12, |ui| {
        ui.set_width(DATE_PICK_W);
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 0.0);
        let (y, m) = month_of(month);
        // The title switches the grid to the twelve months of a year, for jumping far without paging month by month.
        // There, a new year or month also moves the picked date, keeping its day, so Enter saves what is shown.
        let mut moved = false;
        ui.horizontal(|ui| {
            let title = if choosing { MONTHS[m as usize - 1].to_string() } else { format!("{} {y}", MONTHS[m as usize - 1]) };
            if title_button(ui, &title, choosing).clicked() {
                choosing = !choosing;
            }
            if choosing {
                let mut yy = y;
                if ui.add(egui::DragValue::new(&mut yy).range(1..=9999).speed(0.1)).on_hover_text("Type or drag a year").changed() {
                    (month, moved) = (month_idx(yy, m), true);
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (step, unit) = if choosing { (12, "year") } else { (1, "month") };
                if arrow(ui, false, &format!("Next {unit}")).clicked() {
                    (month, moved) = (month + step, choosing);
                }
                if arrow(ui, true, &format!("Previous {unit}")).clicked() {
                    (month, moved) = (month - step, choosing);
                }
            });
        });
        ui.add_space(8.0);
        if choosing {
            // The same height as the days, so the card does not jump when it switches.
            let (grid, _) = ui.allocate_exact_size(egui::vec2(DATE_PICK_W, DATE_HEAD_H + DATE_DAY_H * 6.0), Sense::hover());
            let size = egui::vec2(DATE_PICK_W / 3.0, grid.height() / 4.0);
            let picked = cur.map(civil_from_days);
            let (ty, tm, _) = civil_from_days(o.now.day);
            for (i, name) in MONTHS.iter().enumerate() {
                let cell = egui::Rect::from_min_size(grid.min + egui::vec2(size.x * (i % 3) as f32, size.y * (i / 3) as f32), size).shrink(3.0);
                let r = ui.interact(cell, id.with(("month", i)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                let on = picked.is_some_and(|(py, pm, _)| py == y && pm == i as i64 + 1);
                ui.painter().rect_filled(cell, R_SM, if on { state_fill(ui, PRIMARY()) } else { menu_fill(&r) });
                // This month is outlined like today in the days, so it reads the same with or without a value.
                if !on && ty == y && tm == i as i64 + 1 {
                    ui.painter().rect_stroke(cell, R_SM, Stroke::new(1.0, PRIMARY()), egui::StrokeKind::Inside);
                }
                let fg = if on { ON_PRIMARY() } else { TEXT() };
                let g = ui.painter().layout_no_wrap(name[..3].to_string(), FontId::proportional(TXT_SMALL), fg);
                galley_at(ui.painter(), cell.center().x - g.size().x / 2.0, cell.center().y, g, fg);
                r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, *name));
                if r.clicked() {
                    (month, choosing, moved) = (month_idx(y, i as i64 + 1), false, true);
                }
            }
        } else {
            let cw = DATE_PICK_W / 7.0;
            let (head, _) = ui.allocate_exact_size(egui::vec2(DATE_PICK_W, DATE_HEAD_H), Sense::hover());
            for (i, d) in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"].iter().enumerate() {
                ui.painter().text(egui::pos2(head.left() + cw * (i as f32 + 0.5), head.center().y), egui::Align2::CENTER_CENTER, *d, FontId::proportional(TXT_CAPTION), FAINT());
            }
            let first = days_from_civil(y, m, 1);
            // 1970-01-01 was a Thursday, so Monday is day 0 of the week at (day + 3) mod 7.
            let lead = (first + 3).rem_euclid(7);
            let start = first - lead;
            let (grid, _) = ui.allocate_exact_size(egui::vec2(DATE_PICK_W, DATE_DAY_H * 6.0), Sense::hover());
            for k in 0..42 {
                let day = start + k;
                let cell = egui::Rect::from_min_size(grid.min + egui::vec2(cw * (k % 7) as f32, DATE_DAY_H * (k / 7) as f32), egui::vec2(cw, DATE_DAY_H)).shrink(1.5);
                let r = ui.interact(cell, id.with(("day", day)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                let (_, dm, dd) = civil_from_days(day);
                let picked = cur == Some(day);
                let fill = if picked { state_fill(ui, PRIMARY()) } else { menu_fill(&r) };
                ui.painter().rect_filled(cell, R_SM, fill);
                if day == o.now.day && !picked {
                    ui.painter().rect_stroke(cell, R_SM, Stroke::new(1.0, PRIMARY()), egui::StrokeKind::Inside);
                }
                let color = if picked { ON_PRIMARY() } else if dm != m { FAINT() } else { TEXT() };
                ui.painter().text(cell.center(), egui::Align2::CENTER_CENTER, dd.to_string(), FontId::proportional(TXT_SMALL), color);
                r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, picked, format!("Day {dd}")));
                if r.clicked() {
                    *v = Some(Civil { day, ms: v.map_or(0, |c| c.ms) });
                    changed = true;
                }
            }
        }
        month = month.clamp(MIN_MONTH, MAX_MONTH);
        if moved && let Some(c) = *v {
            let (py, pm, _) = civil_from_days(c.day);
            *v = Some(c.add_months(month - month_idx(py, pm)));
            changed = true;
        }
        if o.time {
            ui.add_space(8.0);
            popover_divider(ui);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Time").size(TXT_SMALL).color(MUTED()));
                ui.add_space(8.0);
                let base = v.unwrap_or(o.now);
                let total = base.ms;
                let (mut h, mut mi, mut s, mut ms) = (total / 3_600_000, total / 60_000 % 60, total / 1000 % 60, total % 1000);
                let part = |ui: &mut egui::Ui, n: &mut i64, max: i64, w: usize, name: &str| {
                    ui.add(egui::DragValue::new(n).range(0..=max).speed(0.2).custom_formatter(move |x, _| format!("{:0w$}", x as i64)).custom_parser(|s| s.trim().parse::<f64>().ok())).on_hover_text(name).changed()
                };
                let mut moved = part(ui, &mut h, 23, 2, "Hours");
                ui.label(RichText::new(":").color(FAINT()));
                moved |= part(ui, &mut mi, 59, 2, "Minutes");
                ui.label(RichText::new(":").color(FAINT()));
                moved |= part(ui, &mut s, 59, 2, "Seconds");
                if o.millis {
                    ui.label(RichText::new(".").color(FAINT()));
                    moved |= part(ui, &mut ms, 999, 3, "Milliseconds");
                }
                if moved {
                    *v = Some(Civil { day: base.day, ms: ((h * 60 + mi) * 60 + s) * 1000 + ms });
                    changed = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if o.now_button && quiet(ui, "Now").on_hover_text("The clock in the zone this column is read in").clicked() {
                        *v = Some(o.now);
                        changed = true;
                    }
                });
            });
        } else {
            ui.add_space(6.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if quiet(ui, "Today").clicked() {
                    *v = Some(Civil { day: o.now.day, ms: 0 });
                    changed = true;
                }
            });
        }
        ui.add_space(8.0);
        match o.zone {
            Zone::Note(n) if !n.is_empty() => {
                ui.label(RichText::new(n).size(TXT_CAPTION).color(FAINT()));
                ui.add_space(8.0);
            }
            Zone::Note(_) => {}
            Zone::Switch { local, offset } => {
                ui.horizontal(|ui| {
                    let mut l = *local;
                    segmented(ui, &mut l, &[(false, "UTC"), (true, "Local")]);
                    if l != *local {
                        *local = l;
                        changed = true;
                    }
                    ui.add_space(6.0);
                    ui.label(RichText::new(if *local { offset.as_str() } else { "stored as UTC" }).monospace().size(TXT_CAPTION).color(FAINT()));
                });
                ui.add_space(8.0);
            }
        }
        popover_divider(ui);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if o.nullable && quiet(ui, "Set NULL").clicked() {
                out = Some(PickOut::Null);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if primary(ui, "Apply", PRIMARY()).on_hover_text("Stage this value (Enter)").clicked() {
                    out = Some(PickOut::Apply);
                }
                if quiet(ui, "Cancel").on_hover_text("Close the calendar and keep the text as it was (Esc)").clicked() {
                    out = Some(PickOut::Cancel);
                }
            });
        });
    });
    ctx.data_mut(|d| d.insert_temp(mid, CalState { month, seen: v.map(|c| c.day), choosing, pass }));
    (changed, out, rect)
}

/// A card floating over the page at `at` (its `pivot` corner), with the menu look. Returns the rect it covers.
fn float(ctx: &egui::Context, id: egui::Id, at: egui::Pos2, pivot: egui::Align2, margin: impl Into<egui::Margin>, add: impl FnOnce(&mut egui::Ui)) -> egui::Rect {
    egui::Area::new(id).order(egui::Order::Foreground).pivot(pivot).fixed_pos(at).constrain(true).show(ctx, |ui| {
        popup_card(ui).inner_margin(margin).show(ui, |ui| {
            menu_look(ui);
            add(ui);
        });
    })
    .response
    .rect
}

/// The fill of a row or button on a popup in its idle, hovered and pressed states.
fn menu_fill(r: &egui::Response) -> Color32 {
    [Color32::TRANSPARENT, MENU_HOVER(), menu_pressed()][state_of(r)]
}

/// The calendar's title as a button with a chevron, which opens the month and year chooser.
fn title_button(ui: &mut egui::Ui, text: &str, open: bool) -> egui::Response {
    let g = ui.painter().layout_job(egui::text::LayoutJob::single_section(text.to_string(), ui_fmt(TXT_TITLE, BRIGHT(), W_SEMIBOLD, 0.0)));
    let (rect, r) = ui.allocate_exact_size(egui::vec2(g.size().x + 26.0, BUTTON_H), Sense::click());
    ui.painter().rect_filled(rect, R_SM, if open && state_of(&r) == 0 { MENU_HOVER() } else { menu_fill(&r) });
    focus_ring(ui, &r, rect, R_SM as f32);
    galley_at(ui.painter(), rect.left() + 6.0, rect.center().y, g, BRIGHT());
    chevron(ui.painter(), egui::pos2(rect.right() - 10.0, rect.center().y), true, MUTED());
    let name = if open { "Back to days" } else { "Choose month and year" };
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, name));
    r.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(name)
}

/// A small round arrow button for the month pager, painted so it centers.
fn arrow(ui: &mut egui::Ui, left: bool, tip: &str) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(BUTTON_H, BUTTON_H), Sense::click());
    ui.painter().rect_filled(rect, R_SM, menu_fill(&r));
    let c = rect.center();
    let s = if left { -1.0 } else { 1.0 };
    let pts = vec![c + egui::vec2(-2.0 * s, -4.0), c + egui::vec2(2.0 * s, 0.0), c + egui::vec2(-2.0 * s, 4.0)];
    ui.painter().add(egui::Shape::line(pts, Stroke::new(1.5, if r.hovered() { BRIGHT() } else { MUTED() })));
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tip));
    r.on_hover_text(tip)
}

/// A calendar glyph, for the button that opens the picker.
pub(crate) fn paint_calendar(p: &egui::Painter, c: egui::Pos2, color: Color32) {
    let r = egui::Rect::from_center_size(c + egui::vec2(0.0, 0.5), egui::vec2(11.0, 10.0));
    let st = Stroke::new(1.2, color);
    p.rect_stroke(r, 1.5, st, egui::StrokeKind::Middle);
    p.line_segment([egui::pos2(r.left(), r.top() + 3.0), egui::pos2(r.right(), r.top() + 3.0)], st);
    for x in [r.left() + 3.0, r.right() - 3.0] {
        p.line_segment([egui::pos2(x, r.top() - 2.0), egui::pos2(x, r.top() + 1.0)], st);
    }
}

/// A square button with a painted glyph, sized like the editor's expand button.
pub(crate) fn glyph_button(ui: &mut egui::Ui, rect: egui::Rect, id: egui::Id, paint: fn(&egui::Painter, egui::Pos2, Color32), on: bool, tip: &str) -> egui::Response {
    let r = ui.interact(rect, id, Sense::click());
    let fill = if on { tint(PRIMARY(), 60) } else { [Color32::TRANSPARENT, HOVER(), PRESSED()][state_of(&r)] };
    ui.painter().rect_filled(rect, R_SM, fill);
    paint(ui.painter(), rect.center(), if on || r.hovered() { BRIGHT() } else { MUTED() });
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tip));
    r.on_hover_text(tip)
}

/// The line under an editor: a problem with its `alert` glyph and an optional fix, or a note in muted text such as
/// what a word saves as. `fix` labels a button that applies the fix. Returns the rect it covers and whether it was clicked.
/// `pivot` is the corner of the line placed at `at`, so it can sit above the cell when a list fills the space under it.
pub(crate) fn callout(ctx: &egui::Context, id: egui::Id, at: egui::Pos2, pivot: egui::Align2, msg: &str, alert: Option<fn(&egui::Painter, egui::Pos2, Color32)>, fix: Option<&str>) -> (egui::Rect, bool) {
    let mut clicked = false;
    let rect = float(ctx, id, at, pivot, egui::Margin::symmetric(10, 6), |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if let Some(paint) = alert {
                let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::hover());
                paint(ui.painter(), r.center(), ALERT());
            }
            ui.label(RichText::new(msg).size(TXT_SMALL).color(if alert.is_some() { BRIGHT() } else { MUTED() }));
            if let Some(f) = fix {
                clicked = primary(ui, f, PRIMARY()).clicked();
                ui.label(RichText::new("or keep typing").size(TXT_CAPTION).color(FAINT()));
            }
        });
    });
    (rect, clicked)
}

/// A list of values under the editor, as for an ENUM or a key looked up in another table. Each item is the text to
/// save and a note shown faint on the right. `hl` is the row Enter picks, if any. Returns the picked index (`items.len()`
/// for the NULL row) and the rect it covers.
pub(crate) fn choice_list(ctx: &egui::Context, id: egui::Id, at: egui::Pos2, w: f32, title: Option<&str>, items: &[(String, String)], saved: Option<&str>, hl: Option<usize>, null: bool, foot: &str) -> (Option<usize>, egui::Rect) {
    let mut picked = None;
    let rect = float(ctx, id, at, egui::Align2::LEFT_TOP, 6, |ui| {
        ui.set_width(w);
        ui.spacing_mut().item_spacing.y = 0.0;
        if let Some(t) = title {
            ui.add_space(2.0);
            ui.label(RichText::new(t).size(TXT_CAPTION).color(FAINT()));
            ui.add_space(4.0);
        }
        menu_scroll(ui, |ui| {
            for (i, (v, note)) in items.iter().enumerate() {
                if menu_row_in(ui, v, TEXT(), saved == Some(v.as_str()), note, "", hl == Some(i)).clicked() {
                    picked = Some(i);
                }
            }
        });
        if null {
            popover_divider(ui);
            if menu_row_in(ui, "NULL", C_NULL(), saved.is_none(), "", "", hl == Some(items.len())).clicked() {
                picked = Some(items.len());
            }
        }
        if !foot.is_empty() {
            ui.add_space(4.0);
            ui.label(RichText::new(foot).size(TXT_CAPTION).color(FAINT()));
        }
    });
    (picked, rect)
}

/// Up and Down move the highlight through `n` rows, wrapping; from no highlight, Down goes to the first and Up to the
/// last. Called before the text box, which would take them.
pub(crate) fn list_keys(ui: &egui::Ui, hl: &mut Option<usize>, n: usize) {
    if n == 0 {
        return;
    }
    ui.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, Key::ArrowDown) {
            *hl = Some(hl.map_or(0, |h| (h + 1) % n));
        }
        if i.consume_key(egui::Modifiers::NONE, Key::ArrowUp) {
            *hl = Some(hl.map_or(n - 1, |h| (h + n - 1) % n));
        }
    });
}

/// Checkboxes for a SET column. `on` holds the chosen options. Returns the index toggled and the rect it covers.
pub(crate) fn check_list(ctx: &egui::Context, id: egui::Id, at: egui::Pos2, w: f32, options: &[String], on: &[bool]) -> (Option<usize>, egui::Rect) {
    let mut toggled = None;
    let rect = float(ctx, id, at, egui::Align2::LEFT_TOP, 6, |ui| {
        ui.set_width(w);
        ui.spacing_mut().item_spacing.y = 0.0;
        menu_scroll(ui, |ui| {
            for (i, o) in options.iter().enumerate() {
                let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), PICK_H), Sense::click());
                ui.painter().rect_filled(rect, R_SM, menu_fill(&r));
                let b = check_box(ui, egui::pos2(rect.left() + 12.0, rect.center().y), id.with(("box", i)), on[i], false);
                ui.painter().text(egui::pos2(rect.left() + 28.0, rect.center().y), egui::Align2::LEFT_CENTER, o, FontId::monospace(TXT_SMALL), TEXT());
                r.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, on[i], o));
                if r.clicked() || b.clicked() {
                    toggled = Some(i);
                }
            }
        });
        ui.add_space(4.0);
        ui.label(RichText::new("Click to add or remove. Enter saves.").size(TXT_CAPTION).color(FAINT()));
    });
    (toggled, rect)
}

/// true, false and NULL as one switch under a boolean cell, with its keys. Returns the choice clicked and the rect.
pub(crate) fn bool_pick(ctx: &egui::Context, id: egui::Id, at: egui::Pos2, cur: Option<bool>, nullable: bool, label: &str) -> (Option<Option<bool>>, egui::Rect) {
    let mut out = None;
    let rect = float(ctx, id, at, egui::Align2::LEFT_TOP, 10, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(RichText::new(label).monospace().size(TXT_CAPTION).color(FAINT()));
        let mut v = cur;
        let opts: Vec<(Option<bool>, &str, bool)> = vec![(Some(true), "true", true), (Some(false), "false", true), (None, "NULL", nullable)];
        segmented_on(ui, &mut v, &opts);
        if v != cur {
            out = Some(v);
        }
        ui.label(RichText::new(if nullable { "Space flips it, T, F and N jump. 1 and 0 also work." } else { "Space flips it, T and F jump. 1 and 0 also work." }).size(TXT_CAPTION).color(FAINT()));
    });
    (out, rect)
}

/// The keys of `bool_pick`, read before anything else takes them.
pub(crate) fn bool_keys(ui: &egui::Ui, cur: Option<bool>, nullable: bool) -> Option<Option<bool>> {
    ui.input_mut(|i| {
        let mut hit = |k: Key| i.consume_key(egui::Modifiers::NONE, k);
        if hit(Key::Space) {
            Some(Some(!cur.unwrap_or(false)))
        } else if hit(Key::T) || hit(Key::Num1) {
            Some(Some(true))
        } else if hit(Key::F) || hit(Key::Num0) {
            Some(Some(false))
        } else if nullable && hit(Key::N) {
            Some(None)
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_and_mongo_text_round_trip() {
        let c = parse_sql("2026-11-09 12:30:19").unwrap();
        assert_eq!(sql_text(c, true), "2026-11-09 12:30:19");
        assert_eq!(sql_text(parse_sql("2026-11-09").unwrap(), false), "2026-11-09");
        let next = Civil { day: c.day + 1, ..c };
        assert_eq!(sql_text_like(next, true, "2026-11-09 12:30:19.123456+02"), "2026-11-10 12:30:19.123456+02");
        assert_eq!(sql_text_like(next, true, "2026-11-09 12:30"), "2026-11-10 12:30:19");
        assert!(parse_sql("2026-02-30").is_none());
        assert_eq!(parse_sql("2026-11-09 12:30:19.123+02").unwrap(), c);
        let ms = parse_mongo("2026-11-09T12:30:19.000Z").unwrap();
        assert_eq!(mongo_text(ms, None), "2026-11-09T12:30:19.000Z");
        assert_eq!(mongo_text(ms, Some(3600)), "2026-11-09T13:30:19.000+01:00");
        assert_eq!(parse_mongo("2026-11-09T13:30:19.000+01:00"), Some(ms));
        assert_eq!(Civil { day: days_from_civil(2026, 1, 31), ms: 5 }.add_months(1), Civil { day: days_from_civil(2026, 2, 28), ms: 5 });
    }
}

#[cfg(test)]
mod ui_tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    /// The calendar alone, showing November 2026 with the 9th picked.
    fn calendar() -> Harness<'static, Option<Civil>> {
        Harness::builder().with_accessibility_check(false).build_ui_state(
            |ui, v: &mut Option<Civil>| {
                let o = DateOpts { time: false, millis: false, zone: Zone::Note(String::new()), nullable: false, now: Civil { day: days_from_civil(2026, 11, 12), ms: 0 }, now_button: true };
                date_picker(ui.ctx(), egui::Id::unique("cal-test"), egui::pos2(10.0, 10.0), v, o);
            },
            Some(Civil { day: days_from_civil(2026, 11, 9), ms: 0 }),
        )
    }

    #[test]
    fn months_page_and_the_title_picks_a_month_and_year() {
        let mut h = calendar();
        h.run();
        h.get_by_label("Next month").click();
        h.run();
        h.get_by_label("Day 25").click();
        h.run();
        assert_eq!(h.state().map(|c| c.ymd()).as_deref(), Some("2026-12-25"));
        h.get_by_label("Choose month and year").click();
        h.run();
        h.get_by_label("Previous year").click();
        h.run();
        h.get_by_label("March").click();
        h.run();
        // Choosing a year and month moves the picked date with them, keeping its day.
        assert_eq!(h.state().map(|c| c.ymd()).as_deref(), Some("2025-03-25"));
        h.get_by_label("Day 15").click();
        h.run();
        assert_eq!(h.state().map(|c| c.ymd()).as_deref(), Some("2025-03-15"));
    }
}
