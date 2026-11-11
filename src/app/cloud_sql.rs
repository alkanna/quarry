//! Cloud SQL: the "Add from Cloud SQL" browser of the projects and PostgreSQL instances gcloud reaches, the cards for a
//! missing gcloud or proxy and a signed-out account, and the card of a connection whose proxy failed.

use super::*;
use crate::cloudsql::{CloudSql, Found, Instance, IpKind, Start};
use std::collections::VecDeque;

/// gcloud is a Python program of some weight, so a long project list is asked a few projects at a time.
const PARALLEL: usize = 6;
/// Content width of the browser, from the design: the project list, a gap and the instance table.
const BROWSER_W: f32 = 812.0;
const PROJECTS_W: f32 = 180.0;
const PANELS_H: f32 = 266.0;
const PANEL_GAP: f32 = 12.0;
/// Where the instance table's columns start, from the left edge of its panel.
const COL_NAME: f32 = 14.0;
const COL_REGION: f32 = 214.0;
const COL_VERSION: f32 = 314.0;
const COL_IP: f32 = 424.0;
const USE_W: f32 = 48.0;
/// The state cards outside the browser keep the design's width.
const CARD_W: f32 = 430.0;
const GCLOUD_INSTALL: &str = "https://cloud.google.com/sdk/docs/install";

/// What gcloud told this session. Kept until Refresh, so the browser reopens at once.
#[derive(Default)]
pub(super) struct CloudCache {
    /// The load in flight or answered; 0 before the first.
    pub(super) req: u64,
    pub(super) start: Option<Start>,
    pub(super) instances: HashMap<String, Result<Vec<Instance>, String>>,
    /// Projects still to ask for, and how many are being asked.
    pub(super) queue: VecDeque<String>,
    pub(super) running: usize,
    /// What the running download or install of the proxy says while it works.
    pub(super) busy: Option<&'static str>,
}

impl CloudCache {
    pub(super) fn account(&self) -> Option<&str> {
        self.start.as_ref().and_then(|s| s.account.as_deref())
    }

    pub(super) fn instance(&self, name: &str) -> Option<&Instance> {
        self.instances.values().filter_map(|r| r.as_ref().ok()).flatten().find(|i| i.connection_name == name)
    }

    fn components(&self) -> bool {
        self.start.as_ref().is_some_and(|s| s.components && s.gcloud.is_some())
    }
}

#[derive(Default)]
pub(super) struct CloudBrowser {
    pub(super) search: String,
    pub(super) stopped: bool,
    /// The project picked on the left; None is all of them.
    pub(super) project: Option<String>,
    /// The instance row picked, by connection name.
    pub(super) sel: Option<String>,
    /// Opened from a connection form: Use fills that form instead of a new one.
    pub(super) change: bool,
}

/// The states a Cloud SQL card explains.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Card {
    NoGcloud,
    NoProxy,
    SignedOut,
}

/// What a click on a card asks for.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum CardAct {
    Again,
    Download,
    Install,
}

fn ip_on(i: &Instance, k: IpKind) -> bool {
    // An instance listed without any address (an old API answer) is not held to what it lacks.
    if !(i.public_ip || i.private_ip || i.psc) {
        return true;
    }
    match k {
        IpKind::Public => i.public_ip,
        IpKind::Private => i.private_ip,
        IpKind::Psc => i.psc,
    }
}

const KINDS: [(IpKind, &str); 3] = [(IpKind::Public, "Public"), (IpKind::Private, "Private"), (IpKind::Psc, "PSC")];

fn running(i: &Instance) -> bool {
    i.state == "RUNNABLE"
}

/// Fills a connection form from an instance the browser listed. A form that already has a name, as one opened by Change
/// does, keeps it.
pub(super) fn use_instance(f: &mut ConnForm, inst: &Instance, account: Option<&str>) {
    let keep = f.cloud_sql.as_ref().map(|c| c.ip).filter(|k| ip_on(inst, *k));
    let ip = keep.or_else(|| KINDS.iter().map(|k| k.0).find(|k| ip_on(inst, *k))).unwrap_or_default();
    f.cloud_sql = Some(CloudSql { instance: inst.connection_name.clone(), iam: inst.iam, ip });
    f.cloud_inst = Some(inst.clone());
    f.engine = db::sql::Engine::Postgres;
    f.addr.scheme = "postgres".into();
    f.addr.port = db::sql::Addr::default_port("postgres");
    f.ssh_on = false;
    if f.name.trim().is_empty() {
        f.name = inst.name.clone();
    }
    if inst.iam {
        if let Some(a) = account {
            f.addr.user = crate::cloudsql::iam_user(a);
        }
        f.addr.password.clear();
    }
    if matches!(f.tab, ConnTab::Ssh | ConnTab::Tls | ConnTab::Proxy) {
        f.tab = ConnTab::Standard;
    }
}

/// The form back to a plain host and port.
pub(super) fn use_host(f: &mut ConnForm) {
    f.cloud_sql = None;
    f.cloud_inst = None;
    if f.addr.host == CLOUD_HOST {
        f.addr.host.clear();
    }
}

/// Why a project's instances could not be listed, in a line under the project list.
fn reason(project: &str, err: &str) -> String {
    let low = err.to_lowercase();
    if low.contains("sqladmin") || low.contains("cloud sql admin api") {
        format!("{project} has the Cloud SQL Admin API off.")
    } else if low.contains("403") || low.contains("permission") || low.contains("not authorized") {
        format!("{project}: this account may not list its instances.")
    } else {
        let said = err.trim_start_matches("Cloud SQL: ").trim_start_matches("gcloud failed: ");
        format!("{project}: {said}")
    }
}

/// "PSC is off", "Private and PSC are off".
fn kinds_off(off: &[&str]) -> String {
    match off {
        [one] => format!("{one} is off"),
        [a, b] => format!("{a} and {b} are off"),
        _ => format!("{} are off", off.join(", ")),
    }
}

/// The proxy's own lines from a "Cloud SQL proxy failed: ..." or timeout message, which the core joins with newlines.
fn proxy_lines(raw: &str) -> Vec<String> {
    let said = raw.split_once("Cloud SQL proxy failed: ").map(|x| x.1).or_else(|| raw.split_once(" s: ").map(|x| x.1)).unwrap_or("");
    said.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
}

/// A round outline with a cross: a failure.
fn paint_failed(p: &egui::Painter, c: egui::Pos2, color: Color32) {
    let s = Stroke::new(1.3, color);
    p.circle_stroke(c, 6.0, s);
    let d = 2.2;
    p.line_segment([c + egui::vec2(-d, -d), c + egui::vec2(d, d)], s);
    p.line_segment([c + egui::vec2(-d, d), c + egui::vec2(d, -d)], s);
}

/// A triangle with an exclamation mark: something to do before going on.
fn paint_warn(p: &egui::Painter, c: egui::Pos2, color: Color32) {
    let s = Stroke::new(1.3, color);
    let pts = vec![c + egui::vec2(0.0, -5.5), c + egui::vec2(6.5, 5.5), c + egui::vec2(-6.5, 5.5), c + egui::vec2(0.0, -5.5)];
    p.add(egui::Shape::line(pts, s));
    p.line_segment([c + egui::vec2(0.0, -1.5), c + egui::vec2(0.0, 1.5)], s);
    p.circle_filled(c + egui::vec2(0.0, 3.6), 0.8, color);
}

/// A round outline with an "i": a notice.
fn paint_info(p: &egui::Painter, c: egui::Pos2, color: Color32) {
    let s = Stroke::new(1.3, color);
    p.circle_stroke(c, 6.0, s);
    p.line_segment([c + egui::vec2(0.0, -0.5), c + egui::vec2(0.0, 3.0)], s);
    p.circle_filled(c + egui::vec2(0.0, -3.0), 0.9, color);
}

/// A box with an arrow out of its corner, after a link that opens the browser.
fn paint_external(p: &egui::Painter, c: egui::Pos2, color: Color32) {
    let s = Stroke::new(1.1, color);
    let r = egui::Rect::from_center_size(c + egui::vec2(-1.0, 1.0), egui::vec2(8.0, 8.0));
    p.line_segment([r.left_top() + egui::vec2(3.0, 0.0), r.left_top()], s);
    p.line_segment([r.left_top(), r.left_bottom()], s);
    p.line_segment([r.left_bottom(), r.right_bottom()], s);
    p.line_segment([r.right_bottom(), r.right_bottom() - egui::vec2(0.0, 3.0)], s);
    let tip = r.right_top() + egui::vec2(2.0, -2.0);
    p.line_segment([tip, c + egui::vec2(-0.5, 0.5)], s);
    p.line_segment([tip, tip - egui::vec2(4.0, 0.0)], s);
    p.line_segment([tip, tip + egui::vec2(0.0, 4.0)], s);
}

/// A painted glyph in a 16 pt slot, then the card's title.
fn card_title(ui: &mut egui::Ui, glyph: fn(&egui::Painter, egui::Pos2, Color32), color: Color32, title: &str, title_c: Color32) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), Sense::hover());
        glyph(ui.painter(), r.center(), color);
        ui.add_space(2.0);
        ui.label(semibold(RichText::new(title).size(TXT_TITLE).color(title_c)));
    });
}

/// Text under a card's title, lined up with the title rather than the glyph.
fn card_indent<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal_top(|ui| {
        ui.add_space(16.0 + 2.0 + ui.spacing().item_spacing.x);
        ui.vertical(add).inner
    })
    .inner
}

fn card_frame<R>(ui: &mut egui::Ui, bar: Option<Color32>, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let r = egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(egui::Margin { left: 12, right: 12, top: 12, bottom: 10 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 6.0;
        add(ui)
    });
    if let Some(c) = bar {
        let b = r.response.rect;
        ui.painter().rect_filled(egui::Rect::from_min_size(b.min, egui::vec2(3.0, b.height())), egui::CornerRadius { nw: R_MD, sw: R_MD, ne: 0, se: 0 }, c);
    }
    r.inner
}

/// The line across a card above its footer.
fn card_rule(ui: &mut egui::Ui) {
    ui.add_space(4.0);
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(egui::vec2(w, 1.0), Sense::hover());
    ui.painter().hline(r.x_range(), r.center().y, Stroke::new(1.0, LINE()));
    ui.add_space(4.0);
}

fn small(ui: &mut egui::Ui, text: &str, color: Color32) {
    ui.add(egui::Label::new(RichText::new(text).size(TXT_SMALL).color(color)).wrap());
}

/// One of the Cloud SQL state cards. `again` names the button that looks again: Refresh in the browser, Retry for a
/// connection. Returns what was clicked.
pub(super) fn state_card(ui: &mut egui::Ui, card: Card, account: Option<&str>, components: bool, busy: Option<&str>, again: &str) -> Option<CardAct> {
    let mut act = None;
    let mut again_button = |ui: &mut egui::Ui, primary_look: bool| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let hit = if primary_look { primary(ui, again, PRIMARY()) } else { ui.button(again) };
            if hit.clicked() {
                act = Some(CardAct::Again);
            }
        });
    };
    match card {
        Card::NoGcloud => card_frame(ui, None, |ui| {
            card_title(ui, paint_info, C_LINK(), "Google Cloud CLI not found", BRIGHT());
            card_indent(ui, |ui| {
                small(ui, "Quarry lists your Cloud SQL instances and signs in through gcloud. It was not on PATH or in the usual install folders.", MUTED());
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.hyperlink_to(RichText::new("Install the Google Cloud CLI").size(TXT_BODY), GCLOUD_INSTALL);
                    let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), Sense::hover());
                    paint_external(ui.painter(), r.center(), C_LINK());
                });
            });
            card_rule(ui);
            card_indent(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Installed it? Quarry looks again on {again}.")).size(TXT_SMALL).color(FAINT()));
                    again_button(ui, false);
                });
            });
        }),
        Card::NoProxy => card_frame(ui, None, |ui| {
            card_title(ui, paint_warn, WARNING(), "Cloud SQL Auth Proxy not found", BRIGHT());
            card_indent(ui, |ui| {
                small(ui, "Quarry starts cloud-sql-proxy for each connection. It was not on PATH or in the gcloud SDK.", MUTED());
                ui.add_space(4.0);
                match busy {
                    Some(b) => {
                        ui.horizontal(|ui| {
                            spin(ui);
                            ui.label(RichText::new(b).color(MUTED()));
                        });
                    }
                    None => {
                        ui.horizontal(|ui| {
                            if components && primary(ui, "Install with gcloud", PRIMARY()).clicked() {
                                act = Some(CardAct::Install);
                            }
                            let download = if components { ui.button("Download (verified)") } else { primary(ui, "Download (verified)", PRIMARY()) };
                            if download.clicked() {
                                act = Some(CardAct::Download);
                            }
                        });
                    }
                }
                if components {
                    small(ui, "Runs gcloud components install cloud-sql-proxy.", FAINT());
                }
            });
            card_rule(ui);
            card_indent(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                small(ui, "Download saves it to", FAINT());
                ui.add(egui::Label::new(RichText::new(tools_dir().display().to_string()).monospace().size(TXT_CAPTION).color(MUTED())).wrap());
                small(ui, "and checks Google's SHA-256 before it ever runs.", FAINT());
            });
        }),
        Card::SignedOut => card_frame(ui, None, |ui| {
            card_title(ui, paint_warn, WARNING(), "gcloud needs you to sign in again", BRIGHT());
            card_indent(ui, |ui| {
                let body = match account {
                    Some(a) => format!("The credentials of {a} expired or were revoked, so no instances can be listed."),
                    None => "gcloud has no active account, so no instances can be listed.".to_string(),
                };
                small(ui, &body, MUTED());
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(RichText::new("Run").color(TEXT()));
                    egui::Frame::new().fill(pal().well).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_SM).inner_margin(egui::Margin::symmetric(6, 2)).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new("gcloud auth login").monospace().size(TXT_SMALL).color(TEXT())).selectable(true));
                    });
                    ui.label(RichText::new(format!(", then {again}.")).color(TEXT()));
                });
            });
            card_rule(ui);
            card_indent(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Quarry checks again on {again}.")).size(TXT_SMALL).color(FAINT()));
                    again_button(ui, true);
                });
            });
        }),
    }
    act
}

/// A card at its design width in the middle of the page, under the "CLOUD SQL" caption.
fn centered<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let w = ui.available_width().min(CARD_W);
    ui.horizontal(|ui| {
        ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
        ui.allocate_ui_with_layout(egui::vec2(w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.set_width(w);
            caption(ui, "CLOUD SQL");
            ui.add_space(2.0);
            add(ui)
        })
        .inner
    })
    .inner
}

impl App {
    /// Where the proxy and gcloud are: Settings first, else where the browser found them, else the worker looks.
    pub(super) fn cloud_tools(&self) -> crate::cloudsql::Tools {
        let set = |s: &str| Some(s.trim()).filter(|s| !s.is_empty()).map(PathBuf::from);
        let found = self.cloud.start.as_ref();
        crate::cloudsql::Tools { proxy: set(&self.prefs.cloud_sql_proxy).or_else(|| found.and_then(|s| s.proxy.clone())), gcloud: set(&self.prefs.gcloud).or_else(|| found.and_then(|s| s.gcloud.clone())) }
    }

    /// Opens the browser. `change` is for a connection form that is open: Use then fills it.
    pub(super) fn open_cloud_browser(&mut self, change: bool) {
        self.cloud_load(false);
        self.cloud_browser = Some(CloudBrowser { change, ..Default::default() });
    }

    /// Asks gcloud again, or for the first time. The demo and tests fill the cache themselves and never run gcloud.
    pub(super) fn cloud_load(&mut self, force: bool) {
        if self.demo || (self.cloud.req != 0 && !force) {
            return;
        }
        self.req += 1;
        let req = self.req;
        let busy = self.cloud.busy.take();
        self.cloud = CloudCache { req, busy, ..Default::default() };
        let (g, p) = (self.prefs.gcloud.clone(), self.prefs.cloud_sql_proxy.clone());
        self.worker.blocking(move || Msg::CloudSql { req, found: Found::Start(crate::cloudsql::start(&g, &p)) });
    }

    /// Starts the next instance lists, up to `PARALLEL` at once.
    fn cloud_pump(&mut self) {
        let req = self.cloud.req;
        let Some(g) = self.cloud.start.as_ref().and_then(|s| s.gcloud.clone()) else { return };
        while self.cloud.running < PARALLEL {
            let Some(project) = self.cloud.queue.pop_front() else { return };
            self.cloud.running += 1;
            let g = g.clone();
            self.worker.blocking(move || {
                let res = crate::cloudsql::instances(&g, &project);
                Msg::CloudSql { req, found: Found::Instances { project, res } }
            });
        }
    }

    pub(super) fn cloud_found(&mut self, req: u64, found: Found) {
        match found {
            Found::Proxy(res) => {
                self.cloud.busy = None;
                match res {
                    Ok(p) => {
                        self.ok(format!("Cloud SQL proxy ready at {}", p.display()));
                        if let Some(s) = &mut self.cloud.start {
                            s.proxy = Some(p);
                        }
                    }
                    Err(e) => self.err(e),
                }
            }
            // An answer to a load that Refresh has replaced.
            _ if req != self.cloud.req => {}
            Found::Start(s) => {
                // A project already answered (a test feeds them first) is not asked again.
                if let Ok(ps) = &s.projects {
                    self.cloud.queue = ps.iter().filter(|p| !self.cloud.instances.contains_key(&p.id)).map(|p| p.id.clone()).collect();
                }
                self.cloud.start = Some(s);
                self.cloud_pump();
            }
            Found::Instances { project, res } => {
                self.cloud.running = self.cloud.running.saturating_sub(1);
                self.cloud.instances.insert(project, res);
                self.cloud_pump();
            }
        }
    }

    /// Downloads the pinned proxy, or installs it with gcloud.
    pub(super) fn cloud_get_proxy(&mut self, install: bool) {
        if self.demo {
            return self.warn("Offline demo: nothing is downloaded.");
        }
        if self.cloud.busy.is_some() {
            return;
        }
        if install {
            let Some(g) = self.cloud.start.as_ref().and_then(|s| s.gcloud.clone()) else { return };
            self.cloud.busy = Some("Installing cloud-sql-proxy with gcloud…");
            self.worker.blocking(move || Msg::CloudSql { req: 0, found: Found::Proxy(crate::cloudsql::install_and_find(&g)) });
        } else {
            self.cloud.busy = Some("Downloading and checking cloud-sql-proxy…");
            self.worker.blocking(|| Msg::CloudSql { req: 0, found: Found::Proxy(crate::cloudsql::download_proxy(&tools_dir())) });
        }
    }

    /// Fills the form the browser was opened for, or a new one.
    fn cloud_use(&mut self, inst: &Instance, change: bool) {
        let account = self.cloud.account().map(str::to_string);
        if !(change && matches!(self.modal, Some(Modal::Conn(_)))) {
            self.conn_test = None;
            self.test_info = None;
            self.modal = Some(Modal::Conn(Box::default()));
        }
        if let Some(Modal::Conn(f)) = self.modal.as_mut() {
            use_instance(f, inst, account.as_deref());
        }
    }

    /// The card a card button asked for, run: `again` is what Refresh or Retry does there.
    pub(super) fn cloud_card_act(&mut self, act: Option<CardAct>, again: impl FnOnce(&mut Self)) {
        match act {
            Some(CardAct::Again) => again(self),
            Some(CardAct::Download) => self.cloud_get_proxy(false),
            Some(CardAct::Install) => self.cloud_get_proxy(true),
            None => {}
        }
    }

    /// The proxy-missing card for a form or a connection whose Test or Connect found no proxy. The browser's load tells
    /// whether gcloud can install it, so it is started if it never ran.
    pub(super) fn proxy_missing_card(&mut self, ui: &mut egui::Ui, again: &str) -> Option<CardAct> {
        self.cloud_load(false);
        state_card(ui, Card::NoProxy, None, self.cloud.components(), self.cloud.busy, again)
    }

    /// The card of a connection that went through Cloud SQL and could not open, in place of the generic one.
    pub(super) fn cloud_failed_card(&mut self, ui: &mut egui::Ui, id: u64, e: &ErrEntry) {
        let instance = self.conn(id).and_then(|c| c.meta.cloud_sql.as_ref()).map(|c| c.instance.clone()).unwrap_or_default();
        let raw = e.raw.clone();
        let mut act = None;
        let mut copy = false;
        ui.add_space(70.0);
        let missing = raw.contains(crate::cloudsql::PROXY_MISSING);
        let signed_out = raw.to_lowercase().contains("gcloud auth login");
        if missing || signed_out {
            let account = self.cloud.account().map(str::to_string);
            act = centered(ui, |ui| if missing { self.proxy_missing_card(ui, "Retry") } else { state_card(ui, Card::SignedOut, account.as_deref(), false, None, "Retry") });
        } else {
            let project = instance.split(':').rev().nth(2).unwrap_or_default().to_string();
            let iam = e.raw.to_lowercase().contains("iam sign-in refused");
            let steps: Vec<String> = if iam {
                vec!["The database user is the IAM account (a service account ends in .iam).".into(), "The instance flag cloudsql.iam_authentication is on.".into(), "Run gcloud auth login if the account changed.".into()]
            } else {
                vec![format!("Ask for the Cloud SQL Client role on {}.", if project.is_empty() { "the project" } else { &project }), "Check that the instance is RUNNABLE.".into(), "Run gcloud auth login if the account changed.".into()]
            };
            let lines = proxy_lines(&raw);
            centered(ui, |ui| {
                card_frame(ui, Some(ALERT()), |ui| {
                    card_title(ui, paint_failed, ALERT(), &e.ex.title, ALERT());
                    card_indent(ui, |ui| {
                        ui.label(RichText::new(&instance).monospace().size(TXT_SMALL).color(FAINT()));
                        ui.add_space(4.0);
                        if iam || lines.is_empty() {
                            small(ui, &e.ex.hint, TEXT());
                        }
                        let (head, body) = if lines.is_empty() { ("SERVER MESSAGE", db::scrub_uri(&raw)) } else { ("LAST LINES FROM THE PROXY", lines.join("\n")) };
                        if !iam || !lines.is_empty() {
                            caption(ui, head);
                            egui::Frame::new().fill(pal().well).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_SM).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                                    ui.add(egui::Label::new(RichText::new(body).monospace().size(TXT_CAPTION).color(TEXT())).wrap().selectable(true));
                                });
                            });
                        }
                        ui.add_space(4.0);
                        caption(ui, "TO FIX");
                        ui.spacing_mut().item_spacing.y = 2.0;
                        for (k, s) in steps.iter().enumerate() {
                            small(ui, &format!("{}. {s}", k + 1), MUTED());
                        }
                        ui.add_space(6.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if primary(ui, "Retry", PRIMARY()).clicked() {
                                act = Some(CardAct::Again);
                            }
                            copy = ui.button("Copy details").clicked();
                        });
                    });
                });
            });
        }
        if copy {
            self.run_act(&ui.ctx().clone(), Act::Copy, Some(e), None);
        }
        self.cloud_card_act(act, |a| a.connect(id));
    }

    /// The "Add from Cloud SQL" browser.
    pub(super) fn cloud_browser_window(&mut self, ctx: &egui::Context) {
        let Some(mut b) = self.cloud_browser.take() else { return };
        let mut close = false;
        let mut pick: Option<Instance> = None;
        let mut act = None;
        let r = dialog(egui::Id::unique(("quarry-layer", "cloud-sql"))).show(ctx, |ui| {
            ui.set_width(BROWSER_W);
            ui.horizontal(|ui| {
                dialog_title(ui, "Add from Cloud SQL");
                if let Some(a) = self.cloud.account() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(a).monospace().size(TXT_SMALL).color(TEXT()));
                        ui.label(RichText::new("gcloud account").size(TXT_SMALL).color(FAINT()));
                    });
                }
            });
            ui.label(RichText::new(if b.change { "PostgreSQL instances your gcloud account can reach. Use puts one in the open form." } else { "PostgreSQL instances your gcloud account can reach. Use fills a new connection." }).color(MUTED()));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let id = egui::Id::unique("cloud-sql-search");
                let w = ui.available_width() - 260.0;
                well_field(ui, id, FieldIcon::Search, None, |ui| {
                    let r = ui.add(field_edit(&mut b.search, id, "Search instances, regions or projects", w, egui::TextStyle::Body));
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Search instances"));
                });
                ui.add_space(8.0);
                let t = toggle(ui, &mut b.stopped);
                t.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, b.stopped, "Show stopped instances"));
                ui.label(RichText::new("Show stopped").color(MUTED()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Refresh").clicked() {
                        act = Some(CardAct::Again);
                    }
                });
            });
            ui.add_space(12.0);
            let start = self.cloud.start.clone();
            let signed_out = start.as_ref().is_some_and(|s| matches!(&s.projects, Err(e) if e.contains("gcloud auth login")));
            match &start {
                Some(s) if s.gcloud.is_none() => act = act.or(state_card(ui, Card::NoGcloud, None, false, None, "Refresh")),
                Some(_) if signed_out => act = act.or(state_card(ui, Card::SignedOut, self.cloud.account(), false, None, "Refresh")),
                _ => {
                    if start.as_ref().is_some_and(|s| s.proxy.is_none()) {
                        act = act.or(state_card(ui, Card::NoProxy, None, self.cloud.components(), self.cloud.busy, "Refresh"));
                        ui.add_space(10.0);
                    }
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = PANEL_GAP;
                        self.cloud_projects(ui, &mut b);
                        if let Some(i) = self.cloud_instances(ui, &mut b) {
                            pick = Some(i);
                        }
                    });
                }
            }
            ui.add_space(10.0);
            let shown = b.sel.as_ref().and_then(|n| self.cloud.instance(n));
            let foot = match shown {
                Some(i) => format!("Only PostgreSQL instances are listed. Use saves {} {}.", i.connection_name, if i.iam { "with IAM sign-in on" } else { "with a database password" }),
                None => "Only PostgreSQL instances are listed.".to_string(),
            };
            ui.label(RichText::new(foot).size(TXT_SMALL).color(FAINT()));
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = ui.button("Cancel").on_hover_text("Esc").clicked();
            });
        });
        self.cloud_card_act(act, |a| a.cloud_load(true));
        if let Some(i) = pick {
            return self.cloud_use(&i, b.change);
        }
        if !(close || r.should_close()) {
            self.cloud_browser = Some(b);
        }
    }

    /// The project list on the left of the browser, with a count of each project's instances.
    fn cloud_projects(&mut self, ui: &mut egui::Ui, b: &mut CloudBrowser) {
        let projects = self.cloud.start.as_ref().and_then(|s| s.projects.as_ref().ok()).cloned().unwrap_or_default();
        let stopped = b.stopped;
        let shown = |r: &Result<Vec<Instance>, String>| r.as_ref().map(|v| v.iter().filter(|i| stopped || running(i)).count()).unwrap_or(0);
        let total: usize = self.cloud.instances.values().map(shown).sum();
        ui.allocate_ui_with_layout(egui::vec2(PROJECTS_W, PANELS_H), egui::Layout::top_down(egui::Align::Min), |ui| {
            egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                ui.set_width(PROJECTS_W - 10.0);
                ui.set_min_height(PANELS_H - 10.0);
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    caption(ui, "PROJECTS");
                });
                ui.add_space(4.0);
                egui::ScrollArea::vertical().id_salt("cloud-projects").max_height(PANELS_H - 40.0).show(ui, |ui| {
                    let all = side_row(ui, SIDE_ROW_H, 0.0, b.project.is_none(), "All projects", |p, r| {
                        galley_at(p, r.left(), r.center().y, p.layout_job(LayoutJob::single_section("All projects".into(), ui_fmt(TXT_BODY, BRIGHT(), W_MEDIUM, 0.0))), BRIGHT());
                        let g = p.layout_no_wrap(total.to_string(), FontId::proportional(TXT_SMALL), FAINT());
                        galley_at(p, r.right() - g.size().x, r.center().y, g, FAINT());
                    });
                    if all.clicked() {
                        b.project = None;
                    }
                    for pr in &projects {
                        let res = self.cloud.instances.get(&pr.id);
                        let n = res.map(shown);
                        let faint = matches!(res, Some(Err(_))) || n == Some(0);
                        let hit = side_row(ui, SIDE_ROW_H, 0.0, b.project.as_ref() == Some(&pr.id), &pr.id, |p, r| {
                            let name_c = if faint { FAINT() } else { TEXT() };
                            galley_at(p, r.left(), r.center().y, p.layout_job(LayoutJob::single_section(pr.id.clone(), mono_fmt(TXT_SMALL, name_c, 400.0))), name_c);
                            let right = match res {
                                None => None,
                                Some(Err(_)) => Some("No access".to_string()),
                                Some(Ok(_)) => n.map(|n| n.to_string()),
                            };
                            match right {
                                Some(t) => {
                                    let g = p.layout_no_wrap(t, FontId::proportional(TXT_SMALL), FAINT());
                                    galley_at(p, r.right() - g.size().x, r.center().y, g, FAINT());
                                }
                                None => {}
                            }
                        });
                        if res.is_none() {
                            spin_at(ui, egui::Rect::from_center_size(egui::pos2(hit.rect.right() - 16.0, hit.rect.center().y), egui::vec2(12.0, 12.0)));
                        }
                        if hit.clicked() {
                            b.project = Some(pr.id.clone());
                        }
                    }
                    let failed: Vec<(String, String)> = projects.iter().filter_map(|p| match self.cloud.instances.get(&p.id) {
                        Some(Err(e)) => Some((p.id.clone(), e.clone())),
                        _ => None,
                    }).collect();
                    if !failed.is_empty() {
                        ui.add_space(10.0);
                        for (p, e) in failed {
                            ui.horizontal(|ui| {
                                ui.add_space(8.0);
                                small(ui, &reason(&p, &e), FAINT());
                            });
                        }
                    }
                });
            });
        });
    }

    /// The instance table, grouped by project. Returns the instance whose Use was clicked.
    fn cloud_instances(&mut self, ui: &mut egui::Ui, b: &mut CloudBrowser) -> Option<Instance> {
        let mut pick = None;
        let w = BROWSER_W - PROJECTS_W - PANEL_GAP;
        let q = b.search.trim().to_lowercase();
        let start = self.cloud.start.as_ref();
        let projects: Vec<String> = start.and_then(|s| s.projects.as_ref().ok()).map(|v| v.iter().map(|p| p.id.clone()).collect()).unwrap_or_default();
        let error = start.and_then(|s| s.projects.as_ref().err()).cloned();
        let loading = start.is_none() || projects.iter().any(|p| !self.cloud.instances.contains_key(p));
        ui.allocate_ui_with_layout(egui::vec2(w, PANELS_H), egui::Layout::top_down(egui::Align::Min), |ui| {
            egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).show(ui, |ui| {
                ui.set_width(w - 2.0);
                ui.set_min_height(PANELS_H - 2.0);
                ui.spacing_mut().item_spacing.y = 0.0;
                let (head, _) = ui.allocate_exact_size(egui::vec2(w - 2.0, TABLE_HEAD_H), Sense::hover());
                let p = ui.painter();
                for (x, t) in [(COL_NAME, "INSTANCE"), (COL_REGION, "REGION"), (COL_VERSION, "VERSION"), (COL_IP, "IP")] {
                    galley_at(p, head.left() + x, head.center().y, p.layout_job(LayoutJob::single_section(t.into(), ui_fmt(TXT_CAPTION, MUTED(), W_SEMIBOLD, 0.6))), MUTED());
                }
                p.hline(head.x_range(), head.bottom(), Stroke::new(1.0, LINE()));
                egui::ScrollArea::vertical().id_salt("cloud-instances").max_height(PANELS_H - TABLE_HEAD_H - 4.0).auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let mut any = false;
                    let only = b.project.clone();
                    for pr in projects.iter().filter(|p| only.as_ref().is_none_or(|x| x == *p)) {
                        let Some(Ok(list)) = self.cloud.instances.get(pr) else { continue };
                        let rows: Vec<&Instance> = list.iter().filter(|i| (b.stopped || running(i)) && (q.is_empty() || [&i.name, &i.region, pr, &i.version].iter().any(|s| s.to_lowercase().contains(&q)))).collect();
                        if rows.is_empty() {
                            continue;
                        }
                        any = true;
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.add_space(COL_NAME);
                            ui.label(RichText::new(pr).monospace().size(TXT_CAPTION).color(MUTED()));
                        });
                        ui.add_space(2.0);
                        for i in rows {
                            if instance_row(ui, i, b) {
                                pick = Some(i.clone());
                            }
                        }
                    }
                    if !any {
                        ui.add_space(24.0);
                        ui.vertical_centered(|ui| {
                            if let Some(e) = &error {
                                ui.add(egui::Label::new(RichText::new(e).size(TXT_SMALL).color(ALERT())).wrap());
                            } else if loading {
                                ui.horizontal(|ui| {
                                    spin(ui);
                                    ui.label(RichText::new("Asking gcloud for your instances…").color(MUTED()));
                                });
                            } else if !q.is_empty() {
                                ui.label(RichText::new(format!("No instance matches \u{201c}{}\u{201d}.", b.search.trim())).color(MUTED()));
                            } else {
                                ui.label(RichText::new("No PostgreSQL instances here.").color(MUTED()));
                            }
                        });
                    } else if loading {
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.add_space(COL_NAME);
                            spin(ui);
                            ui.label(RichText::new("Listing more projects…").size(TXT_SMALL).color(FAINT()));
                        });
                    }
                });
            });
        });
        pick
    }
}

/// One instance row, painted. A click selects it, a double click or its Use button uses it.
fn instance_row(ui: &mut egui::Ui, i: &Instance, b: &mut CloudBrowser) -> bool {
    let w = ui.available_width();
    let (row, r) = ui.allocate_exact_size(egui::vec2(w, POP_ROW_H), Sense::click());
    let rect = row.shrink2(egui::vec2(4.0, 0.0));
    let sel = b.sel.as_deref() == Some(i.connection_name.as_str());
    let state = state_of(&r);
    let hot = sel || state > 0 || ui.rect_contains_pointer(rect);
    let fill = match (sel, state) {
        (true, 0) => SEL(),
        (true, _) => SEL_HOVER(),
        (false, 2) => PRESSED(),
        (false, 1) => HOVER(),
        _ => Color32::TRANSPARENT,
    };
    let left = row.left();
    let cy = rect.center().y;
    let p = ui.painter();
    p.rect_filled(rect, R_SM, fill);
    let up = running(i);
    let dot = egui::pos2(left + COL_NAME + 3.5, cy);
    if up {
        p.circle_filled(dot, 3.5, SUCCESS());
    } else {
        p.circle_stroke(dot, 3.0, Stroke::new(1.0, FAINT()));
    }
    let name_c = if up { BRIGHT() } else { MUTED() };
    let name = p.layout_job(LayoutJob::single_section(i.name.clone(), mono_fmt(TXT_BODY, name_c, 400.0)));
    let mut x = left + COL_NAME + 15.0;
    let name_w = name.size().x;
    galley_at(p, x, cy, name, name_c);
    x += name_w + 8.0;
    if i.iam {
        x = iam_badge(p, x, cy) + 6.0;
    }
    if !up {
        let g = p.layout_job(LayoutJob::single_section(i.state.clone(), mono_fmt(TXT_CAPTION, FAINT(), 400.0)));
        galley_at(p, x, cy, g, FAINT());
    }
    galley_at(p, left + COL_REGION, cy, p.layout_job(LayoutJob::single_section(i.region.clone(), ui_fmt(TXT_SMALL, MUTED(), 400.0, 0.0))), MUTED());
    galley_at(p, left + COL_VERSION, cy, p.layout_job(LayoutJob::single_section(i.version.clone(), mono_fmt(TXT_SMALL, MUTED(), 400.0))), MUTED());
    let mut x = left + COL_IP;
    for (k, label) in KINDS {
        let has = match k {
            IpKind::Public => i.public_ip,
            IpKind::Private => i.private_ip,
            IpKind::Psc => i.psc,
        };
        if has {
            let g = p.layout_job(LayoutJob::single_section(label.into(), ui_fmt(TXT_CAPTION, MUTED(), 400.0, 0.0)));
            let chip = egui::Rect::from_min_size(egui::pos2(x, cy - 8.5), egui::vec2(g.size().x + 12.0, 17.0));
            p.rect_stroke(chip, R_SM, Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
            galley_at(p, chip.left() + 6.0, cy, g, MUTED());
            x = chip.right() + 4.0;
        }
    }
    let name = i.connection_name.clone();
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, sel, &name));
    let at = egui::Rect::from_center_size(egui::pos2(rect.right() - 6.0 - USE_W / 2.0, cy), egui::vec2(USE_W, BUTTON_H));
    let used = put_at(ui, at, |ui| {
        let u = if hot { ui.add_sized(at.size(), primary_button(ui, "Use", PRIMARY())) } else { ui.add_sized(at.size(), egui::Button::new("Use")) };
        u.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Use {name}")));
        u.clicked()
    });
    let r = r.on_hover_cursor(egui::CursorIcon::PointingHand);
    if r.clicked() {
        b.sel = Some(i.connection_name.clone());
    }
    used || r.double_clicked()
}

/// The IAM badge after an instance name; returns its right edge.
pub(super) fn iam_badge(p: &egui::Painter, x: f32, cy: f32) -> f32 {
    let g = p.layout_job(LayoutJob::single_section("IAM".into(), ui_fmt(TXT_CAPTION, C_LINK(), W_SEMIBOLD, 0.0)));
    let rect = egui::Rect::from_min_size(egui::pos2(x, cy - 8.0), egui::vec2(g.size().x + 14.0, 16.0));
    p.rect_filled(rect, R_SM, tint(C_LINK(), 46));
    galley_at(p, rect.left() + 7.0, cy, g, C_LINK());
    rect.right()
}

/// The IP kinds an instance lacks, and the note saying so; empty when it has them all or is unknown.
pub(super) fn ip_choices(inst: Option<&Instance>) -> ([(IpKind, &'static str, bool); 3], String) {
    let on = |k| inst.is_none_or(|i| ip_on(i, k));
    let opts = KINDS.map(|(k, l)| (k, l, on(k)));
    let off: Vec<&str> = opts.iter().filter(|o| !o.2).map(|o| o.1).collect();
    let note = match inst {
        Some(i) if !off.is_empty() => format!("{} for {}", kinds_off(&off), i.name),
        _ => String::new(),
    };
    (opts, note)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(public: bool, private: bool, psc: bool, iam: bool) -> Instance {
        Instance { connection_name: "acme-prod:europe-west1:orders-db".into(), name: "orders-db".into(), region: "europe-west1".into(), version: "POSTGRES_17".into(), state: "RUNNABLE".into(), public_ip: public, private_ip: private, psc, iam }
    }

    #[test]
    fn use_fills_the_form_with_the_instance_the_iam_user_and_an_ip_kind_it_has() {
        let mut f = ConnForm { addr: db::sql::Addr { password: "old".into(), ..Default::default() }, ssh_on: true, tab: ConnTab::Tls, ..Default::default() };
        use_instance(&mut f, &inst(false, true, true, true), Some("ci@acme-prod.iam.gserviceaccount.com"));
        let c = f.cloud_sql.clone().unwrap();
        assert_eq!((c.instance.as_str(), c.iam, c.ip), ("acme-prod:europe-west1:orders-db", true, IpKind::Private));
        assert_eq!(f.addr.user, "ci@acme-prod.iam");
        assert!(f.addr.password.is_empty() && !f.ssh_on && f.tab == ConnTab::Standard && f.name == "orders-db" && f.engine == db::sql::Engine::Postgres);
        // An IAM sign-in saves no password, and the proxy replaces the placeholder host.
        f.addr.password = "typed".into();
        let a = f.sql_addr();
        assert!(a.password.is_empty() && a.host == CLOUD_HOST && !db::mask_uri(&a.to_uri()).contains("****"));
        // Change keeps the name and an IP kind the new instance also has.
        f.name = "mine".into();
        f.cloud_sql.as_mut().unwrap().ip = IpKind::Psc;
        use_instance(&mut f, &inst(true, true, true, false), None);
        assert_eq!((f.name.as_str(), f.cloud_sql.as_ref().unwrap().ip, f.cloud_sql.as_ref().unwrap().iam), ("mine", IpKind::Psc, false));
        use_host(&mut f);
        assert!(f.cloud_sql.is_none() && f.addr.host.is_empty());
    }

    #[test]
    fn ip_kinds_an_instance_lacks_are_off_with_a_note() {
        let (opts, note) = ip_choices(Some(&inst(true, true, false, true)));
        assert_eq!(opts.map(|o| o.2), [true, true, false]);
        assert_eq!(note, "PSC is off for orders-db");
        assert_eq!(ip_choices(Some(&inst(false, true, false, true))).1, "Public and PSC are off for orders-db");
        assert!(ip_choices(None).0.iter().all(|o| o.2));
    }

    #[test]
    fn a_failed_project_says_why_and_proxy_lines_come_out_of_the_error() {
        assert_eq!(reason("acme-sandbox", "Cloud SQL: gcloud failed: ERROR: Cloud SQL Admin API has not been used in project acme-sandbox"), "acme-sandbox has the Cloud SQL Admin API off.");
        assert_eq!(reason("p", "Cloud SQL: gcloud failed: HTTPError 403: not authorized"), "p: this account may not list its instances.");
        assert_eq!(proxy_lines("Cloud SQL proxy failed: a\n\n b \nc"), ["a", "b", "c"]);
        assert_eq!(proxy_lines("Cloud SQL proxy did not accept connections within 20 s: x"), ["x"]);
        assert!(proxy_lines("Cloud SQL IAM sign-in refused (the proxy used x): y").is_empty());
    }
}
