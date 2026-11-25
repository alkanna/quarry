//! Where errors show: the session list behind the status bar count, toasts, the panel in a view
//! whose read failed, the card of a connection that failed, and the details dialog.

use super::*;
use db::{ErrKind, Fix};
use quarry_core::action::{Action, AppA, LibA, PrefsA};
use quarry_core::core::notify::fix_label;
use std::time::Instant;
pub(crate) use quarry_core::state::errors::*;

const TOAST_W: f32 = 380.0;
const PANEL_W: f32 = 620.0;

/// What Copy details puts on the clipboard: the explanation and the raw text, without passwords.
fn details_text(e: &ErrEntry) -> String {
    let mut s = format!("{}\n", e.ex.title);
    if !e.ex.hint.is_empty() {
        s += &format!("{}\n", e.ex.hint);
    }
    s += "\n";
    for (k, v) in detail_rows(e) {
        s += &format!("{k}: {v}\n");
    }
    s += &format!("\nServer message:\n{}\n", db::scrub_uri(&e.raw));
    s
}

fn detail_rows(e: &ErrEntry) -> Vec<(&'static str, String)> {
    let mut rows = Vec::new();
    if !e.conn_name.is_empty() {
        rows.push(("Connection", e.conn_name.clone()));
    }
    if !e.user.is_empty() {
        rows.push(("User", e.user.clone()));
    }
    if let Some(s) = &e.sel {
        rows.push(("Namespace", if s.coll.is_empty() { s.db.clone() } else { format!("{}.{}", s.db, s.coll) }));
    }
    if let Some(c) = &e.ex.code {
        rows.push(("Code", c.clone()));
    }
    if let Some((need, role)) = &e.ex.need {
        rows.push(("Needs", match role {
            Some(r) => format!("{need} (role: {r})"),
            None => need.clone(),
        }));
    }
    rows
}

fn tone_color(t: Tone) -> Color32 {
    match t {
        Tone::Ok => SUCCESS(),
        Tone::Warn => WARNING(),
        Tone::Err => ALERT(),
    }
}

/// Connection trouble is often passing, so it reads as a warning; refusals and failures as errors.
fn kind_color(k: ErrKind) -> Color32 {
    match k {
        ErrKind::Timeout | ErrKind::Lost | ErrKind::Unreachable => WARNING(),
        _ => ALERT(),
    }
}

fn paint_kind(p: &egui::Painter, c: egui::Pos2, k: ErrKind, color: Color32) {
    let s = Stroke::new(1.5, color);
    let pt = |x: f32, y: f32| c + egui::vec2(x, y);
    match k {
        ErrKind::Privilege => {
            p.rect_stroke(egui::Rect::from_min_max(pt(-6.0, -1.0), pt(6.0, 7.5)), 2.0, s, egui::StrokeKind::Middle);
            let arc: Vec<egui::Pos2> = (0..=8).map(|i| std::f32::consts::PI * i as f32 / 8.0).map(|a| pt(-3.5 * a.cos(), -4.0 - 3.5 * a.sin())).collect();
            p.add(egui::Shape::line(arc, s));
            p.line_segment([pt(-3.5, -4.0), pt(-3.5, -1.0)], s);
            p.line_segment([pt(3.5, -4.0), pt(3.5, -1.0)], s);
            p.circle_filled(pt(0.0, 3.2), 1.2, color);
        }
        ErrKind::SignIn => {
            p.circle_stroke(pt(-3.0, 0.0), 4.0, s);
            p.line_segment([pt(1.0, 0.0), pt(8.0, 0.0)], s);
            p.line_segment([pt(6.0, 0.0), pt(6.0, 3.0)], s);
            p.line_segment([pt(3.5, 0.0), pt(3.5, 2.5)], s);
        }
        ErrKind::Timeout => paint_clock(p, c, color),
        ErrKind::Lost | ErrKind::Unreachable | ErrKind::Tunnel | ErrKind::Proxy | ErrKind::CloudSql => {
            p.rect_stroke(egui::Rect::from_min_max(pt(-5.0, -3.0), pt(5.0, 4.0)), 2.0, s, egui::StrokeKind::Middle);
            p.line_segment([pt(-2.5, -3.0), pt(-2.5, -7.0)], s);
            p.line_segment([pt(2.5, -3.0), pt(2.5, -7.0)], s);
            p.line_segment([pt(0.0, 4.0), pt(0.0, 8.0)], s);
            p.line_segment([pt(-8.0, -8.0), pt(8.0, 8.0)], s);
        }
        ErrKind::Missing => {
            p.rect_stroke(egui::Rect::from_min_max(pt(-6.5, -7.0), pt(6.5, 7.0)), 2.0, s, egui::StrokeKind::Middle);
            p.line_segment([pt(-3.0, -2.0), pt(3.0, -2.0)], s);
            p.line_segment([pt(-3.0, 1.5), pt(1.0, 1.5)], s);
        }
        _ => {
            p.circle_stroke(c, 7.5, s);
            p.line_segment([pt(0.0, -3.6), pt(0.0, 0.8)], Stroke::new(1.6, color));
            p.circle_filled(pt(0.0, 3.6), 1.0, color);
        }
    }
}

pub(super) fn paint_alert(p: &egui::Painter, c: egui::Pos2, color: Color32) {
    paint_kind(p, c, ErrKind::Other, color);
}

/// The tinted round icon in front of an error.
fn kind_icon(ui: &mut egui::Ui, k: ErrKind, r: f32) {
    let color = kind_color(k);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(r * 2.0, r * 2.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), r, tint(color, 34));
    paint_kind(ui.painter(), rect.center(), k, color);
}

/// The raw server text in a well, without passwords.
fn raw_well(ui: &mut egui::Ui, e: &ErrEntry, color: Color32) {
    egui::Frame::new().fill(pal().well).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_BAR).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(db::scrub_uri(&e.raw)).monospace().size(TXT_CAPTION).color(color)).wrap().selectable(true));
        });
    });
}

/// "Server message" with the code chip, folding the raw text. Open state is kept per error.
fn raw_block(ui: &mut egui::Ui, label: &str, e: &ErrEntry, open_first: bool) {
    let key = egui::Id::unique(("err-raw", e.id));
    let mut open = ui.data(|d| d.get_temp::<bool>(key)).unwrap_or(open_first);
    ui.horizontal(|ui| {
        // The chevron sits inside the text column, not in the gutter left of it.
        ui.add_space(14.0);
        let r = ui.add(egui::Label::new(RichText::new(label).size(TXT_SMALL).color(MUTED())).sense(Sense::click()));
        chevron(ui.painter(), egui::pos2(r.rect.left() - 8.0, r.rect.center().y), open, MUTED());
        if let Some(c) = &e.ex.code {
            ui.add_space(4.0);
            badge(ui, c, ALERT());
        }
        if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            open = !open;
        }
    });
    ui.data_mut(|d| d.insert_temp(key, open));
    if open {
        ui.add_space(6.0);
        raw_well(ui, e, MUTED());
    }
}

impl App {
    /// The newest error and the toasts on screen, for the message a timed-out UI test panics with.
    #[cfg(test)]
    pub(super) fn last_problem(&self) -> String {
        let toasts: Vec<String> = self.core.toasts.iter().map(|t| format!("{}: {}", t.title, t.body)).collect();
        format!("{} (toasts: {toasts:?})", self.core.errors.last().map_or_else(|| "no error logged".into(), |e| e.raw.clone()))
    }

    // The toasts and the error list live in Core; these forward so the app's call sites stay short.
    pub(super) fn log_err(&mut self, raw: impl Into<String>, conn: Option<u64>, sel: Option<Sel>, place: String, what: &str) -> ErrEntry {
        self.core.log_err(raw, conn, sel, place, what)
    }

    pub(super) fn err(&mut self, s: impl Into<String>) {
        self.core.err(s)
    }

    pub(super) fn conn_card_shown(&self, conn: u64) -> bool {
        self.core.conn_card_shown(conn)
    }

    pub(super) fn warn(&mut self, s: impl Into<String>) {
        self.core.warn(s)
    }

    pub(super) fn warn_ro(&mut self) {
        self.core.warn_ro()
    }

    pub(super) fn warn_offline(&mut self) {
        self.core.warn_offline()
    }

    /// A failure of work the user did not start, like listing a database's collections on
    /// connect: it goes to the list with its own connection and place, without a toast.
    pub(super) fn err_quiet(&mut self, raw: impl Into<String>, conn: Option<u64>, sel: Option<Sel>, place: String) {
        self.core.err_quiet(raw, conn, sel, place)
    }

    /// The read for `view` in tab `i` failed: the view shows the panel until its next read works.
    pub(super) fn fail_view(&mut self, i: usize, view: View, raw: String, what: &str) {
        self.core.fail_view(i, view, raw, what)
    }

    pub(super) fn toast_ok(&mut self, s: String, undo_at: Option<Instant>) {
        self.core.toast_ok(s, undo_at)
    }

    /// A note with buttons that fades like a success, such as a script waiting for its connection with Cancel.
    pub(super) fn toast_acts(&mut self, s: String, acts: Vec<(String, Act)>) {
        self.core.toast_acts(s, acts)
    }

    /// A success note whose Undo puts back something that is not a database edit, which `apply_undo` could not do on a
    /// read-only connection: a group, saved scripts or a file.
    pub(super) fn toast_restore(&mut self, s: String, r: Restore) {
        self.core.toast_restore(s, r)
    }

    /// Runs a toast, panel or card button. `tab` is the tab whose panel it was on.
    pub(super) fn run_act(&mut self, ctx: &egui::Context, act: Act, e: Option<&ErrEntry>, tab: Option<(usize, View)>) {
        let conn = e.and_then(|e| e.conn);
        match act {
            Act::Fix(Fix::Retry) => match (tab, conn) {
                (Some((i, v)), _) => self.retry_view(i, v),
                (None, Some(c)) => self.connect_active(c),
                _ => {}
            },
            Act::Fix(Fix::EditConn) => {
                if let Some(c) = conn {
                    self.edit_conn(c);
                }
            }
            Act::Fix(Fix::Explain) => {
                if let Some((i, _)) = tab {
                    self.set_cur(i);
                }
                self.explain();
            }
            Act::Fix(Fix::RaiseLimit) => {
                self.exec(Action::App(AppA::RaiseLimit));
                if let Some((i, v)) = tab {
                    self.retry_view(i, v);
                }
            }
            Act::Fix(Fix::Refresh) => {
                let Some(c) = conn else { return };
                // Reloading the one database's list keeps the connection, its tabs and an unlock; reconnecting is the fallback.
                // A dropped database would stay listed, since only connecting reads the list of databases.
                let gone = e.is_some_and(|e| db::database_gone(&e.raw));
                let reloaded = !gone && e.and_then(|e| e.sel.clone()).is_some_and(|s| self.refresh_db(c, &s.db));
                if !reloaded {
                    self.connect(c);
                }
            }
            Act::Fix(Fix::Validator) => {
                if let Some((i, _)) = tab {
                    self.set_cur(i);
                }
                self.set_view(View::Schema);
            }
            Act::Fix(Fix::Structure) => {
                if let Some((i, _)) = tab {
                    self.set_cur(i);
                }
                self.set_view(View::Structure);
            }
            Act::Undo(at) => {
                if let Some(i) = self.core.undo.iter().position(|a| a.at == at) {
                    self.apply_undo(i);
                }
            }
            Act::Restore(id) => self.apply_restore(id),
            Act::EditGroup(g) => self.edit_group(g),
            Act::Unlock(c) => self.modal = Some(Modal::Unlock(c, None)),
            Act::Connect(c) => self.connect_active(c),
            Act::Settings => self.exec(Action::Prefs(PrefsA::Open(settings::SettingsCat::Tools))),
            Act::Details => {
                if let Some(e) = e {
                    self.exec(Action::App(AppA::Details(e.id)));
                }
            }
            Act::Copy => {
                if let Some(e) = e {
                    ctx.copy_text(details_text(e));
                }
            }
            Act::CancelOpen => self.exec(Action::Library(LibA::CancelPending)),
            Act::ShowDup => {
                let (Some(e), Some((f, v))) = (e, e.and_then(|e| e.ex.dup.clone())) else { return };
                let Some(sel) = e.sel.clone() else { return };
                if self.conn(sel.conn).is_some_and(|c| c.meta.engine.is_sql()) {
                    let Some(index) = db::dup_filterable(&e.raw, &sel.coll) else { return };
                    return self.sql_show_dup(sel, index, v);
                }
                self.open_collection(sel, Open::Here);
                if let Some(t) = self.tm() {
                    t.view = View::Documents;
                    t.filter_txt = format!("{{ {f}: {v} }}");
                }
                self.run_query();
            }
        }
    }

    fn connect_active(&mut self, c: u64) {
        self.core.active = Some(c);
        self.connect(c);
    }

    /// Asks again for what the failed view shows.
    fn retry_view(&mut self, i: usize, view: View) {
        if i >= self.core.tabs.len() {
            return;
        }
        self.core.tabs[i].clear_failed(view);
        match view {
            View::Indexes => self.load_indexes_of(i),
            View::Schema => {
                // The view analyzes again by itself once nothing says it ran.
                let s = &mut self.core.tabs[i].schema;
                (s.err, s.ms) = (None, None);
            }
            View::Aggregate => {
                let here = self.core.cur;
                self.core.cur = i;
                let text = self.core.tabs[i].agg_txt.clone();
                match db::parse_pipeline(&text) {
                    Ok(p) => self.run_pipeline(p),
                    Err(e) => self.err(e),
                }
                self.core.cur = here;
            }
            _ => self.reload_of(i),
        }
    }

    /// The toasts, newest at the bottom, above the status bar.
    pub(super) fn toasts(&mut self, ctx: &egui::Context) {
        let fading = self.core.toasts_tick();
        if self.core.toasts.is_empty() {
            return;
        }
        if fading {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        let mut close = None;
        let mut run: Option<(Act, Option<ErrEntry>, u64)> = None;
        egui::Area::new(egui::Id::unique(("quarry-layer", "toasts"))).order(egui::Order::Foreground).anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -(STATUS_H + 14.0))).show(ctx, |ui| {
            ui.set_width(TOAST_W);
            ui.spacing_mut().item_spacing.y = 10.0;
            for t in &self.core.toasts {
                let color = tone_color(t.tone);
                egui::Frame::new().fill(RAISED()).stroke(Stroke::new(1.0, color)).corner_radius(R_LG).shadow(egui::epaint::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(46) }).inner_margin(egui::Margin { left: 16, right: 10, top: 10, bottom: 10 }).show(ui, |ui| {
                    ui.set_width(TOAST_W - 26.0);
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 18.0), Sense::hover());
                        match t.tone {
                            Tone::Ok => paint_check(ui.painter(), r.center(), color),
                            Tone::Warn if matches!(t.acts.first(), Some((_, Act::Unlock(_)))) => paint_kind(ui.painter(), r.center(), ErrKind::Privilege, color),
                            _ => paint_kind(ui.painter(), r.center(), ErrKind::Other, color),
                        }
                        ui.add_space(4.0);
                        // A wrapping label would take the whole row and push the close button out.
                        let room = ui.available_width() - if t.tone == Tone::Ok && !t.acts.is_empty() { 72.0 } else { 28.0 };
                        ui.scope(|ui| {
                            ui.set_max_width(room);
                            ui.add(egui::Label::new(semibold(RichText::new(&t.title).size(TXT_BODY).color(BRIGHT()))).wrap());
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if close_x(ui).on_hover_text("Dismiss").clicked() {
                                close = Some(t.id);
                            }
                            if t.tone == Tone::Ok {
                                for (label, act) in &t.acts {
                                    if quiet(ui, RichText::new(label).color(PRIMARY())).clicked() {
                                        run = Some((act.clone(), None, t.id));
                                    }
                                }
                            }
                        });
                    });
                    if !t.body.is_empty() {
                        ui.horizontal(|ui| {
                            ui.add_space(24.0);
                            ui.add(egui::Label::new(RichText::new(&t.body).size(TXT_SMALL).color(TEXT())).wrap());
                        });
                    }
                    if t.tone != Tone::Ok && !t.acts.is_empty() {
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            ui.add_space(24.0);
                            for (k, (label, act)) in t.acts.iter().enumerate() {
                                let hit = if k == 0 && t.tone == Tone::Warn { primary(ui, label, PRIMARY()) } else { ui.button(label.as_str()) };
                                if hit.clicked() {
                                    run = Some((act.clone(), t.err.clone(), t.id));
                                }
                            }
                        });
                    }
                });
            }
        });
        if let Some((act, e, id)) = run {
            // Copy and Details leave the error up; acting on the fix closes it.
            if !matches!(act, Act::Copy | Act::Details) {
                close = Some(id);
            }
            self.run_act(ctx, act, e.as_ref(), None);
        }
        if let Some(id) = close {
            self.exec(Action::App(AppA::Dismiss(id)));
        }
    }

    /// The panel a view shows when its read failed. True when there was one to show.
    pub(super) fn failed_panel(&mut self, ui: &mut egui::Ui, view: View) -> bool {
        let i = self.core.cur;
        let timeout_ms = self.core.prefs.timeout_ms;
        let Some(e) = self.core.tabs.get(i).and_then(|t| t.failed.iter().find(|f| f.view == view)).map(|f| &f.e) else { return false };
        let mut run = None;
        ui.add_space(40.0);
        let w = ui.available_width().min(PANEL_W);
        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
            ui.allocate_ui_with_layout(egui::vec2(w, 0.0), egui::Layout::left_to_right(egui::Align::Min), |ui| {
                kind_icon(ui, e.ex.kind, 18.0);
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.add(egui::Label::new(semibold(RichText::new(&e.ex.title).size(TXT_SUBHEAD).color(BRIGHT()))).wrap());
                    if !e.ex.hint.is_empty() {
                        ui.add(egui::Label::new(RichText::new(&e.ex.hint).size(TXT_BODY).color(TEXT())).wrap());
                    }
                    if let Some((need, role)) = &e.ex.need {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Needs").size(TXT_SMALL).color(MUTED()));
                            badge(ui, need, PRIMARY());
                            if let Some(r) = role {
                                ui.label(RichText::new(format!("· the {r} role grants it")).size(TXT_SMALL).color(MUTED()));
                            }
                        });
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        // Compare has its own Run button above the panel, so it offers no Retry.
                        let fixes: Vec<Fix> = e.ex.fixes.iter().copied().filter(|f| !(view == View::Compare && *f == Fix::Retry)).collect();
                        for (k, f) in fixes.iter().enumerate() {
                            let label = fix_label(*f, timeout_ms);
                            let hit = if k == 0 { primary(ui, &label, PRIMARY()) } else { ui.button(label) };
                            if hit.clicked() {
                                run = Some(Act::Fix(*f));
                            }
                        }
                        if ui.button("Copy error").clicked() {
                            run = Some(Act::Copy);
                        }
                    });
                    ui.add_space(10.0);
                    raw_block(ui, "Server message", e, e.ex.kind == ErrKind::Privilege);
                });
            });
        });
        if let Some(a) = run {
            let e = e.clone();
            self.run_act(&ui.ctx().clone(), a, Some(&e), Some((i, view)));
        }
        true
    }

    /// The card shown for a connection that could not open, with the steps that passed and the one that failed.
    pub(super) fn conn_failed_card(&mut self, ui: &mut egui::Ui, id: u64) {
        let Some(c) = self.conn(id) else { return };
        let Status::Failed(raw) = &c.status else { return };
        let raw = raw.clone();
        let (name, uri) = (c.meta.name.clone(), c.masked_uri.clone());
        let ssh = c.meta.ssh.is_some();
        let proxy = c.meta.proxy.is_some();
        // The latest logged error of this connection; after Clear, one built by the same rule.
        let e = match self.core.errors.iter().rev().find(|e| e.conn == Some(id) && e.raw == raw) {
            Some(e) => e.clone(),
            None => self.core.entry(raw.clone(), Some(id), None, format!("Connect · {name}"), "connect", 0),
        };
        if e.ex.kind == ErrKind::CloudSql {
            return self.cloud_failed_card(ui, id, &e);
        }
        let host = host_of(&uri);
        // The server never answered, so its TLS flag is unknown; the address says whether TLS was asked for.
        let tls = uri.contains("tls=true") || uri.contains("ssl=true") || uri.starts_with("mongodb+srv") || e.ex.kind == ErrKind::Tls;
        let user = e.user.clone();
        let mut steps: Vec<String> = Vec::new();
        if ssh {
            steps.push("Opened the SSH tunnel".into());
        }
        if proxy {
            steps.push("Reached the SOCKS5 proxy".into());
        }
        steps.push(format!("Found {host}"));
        steps.push("Opened a connection".into());
        if tls {
            steps.push("Opened a TLS connection".into());
        }
        if !user.is_empty() {
            steps.push(format!("Signed in as {user}"));
        }
        steps.push("Listed the databases".into());
        let low = raw.to_lowercase();
        let failed = match e.ex.kind {
            ErrKind::Tunnel | ErrKind::Proxy | ErrKind::CloudSql => 0,
            ErrKind::Unreachable if low.contains("lookup") || low.contains("servname") || low.contains("not known") => usize::from(ssh || proxy),
            ErrKind::Unreachable | ErrKind::Lost => usize::from(ssh || proxy) + 1,
            ErrKind::Tls => usize::from(ssh || proxy) + 2,
            ErrKind::SignIn => steps.iter().position(|s| s.starts_with("Signed in")).unwrap_or(steps.len() - 1),
            _ => steps.len() - 1,
        };
        let checks: &[&str] = match e.ex.kind {
            ErrKind::SignIn => &["The password was changed, or was saved for another user.", "The user is defined in another database than the auth source.", "On Atlas, the database user exists in this project and its IP is allowed."],
            ErrKind::Unreachable | ErrKind::Lost => &["The host name and port are right.", "A VPN is needed to reach this network.", "The server only accepts connections through an SSH tunnel."],
            ErrKind::Tls => &["The CA file in the connection's TLS settings signs this server's certificate.", "The host name matches the certificate."],
            ErrKind::Proxy => &["The proxy host and port are right, and the proxy is running.", "The proxy user and password are right, if it asks for one.", "The proxy is allowed to connect to the database host and port."],
            ErrKind::Tunnel => &["The SSH host, port, user and key are right.", "The SSH server's host key has not changed."],
            ErrKind::CloudSql => &["IAM sign-in has Application Default Credentials (`gcloud auth application-default login`); a password sign-in has gcloud signed in (`gcloud auth login`) or those credentials.", "The account may connect to the instance (Cloud SQL Client role).", "The instance name is project:region:instance, and its IP kind (public, private or PSC) is reachable from here."],
            ErrKind::Privilege => &["The user may sign in but not list databases. Name a database in the connection string."],
            _ => &[],
        };
        let mut run = None;
        ui.add_space(70.0);
        let w = ui.available_width().min(PANEL_W);
        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
            ui.allocate_ui_with_layout(egui::vec2(w, 0.0), egui::Layout::left_to_right(egui::Align::Min), |ui| {
                kind_icon(ui, e.ex.kind, 22.0);
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    let title = if e.ex.kind == ErrKind::SignIn { format!("Could not sign in to {name}") } else { e.ex.title.clone() };
                    ui.add(egui::Label::new(semibold(RichText::new(title).size(TXT_HEADING).color(BRIGHT()))).wrap());
                    if !e.ex.hint.is_empty() {
                        ui.add(egui::Label::new(RichText::new(&e.ex.hint).size(TXT_BODY).color(TEXT())).wrap());
                    }
                    ui.add_space(8.0);
                    egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_LG).inner_margin(egui::Margin::symmetric(14, 6)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for (k, s) in steps.iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.set_height(ROW_H);
                                let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), Sense::hover());
                                let color = if k < failed { SUCCESS() } else if k == failed { ALERT() } else { FAINT() };
                                if k < failed {
                                    paint_check(ui.painter(), r.center(), color);
                                } else if k == failed {
                                    let (cc, d) = (r.center(), 3.4);
                                    ui.painter().line_segment([cc + egui::vec2(-d, -d), cc + egui::vec2(d, d)], Stroke::new(1.6, color));
                                    ui.painter().line_segment([cc + egui::vec2(-d, d), cc + egui::vec2(d, -d)], Stroke::new(1.6, color));
                                } else {
                                    ui.painter().circle_stroke(r.center(), 3.0, Stroke::new(1.3, color));
                                }
                                ui.add_space(6.0);
                                let text = RichText::new(s).size(TXT_BODY);
                                ui.label(if k == failed { medium(text.color(BRIGHT())) } else if k < failed { text.color(TEXT()) } else { text.color(FAINT()) });
                            });
                        }
                    });
                    if !checks.is_empty() {
                        ui.add_space(8.0);
                        ui.label(semibold(RichText::new("What to check").size(TXT_SMALL).color(MUTED())));
                        for c in checks {
                            ui.label(RichText::new(format!("•  {c}")).size(TXT_BODY).color(TEXT()));
                        }
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if primary(ui, "Edit connection", PRIMARY()).clicked() {
                            run = Some(Act::Fix(Fix::EditConn));
                        }
                        if ui.button("Retry").clicked() {
                            run = Some(Act::Connect(id));
                        }
                        if ui.button("Copy details").clicked() {
                            run = Some(Act::Copy);
                        }
                    });
                    ui.add_space(10.0);
                    raw_block(ui, "Driver message", &e, false);
                });
            });
        });
        if let Some(a) = run {
            self.run_act(&ui.ctx().clone(), a, Some(&e), None);
        }
    }

    /// The status bar count of this session's errors, and the list it opens.
    pub(super) fn errors_chip(&mut self, ui: &mut egui::Ui) {
        if self.core.errors.is_empty() {
            return;
        }
        let n = self.core.errors.len();
        let unseen = self.core.errors_unseen.min(n);
        let label = if unseen > 0 { format!("{unseen} error{}", if unseen == 1 { "" } else { "s" }) } else { format!("{n} error{}", if n == 1 { "" } else { "s" }) };
        let fg = if unseen > 0 { ALERT() } else { MUTED() };
        let g = ui.painter().layout_no_wrap(label, FontId::proportional(TXT_CAPTION), fg);
        let (rect, r) = ui.allocate_exact_size(egui::vec2(g.size().x + 26.0, STATUS_CHIP_H), Sense::click());
        let state = state_of(&r);
        let p = ui.painter();
        p.rect_filled(rect, STATUS_CHIP_H / 2.0, tint(fg, [31, 51, 71][state]));
        p.circle_filled(egui::pos2(rect.left() + 9.0, rect.center().y), 3.0, fg);
        p.galley(egui::pos2(rect.left() + 17.0, rect.center().y - g.size().y / 2.0 + optical_dy(&g)), g, fg);
        focus_ring(ui, &r, rect, STATUS_CHIP_H / 2.0);
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Errors of this session"));
        let r = r.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Errors of this session");
        if r.clicked() {
            self.exec(Action::App(AppA::ErrorsSeen));
        }
        let mut pick = None;
        let mut clear = false;
        let mut log = false;
        popover(&r, 440.0, std::mem::take(&mut self.core.errors_open), |ui| {
            menu_look(ui);
            popover_body(ui, 440.0, |ui| {
                popover_head(ui, "Errors this session", &format!("{n}"), "Newest first. Server errors are also in the Query log.");
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for (k, e) in self.core.errors.iter().enumerate().rev() {
                        let (row, hot) = pop_row(ui, 46.0);
                        let p = ui.painter();
                        let color = kind_color(e.ex.kind);
                        p.circle_filled(egui::pos2(row.rect.left() + 12.0, row.rect.top() + 16.0), 3.5, color);
                        let x = row.rect.left() + 24.0;
                        let title = p.layout_job(egui::text::LayoutJob { wrap: egui::text::TextWrapping { max_width: row.rect.width() - 90.0, max_rows: 1, break_anywhere: true, overflow_character: Some('…') }, ..egui::text::LayoutJob::single_section(e.ex.title.clone(), ui_fmt(TXT_BODY, if hot { BRIGHT() } else { TEXT() }, W_MEDIUM, 0.0)) });
                        galley_at(p, x, row.rect.top() + 16.0, title, TEXT());
                        let meta = [super::log::ago(e.at), e.conn_name.clone(), e.place.clone()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                        let g = p.layout_no_wrap(meta, FontId::proportional(TXT_CAPTION), FAINT());
                        galley_at(p, x, row.rect.top() + 33.0, g, FAINT());
                        if hot {
                            let g = p.layout_no_wrap("Details".into(), FontId::proportional(TXT_SMALL), PRIMARY());
                            galley_at(p, row.rect.right() - 12.0 - g.size().x, row.rect.center().y, g, PRIMARY());
                        }
                        if row.clicked() {
                            pick = Some(k);
                        }
                    }
                });
                popover_divider(ui);
                ui.horizontal(|ui| {
                    clear = quiet(ui, "Clear").clicked();
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        log = quiet(ui, RichText::new("Open Query log").color(PRIMARY())).clicked();
                    });
                });
            });
        });
        if let Some(k) = pick {
            let id = self.core.errors[k].id;
            self.exec(Action::App(AppA::Details(id)));
        }
        if clear {
            self.exec(Action::App(AppA::ClearErrors));
        }
        if log {
            self.core.log.open = true;
        }
        // Picking a row or clearing closes the list, as a menu would.
        if pick.is_some() || clear || log {
            ui.ctx().data_mut(|d| d.insert_temp(r.id.with("clause-popover"), false));
        }
    }

    /// The details dialog of one error. False once it closes.
    pub(super) fn error_details(&mut self, ui: &mut egui::Ui, e: &ErrEntry) -> bool {
        let mut keep = true;
        let mut run = None;
        ui.set_width(600.0);
        let when = [e.place.clone(), super::log::ago(e.at)].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("  ·  ");
        dialog_head(ui, &Head::new(e.ex.title.as_str()).sub(when).glyph(glyphs::warn).tone(kind_color(e.ex.kind)));
        dialog_body(ui, |ui| {
            if !e.ex.hint.is_empty() {
                ui.add(egui::Label::new(RichText::new(&e.ex.hint).size(TXT_BODY).color(TEXT())).wrap());
                ui.add_space(14.0);
            }
            form_card(ui, "Details", "", |rows| {
                rows.label_width(110.0);
                for (k, v) in detail_rows(e) {
                    rows.row(k, "", |ui| {
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(egui::Label::new(RichText::new(v).monospace().size(TXT_SMALL).color(if k == "Code" { ALERT() } else { TEXT() })).truncate());
                        });
                    });
                }
            });
            card_caption(ui, "Server message", "");
            raw_well(ui, e, TEXT());
            ui.add_space(6.0);
            ui.label(RichText::new("Copied text leaves out passwords in connection strings.").size(TXT_SMALL).color(FAINT()));
        });
        dialog_foot(ui, |ui| {
            if primary(ui, "Close", PRIMARY()).on_hover_text("Esc").clicked() {
                keep = false;
            }
            if e.conn.is_some() && ui.button("Edit connection").clicked() {
                run = Some(Act::Fix(Fix::EditConn));
                keep = false;
            }
            foot_left(ui, |ui| {
                if ui.button("Copy details").clicked() {
                    run = Some(Act::Copy);
                }
            });
        });
        if let Some(a) = run {
            self.run_act(&ui.ctx().clone(), a, Some(e), None);
        }
        keep
    }
}
