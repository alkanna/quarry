//! The activity rail: a thin column of icons left of the sidebar for the app's panels.

use super::*;

#[derive(Clone, Copy)]
pub(super) enum RailIcon {
    Explorer,
    Connections,
    Library,
    Search,
    Log,
    Server,
    Settings,
}

impl App {
    pub(super) fn rail(&mut self, ui: &mut egui::Ui) {
        let frame = egui::Frame::new().fill(CHROME()).inner_margin(egui::Margin { left: 8, right: 8, top: 10, bottom: 10 });
        egui::Panel::left("rail").exact_size(RAIL_W).resizable(false).show_separator_line(false).frame(frame).show(ui, |ui| {
            let full = ui.max_rect().expand2(egui::vec2(8.0, 10.0));
            ui.painter().vline(full.right() - 0.5, full.y_range(), Stroke::new(1.0, LINE()));
            ui.spacing_mut().item_spacing.y = 8.0;
            let connected = self.active.and_then(|id| self.conn(id)).is_some_and(|c| c.client.is_some() || c.sql.is_some());
            let palette = matches!(self.modal, Some(Modal::Palette { .. }));
            let mode = self.prefs.side_mode();
            let shown = !self.prefs.sidebar_hidden;
            if rail_button(ui, RailIcon::Explorer, shown && mode == SideMode::Explorer, true, "Database explorer").clicked() {
                self.pick_side_mode(SideMode::Explorer);
            }
            if conns_panel::READY && rail_button(ui, RailIcon::Connections, shown && mode == SideMode::Connections, true, "Saved connections").clicked() {
                self.pick_side_mode(SideMode::Connections);
            }
            if library::panel::READY && rail_button(ui, RailIcon::Library, shown && mode == SideMode::Library, true, "Script library").clicked() {
                self.pick_side_mode(SideMode::Library);
            }
            if rail_button(ui, RailIcon::Search, palette, true, "Jump to a collection (Cmd+K)").clicked() {
                self.open_palette();
            }
            if rail_button(ui, RailIcon::Log, self.log.open, true, "Query log (Cmd+L)").clicked() {
                self.log.open = !self.log.open;
            }
            let tip = if connected { "Server status" } else { "Server status (connect first)" };
            if rail_button(ui, RailIcon::Server, self.server.is_some() || self.sql_server.is_some() || self.pg_server.is_some(), connected, tip).clicked()
                && let Some(conn) = self.active
            {
                self.open_server(conn);
            }
            self.conn_shortcuts(ui);
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                if rail_button(ui, RailIcon::Settings, matches!(self.modal, Some(Modal::Settings)), true, "Settings (Cmd+,)").clicked() {
                    self.modal = Some(Modal::Settings);
                }
            });
        });
    }
}

impl App {
    /// A click on a panel button of the rail: the panel that is showing folds the sidebar away, any other shows in it.
    pub(super) fn pick_side_mode(&mut self, m: SideMode) {
        if !self.prefs.sidebar_hidden && self.prefs.side_mode() == m {
            self.prefs.sidebar_hidden = true;
        } else {
            self.prefs.set_side_mode(m);
            self.prefs.sidebar_hidden = false;
        }
        self.save_prefs();
    }

    /// One square per open connection under the panel buttons, scrolling when they outgrow the rail.
    fn conn_shortcuts(&mut self, ui: &mut egui::Ui) {
        let open: Vec<u64> = self.conns.iter().filter(|c| matches!(c.status, Status::Ready | Status::Connecting)).map(|c| c.meta.id).collect();
        sync_order(&mut self.rail_order, &open);
        if self.rail_order.is_empty() {
            return;
        }
        let (line, _) = ui.allocate_exact_size(egui::vec2(28.0, 1.0), Sense::hover());
        ui.painter().hline(line.shrink2(egui::vec2(6.0, 0.0)).x_range(), line.center().y, Stroke::new(1.0, LINE()));
        // Leaves room for the Settings button, which sits at the bottom of the rail.
        let room = (ui.available_height() - 36.0).max(28.0);
        let mut pick = None;
        let mut act = None;
        egui::ScrollArea::vertical().max_height(room).auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.spacing_mut().scroll.bar_width = 3.0;
            for &id in &self.rail_order {
                let Some(c) = self.conns.iter().find(|c| c.meta.id == id) else { continue };
                let r = conn_shortcut(ui, c, self.active == Some(id));
                if r.clicked() {
                    pick = Some(id);
                }
                r.context_menu(|ui| {
                    menu_look(ui);
                    if ui.button("Reconnect").clicked() {
                        act = Some((id, 0));
                    }
                    if ui.button("Edit connection…").clicked() {
                        act = Some((id, 1));
                    }
                    if danger_item(ui, "Disconnect").clicked() {
                        act = Some((id, 2));
                    }
                });
            }
        });
        if let Some(id) = pick {
            self.active = Some(id);
        }
        match act {
            Some((id, 0)) => {
                self.active = Some(id);
                self.connect(id);
            }
            Some((id, 1)) => self.edit_conn(id),
            Some((id, _)) => self.disconnect(id),
            None => {}
        }
    }

    pub(super) fn open_server(&mut self, conn: u64) {
        self.req += 1;
        match self.conn(conn).map(|c| (c.meta.engine, c.sql.as_ref().map(|d| d.dialect), c.meta.advanced.clone())) {
            Some((db::sql::Engine::MySql, dialect, adv)) => self.sql_server = Some(SqlServerWin::new(conn, dialect.map(|d| db::sql::server::set_by_quarry(&adv, d, db::sql::advanced::local_offset_secs())).unwrap_or_default())),
            Some((db::sql::Engine::Postgres, _, adv)) => self.pg_server = Some(PgServerWin::new(conn, db::sql::pg::server::set_by_quarry(&adv))),
            _ => self.server = Some(ServerWin::new(conn, self.req)),
        }
    }

    /// Drops the session but keeps the saved connection, which then leaves the rail.
    pub(super) fn disconnect(&mut self, id: u64) {
        let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == id) else { return };
        (c.client, c.sql, c.tunnel, c.status, c.writable) = (None, None, None, Status::Idle, false);
        c.forget_lists();
        self.pg_tables_asked.forget(id, None, None);
        let name = c.meta.name.clone();
        self.ok(format!("Disconnected from {name}"));
    }
}

/// Keeps the ids still open in their old order and appends the newly open ones, so a shortcut
/// never jumps when another connection opens or closes.
fn sync_order(order: &mut Vec<u64>, open: &[u64]) {
    order.retain(|id| open.contains(id));
    for id in open {
        if !order.contains(id) {
            order.push(*id);
        }
    }
}

/// Two-letter mark and product name. MariaDB shares the MySQL engine, so only the server
/// reply, once connected, tells it apart.
pub(super) fn engine_mark(engine: db::sql::Engine, product: &str) -> (&'static str, &'static str) {
    match (engine, product) {
        (db::sql::Engine::Mongo, _) => ("MO", "MongoDB"),
        (db::sql::Engine::Postgres, _) => ("PG", "PostgreSQL"),
        (_, "MariaDB") => ("MA", "MariaDB"),
        _ => ("MY", "MySQL"),
    }
}

/// The colors of the connection form's engine cards, so an engine reads the same everywhere.
pub(super) fn mark_color(mark: &str) -> Color32 {
    match mark {
        "MO" => C_STRING(),
        "MA" => C_KEYWORD(),
        "PG" => ENGINE_PG(),
        _ => C_LINK(),
    }
}

/// The engine's logo as a white mask, tinted when painted. Decoded once and kept in egui's memory.
/// The shapes are Simple Icons' (CC0), baked to 64 px PNGs so the binary needs no SVG renderer.
pub(super) fn engine_logo(ctx: &egui::Context, mark: &str) -> Option<egui::TextureHandle> {
    if mark == "PG" {
        // No permissively licensed PostgreSQL mark was available offline, so the "PG" monogram is drawn here.
        let id = egui::Id::unique("engine-postgres");
        return ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)).or_else(|| {
            let tex = ctx.load_texture("engine-postgres", pg_monogram(), egui::TextureOptions::LINEAR);
            ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
            Some(tex)
        });
    }
    let (name, png): (&str, &[u8]) = match mark {
        "MO" => ("engine-mongodb", include_bytes!("../../assets/engines/mongodb.png")),
        "MA" => ("engine-mariadb", include_bytes!("../../assets/engines/mariadb.png")),
        _ => ("engine-mysql", include_bytes!("../../assets/engines/mysql.png")),
    };
    let id = egui::Id::unique(name);
    ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)).or_else(|| {
        let icon = eframe::icon_data::from_png_bytes(png).ok()?;
        let img = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
        let tex = ctx.load_texture(name, img, egui::TextureOptions::LINEAR);
        ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
        Some(tex)
    })
}

/// "PG" as a white 64 px mask, rasterized from two stroked outlines by distance to the segments.
fn pg_monogram() -> egui::ColorImage {
    const P: &[(f32, f32)] = &[(14.0, 52.0), (14.0, 12.0), (26.0, 12.0), (32.0, 16.0), (34.0, 23.0), (32.0, 30.0), (26.0, 34.0), (14.0, 34.0)];
    const G: &[(f32, f32)] = &[(58.0, 21.0), (53.0, 14.0), (47.0, 12.0), (41.0, 15.0), (38.0, 22.0), (38.0, 42.0), (41.0, 49.0), (47.0, 52.0), (54.0, 50.0), (58.0, 44.0), (58.0, 36.0), (48.0, 36.0)];
    const HALF: f32 = 3.5;
    fn dist(p: egui::Vec2, a: (f32, f32), b: (f32, f32)) -> f32 {
        let (a, b) = (egui::vec2(a.0, a.1), egui::vec2(b.0, b.1));
        let ab = b - a;
        let t = ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0);
        (p - (a + ab * t)).length()
    }
    let mut img = egui::ColorImage::filled([64, 64], Color32::TRANSPARENT);
    for y in 0..64 {
        for x in 0..64 {
            let p = egui::vec2(x as f32 + 0.5, y as f32 + 0.5);
            let d = [P, G].iter().flat_map(|line| line.windows(2).map(|w| dist(p, w[0], w[1]))).fold(f32::MAX, f32::min);
            let a = ((HALF + 0.5 - d).clamp(0.0, 1.0) * 255.0) as u8;
            img.pixels[y * 64 + x] = Color32::from_rgba_unmultiplied(255, 255, 255, a);
        }
    }
    img
}

/// A 28 px square with the engine logo, a status dot at the bottom right and, for a tagged
/// connection, a bar in the tag color on the left edge.
fn conn_shortcut(ui: &mut egui::Ui, c: &Conn, active: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), Sense::click());
    let state = state_of(&r);
    let (mark, product) = engine_mark(c.meta.engine, &c.server.product);
    let color = mark_color(mark);
    let fill = if active { tint(PRIMARY(), if state == 2 { 90 } else { 64 }) } else { hover_fill(state) };
    let p = ui.painter();
    p.rect_filled(rect, R_MD, fill);
    if !active && state == 0 {
        p.rect_filled(rect.shrink(3.0), R_SM, tint(color, 40));
    }
    let fg = if active || state > 0 { BRIGHT() } else { color };
    if let Some(tex) = engine_logo(ui.ctx(), mark) {
        let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
        p.image(tex.id(), egui::Rect::from_center_size(rect.center(), egui::vec2(16.0, 16.0)), uv, fg);
    }
    if !c.meta.tag.is_empty() {
        p.rect_filled(egui::Rect::from_min_size(rect.left_top() + egui::vec2(0.0, 6.0), egui::vec2(2.0, 16.0)), 1, tag_color(&c.meta.tag));
    }
    // A ring in the rail's own color keeps the dot readable over the tinted square.
    let dot = rect.right_bottom() - egui::vec2(4.5, 4.5);
    p.circle_filled(dot, 4.5, CHROME());
    p.circle_filled(dot, 3.0, status_color(&c.status));
    focus_ring(ui, &r, rect, R_MD as f32);
    let mut tip = c.meta.name.clone();
    if !c.meta.tag.is_empty() {
        tip += &format!("  ({})", c.meta.tag);
    }
    tip += &format!("\n{product}");
    if !c.server.version.is_empty() {
        tip += &format!(" {}", c.server.version);
    }
    tip += if c.status == Status::Connecting { "\nConnecting" } else if c.writable { "\nWrites on" } else { "\nRead-only" };
    r.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A 28 px square icon button: a `HOVER` fill on hover, `PRESSED` while held, and an accent
/// tint with a bright icon while its panel is open.
fn rail_button(ui: &mut egui::Ui, icon: RailIcon, active: bool, enabled: bool, tip: &str) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), if enabled { Sense::click() } else { Sense::hover() });
    // A disabled button only senses hover, so it never reports a press.
    let state = state_of(&r);
    let fill = if active { tint(PRIMARY(), if state == 2 { 90 } else { 64 }) } else if enabled { hover_fill(state) } else { Color32::TRANSPARENT };
    let p = ui.painter();
    p.rect_filled(rect, R_MD, fill);
    let color = if !enabled { FAINT() } else if active || state > 0 { BRIGHT() } else { MUTED() };
    paint_icon(p, rect.center(), icon, Stroke::new(1.3, color));
    focus_ring(ui, &r, rect, R_MD as f32);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, tip));
    let r = r.on_hover_text(tip);
    if enabled { r.on_hover_cursor(egui::CursorIcon::PointingHand) } else { r }
}

/// Line icons drawn in a 16 px box, so they need no icon font.
pub(super) fn paint_icon(p: &egui::Painter, c: egui::Pos2, icon: RailIcon, s: Stroke) {
    let v = egui::vec2;
    match icon {
        // A window with a left panel.
        RailIcon::Explorer => {
            let r = egui::Rect::from_center_size(c, v(15.0, 13.0));
            p.rect_stroke(r, 2.5, s, egui::StrokeKind::Middle);
            p.vline(r.left() + 5.0, r.y_range(), s);
        }
        // A database cylinder: the top ellipse, the sides and two curves below.
        RailIcon::Connections => {
            let arc = |dy: f32, from: f32, to: f32| -> Vec<egui::Pos2> { (0..=16).map(|i| from + (to - from) * i as f32 / 16.0).map(|a| c + v(6.0 * a.cos(), dy + 2.4 * a.sin())).collect() };
            p.add(egui::Shape::closed_line(arc(-4.8, 0.0, std::f32::consts::TAU), s));
            p.line_segment([c + v(-6.0, -4.8), c + v(-6.0, 4.8)], s);
            p.line_segment([c + v(6.0, -4.8), c + v(6.0, 4.8)], s);
            p.line(arc(0.0, 0.0, std::f32::consts::PI), s);
            p.line(arc(4.8, 0.0, std::f32::consts::PI), s);
        }
        // A book: the cover with its spine and two lines of text.
        RailIcon::Library => {
            let r = egui::Rect::from_center_size(c, v(11.5, 14.0));
            p.rect_stroke(r, 2.0, s, egui::StrokeKind::Middle);
            p.vline(r.left() + 3.0, r.y_range(), s);
            for dy in [-2.5, 1.0] {
                p.line_segment([c + v(-1.0, dy), c + v(3.0, dy)], s);
            }
        }
        RailIcon::Search => {
            p.circle_stroke(c + v(-1.5, -1.5), 5.0, s);
            p.line_segment([c + v(2.2, 2.2), c + v(6.0, 6.0)], s);
        }
        // Rows of a log, with a bullet each.
        RailIcon::Log => {
            for dy in [-4.5, 0.0, 4.5] {
                p.circle_filled(c + v(-5.5, dy), 1.1, s.color);
                p.line_segment([c + v(-2.5, dy), c + v(6.5, dy)], s);
            }
        }
        // A pulse line.
        RailIcon::Server => {
            let pts = [v(-7.5, 0.5), v(-4.0, 0.5), v(-2.0, -5.0), v(1.0, 5.5), v(3.0, 0.5), v(7.5, 0.5)];
            p.line(pts.iter().map(|d| c + *d).collect(), s);
        }
        // A gear: ring, hub and eight teeth.
        RailIcon::Settings => {
            p.circle_stroke(c, 4.6, s);
            p.circle_stroke(c, 1.8, s);
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                let d = v(a.cos(), a.sin());
                p.line_segment([c + d * 4.6, c + d * 7.0], s);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_keep_the_order_they_opened_in() {
        let mut order = vec![];
        sync_order(&mut order, &[3, 1]);
        assert_eq!(order, [3, 1]);
        sync_order(&mut order, &[1, 2, 3]);
        assert_eq!(order, [3, 1, 2]);
        sync_order(&mut order, &[2, 3]);
        assert_eq!(order, [3, 2]);
    }

    #[test]
    fn engine_marks() {
        use db::sql::Engine::{Mongo, MySql};
        assert_eq!(engine_mark(Mongo, "").0, "MO");
        assert_eq!(engine_mark(MySql, "MySQL").0, "MY");
        assert_eq!(engine_mark(MySql, "").0, "MY");
        assert_eq!(engine_mark(MySql, "MariaDB"), ("MA", "MariaDB"));
    }
}
