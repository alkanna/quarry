//! The Roles and privileges window of a PostgreSQL connection (design B, the object matrix), with its New role, Change
//! password and Drop role dialogs. The Privileges tab lists the objects of one kind by roles; a cell opens its editor. The
//! Roles tab holds what the design's variant A shows for one role: attributes, memberships and default privileges. Every
//! edit lands in one staged list, previewed in the bar at the bottom and run by Apply in one transaction; the statements,
//! their checks and their inverses come from `db::sql::roles_pg`.

use super::sql_users::{forget_secret, secret_id};
use super::*;
use db::sql::roles_pg::{self as rp, Answer, Attrs, Batch, Change, Cluster, DefKind, Defaults, Did, DropInfo, DropMode, Kind, NewRole, Obj, Objects, Opts, Privs, Role};
use std::sync::Arc;
use zeroize::Zeroize;

const NOTE_LOCKED: &str = "Read-only: unlock writes to change roles and privileges.";
/// What a hash computed here means for a server that checks passwords.
const POLICY: &str = "The server gets no password to check, so policy extensions such as passwordcheck or credcheck cannot hold it to their rules, and one set to accept passwords only in clear refuses it.";

fn hash_name(encryption: &str) -> &'static str {
    if rp::wants_md5(encryption) { "the MD5 hash" } else { "a SCRAM-SHA-256 verifier" }
}

/// On a server that keeps MD5, says so: the hash is what psql's \password would send there, and weaker than SCRAM.
fn md5_note(ui: &mut egui::Ui, encryption: &str) {
    if rp::wants_md5(encryption) {
        ui.add(egui::Label::new(RichText::new(format!("This server keeps passwords as MD5 (password_encryption = {encryption}), so the MD5 hash goes out, as psql's \\password would send. MD5 is weaker than SCRAM-SHA-256 and deprecated from PostgreSQL 18.")).size(TXT_SMALL).color(WARNING())).wrap());
    }
}
const NAME_W: f32 = 170.0;
const COL_W: f32 = 112.0;
const HEAD_H: f32 = 44.0;
const CELL_H: f32 = 31.0;
const CHIP_PRIV_H: f32 = 18.0;
const SIDE_W: f32 = 220.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PTab {
    Privileges,
    Roles,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RTab {
    Role,
    Membership,
    Defaults,
}

/// The + Default privilege form of the Roles tab.
pub(super) struct DefForm {
    owner: String,
    schema: String,
    kind: DefKind,
    privs: BTreeSet<String>,
}

pub(super) struct PgRolesWin {
    pub(super) conn: u64,
    pub(super) tab: PTab,
    pub(super) rtab: RTab,
    pub(super) req_cluster: u64,
    /// Shared so a frame borrows it alongside the window instead of copying every role and membership.
    pub(super) cluster: Option<db::Res<Arc<Cluster>>>,
    /// Each role's `Cluster::chain`, worked out once per answer rather than for every cell of every frame.
    chains: HashMap<String, Vec<String>>,
    pub(super) database: String,
    pub(super) schema: String,
    pub(super) kind: Kind,
    pub(super) inherited: bool,
    /// Whether the pg_ roles are listed.
    pub(super) system: bool,
    pub(super) req_objs: u64,
    pub(super) objs: Option<db::Res<Objects>>,
    pub(super) req_defs: u64,
    pub(super) defs: Option<db::Res<Defaults>>,
    /// The role of the side panel and the Roles tab.
    pub(super) role: Option<String>,
    /// The cell whose editor is open: the object's oid and the grantee ("" for PUBLIC).
    pub(super) cell: Option<(String, String)>,
    pub(super) filter: String,
    pub(super) staged: Vec<Change>,
    pub(super) busy: bool,
    /// The request id of the Apply in flight; an answer with another one is from an Apply this window no longer waits for.
    pub(super) req_apply: u64,
    /// The signed-in role's name typed to confirm a change to its own membership.
    confirm: String,
    /// The Role tab's text fields as typed, for the role in `fields_for`.
    lim: String,
    until: String,
    fields_for: String,
    field_err: Option<String>,
    def_form: Option<DefForm>,
}

impl PgRolesWin {
    pub(super) fn new(conn: u64, database: String) -> PgRolesWin {
        PgRolesWin {
            conn,
            tab: PTab::Privileges,
            rtab: RTab::Role,
            req_cluster: 0,
            cluster: None,
            chains: HashMap::new(),
            database,
            schema: "public".into(),
            kind: Kind::Tables,
            inherited: true,
            system: false,
            req_objs: 0,
            objs: None,
            req_defs: 0,
            defs: None,
            role: None,
            cell: None,
            filter: String::new(),
            staged: Vec::new(),
            busy: false,
            req_apply: 0,
            confirm: String::new(),
            lim: String::new(),
            until: String::new(),
            fields_for: String::new(),
            field_err: None,
            def_form: None,
        }
    }

    fn cl(&self) -> Option<&Cluster> {
        self.cluster.as_ref().and_then(|c| c.as_ref().ok()).map(|c| &**c)
    }

    fn cl_arc(&self) -> Option<Arc<Cluster>> {
        self.cluster.as_ref().and_then(|c| c.as_ref().ok()).cloned()
    }

    fn set_cluster(&mut self, res: db::Res<Cluster>) {
        self.chains = match &res {
            Ok(c) => c.roles.iter().map(|r| (r.name.clone(), c.chain(&r.name))).chain([(String::new(), vec![String::new()])]).collect(),
            Err(_) => HashMap::new(),
        };
        self.cluster = Some(res.map(Arc::new));
    }

    /// The staged changes to the signed-in role's own membership that need a typed confirmation, by index, with why.
    fn risky(&self) -> Vec<(usize, String)> {
        let Some(c) = self.cl() else { return Vec::new() };
        self.staged.iter().enumerate().filter_map(|(i, s)| c.self_membership_risk(s).map(|why| (i, why))).collect()
    }

    fn objects(&self) -> Option<&Objects> {
        self.objs.as_ref().and_then(|o| o.as_ref().ok())
    }

    fn defaults(&self) -> Option<&Defaults> {
        self.defs.as_ref().and_then(|o| o.as_ref().ok())
    }

    /// The roles shown as matrix columns and in the list, pg_ roles only when asked for.
    fn roles(&self) -> Vec<&Role> {
        self.cl().map(|c| c.roles.iter().filter(|r| self.system || !r.system()).collect()).unwrap_or_default()
    }

    /// Staged changes of objects or default privileges, which tie the batch to the picked database.
    fn tied(&self) -> bool {
        self.staged.iter().any(Change::in_database)
    }

    /// Adds a change, folded into one staged earlier on the same thing so the batch holds one change per thing, from what the
    /// server had to what the boxes say now. A change back to where it started is dropped.
    fn stage(&mut self, c: Change) {
        let k = key(&c);
        if let Some(i) = self.staged.iter().position(|s| key(s) == k) {
            let first = self.staged.remove(i);
            let merged = match (first, c) {
                (Change::Acl { target, grantee, from, .. }, Change::Acl { to, .. }) => Change::Acl { target, grantee, from, to },
                (Change::Attrs { role, from, .. }, Change::Attrs { to, .. }) => Change::Attrs { role, from, to },
                (Change::Member { group, member, from, .. }, Change::Member { to, .. }) => Change::Member { group, member, from, to },
                (Change::Default { owner, schema, kind, grantee, from, .. }, Change::Default { to, .. }) => Change::Default { owner, schema, kind, grantee, from, to },
                (_, c) => c,
            };
            if !merged.is_noop() {
                self.staged.insert(i, merged);
            }
        } else if !c.is_noop() {
            self.staged.push(c);
        }
    }

    fn privs_now(&self, o: &Obj, grantee: &str) -> Privs {
        let k = format!("acl|{:?}|{}|{grantee}", self.kind, o.oid);
        match self.staged.iter().find(|s| key(s) == k) {
            Some(Change::Acl { to, .. }) => to.clone(),
            _ => o.direct(grantee),
        }
    }

    fn attrs_now(&self, role: &str) -> Option<Attrs> {
        let k = format!("attrs|{role}");
        match self.staged.iter().find(|s| key(s) == k) {
            Some(Change::Attrs { to, .. }) => Some(to.clone()),
            _ => self.cl()?.role(role).map(|r| r.attrs.clone()),
        }
    }

    fn member_now(&self, group: &str, member: &str) -> Option<Opts> {
        let k = format!("mem|{group}|{member}");
        match self.staged.iter().find(|s| key(s) == k) {
            Some(Change::Member { to, .. }) => *to,
            _ => self.cl()?.membership(group, member),
        }
    }

    fn staged_key(&self, k: &str) -> bool {
        self.staged.iter().any(|s| key(s) == k)
    }
}

fn key(c: &Change) -> String {
    match c {
        Change::Acl { target, grantee, .. } => format!("acl|{:?}|{}|{grantee}", target.kind, target.oid),
        Change::Attrs { role, .. } => format!("attrs|{role}"),
        Change::Member { group, member, .. } => format!("mem|{group}|{member}"),
        Change::Default { owner, schema, kind, grantee, .. } => format!("def|{owner}|{schema}|{:?}|{grantee}", kind),
    }
}

pub(super) struct NewRoleForm {
    conn: u64,
    name: String,
    attrs: Attrs,
    groups: Vec<String>,
    /// Typed here and handed to the worker by Create; never shown, logged or kept after.
    password: String,
    serial: u64,
}

pub(super) struct PgPwForm {
    conn: u64,
    role: String,
    oid: String,
    password: String,
    again: String,
    serial: u64,
}

/// The typed passwords are wiped whenever a dialog goes away, by a button, Esc or another dialog taking its place.
impl Drop for NewRoleForm {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

impl Drop for PgPwForm {
    fn drop(&mut self) {
        self.password.zeroize();
        self.again.zeroize();
    }
}

#[cfg(test)]
impl PgPwForm {
    pub(super) fn for_test(conn: u64, role: &str, serial: u64) -> PgPwForm {
        PgPwForm { conn, role: role.into(), oid: String::new(), password: String::new(), again: String::new(), serial }
    }

    pub(super) fn typed(&self) -> &str {
        &self.password
    }
}

/// Drops what egui keeps of a closing password dialog's fields, the undo history of their text included.
pub(super) fn forget_pg_secrets(ctx: &egui::Context, m: &Modal) {
    match m {
        Modal::PgNewRole(f) => forget_secret(ctx, "pg-new-role", f.serial, 1),
        Modal::PgPassword(f) => forget_secret(ctx, "pg-password", f.serial, 2),
        _ => {}
    }
}

pub(super) struct DropForm {
    conn: u64,
    role: String,
    req: u64,
    info: Option<db::Res<DropInfo>>,
    reassign: bool,
    heir: String,
    typed: String,
}

enum Act {
    Tab(PTab),
    RTab(RTab),
    Database(String),
    Schema(String),
    Kind(Kind),
    Role(String),
    OpenRole(String),
    Stage(Change),
    Apply,
    Discard,
    Close,
    Unlock,
    Refresh,
    NewRole,
    Drop(String),
    Password(String),
}

/// What the tabs need to know about the connection and the login.
struct Env {
    major: u32,
    /// The lock's note while writes are locked.
    locked: Option<String>,
    /// Why privileges and attributes cannot be staged: the lock, or a login without CREATEROLE. Its own password and the members
    /// of roles it holds ADMIN on have checks of their own.
    blocked: Option<String>,
}

impl Env {
    /// Whether `c` may be staged: memberships go by the ADMIN option the server asks for, not by CREATEROLE.
    fn may_stage(&self, c: &Change) -> bool {
        self.locked.is_none() && (self.blocked.is_none() || matches!(c, Change::Member { .. }))
    }
}

/// The matrix columns `left..right` shows, of `n` columns `w` wide from `x0`.
fn col_range(left: f32, right: f32, x0: f32, w: f32, n: usize) -> std::ops::Range<usize> {
    let first = (((left - x0) / w).floor().max(0.0) as usize).min(n);
    let last = (((right - x0) / w).ceil().max(0.0) as usize).min(n);
    first..last.max(first)
}

/// The chip of one privilege in a matrix cell: solid when granted, teal with "G" with the grant option, dashed when held through
/// another role, green for the owner.
#[derive(Clone, Copy, PartialEq)]
enum ChipLook {
    Granted,
    Grant,
    Through,
    Owner,
}

fn chip_text_w(ui: &egui::Ui, text: &str) -> f32 {
    text_w(ui, text, FontId::monospace(TXT_CAPTION)) + 16.0
}

fn paint_priv(ui: &egui::Ui, rect: egui::Rect, text: &str, look: ChipLook) {
    let p = ui.painter();
    let color = match look {
        ChipLook::Granted => C_DATE(),
        ChipLook::Grant => C_NUMBER(),
        ChipLook::Owner => SUCCESS(),
        ChipLook::Through => MUTED(),
    };
    if look == ChipLook::Through {
        let path = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom(), rect.left_top()];
        p.extend(egui::Shape::dashed_line(&path, Stroke::new(1.0, tint(color, 102)), 3.0, 2.0));
    } else {
        p.rect(rect, R_SM, tint(color, 31), Stroke::new(1.0, tint(color, 102)), egui::StrokeKind::Inside);
    }
    let g = p.layout_no_wrap(text.to_string(), FontId::monospace(TXT_CAPTION), color);
    let dy = optical_dy(&g);
    p.galley(rect.center() - g.size() / 2.0 + egui::vec2(0.0, dy), g, color);
}

/// A chip laid out in the flow of a row, for the side panel, the legend and the Roles tab.
fn priv_chip(ui: &mut egui::Ui, text: &str, look: ChipLook) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(chip_text_w(ui, text), CHIP_PRIV_H), Sense::hover());
    paint_priv(ui, rect, text, look);
    r
}

/// The chips of a cell in the order the editor lists them: what is granted, then what comes through a role.
fn cell_chips(cell: &rp::Cell, privs: &Privs, order: &[&str], inherited: bool) -> Vec<(String, ChipLook)> {
    if cell.owner {
        return vec![("owner".into(), ChipLook::Owner)];
    }
    let mut out: Vec<(String, ChipLook)> = order.iter().filter_map(|p| privs.get(*p).map(|g| if *g { (format!("{p} G"), ChipLook::Grant) } else { (p.to_string(), ChipLook::Granted) })).collect();
    if inherited {
        out.extend(order.iter().filter(|p| !privs.contains_key(**p) && cell.via.contains_key(**p)).map(|p| (p.to_string(), ChipLook::Through)));
    }
    out
}

fn banner(ui: &mut egui::Ui, text: &str, color: Color32, unlock: bool, acts: &mut Vec<Act>) {
    egui::Frame::new().fill(tint(color, 26)).stroke(Stroke::new(1.0, tint(color, 90))).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(RichText::new(text).color(if unlock { color } else { BRIGHT() })).wrap());
            if unlock {
                right_group(ui, |ui| {
                    if ui.button("Unlock…").clicked() {
                        acts.push(Act::Unlock);
                    }
                });
            }
        });
    });
}

/// What the role is, in a few words: "login role, member of reporting", "group, 3 members".
fn role_sub(c: &Cluster, r: &Role) -> String {
    let groups: Vec<String> = c.member_of(&r.name).iter().map(|m| m.group.clone()).collect();
    let mut s = if r.attrs.superuser { "superuser".to_string() } else if r.attrs.login { "login role".to_string() } else { "group".to_string() };
    if !groups.is_empty() {
        s.push_str(&format!(", member of {}", groups.join(", ")));
    } else if !r.attrs.login {
        let n = c.members_of(&r.name).len();
        s.push_str(&format!(", {n} member{}", if n == 1 { "" } else { "s" }));
    }
    s
}

/// The matrix column's second line: "login", "group", "login, in reporting".
fn col_sub(c: &Cluster, r: &Role) -> String {
    let base = if r.attrs.superuser { "superuser" } else if r.attrs.login { "login" } else { "group" };
    match c.member_of(&r.name).first() {
        Some(m) => format!("{base}, in {}", m.group),
        None => base.into(),
    }
}

fn mono_label(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    ui.label(RichText::new(text).monospace().size(TXT_BODY).color(color))
}

impl App {
    fn pg_roles_db(&self, conn: u64) -> Option<db::sql::Db> {
        self.conn(conn).and_then(|c| c.sql.clone())
    }

    /// Opens the window on the connection's own database.
    pub(super) fn open_pg_roles(&mut self, conn: u64) {
        let database = self.pg_roles_db(conn).map(|d| d.pg_database()).filter(|d| !d.is_empty()).unwrap_or_else(|| "postgres".into());
        self.pg_roles = Some(PgRolesWin::new(conn, database));
        self.pg_roles_reload();
    }

    /// Reads the roles again, then the objects and default privileges the window shows.
    fn pg_roles_reload(&mut self) {
        let Some((conn, database, schema, kind)) = self.pg_roles.as_ref().map(|w| (w.conn, w.database.clone(), w.schema.clone(), w.kind)) else { return };
        let Some(sql) = self.pg_roles_db(conn) else { return };
        self.req += 3;
        let r = self.req;
        if let Some(w) = &mut self.pg_roles {
            (w.req_cluster, w.req_objs, w.req_defs) = (r - 2, r - 1, r);
            w.cluster = None;
            w.objs = None;
            w.defs = None;
        }
        self.worker.pg_roles_cluster(conn, r - 2, sql.clone());
        self.worker.pg_roles_objects(conn, r - 1, sql.clone(), database.clone(), schema, kind);
        self.worker.pg_roles_defaults(conn, r, sql, database);
    }

    fn pg_roles_objects(&mut self) {
        let Some((conn, database, schema, kind)) = self.pg_roles.as_ref().map(|w| (w.conn, w.database.clone(), w.schema.clone(), w.kind)) else { return };
        let Some(sql) = self.pg_roles_db(conn) else { return };
        self.req += 1;
        let r = self.req;
        if let Some(w) = &mut self.pg_roles {
            w.req_objs = r;
            w.objs = None;
            w.cell = None;
        }
        self.worker.pg_roles_objects(conn, r, sql, database, schema, kind);
    }

    fn pg_roles_defaults(&mut self) {
        let Some((conn, database)) = self.pg_roles.as_ref().map(|w| (w.conn, w.database.clone())) else { return };
        let Some(sql) = self.pg_roles_db(conn) else { return };
        self.req += 1;
        let r = self.req;
        if let Some(w) = &mut self.pg_roles {
            w.req_defs = r;
            w.defs = None;
        }
        self.worker.pg_roles_defaults(conn, r, sql, database);
    }

    pub(super) fn pg_roles_answer(&mut self, conn: u64, req: u64, ans: Answer) {
        match ans {
            Answer::Cluster(res) => {
                if let Some(w) = self.pg_roles.as_mut().filter(|w| w.conn == conn && w.req_cluster == req) {
                    if let Ok(c) = &res {
                        if w.role.as_ref().is_none_or(|r| c.role(r).is_none()) {
                            w.role = c.roles.iter().find(|r| !r.system() && r.name != c.me).or_else(|| c.me()).map(|r| r.name.clone());
                        }
                    }
                    w.set_cluster(res);
                }
            }
            Answer::Objects(res) => {
                if let Some(w) = self.pg_roles.as_mut().filter(|w| w.conn == conn && w.req_objs == req) {
                    // A schema that is not in this database falls back to public or the first one listed.
                    if let Ok(o) = &res {
                        if w.kind.in_schema() && !o.schemas.contains(&w.schema) {
                            if let Some(s) = o.schemas.iter().find(|s| *s == "public").or(o.schemas.first()).cloned() {
                                w.schema = s;
                                w.objs = None;
                                self.pg_roles_objects();
                                return;
                            }
                        }
                    }
                    w.objs = Some(res);
                }
            }
            Answer::Defaults(res) => {
                if let Some(w) = self.pg_roles.as_mut().filter(|w| w.conn == conn && w.req_defs == req) {
                    w.defs = Some(res);
                }
            }
            Answer::DropInfo(res) => {
                if let Some(Modal::PgDropRole(f)) = &mut self.modal {
                    if f.conn == conn && f.req == req {
                        f.info = Some(res);
                    }
                }
            }
            Answer::Done { did, res, undo } => self.pg_roles_done(conn, req, did, res, undo),
        }
    }

    fn pg_roles_done(&mut self, conn: u64, req: u64, did: Did, res: db::Res<String>, undo: Option<UndoAction>) {
        let ok = res.is_ok();
        match res {
            Ok(s) => self.ok(s),
            Err(e) => self.err(e),
        }
        if let Some(u) = undo {
            self.push_undo(u);
        }
        let Some(w) = self.pg_roles.as_mut().filter(|w| w.conn == conn) else { return };
        match &did {
            // Only the Apply this window waits for: one from before Esc closed it must not clear what was staged since.
            Did::Applied if req == w.req_apply => {
                w.busy = false;
                // A failed batch ran nothing and stays staged, so its reason can be read against what the server holds now.
                if ok {
                    w.staged.clear();
                    w.confirm.clear();
                }
            }
            Did::Created(name) if ok => w.role = Some(name.clone()),
            Did::Dropped(name) if ok && w.role.as_deref() == Some(name.as_str()) => w.role = None,
            _ => {}
        }
        w.fields_for.clear();
        if did != Did::Password {
            self.pg_roles_reload();
        }
    }

    fn pg_roles_env(&self, w: &PgRolesWin) -> Env {
        let major = w.cl().map(|c| c.major).or_else(|| self.pg_roles_db(w.conn).map(|d| d.version.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0))).unwrap_or(17);
        let locked = (!self.conn_writable(w.conn)).then(|| NOTE_LOCKED.to_string());
        let blocked = locked.clone().or_else(|| w.cl().and_then(Cluster::read_only_reason));
        Env { major, locked, blocked }
    }

    pub(super) fn pg_roles_window(&mut self, ctx: &egui::Context) {
        let Some(mut w) = self.pg_roles.take() else { return };
        let conn = w.conn;
        let env = self.pg_roles_env(&w);
        let locked = !self.conn_writable(conn);
        let conn_name = self.conn_name(conn);
        let version = self.conn(conn).map(|c| c.server.version.clone()).unwrap_or_default();
        let popover_open = w.cell.is_some();
        let mut acts: Vec<Act> = Vec::new();
        let screen = ctx.content_rect();
        let r = dialog(egui::Id::unique(("quarry-layer", "pg-roles-window"))).show(ctx, |ui| {
            ui.set_width((screen.width() * 0.8).clamp(860.0, 1240.0));
            dialog_title(ui, "Roles and privileges");
            ui.horizontal(|ui| {
                let who = match (w.cl(), &env.blocked) {
                    (Some(c), Some(_)) if !locked => format!(" · signed in as {}", c.me),
                    _ => String::new(),
                };
                ui.label(RichText::new(format!("{conn_name} · PostgreSQL {version}{who}")).color(MUTED()));
                right_group(ui, |ui| {
                    if quiet(ui, "Refresh").clicked() {
                        acts.push(Act::Refresh);
                    }
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 18.0;
                for (t, label) in [(PTab::Privileges, "Privileges"), (PTab::Roles, "Roles")] {
                    if view_tab(ui, label, w.tab == t).clicked() {
                        acts.push(Act::Tab(t));
                    }
                }
            });
            ui.add_space(8.0);
            if let Some(why) = &env.blocked {
                if locked {
                    banner(ui, why, SUCCESS(), true, &mut acts);
                } else {
                    banner(ui, why, PRIMARY(), false, &mut acts);
                }
                ui.add_space(8.0);
            }
            match &w.cluster {
                None => {
                    spin(ui);
                }
                Some(Err(e)) => {
                    ui.add(egui::Label::new(RichText::new(format!("Could not read the roles: {e}")).color(ALERT())).wrap());
                }
                Some(Ok(_)) => match w.tab {
                    PTab::Privileges => self.pg_privileges_tab(ui, &mut w, &env, &mut acts),
                    PTab::Roles => self.pg_roles_tab(ui, &mut w, &env, &mut acts),
                },
            }
            ui.add_space(10.0);
            self.pg_staged_bar(ui, &mut w, &env, &mut acts);
        });
        let mut open = true;
        for a in acts {
            match a {
                Act::Tab(t) => {
                    w.tab = t;
                    w.cell = None;
                }
                Act::RTab(t) => w.rtab = t,
                Act::Database(d) => {
                    w.database = d;
                    self.pg_roles = Some(w);
                    self.pg_roles_objects();
                    self.pg_roles_defaults();
                    w = self.pg_roles.take().unwrap();
                }
                Act::Schema(s) => {
                    w.schema = s;
                    self.pg_roles = Some(w);
                    self.pg_roles_objects();
                    w = self.pg_roles.take().unwrap();
                }
                Act::Kind(k) => {
                    w.kind = k;
                    self.pg_roles = Some(w);
                    self.pg_roles_objects();
                    w = self.pg_roles.take().unwrap();
                }
                Act::Role(r) => w.role = Some(r),
                Act::OpenRole(r) => {
                    w.role = Some(r);
                    w.tab = PTab::Roles;
                    w.rtab = RTab::Role;
                    w.cell = None;
                }
                Act::Stage(c) => {
                    let me = w.cl().map(|c| c.me.clone()).unwrap_or_default();
                    if let Some(why) = Cluster::self_lockout(&me, &c) {
                        self.warn(why);
                    } else if env.may_stage(&c) && !w.busy {
                        w.stage(c);
                    }
                }
                Act::Apply => {
                    self.pg_roles = Some(w);
                    self.pg_roles_apply();
                    w = self.pg_roles.take().unwrap();
                }
                Act::Discard => {
                    w.staged.clear();
                    w.fields_for.clear();
                    w.confirm.clear();
                }
                Act::Close => open = false,
                Act::Unlock => self.modal = Some(Modal::Unlock(conn, None)),
                Act::Refresh => {
                    self.pg_roles = Some(w);
                    self.pg_roles_reload();
                    w = self.pg_roles.take().unwrap();
                }
                Act::NewRole => self.pg_new_role(conn),
                Act::Drop(role) => self.pg_drop_role_ask(conn, role),
                Act::Password(role) => {
                    let oid = w.cl().and_then(|c| c.role(&role)).map(|r| r.oid.clone()).unwrap_or_default();
                    self.req += 1;
                    self.modal = Some(Modal::PgPassword(Box::new(PgPwForm { conn, role, oid, password: String::new(), again: String::new(), serial: self.req })));
                }
            }
        }
        // Esc closes the cell editor first, and the window only on the next press.
        let esc_closes = r.should_close() && self.modal.is_none() && !popover_open;
        if !open || esc_closes {
            return;
        }
        if r.should_close() && popover_open {
            w.cell = None;
        }
        self.pg_roles = Some(w);
    }

    fn pg_roles_apply(&mut self) {
        let Some(w) = self.pg_roles.as_ref() else { return };
        if w.staged.is_empty() || w.busy {
            return;
        }
        let conn = w.conn;
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        if !self.conn_writable(conn) {
            return self.modal = Some(Modal::Unlock(conn, None));
        }
        let Some(sql) = self.pg_roles_db(conn) else { return self.warn_offline() };
        self.req += 1;
        let req = self.req;
        let Some(w) = self.pg_roles.as_mut() else { return };
        let risky = w.risky();
        let me = w.cl().map(|c| c.me.clone()).unwrap_or_default();
        if !risky.is_empty() && w.confirm != me {
            return self.warn(format!("Type {me} to confirm the change to your own membership."));
        }
        // Objects and default privileges run in the database they were read from; role changes run anywhere, so there too.
        let db_oid = if w.tied() { w.objects().map(|o| o.db_oid.clone()).or_else(|| w.defaults().map(|d| d.db_oid.clone())).unwrap_or_default() } else { String::new() };
        let roles = w.cl().map(|c| c.roles.iter().map(|r| (r.name.clone(), r.oid.clone())).collect()).unwrap_or_default();
        let batch = Batch { database: w.database.clone(), db_oid, changes: w.staged.clone(), roles, no_undo: risky.into_iter().map(|(i, _)| i).collect(), ..Default::default() };
        w.busy = true;
        w.req_apply = req;
        self.worker.pg_roles_apply(conn, req, sql, batch, false);
    }

    fn pg_staged_bar(&self, ui: &mut egui::Ui, w: &mut PgRolesWin, env: &Env, acts: &mut Vec<Act>) {
        let preview = rp::preview(env.major, &w.staged);
        let risky = w.risky();
        let me = w.cl().map(|c| c.me.clone()).unwrap_or_default();
        if !risky.is_empty() {
            egui::Frame::new().fill(tint(WARNING(), 26)).stroke(Stroke::new(1.0, tint(WARNING(), 90))).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                for (_, why) in &risky {
                    ui.add(egui::Label::new(RichText::new(why.as_str()).color(BRIGHT())).wrap());
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Type {me} to apply")).color(MUTED()));
                    ui.add(egui::TextEdit::singleline(&mut w.confirm).font(egui::TextStyle::Monospace).desired_width(160.0)).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Confirm own membership"));
                });
            });
            ui.add_space(6.0);
        }
        let gate = env.locked.clone().or_else(|| if w.staged.iter().all(|c| matches!(c, Change::Member { .. })) { None } else { env.blocked.clone() });
        egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, tint(PRIMARY(), 89))).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let n = w.staged.len();
                ui.label(RichText::new(match n {
                    0 => "No changes".to_string(),
                    1 => "1 change".to_string(),
                    n => format!("{n} changes"),
                }).color(BRIGHT()));
                ui.add_space(12.0);
                let right = 170.0;
                let width = (ui.available_width() - right).max(120.0);
                ui.allocate_ui_with_layout(egui::vec2(width, 90.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    egui::ScrollArea::vertical().id_salt("pg-roles-staged").max_height(84.0).auto_shrink([false, true]).show(ui, |ui| match &preview {
                        Ok(t) if t.is_empty() => {
                            ui.label(RichText::new(if env.blocked.is_some() { "Nothing can be staged on this connection." } else { "Click a cell, or change a role, to stage a statement. Nothing runs until Apply." }).size(TXT_SMALL).color(FAINT()));
                        }
                        Ok(t) => {
                            ui.add(egui::Label::new(sql_job(t, TXT_SMALL)).wrap());
                        }
                        Err(e) => {
                            ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(ALERT()));
                        }
                    });
                });
                right_group(ui, |ui| {
                    let confirmed = risky.is_empty() || w.confirm == me;
                    let can = !w.staged.is_empty() && preview.is_ok() && !w.busy && gate.is_none() && confirmed;
                    let why = if w.busy { "Applying…" } else if let Some(b) = &gate { b.as_str() } else if !confirmed { "Type the role name above to confirm" } else { "Nothing to apply" };
                    if ui.add_enabled(can, primary_button(ui, if w.busy { "Applying…" } else { "Apply" }, PRIMARY())).on_disabled_hover_text(why).clicked() {
                        acts.push(Act::Apply);
                    }
                    let cancel = if w.staged.is_empty() { "Close" } else { "Cancel" };
                    if ui.add_enabled(!w.busy, egui::Button::new(cancel)).on_hover_text(if w.staged.is_empty() { "Esc" } else { "Drop every staged change" }).clicked() {
                        acts.push(if w.staged.is_empty() { Act::Close } else { Act::Discard });
                    }
                });
            });
        });
    }

    // Privileges.

    fn pg_privileges_tab(&self, ui: &mut egui::Ui, w: &mut PgRolesWin, env: &Env, acts: &mut Vec<Act>) {
        let Some(c) = w.cl_arc() else { return };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let tied = w.tied();
            ui.add_enabled_ui(!tied, |ui| {
                let combo = egui::ComboBox::from_id_salt("pg-roles-db").width(170.0).selected_text(RichText::new(&w.database).monospace()).show_ui(ui, |ui| {
                    menu_look(ui);
                    for d in &c.databases {
                        if ui.selectable_label(*d == w.database, d).clicked() && *d != w.database {
                            acts.push(Act::Database(d.clone()));
                        }
                    }
                });
                combo.response.on_disabled_hover_text("Apply or cancel the staged changes first: they run in this database.").widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Database picker"));
            });
            if w.kind.in_schema() {
                let schemas = w.objects().map(|o| o.schemas.clone()).unwrap_or_default();
                let combo = egui::ComboBox::from_id_salt("pg-roles-schema").width(150.0).selected_text(RichText::new(&w.schema).monospace()).show_ui(ui, |ui| {
                    menu_look(ui);
                    for s in &schemas {
                        if ui.selectable_label(*s == w.schema, s).clicked() && *s != w.schema {
                            acts.push(Act::Schema(s.clone()));
                        }
                    }
                });
                combo.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Schema picker"));
            }
            let mut kind = w.kind;
            segmented(ui, &mut kind, &rp::KINDS.map(|k| (k, k.label())));
            if kind != w.kind {
                acts.push(Act::Kind(kind));
            }
            right_group(ui, |ui| {
                ui.checkbox(&mut w.inherited, "Show inherited").on_hover_text("Privileges a role holds through the roles it inherits from, and through PUBLIC, as dashed chips");
                // A server with many roles makes a wide matrix; the filter keeps the columns that matter.
                let f = ui.add(egui::TextEdit::singleline(&mut w.filter).hint_text("Filter roles").desired_width(120.0));
                f.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Filter roles"));
            });
        });
        ui.add_space(10.0);
        let side = SIDE_W + 16.0;
        ui.horizontal_top(|ui| {
            let mw = (ui.available_width() - side).max(360.0);
            // A zero height would leave the scroll areas inside no room; the child keeps only the height it uses.
            ui.allocate_ui_with_layout(egui::vec2(mw, 480.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(mw);
                self.pg_matrix(ui, w, &c, env, acts);
            });
            ui.add_space(16.0);
            ui.allocate_ui_with_layout(egui::vec2(SIDE_W, 480.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(SIDE_W);
                pg_side_panel(ui, w, &c, acts);
            });
        });
    }

    fn pg_matrix(&self, ui: &mut egui::Ui, w: &mut PgRolesWin, c: &Cluster, env: &Env, acts: &mut Vec<Act>) {
        // Taken out for the frame instead of cloned: a schema can list thousands of objects.
        let objs = match w.objs.take() {
            None => {
                spin(ui);
                return;
            }
            Some(Err(e)) => {
                ui.add(egui::Label::new(RichText::new(format!("Could not read the {}s of {}: {e}", w.kind.noun(), w.database)).color(ALERT())).wrap());
                w.objs = Some(Err(e));
                return;
            }
            Some(Ok(o)) => o,
        };
        self.pg_matrix_of(ui, w, c, env, acts, &objs);
        w.objs = Some(Ok(objs));
    }

    fn pg_matrix_of(&self, ui: &mut egui::Ui, w: &mut PgRolesWin, c: &Cluster, env: &Env, acts: &mut Vec<Act>, objs: &Objects) {
        let find = w.filter.trim().to_lowercase();
        let mut cols: Vec<(String, String)> = w.roles().iter().filter(|r| find.is_empty() || r.name.to_lowercase().contains(&find)).map(|r| (r.name.clone(), col_sub(c, r))).collect();
        cols.push((String::new(), "every role".into()));
        let order = w.kind.privs(env.major);
        let total_w = NAME_W + COL_W * cols.len() as f32;
        let head = match w.kind {
            Kind::Tables => "Table",
            Kind::Sequences => "Sequence",
            Kind::Functions => "Function",
            Kind::Schemas => "Schema",
            Kind::Databases => "Database",
        };
        if objs.objs.is_empty() {
            ui.label(RichText::new(format!("No {}s here.", w.kind.noun())).color(MUTED()));
            return;
        }
        if objs.cut {
            ui.label(RichText::new(format!("Only the first {} are listed.", rp::OBJ_LIMIT)).size(TXT_SMALL).color(WARNING()));
        }
        let frame = egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD);
        frame.show(ui, |ui| {
            egui::ScrollArea::horizontal().id_salt("pg-matrix-h").auto_shrink([false, true]).show(ui, |ui| {
                // The header row.
                let (hr, _) = ui.allocate_exact_size(egui::vec2(total_w, HEAD_H), Sense::hover());
                ui.painter().rect_filled(hr, egui::CornerRadius { nw: R_MD, ne: 0, sw: 0, se: 0 }, HDR());
                ui.painter().text(egui::pos2(hr.left() + 12.0, hr.center().y), egui::Align2::LEFT_CENTER, head, FontId::proportional(TXT_SMALL), MUTED());
                // A server with hundreds of roles makes a matrix far wider than the window; only the columns in view are drawn.
                let clip = ui.clip_rect();
                let vis = col_range(clip.left(), clip.right(), hr.left() + NAME_W, COL_W, cols.len());
                for (i, (name, sub)) in cols.iter().enumerate().take(vis.end).skip(vis.start) {
                    let rect = egui::Rect::from_min_size(egui::pos2(hr.left() + NAME_W + COL_W * i as f32, hr.top()), egui::vec2(COL_W, HEAD_H));
                    let label = if name.is_empty() { "Role PUBLIC".to_string() } else { format!("Role {name}") };
                    let resp = ui.interact(rect, egui::Id::unique(("pg-col", w.conn, i)), if name.is_empty() { Sense::hover() } else { Sense::click() });
                    let active = !name.is_empty() && w.role.as_deref() == Some(name.as_str());
                    if active {
                        ui.painter().rect_filled(rect, 0.0, tint(PRIMARY(), 20));
                    } else if resp.hovered() && !name.is_empty() {
                        ui.painter().rect_filled(rect, 0.0, HOVER());
                    }
                    place(ui, egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), rect.top() + 26.0)), if name.is_empty() { "PUBLIC" } else { name }, FontId::monospace(TXT_BODY), BRIGHT(), false, 8.0);
                    place(ui, egui::Rect::from_min_max(egui::pos2(rect.left(), rect.top() + 22.0), egui::pos2(rect.right(), rect.bottom() - 4.0)), sub, FontId::proportional(TXT_CAPTION), FAINT(), false, 8.0);
                    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label.clone()));
                    if resp.clicked() {
                        acts.push(Act::Role(name.clone()));
                    }
                }
                // Room for the horizontal scroll bar under the last row.
                // The horizontal bar floats over the content, so the room is the bar's whole height, not a margin.
                let h = (objs.objs.len() as f32 * CELL_H + 24.0).min(330.0);
                egui::ScrollArea::vertical().id_salt("pg-matrix-v").max_height(h).min_scrolled_height(h).auto_shrink([false, true]).show_rows(ui, CELL_H, objs.objs.len(), |ui, range| {
                    for i in range {
                        let o = &objs.objs[i];
                        let (row, _) = ui.allocate_exact_size(egui::vec2(total_w, CELL_H), Sense::hover());
                        if i % 2 == 1 {
                            ui.painter().rect_filled(row, 0.0, pal().stripe);
                        }
                        let name_rect = egui::Rect::from_min_size(row.min, egui::vec2(NAME_W, CELL_H));
                        let shown = if w.kind == Kind::Functions { format!("{}({})", o.name, o.sig) } else { o.name.clone() };
                        let at = place(ui, name_rect, &shown, FontId::monospace(TXT_BODY), TEXT(), false, 12.0);
                        if !o.detail.is_empty() && at.right() + 40.0 < name_rect.right() {
                            place(ui, egui::Rect::from_min_max(egui::pos2(at.right() + 6.0, row.top()), name_rect.max), &o.detail, FontId::proportional(TXT_CAPTION), FAINT(), false, 0.0);
                        }
                        for (j, (role, _)) in cols.iter().enumerate().take(vis.end).skip(vis.start) {
                            let rect = egui::Rect::from_min_size(egui::pos2(row.left() + NAME_W + COL_W * j as f32, row.top()), egui::vec2(COL_W, CELL_H));
                            if w.role.as_deref() == Some(role.as_str()) && !role.is_empty() {
                                ui.painter().rect_filled(rect, 0.0, tint(PRIMARY(), 12));
                            }
                            self.pg_cell(ui, w, c, o, role, rect, &order, env, acts);
                        }
                    }
                });
            });
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn pg_cell(&self, ui: &mut egui::Ui, w: &mut PgRolesWin, c: &Cluster, o: &Obj, role: &str, rect: egui::Rect, order: &[&str], env: &Env, acts: &mut Vec<Act>) {
        let id = egui::Id::unique(("pg-cell", w.conn, &o.oid, role));
        let resp = ui.interact(rect.shrink(2.0), id, Sense::click());
        let cell = match w.chains.get(role) {
            Some(chain) => rp::cell(o, chain),
            None => rp::cell(o, &if role.is_empty() { vec![String::new()] } else { c.chain(role) }),
        };
        let privs = w.privs_now(o, role);
        let staged = w.staged_key(&format!("acl|{:?}|{}|{role}", w.kind, o.oid));
        let this = |w: &PgRolesWin| w.cell.as_ref().is_some_and(|(oid, g)| *oid == o.oid && g == role);
        let open = this(w);
        if resp.hovered() && !open {
            ui.painter().rect_filled(rect.shrink(2.0), R_SM, HOVER());
        }
        let chips = cell_chips(&cell, &privs, order, w.inherited);
        let inner = rect.shrink2(egui::vec2(8.0, 0.0));
        if chips.is_empty() {
            ui.painter().text(egui::pos2(inner.left(), rect.center().y), egui::Align2::LEFT_CENTER, "none", FontId::proportional(TXT_SMALL), FAINT());
        } else {
            let first = &chips[0];
            let fw = chip_text_w(ui, &first.0).min(inner.width() - 30.0);
            let r1 = egui::Rect::from_min_size(egui::pos2(inner.left(), rect.center().y - CHIP_PRIV_H / 2.0), egui::vec2(fw, CHIP_PRIV_H));
            paint_priv(ui, r1, &first.0, first.1);
            if chips.len() > 1 {
                let more = format!("+{}", chips.len() - 1);
                let look = if chips[1..].iter().any(|c| c.1 != ChipLook::Through) { ChipLook::Granted } else { ChipLook::Through };
                let r2 = egui::Rect::from_min_size(egui::pos2(r1.right() + 4.0, r1.top()), egui::vec2(chip_text_w(ui, &more), CHIP_PRIV_H));
                paint_priv(ui, r2, &more, look);
            }
        }
        if staged {
            ui.painter().circle_filled(egui::pos2(rect.right() - 6.0, rect.top() + 6.0), 3.0, PRIMARY());
        }
        if open {
            ui.painter().rect_stroke(rect.shrink(1.0), 5.0, Stroke::new(1.5, PRIMARY()), egui::StrokeKind::Inside);
        }
        let who = if role.is_empty() { "PUBLIC" } else { role };
        let label = format!("{who} on {}", o.name);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label.clone()));
        let resp = if open {
            resp
        } else {
            let lines: Vec<String> = if cell.owner {
                vec![format!("{who} owns {}: it holds every privilege on it.", o.name)]
            } else {
                order.iter().filter_map(|p| match (privs.get(*p), cell.via.get(*p)) {
                    (Some(true), _) => Some(format!("{p}, with grant option")),
                    (Some(false), _) => Some(p.to_string()),
                    (None, Some(by)) => Some(format!("{p}, through {}", if by.is_empty() { "PUBLIC" } else { by })),
                    _ => None,
                }).collect()
            };
            if lines.is_empty() { resp.on_hover_text(format!("{who} holds nothing on {}", o.name)) } else { resp.on_hover_text(lines.join("\n")) }
        };
        let pid = resp.id.with("clause-popover");
        ui.ctx().data_mut(|d| d.insert_temp(pid, open));
        let edit = env.blocked.clone();
        let out = popover(&resp, 230.0, open, |ui| {
            menu_look(ui);
            let mut changed: Option<Privs> = None;
            popover_body(ui, 230.0, |ui| {
                ui.label(semibold(RichText::new(format!("{who} on {}", if o.schema.is_empty() { o.name.clone() } else { format!("{}.{}", o.schema, o.name) })).size(TXT_TITLE).color(BRIGHT())));
                ui.add_space(8.0);
                let (hr, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), Sense::hover());
                ui.painter().text(egui::pos2(hr.right() - 104.0, hr.center().y), egui::Align2::CENTER_CENTER, "Granted", FontId::proportional(TXT_SMALL), MUTED());
                ui.painter().text(egui::pos2(hr.right() - 30.0, hr.center().y), egui::Align2::CENTER_CENTER, "Grant option", FontId::proportional(TXT_SMALL), MUTED());
                ui.add_space(4.0);
                let can = edit.is_none() && !w.busy;
                for p in order {
                    let (row, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), Sense::hover());
                    ui.painter().text(egui::pos2(row.left(), row.center().y), egui::Align2::LEFT_CENTER, *p, FontId::monospace(TXT_BODY), TEXT());
                    let held = privs.get(*p).copied();
                    let g = check_box(ui, egui::pos2(row.right() - 104.0, row.center().y), id.with(("g", *p)), held.is_some(), false);
                    g.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, can, held.is_some(), format!("Granted {p}")));
                    let opt_ok = can && !role.is_empty();
                    let go = check_box(ui, egui::pos2(row.right() - 30.0, row.center().y), id.with(("o", *p)), held == Some(true), false);
                    go.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, opt_ok, held == Some(true), format!("Grant option {p}")));
                    let mut next = privs.clone();
                    if can && g.clicked() {
                        if held.is_some() {
                            next.remove(*p);
                        } else {
                            next.insert(p.to_string(), false);
                        }
                        changed = Some(next.clone());
                    }
                    if opt_ok && go.clicked() {
                        next.insert(p.to_string(), held != Some(true));
                        changed = Some(next);
                    }
                }
                if let Some(why) = &edit {
                    ui.add_space(6.0);
                    ui.add(egui::Label::new(RichText::new(why.as_str()).size(TXT_SMALL).color(FAINT())).wrap());
                }
                let others = o.other_grantors(role, &c.me);
                if !others.is_empty() {
                    ui.add_space(6.0);
                    ui.add(egui::Label::new(RichText::new(format!("Also granted by {}: only that role can revoke its grant.", others.join(", "))).size(TXT_SMALL).color(WARNING())).wrap());
                }
            });
            (changed, ui.min_rect())
        });
        let now_open = ui.ctx().data(|d| d.get_temp::<bool>(pid)).unwrap_or(false);
        if let Some((changed, area)) = out {
            if let Some(to) = changed {
                acts.push(Act::Stage(Change::Acl { target: o.target(w.kind), grantee: role.into(), from: o.direct(role), to }));
            }
            // A click in the window but outside the editor and its cell closes it; the page behind is covered by the window.
            let click = ui.input(|i| i.pointer.any_click().then(|| i.pointer.interact_pos()).flatten());
            if click.is_some_and(|p| !area.contains(p) && !rect.contains(p)) {
                ui.ctx().data_mut(|d| d.insert_temp(pid, false));
                if this(w) {
                    w.cell = None;
                }
                return;
            }
        }
        if now_open && !open {
            w.cell = Some((o.oid.clone(), role.to_string()));
        } else if !now_open && this(w) {
            w.cell = None;
        }
    }

    // Roles.

    fn pg_roles_tab(&self, ui: &mut egui::Ui, w: &mut PgRolesWin, env: &Env, acts: &mut Vec<Act>) {
        let Some(c) = w.cl_arc() else { return };
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(egui::vec2(240.0, 480.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(240.0);
                pg_role_list(ui, w, &c, env, acts);
            });
            ui.add_space(20.0);
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 18.0;
                    for (t, label) in [(RTab::Role, "Role"), (RTab::Membership, "Membership"), (RTab::Defaults, "Default privileges")] {
                        if view_tab(ui, label, w.rtab == t).clicked() {
                            acts.push(Act::RTab(t));
                        }
                    }
                });
                ui.add_space(8.0);
                let Some(role) = w.role.clone().and_then(|r| c.role(&r).cloned()) else {
                    ui.label(RichText::new("No role chosen.").color(MUTED()));
                    return;
                };
                match w.rtab {
                    RTab::Role => self.pg_role_attrs(ui, w, &c, &role, env, acts),
                    RTab::Membership => pg_role_members(ui, w, &c, &role, env, acts),
                    RTab::Defaults => pg_role_defaults(ui, w, &c, &role, env, acts),
                }
            });
        });
    }

    fn pg_role_attrs(&self, ui: &mut egui::Ui, w: &mut PgRolesWin, c: &Cluster, role: &Role, env: &Env, acts: &mut Vec<Act>) {
        let may = c.may_alter(role);
        let can = env.blocked.is_none() && may.is_ok() && !w.busy;
        if let (None, Err(why)) = (&env.blocked, &may) {
            ui.label(RichText::new(why.as_str()).size(TXT_SMALL).color(WARNING()));
            ui.add_space(4.0);
        }
        let now = w.attrs_now(&role.name).unwrap_or_default();
        if w.fields_for != role.name {
            w.fields_for = role.name.clone();
            w.lim = now.conn_limit.to_string();
            w.until = now.valid_until.clone();
            w.field_err = None;
        }
        caption(ui, "ATTRIBUTES");
        ui.add_space(4.0);
        egui::Grid::new("pg-role-attrs").num_columns(4).spacing([14.0, 8.0]).show(ui, |ui| {
            // The design's two columns: LOGIN, CREATEDB, INHERIT, BYPASSRLS on the left.
            let pairs = [("LOGIN", "SUPERUSER"), ("CREATEDB", "CREATEROLE"), ("INHERIT", "REPLICATION"), ("BYPASSRLS", "")];
            for (a, b) in pairs {
                for f in [a, b] {
                    if f.is_empty() {
                        continue;
                    }
                    let desc = rp::FLAGS.iter().find(|(n, _)| *n == f).map(|(_, d)| *d).unwrap_or_default();
                    let mut off = now.clone();
                    if let Some(v) = off.flag_mut(f) {
                        *v = false;
                    }
                    let flag_ok = c.may_set_flag(f).and_then(|_| match Cluster::self_lockout(&c.me, &Change::Attrs { role: role.name.clone(), from: now.clone(), to: off }) {
                        Some(why) => Err(why),
                        None => Ok(()),
                    });
                    let mut on = now.flag(f);
                    let r = ui.add_enabled(can && flag_ok.is_ok(), egui::Checkbox::new(&mut on, RichText::new(f).monospace().size(TXT_BODY)));
                    if let Err(why) = &flag_ok {
                        r.on_disabled_hover_text(why.as_str());
                    }
                    ui.label(RichText::new(desc).size(TXT_SMALL).color(FAINT()));
                    if on != now.flag(f) {
                        let mut to = now.clone();
                        if let Some(v) = to.flag_mut(f) {
                            *v = on;
                        }
                        acts.push(Act::Stage(Change::Attrs { role: role.name.clone(), from: role.attrs.clone(), to }));
                    }
                }
                ui.end_row();
            }
        });
        ui.add_space(12.0);
        caption(ui, "SIGN IN");
        ui.add_space(4.0);
        egui::Grid::new("pg-role-signin").num_columns(3).spacing([14.0, 8.0]).min_col_width(140.0).show(ui, |ui| {
            form_label(ui, "Connection limit");
            let r = ui.add_enabled(can, egui::TextEdit::singleline(&mut w.lim).font(egui::TextStyle::Monospace).desired_width(90.0));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Connection limit"));
            let was = role.attrs.conn_limit;
            ui.label(RichText::new(if now.conn_limit != was { format!("was {was}, -1 is no limit") } else { "-1 is no limit".into() }).size(TXT_SMALL).color(FAINT()));
            ui.end_row();
            if r.changed() {
                match w.lim.trim().parse::<i64>() {
                    Ok(n) if (-1..=i32::MAX as i64).contains(&n) => {
                        w.field_err = None;
                        acts.push(Act::Stage(Change::Attrs { role: role.name.clone(), from: role.attrs.clone(), to: Attrs { conn_limit: n, ..now.clone() } }));
                    }
                    _ => w.field_err = Some("The connection limit is a whole number, -1 for none.".into()),
                }
            }
            form_label(ui, "Valid until");
            let r = ui.add_enabled(can, egui::TextEdit::singleline(&mut w.until).font(egui::TextStyle::Monospace).desired_width(220.0).hint_text("never"));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Valid until"));
            ui.label(RichText::new(if now.valid_until == "-infinity" { "expired: -infinity, never valid" } else { "a date, or empty for never" }).size(TXT_SMALL).color(FAINT()));
            ui.end_row();
            if r.changed() {
                let v = w.until.trim();
                let v = if v.eq_ignore_ascii_case("never") { "" } else { v };
                acts.push(Act::Stage(Change::Attrs { role: role.name.clone(), from: role.attrs.clone(), to: Attrs { valid_until: v.to_string(), ..now.clone() } }));
            }
            form_label(ui, "Password");
            // Every role may set its own password, CREATEROLE or not.
            let may_pw = c.may_password(role);
            let pw_ok = env.locked.is_none() && may_pw.is_ok();
            if ui.add_enabled(pw_ok, egui::Button::new("Change password…")).on_disabled_hover_text(env.locked.clone().or(may_pw.err()).unwrap_or_default()).clicked() {
                acts.push(Act::Password(role.name.clone()));
            }
            ui.label(RichText::new("A new password cannot be undone").size(TXT_SMALL).color(FAINT()));
            ui.end_row();
        });
        if let Some(e) = &w.field_err {
            ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(ALERT()));
        }
        ui.add_space(12.0);
        let drop_ok = env.blocked.is_none() && may.is_ok() && role.name != c.me;
        let why = if role.name == c.me { "This is the role you are signed in as.".to_string() } else { env.blocked.clone().or(may.err()).unwrap_or_default() };
        if ui.add_enabled_ui(drop_ok, |ui| danger(ui, "Drop role…")).inner.on_disabled_hover_text(why).clicked() {
            acts.push(Act::Drop(role.name.clone()));
        }
    }

    // Dialogs.

    fn pg_new_role(&mut self, conn: u64) {
        if !self.conn_writable(conn) {
            return self.modal = Some(Modal::Unlock(conn, None));
        }
        self.req += 1;
        self.modal = Some(Modal::PgNewRole(Box::new(NewRoleForm { conn, name: String::new(), attrs: Attrs { login: true, ..Default::default() }, groups: Vec::new(), password: String::new(), serial: self.req })));
    }

    pub(super) fn pg_new_role_dialog(&mut self, ui: &mut egui::Ui, f: &mut NewRoleForm, keep: &mut bool) {
        ui.set_width(540.0);
        let writable = self.conn_writable(f.conn);
        let cluster = self.pg_roles.as_ref().and_then(|w| w.cl_arc()).unwrap_or_default();
        dialog_title(ui, "New role");
        ui.label(RichText::new(self.conn_name(f.conn)).color(MUTED()));
        ui.add_space(10.0);
        egui::Grid::new("pg-new-role").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W + 30.0).show(ui, |ui| {
            form_label(ui, "Name");
            let r = ui.add(egui::TextEdit::singleline(&mut f.name).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Role name"));
            if f.name.is_empty() && !r.has_focus() && f.password.is_empty() {
                r.request_focus();
            }
            ui.end_row();
            form_label(ui, "Kind");
            segmented(ui, &mut f.attrs.login, &[(true, "Can sign in"), (false, "Group only")]);
            ui.end_row();
            if f.attrs.login {
                form_label(ui, "Password");
                ui.vertical(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut f.password).id(secret_id("pg-new-role", f.serial, 0)).password(true).desired_width(f32::INFINITY)).widget_info(|| egui::WidgetInfo::labeled(egui::Role::PasswordInput, true, "Password"));
                    ui.add(egui::Label::new(RichText::new(format!("Sent once as {}, shown as *** in the log; never kept by Quarry. Empty for none. {POLICY}", hash_name(&cluster.password_encryption))).size(TXT_SMALL).color(FAINT())).wrap());
                    md5_note(ui, &cluster.password_encryption);
                });
                ui.end_row();
            }
            form_label(ui, "Attributes");
            egui::Grid::new("pg-new-role-flags").num_columns(3).spacing([18.0, 6.0]).show(ui, |ui| {
                for (i, flag) in ["CREATEDB", "CREATEROLE", "INHERIT", "REPLICATION", "BYPASSRLS", "SUPERUSER"].into_iter().enumerate() {
                    let ok = cluster.may_set_flag(flag);
                    if let Some(v) = f.attrs.flag_mut(flag) {
                        let r = ui.add_enabled(ok.is_ok() || flag == "INHERIT", egui::Checkbox::new(v, RichText::new(flag).monospace().size(TXT_BODY)));
                        if let Err(why) = ok {
                            r.on_disabled_hover_text(why);
                        }
                    }
                    if i % 3 == 2 {
                        ui.end_row();
                    }
                }
            });
            ui.end_row();
            form_label(ui, "Connection limit");
            ui.horizontal(|ui| {
                let mut t = f.attrs.conn_limit.to_string();
                if ui.add(egui::TextEdit::singleline(&mut t).font(egui::TextStyle::Monospace).desired_width(70.0)).changed() {
                    f.attrs.conn_limit = t.trim().parse().unwrap_or(if t.trim().is_empty() { -1 } else { f.attrs.conn_limit });
                }
                ui.label(RichText::new("-1 is no limit").size(TXT_SMALL).color(FAINT()));
            });
            ui.end_row();
            form_label(ui, "Member of");
            ui.horizontal_wrapped(|ui| {
                let mut gone = None;
                for (i, g) in f.groups.iter().enumerate() {
                    if chip(ui, &format!("{g} ×"), C_DATE(), true).on_hover_text("Remove").clicked() {
                        gone = Some(i);
                    }
                }
                if let Some(i) = gone {
                    f.groups.remove(i);
                }
                let mut pick = None;
                egui::ComboBox::from_id_salt("pg-new-role-group").selected_text("+ Add").width(140.0).show_ui(ui, |ui| {
                    menu_look(ui);
                    for r in cluster.roles.iter().filter(|r| !r.system() && !f.groups.contains(&r.name)) {
                        if ui.selectable_label(false, &r.name).clicked() {
                            pick = Some(r.name.clone());
                        }
                    }
                });
                if let Some(p) = pick {
                    f.groups.push(p);
                }
            });
            ui.end_row();
        });
        ui.add_space(10.0);
        let secret = (!f.password.is_empty() && f.attrs.login).then_some(rp::REDACTED);
        let spec = NewRole { name: f.name.trim().to_string(), attrs: f.attrs.clone(), groups: f.groups.clone() };
        let check = rp::create_sql(&spec, secret).and_then(|_| if f.password.contains('\0') { Err("A password cannot hold a NUL character.".into()) } else { Ok(()) });
        super::sql_create::sql_block(ui, &match rp::create_sql(&spec, secret) {
            Ok(s) => format!("{s};"),
            Err(_) => "-- Not valid yet.".into(),
        });
        if let (Err(e), false) = (&check, f.name.is_empty()) {
            ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(ALERT()));
        }
        ui.add_space(4.0);
        ui.label(RichText::new("Undo drops the role again while it owns nothing and holds no privileges.").size(TXT_SMALL).color(MUTED()));
        if !writable {
            ui.label(RichText::new(NOTE_LOCKED).size(TXT_SMALL).color(FAINT()));
        }
        dialog_actions(ui, |ui| {
            if ui.add_enabled(writable && check.is_ok(), primary_button(ui, "Create", PRIMARY())).clicked() {
                // Whatever does not reach the worker is wiped here, a group's password included.
                let mut password = zeroize::Zeroizing::new(std::mem::take(&mut f.password));
                if !spec.attrs.login {
                    password.zeroize();
                }
                if self.demo {
                    self.ok("Offline demo: no database is connected.");
                } else if let Some(sql) = self.pg_roles_db(f.conn) {
                    self.worker.pg_role_create(f.conn, sql, spec.clone(), std::mem::take(&mut *password), false);
                }
                *keep = false;
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
        });
        if !*keep {
            f.password.zeroize();
            forget_secret(ui.ctx(), "pg-new-role", f.serial, 1);
        }
    }

    pub(super) fn pg_password_dialog(&mut self, ui: &mut egui::Ui, f: &mut PgPwForm, keep: &mut bool) {
        ui.set_width(520.0);
        let writable = self.conn_writable(f.conn);
        let encryption = self.pg_roles.as_ref().and_then(|w| w.cl().map(|c| c.password_encryption.clone())).unwrap_or_default();
        dialog_title(ui, "Change password");
        ui.label(RichText::new(&f.role).monospace().color(MUTED()));
        ui.add_space(8.0);
        egui::Grid::new("pg-newpw").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W + 14.0).show(ui, |ui| {
            form_label(ui, "New password");
            let r = ui.add(egui::TextEdit::singleline(&mut f.password).id(secret_id("pg-password", f.serial, 0)).password(true).desired_width(340.0));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::PasswordInput, true, "New password"));
            if !r.has_focus() && f.password.is_empty() && f.again.is_empty() {
                r.request_focus();
            }
            ui.end_row();
            form_label(ui, "Repeat");
            ui.add(egui::TextEdit::singleline(&mut f.again).id(secret_id("pg-password", f.serial, 1)).password(true).desired_width(340.0)).widget_info(|| egui::WidgetInfo::labeled(egui::Role::PasswordInput, true, "Repeat password"));
            ui.end_row();
        });
        ui.add_space(8.0);
        super::sql_create::sql_block(ui, &format!("{};", rp::password_sql(&f.role, rp::REDACTED)));
        let check = rp::check_password(&f.password).and_then(|_| if f.password == f.again { Ok(()) } else { Err("The two passwords differ.".to_string()) });
        if let Err(e) = &check {
            if !f.password.is_empty() || !f.again.is_empty() {
                ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(ALERT()));
            }
        }
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(format!("A password change cannot be undone. Quarry computes {} here and sends only that, once; the password is not shown, logged or kept. {POLICY}", hash_name(&encryption))).size(TXT_SMALL).color(MUTED())).wrap());
        md5_note(ui, &encryption);
        if !writable {
            ui.label(RichText::new(NOTE_LOCKED).size(TXT_SMALL).color(FAINT()));
        }
        dialog_actions(ui, |ui| {
            if ui.add_enabled(writable && check.is_ok(), primary_button(ui, "Change password", PRIMARY())).clicked() {
                let mut password = zeroize::Zeroizing::new(std::mem::take(&mut f.password));
                f.again.zeroize();
                if self.demo {
                    self.ok("Offline demo: no database is connected.");
                } else if let Some(sql) = self.pg_roles_db(f.conn) {
                    self.worker.pg_role_password(f.conn, sql, f.role.clone(), f.oid.clone(), std::mem::take(&mut *password), false);
                }
                *keep = false;
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
        });
        if !*keep {
            f.password.zeroize();
            f.again.zeroize();
            forget_secret(ui.ctx(), "pg-password", f.serial, 2);
        }
    }

    fn pg_drop_role_ask(&mut self, conn: u64, role: String) {
        if !self.conn_writable(conn) {
            return self.modal = Some(Modal::Unlock(conn, None));
        }
        self.req += 1;
        let req = self.req;
        let heir = self.pg_roles.as_ref().and_then(|w| w.cl().map(|c| c.me.clone())).unwrap_or_default();
        self.modal = Some(Modal::PgDropRole(Box::new(DropForm { conn, role: role.clone(), req, info: None, reassign: true, heir, typed: String::new() })));
        if let Some(sql) = self.pg_roles_db(conn) {
            self.worker.pg_role_drop_info(conn, req, sql, role);
        }
    }

    pub(super) fn pg_drop_role_dialog(&mut self, ui: &mut egui::Ui, f: &mut DropForm, keep: &mut bool) {
        ui.set_width(540.0);
        let writable = self.conn_writable(f.conn);
        let tag = self.conn(f.conn).map(|c| c.meta.tag.clone()).unwrap_or_default();
        let home = self.pg_roles_db(f.conn).map(|d| d.pg_database()).unwrap_or_default();
        let roles: Vec<String> = self.pg_roles.as_ref().and_then(|w| w.cl().map(|c| c.roles.iter().filter(|r| !r.system() && r.name != f.role).map(|r| r.name.clone()).collect())).unwrap_or_default();
        dialog_title(ui, format!("Drop role {}?", f.role));
        let plan = match &f.info {
            None => {
                spin(ui);
                None
            }
            Some(Err(e)) => {
                ui.add(egui::Label::new(RichText::new(format!("Could not read what {} holds: {e}", f.role)).color(ALERT())).wrap());
                None
            }
            Some(Ok(info)) => {
                let dbs = info.dbs.len();
                ui.label(RichText::new(format!("{} owns {} and holds privileges in {} on {}.", f.role, plural(info.owned_count() as u64, "object"), plural(dbs as u64, "database"), self.conn_name(f.conn))).color(TEXT()));
                ui.label(RichText::new("PostgreSQL refuses DROP ROLE until both are gone. This cannot be undone.").color(MUTED()));
                ui.add_space(8.0);
                let owned: Vec<(String, String)> = info.dbs.iter().flat_map(|d| d.owned.iter().map(|o| (d.database.clone(), o.clone()))).chain(info.shared.iter().map(|s| (String::new(), s.clone()))).collect();
                if !owned.is_empty() {
                    caption(ui, format!("OWNED BY {}", f.role.to_uppercase()));
                    ui.add_space(4.0);
                    egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        egui::ScrollArea::vertical().id_salt("pg-drop-owned").max_height(120.0).show(ui, |ui| {
                            egui::Grid::new("pg-drop-owned-grid").num_columns(2).spacing([18.0, 6.0]).striped(true).show(ui, |ui| {
                                for (d, o) in &owned {
                                    ui.label(RichText::new(if d.is_empty() { "server" } else { d.as_str() }).color(MUTED()));
                                    mono_label(ui, o, TEXT());
                                    ui.end_row();
                                }
                            });
                        });
                    });
                    ui.add_space(8.0);
                }
                let held: usize = info.dbs.iter().map(|d| d.held).sum();
                caption(ui, "ALSO HELD");
                if held > 0 || info.shared_held.is_empty() {
                    ui.label(RichText::new(if held == 0 { "No privileges or default privileges.".to_string() } else { format!("{} in {}.", plural(held as u64, "privilege or default privilege"), info.dbs.iter().filter(|d| d.held > 0).map(|d| d.database.clone()).collect::<Vec<_>>().join(", ")) }).color(TEXT()));
                }
                if !info.shared_held.is_empty() {
                    ui.add(egui::Label::new(RichText::new(format!("Privileges on {}, revoked by DROP OWNED.", info.shared_held.join(", "))).color(TEXT())).wrap());
                }
                for (d, why) in &info.unread {
                    ui.label(RichText::new(format!("{d} could not be read: {why}")).size(TXT_SMALL).color(WARNING()));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.radio_value(&mut f.reassign, true, "Give the objects to");
                    egui::ComboBox::from_id_salt("pg-drop-heir").width(160.0).selected_text(RichText::new(&f.heir).monospace()).show_ui(ui, |ui| {
                        menu_look(ui);
                        for r in &roles {
                            ui.selectable_value(&mut f.heir, r.clone(), r);
                        }
                    });
                });
                ui.label(RichText::new(format!("REASSIGN OWNED keeps them; DROP OWNED then removes only {}'s privileges.", f.role)).size(TXT_SMALL).color(FAINT()));
                ui.radio_value(&mut f.reassign, false, "Drop the objects with the role");
                ui.label(RichText::new(format!("DROP OWNED deletes the {} and anything that depends on them.", plural(info.owned_count() as u64, "object"))).size(TXT_SMALL).color(FAINT()));
                ui.add_space(8.0);
                let mode = if f.reassign { DropMode::Reassign(f.heir.clone()) } else { DropMode::DropOwned };
                let plan = rp::drop_plan(info, &mode, &home);
                match &plan {
                    Ok(p) => super::sql_create::sql_block(ui, &rp::drop_preview(p)),
                    Err(e) => {
                        ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(ALERT()));
                    }
                }
                plan.ok().map(|_| (info.clone(), mode))
            }
        };
        ui.add_space(8.0);
        ui.label(RichText::new("Type the role name to confirm").color(MUTED()));
        ui.add(egui::TextEdit::singleline(&mut f.typed).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY)).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Confirmation"));
        if !tag.is_empty() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                badge(ui, &tag, super::sidebar::tag_color(&tag));
                ui.label(RichText::new(format!("This connection is tagged {tag}")).color(super::sidebar::tag_color(&tag)));
            });
        }
        if !writable {
            ui.label(RichText::new(NOTE_LOCKED).size(TXT_SMALL).color(FAINT()));
        }
        dialog_actions(ui, |ui| {
            let ok = writable && plan.is_some() && f.typed == f.role;
            if ui.add_enabled(ok, primary_button(ui, "Drop role", ALERT())).clicked() {
                if let (Some((info, mode)), Some(sql)) = (plan, self.pg_roles_db(f.conn)) {
                    self.worker.pg_role_drop(f.conn, sql, info, mode, false);
                }
                *keep = false;
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
        });
    }

    /// QUARRY_TEST_PG=roles (the Privileges tab with a cell editor open and one change staged) or roles-role (the Roles tab on
    /// app_writer with its connection limit staged).
    pub(super) fn pg_roles_demo(&mut self, which: &str, conn: u64) {
        if !matches!(which, "roles" | "roles-role") {
            return;
        }
        if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
            c.writable = true;
        }
        let mut w = PgRolesWin::new(conn, "analytics".into());
        let (mut cl, objs, defs) = demo_data();
        w.role = Some("app_writer".into());
        if which == "roles" {
            let o = objs.objs.iter().find(|o| o.name == "orders").cloned().unwrap_or_default();
            let mut to = o.direct("app_writer");
            to.insert("DELETE".into(), false);
            w.staged.push(Change::Acl { target: o.target(Kind::Tables), grantee: "app_writer".into(), from: o.direct("app_writer"), to });
            w.cell = Some((o.oid.clone(), "app_writer".into()));
        } else {
            w.tab = PTab::Roles;
            // The server has no limit yet, so the staged change reads as in the design.
            if let Some(r) = cl.roles.iter_mut().find(|r| r.name == "app_writer") {
                r.attrs.conn_limit = -1;
            }
            let r = cl.role("app_writer").cloned().unwrap_or_default();
            w.staged.push(Change::Attrs { role: r.name.clone(), from: r.attrs.clone(), to: Attrs { conn_limit: 20, ..r.attrs.clone() } });
        }
        w.set_cluster(Ok(cl));
        w.objs = Some(Ok(objs));
        w.defs = Some(Ok(defs));
        self.pg_roles = Some(w);
    }
}

fn pg_side_panel(ui: &mut egui::Ui, w: &PgRolesWin, c: &Cluster, acts: &mut Vec<Act>) {
    egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(12, 12)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let Some(r) = w.role.as_ref().and_then(|r| c.role(r)) else {
            ui.label(RichText::new("Click a role's column to see it here.").size(TXT_SMALL).color(MUTED()));
            return;
        };
        let a = w.attrs_now(&r.name).unwrap_or_default();
        ui.label(RichText::new(&r.name).monospace().size(TXT_SUBHEAD).color(BRIGHT()));
        ui.label(RichText::new(role_sub(c, r)).size(TXT_SMALL).color(MUTED()));
        ui.add_space(10.0);
        caption(ui, "ATTRIBUTES");
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
            let on: Vec<&str> = rp::FLAGS.iter().filter(|(f, _)| a.flag(f) && *f != "BYPASSRLS").map(|(f, _)| *f).collect();
            if on.is_empty() {
                ui.label(RichText::new("none").size(TXT_SMALL).color(FAINT()));
            }
            for f in on {
                priv_chip(ui, f, ChipLook::Owner);
            }
        });
        ui.add_space(8.0);
        let row = |ui: &mut egui::Ui, k: &str, v: String, color: Color32| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(k).color(MUTED()));
                right_group(ui, |ui| {
                    ui.label(RichText::new(v).monospace().color(color));
                });
            });
        };
        row(ui, "Connection limit", if a.conn_limit < 0 { "none".into() } else { a.conn_limit.to_string() }, if a.conn_limit < 0 { TEXT() } else { C_NUMBER() });
        row(ui, "Valid until", rp::until_shown(&a.valid_until).to_string(), if a.valid_until == "-infinity" { ALERT() } else { TEXT() });
        row(ui, "Bypasses RLS", if a.bypassrls { "yes".into() } else { "no".into() }, TEXT());
        ui.add_space(8.0);
        if ui.button("Open role…").clicked() {
            acts.push(Act::OpenRole(r.name.clone()));
        }
        ui.add_space(12.0);
        caption(ui, "LEGEND");
        ui.add_space(4.0);
        for (t, look, what) in [("SELECT", ChipLook::Granted, "granted"), ("SELECT", ChipLook::Through, "through a role"), ("SELECT G", ChipLook::Grant, "with grant option"), ("owner", ChipLook::Owner, "owns it")] {
            ui.horizontal(|ui| {
                let at = ui.cursor().left();
                priv_chip(ui, t, look);
                ui.add_space((at + 70.0 - ui.cursor().left()).max(6.0));
                ui.label(RichText::new(what).size(TXT_SMALL).color(MUTED()));
            });
        }
    });
}

fn pg_role_list(ui: &mut egui::Ui, w: &mut PgRolesWin, c: &Cluster, env: &Env, acts: &mut Vec<Act>) {
    ui.add(egui::TextEdit::singleline(&mut w.filter).hint_text("Filter roles").desired_width(f32::INFINITY));
    ui.add_space(6.0);
    let find = w.filter.trim().to_lowercase();
    let shown: Vec<Role> = w.roles().into_iter().filter(|r| find.is_empty() || r.name.to_lowercase().contains(&find)).cloned().collect();
    egui::ScrollArea::vertical().id_salt("pg-role-list").max_height(330.0).auto_shrink([false, true]).show(ui, |ui| {
        for (title, login) in [("LOGIN ROLES", true), ("GROUP ROLES", false)] {
            let list: Vec<&Role> = shown.iter().filter(|r| r.attrs.login == login).collect();
            if list.is_empty() {
                continue;
            }
            caption(ui, title);
            ui.add_space(2.0);
            for r in list {
                let active = w.role.as_deref() == Some(r.name.as_str());
                let dot = if r.attrs.superuser { ALERT() } else if !r.attrs.valid_until.is_empty() { WARNING() } else if r.attrs.login { SUCCESS() } else { FAINT() };
                let sub = if r.attrs.superuser {
                    "superuser".to_string()
                } else if r.attrs.login {
                    let mut s = "login".to_string();
                    if r.attrs.conn_limit >= 0 {
                        s.push_str(&format!(" · limit {}", r.attrs.conn_limit));
                    }
                    if r.attrs.valid_until == "-infinity" {
                        s.push_str(" · expired");
                    } else if !r.attrs.valid_until.is_empty() {
                        s.push_str(&format!(" · expires {}", r.attrs.valid_until.split(' ').next().unwrap_or_default()));
                    }
                    s
                } else {
                    let n = c.members_of(&r.name).len();
                    format!("no login · {n} member{}", if n == 1 { "" } else { "s" })
                };
                let resp = side_row(ui, 40.0, 0.0, active, &r.name, |p, rect| {
                    p.circle_filled(egui::pos2(rect.left() + 5.0, rect.top() + 12.0), 3.5, dot);
                    p.text(egui::pos2(rect.left() + 18.0, rect.top() + 12.0), egui::Align2::LEFT_CENTER, &r.name, FontId::proportional(TXT_TITLE), if active { BRIGHT() } else { TEXT() });
                    p.text(egui::pos2(rect.left() + 18.0, rect.top() + 28.0), egui::Align2::LEFT_CENTER, &sub, FontId::monospace(TXT_CAPTION), FAINT());
                });
                if resp.clicked() && !active {
                    acts.push(Act::Role(r.name.clone()));
                }
            }
            ui.add_space(6.0);
        }
    });
    ui.add_space(6.0);
    let may_create = env.blocked.is_none() && c.me().is_some_and(|m| m.attrs.superuser || m.attrs.createrole);
    if ui.add_enabled(may_create, egui::Button::new("+ Role")).on_disabled_hover_text(env.blocked.clone().unwrap_or_else(|| "Creating a role needs CREATEROLE or SUPERUSER.".into())).clicked() {
        acts.push(Act::NewRole);
    }
    ui.add_space(6.0);
    let hidden = c.roles.iter().filter(|r| r.system()).count();
    let t = if w.system { format!("pg_ roles shown ({hidden})") } else { "pg_ roles hidden".into() };
    if ui.add(egui::Label::new(RichText::new(t).size(TXT_SMALL).color(FAINT())).sense(Sense::click())).on_hover_text("Show or hide the server's built-in pg_ roles").clicked() {
        w.system = !w.system;
    }
}

fn pg_role_members(ui: &mut egui::Ui, w: &mut PgRolesWin, c: &Cluster, role: &Role, env: &Env, acts: &mut Vec<Act>) {
    let opts = c.options();
    // Both lists, each a set of (group, member) pairs: what the server holds plus what is staged to be added.
    let pairs = |of: bool| -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = if of { c.member_of(&role.name).iter().map(|m| (m.group.clone(), m.member.clone())).collect() } else { c.members_of(&role.name).iter().map(|m| (m.group.clone(), m.member.clone())).collect() };
        for s in &w.staged {
            if let Change::Member { group, member, from: None, .. } = s {
                if (of && *member == role.name) || (!of && *group == role.name) {
                    v.push((group.clone(), member.clone()));
                }
            }
        }
        v
    };
    for of in [true, false] {
        caption(ui, if of { "MEMBER OF" } else { "MEMBERS" });
        ui.add_space(4.0);
        let list = pairs(of);
        egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::Grid::new(if of { "pg-member-of" } else { "pg-members" }).num_columns(if opts { 6 } else { 4 }).spacing([22.0, 6.0]).min_col_width(40.0).show(ui, |ui| {
                ui.label(RichText::new("Role").size(TXT_SMALL).color(MUTED()));
                ui.label(RichText::new("Admin").size(TXT_SMALL).color(MUTED()));
                if opts {
                    ui.label(RichText::new("Inherit").size(TXT_SMALL).color(MUTED()));
                    ui.label(RichText::new("Set").size(TXT_SMALL).color(MUTED()));
                }
                ui.label(RichText::new("Granted by").size(TXT_SMALL).color(MUTED()));
                ui.label("");
                ui.end_row();
                if list.is_empty() {
                    ui.label(RichText::new("none").size(TXT_SMALL).color(FAINT()));
                    ui.end_row();
                }
                for (g, m) in &list {
                    let other = if of { g } else { m };
                    let server = c.membership(g, m);
                    let now = w.member_now(g, m);
                    let group = c.role(g);
                    let may = group.map_or(Err("The role is gone.".to_string()), |r| c.may_grant_role(r));
                    let can = env.locked.is_none() && may.is_ok() && !w.busy;
                    ui.horizontal(|ui| {
                        mono_label(ui, other, if now.is_none() { FAINT() } else { TEXT() });
                        if server.is_none() {
                            badge(ui, "new", C_DATE());
                        } else if now.is_none() {
                            badge(ui, "removed", ALERT());
                        }
                    });
                    let o = now.or(server).unwrap_or_default();
                    let stage = |acts: &mut Vec<Act>, to: Option<Opts>| acts.push(Act::Stage(Change::Member { group: g.clone(), member: m.clone(), from: server, to }));
                    let mut admin = o.admin;
                    let ar = ui.add_enabled(can && now.is_some(), egui::Checkbox::without_text(&mut admin));
                    ar.widget_info(|| egui::WidgetInfo::labeled(egui::Role::CheckBox, true, format!("Admin {other}")));
                    if ar.clicked() {
                        stage(acts, Some(Opts { admin, ..o }));
                    }
                    if opts {
                        let mut inh = o.inherit.unwrap_or(true);
                        if ui.add_enabled(can && now.is_some(), egui::Checkbox::without_text(&mut inh)).clicked() {
                            stage(acts, Some(Opts { inherit: Some(inh), ..o }));
                        }
                        let mut set = o.set.unwrap_or(true);
                        if ui.add_enabled(can && now.is_some(), egui::Checkbox::without_text(&mut set)).clicked() {
                            stage(acts, Some(Opts { set: Some(set), ..o }));
                        }
                    }
                    let grantors = c.members.iter().find(|x| x.group == *g && x.member == *m).map(|x| x.grantors.join(", ")).unwrap_or_else(|| c.me.clone());
                    mono_label(ui, &grantors, MUTED());
                    let x = ui.add_enabled(can, egui::Button::new(if now.is_none() { "Keep" } else { "×" }).frame_when_inactive(false));
                    x.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Remove {other}")));
                    let x = if let Err(why) = &may { x.on_disabled_hover_text(why.as_str()) } else { x };
                    if x.clicked() {
                        stage(acts, if now.is_none() { server } else { None });
                    }
                    ui.end_row();
                }
            });
        });
        ui.add_space(4.0);
        let taken: Vec<&String> = list.iter().map(|(g, m)| if of { g } else { m }).collect();
        let can_add = env.locked.is_none() && !w.busy;
        ui.add_enabled_ui(can_add, |ui| {
            let mut pick = None;
            let combo = egui::ComboBox::from_id_salt(if of { "pg-add-group" } else { "pg-add-member" }).selected_text(if of { "+ Add to a role" } else { "+ Add a member" }).width(170.0).show_ui(ui, |ui| {
                menu_look(ui);
                for r in w.roles().iter().filter(|r| r.name != role.name && !taken.contains(&&r.name)) {
                    let group = if of { *r } else { role };
                    let ok = c.may_grant_role(group);
                    if ui.add_enabled(ok.is_ok(), egui::Button::selectable(false, &r.name)).on_disabled_hover_text(ok.err().unwrap_or_default()).clicked() {
                        pick = Some(r.name.clone());
                    }
                }
            });
            combo.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, if of { "Add to a role" } else { "Add a member" }));
            if let Some(p) = pick {
                let (g, m) = if of { (p, role.name.clone()) } else { (role.name.clone(), p) };
                let inherit = c.role(&m).is_none_or(|r| r.attrs.inherit);
                acts.push(Act::Stage(Change::Member { group: g, member: m, from: None, to: Some(Opts::new(c.major, inherit)) }));
            }
        });
        ui.add_space(10.0);
    }
    let help = if opts {
        format!("Admin lets the member manage this role's members. Inherit uses its privileges without SET ROLE; Set allows SET ROLE {}. Inherit and Set per membership need PostgreSQL 16.", role.name)
    } else {
        "Admin lets the member manage this role's members. Whether privileges are inherited is the member's INHERIT attribute before PostgreSQL 16.".to_string()
    };
    ui.add(egui::Label::new(RichText::new(help).size(TXT_SMALL).color(FAINT())).wrap());
}

fn pg_role_defaults(ui: &mut egui::Ui, w: &mut PgRolesWin, c: &Cluster, role: &Role, env: &Env, acts: &mut Vec<Act>) {
    let defs = match &w.defs {
        None => {
            spin(ui);
            return;
        }
        Some(Err(e)) => {
            ui.add(egui::Label::new(RichText::new(format!("Could not read the default privileges of {}: {e}", w.database)).color(ALERT())).wrap());
            return;
        }
        Some(Ok(d)) => d.clone(),
    };
    let can = env.blocked.is_none() && !w.busy;
    ui.horizontal(|ui| {
        caption(ui, format!("GRANTED TO {} ON NEW OBJECTS", role.name.to_uppercase()));
        ui.label(RichText::new(format!("in database {}", w.database)).size(TXT_SMALL).color(FAINT()));
    });
    ui.add_space(4.0);
    // Rows that name the role as grantee, plus staged ones that are not on the server yet.
    let mut rows: Vec<(String, String, DefKind, Privs)> = defs.rows.iter().filter(|d| !d.granted(&role.name).is_empty()).map(|d| (d.owner.clone(), d.schema.clone(), d.kind, d.granted(&role.name))).collect();
    for s in &w.staged {
        if let Change::Default { owner, schema, kind, grantee, from, .. } = s {
            if *grantee == role.name && !rows.iter().any(|(o, sc, k, _)| o == owner && sc == schema && k == kind) {
                rows.push((owner.clone(), schema.clone(), *kind, from.clone()));
            }
        }
    }
    egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::Grid::new("pg-defaults").num_columns(5).spacing([22.0, 6.0]).show(ui, |ui| {
            for h in ["Created by", "In schema", "Kind", "Privileges", ""] {
                ui.label(RichText::new(h).size(TXT_SMALL).color(MUTED()));
            }
            ui.end_row();
            if rows.is_empty() {
                ui.label(RichText::new("none").size(TXT_SMALL).color(FAINT()));
                ui.end_row();
            }
            for (owner, schema, kind, server) in &rows {
                let k = format!("def|{owner}|{schema}|{kind:?}|{}", role.name);
                let now = match w.staged.iter().find(|s| key(s) == k) {
                    Some(Change::Default { to, .. }) => to.clone(),
                    _ => server.clone(),
                };
                mono_label(ui, owner, TEXT());
                mono_label(ui, if schema.is_empty() { "all schemas" } else { schema }, TEXT());
                ui.label(RichText::new(kind.label()).color(MUTED()));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for (p, g) in &now {
                        priv_chip(ui, &if *g { format!("{p} G") } else { p.clone() }, if *g { ChipLook::Grant } else { ChipLook::Granted });
                    }
                    if server.is_empty() {
                        badge(ui, "new", C_DATE());
                    } else if now.is_empty() {
                        badge(ui, "removed", ALERT());
                    }
                });
                let x = ui.add_enabled(can, egui::Button::new(if now.is_empty() { "Keep" } else { "×" }).frame_when_inactive(false));
                if x.on_hover_text("Revoke this default privilege").clicked() {
                    let to = if now.is_empty() { server.clone() } else { Privs::new() };
                    acts.push(Act::Stage(Change::Default { owner: owner.clone(), schema: schema.clone(), kind: *kind, grantee: role.name.clone(), from: server.clone(), to }));
                }
                ui.end_row();
            }
        });
    });
    ui.add_space(4.0);
    match &mut w.def_form {
        None => {
            if ui.add_enabled(can, egui::Button::new("+ Default privilege")).clicked() {
                w.def_form = Some(DefForm { owner: c.me.clone(), schema: String::new(), kind: DefKind::Tables, privs: BTreeSet::new() });
            }
        }
        Some(f) => {
            let mut close = false;
            egui::Frame::new().stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("When").color(MUTED()));
                    egui::ComboBox::from_id_salt("pg-def-owner").width(130.0).selected_text(RichText::new(&f.owner).monospace()).show_ui(ui, |ui| {
                        menu_look(ui);
                        for r in &c.roles {
                            if !r.system() {
                                ui.selectable_value(&mut f.owner, r.name.clone(), &r.name);
                            }
                        }
                    });
                    ui.label(RichText::new("creates").color(MUTED()));
                    egui::ComboBox::from_id_salt("pg-def-kind").width(110.0).selected_text(f.kind.label()).show_ui(ui, |ui| {
                        menu_look(ui);
                        for k in rp::DEF_KINDS {
                            ui.selectable_value(&mut f.kind, k, k.label());
                        }
                    });
                    ui.label(RichText::new("in").color(MUTED()));
                    egui::ComboBox::from_id_salt("pg-def-schema").width(130.0).selected_text(if f.schema.is_empty() { "all schemas" } else { f.schema.as_str() }).show_ui(ui, |ui| {
                        menu_look(ui);
                        ui.selectable_value(&mut f.schema, String::new(), "all schemas");
                        if f.kind != DefKind::Schemas {
                            for s in &defs.schemas {
                                ui.selectable_value(&mut f.schema, s.clone(), s);
                            }
                        }
                    });
                });
                if f.kind == DefKind::Schemas {
                    f.schema.clear();
                }
                let allowed = f.kind.privs(env.major);
                f.privs.retain(|p| allowed.contains(&p.as_str()));
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("grant {} ", role.name)).color(MUTED()));
                    for p in &allowed {
                        let mut on = f.privs.contains(*p);
                        if ui.checkbox(&mut on, RichText::new(*p).monospace().size(TXT_SMALL)).changed() {
                            if on {
                                f.privs.insert(p.to_string());
                            } else {
                                f.privs.remove(*p);
                            }
                        }
                    }
                });
                ui.horizontal(|ui| {
                    if ui.add_enabled(!f.privs.is_empty() && !f.owner.is_empty(), egui::Button::new("Stage")).clicked() {
                        let server = defs.rows.iter().find(|d| d.owner == f.owner && d.schema == f.schema && d.kind == f.kind).map(|d| d.granted(&role.name)).unwrap_or_default();
                        let mut to = server.clone();
                        for p in &f.privs {
                            to.entry(p.clone()).or_insert(false);
                        }
                        acts.push(Act::Stage(Change::Default { owner: f.owner.clone(), schema: f.schema.clone(), kind: f.kind, grantee: role.name.clone(), from: server, to }));
                        close = true;
                    }
                    if quiet(ui, "Cancel").clicked() {
                        close = true;
                    }
                });
            });
            if close {
                w.def_form = None;
            }
        }
    }
    ui.add_space(12.0);
    caption(ui, format!("WHEN {} CREATES OBJECTS", role.name.to_uppercase()));
    ui.add_space(4.0);
    let own: Vec<String> = defs
        .rows
        .iter()
        .filter(|d| d.owner == role.name)
        .flat_map(|d| d.acl.iter().filter(|e| e.grantee != role.name).map(move |e| format!("{} {}: {} gets {}", d.kind.label(), if d.schema.is_empty() { "everywhere".to_string() } else { format!("in {}", d.schema) }, if e.grantee.is_empty() { "PUBLIC" } else { &e.grantee }, rp::privs_text(&e.privs))))
        .collect();
    if own.is_empty() {
        ui.label(RichText::new(format!("No default privileges: what {} creates stays private to it until granted.", role.name)).color(TEXT()));
    }
    for l in own {
        ui.label(RichText::new(l).size(TXT_SMALL).color(TEXT()));
    }
    ui.add_space(6.0);
    ui.add(egui::Label::new(RichText::new("Default privileges apply to objects created after the change, not to existing ones; grant those on the Privileges tab.").size(TXT_SMALL).color(FAINT())).wrap());
}

/// The sample server of QUARRY_TEST_PG=roles: PostgreSQL 17, the roles of the design, the tables of `public` and a few default
/// privileges.
fn demo_data() -> (Cluster, Objects, Defaults) {
    let role = |oid: &str, name: &str, a: Attrs| Role { oid: oid.into(), name: name.into(), attrs: a, admin: true };
    let login = Attrs { login: true, ..Default::default() };
    let roles = vec![
        role("16401", "app_reader", login.clone()),
        role("16402", "app_writer", Attrs { conn_limit: 20, ..login.clone() }),
        role("16405", "jane", Attrs { valid_until: "2026-12-31 00:00:00+00".into(), ..login.clone() }),
        role("10", "postgres", Attrs { superuser: true, createdb: true, createrole: true, replication: true, bypassrls: true, ..login.clone() }),
        role("16403", "readonly", Attrs::default()),
        role("16404", "reporting", Attrs::default()),
        role("3373", "pg_monitor", Attrs::default()),
    ];
    let m = |g: &str, mem: &str| rp::Membership { group: g.into(), member: mem.into(), opts: Opts { admin: false, inherit: Some(true), set: Some(true) }, grantors: vec!["postgres".into()] };
    let members = vec![m("reporting", "app_writer"), m("reporting", "jane"), m("readonly", "app_reader"), m("readonly", "reporting")];
    let cl = Cluster { major: 17, me: "postgres".into(), roles, members, databases: vec!["analytics".into(), "billing".into(), "postgres".into()], password_encryption: "scram-sha-256".into() };
    let p = |v: &[(&str, bool)]| -> Privs { v.iter().map(|(k, g)| (k.to_string(), *g)).collect() };
    let e = |g: &str, v: &[(&str, bool)]| rp::AclEntry { grantee: g.into(), grantor: "postgres".into(), privs: p(v) };
    let all: Vec<(&str, bool)> = ["SELECT", "INSERT", "UPDATE", "DELETE", "TRUNCATE", "REFERENCES", "TRIGGER", "MAINTAIN"].iter().map(|k| (*k, false)).collect();
    let write = [("SELECT", false), ("INSERT", false), ("UPDATE", false)];
    let table = |oid: &str, name: &str, detail: &str, owner: &str, acl: Vec<rp::AclEntry>| Obj { oid: oid.into(), schema: "public".into(), name: name.into(), detail: detail.into(), owner: owner.into(), acl, ..Default::default() };
    let objs = vec![
        table("16501", "orders", "", "postgres", vec![e("postgres", &all), e("app_reader", &[("SELECT", false)]), e("app_writer", &[("SELECT", false), ("INSERT", false), ("UPDATE", false), ("TRUNCATE", false)]), e("reporting", &[("SELECT", false)])]),
        table("16502", "customers", "", "postgres", vec![e("postgres", &all), e("app_reader", &[("SELECT", false)]), e("app_writer", &write), e("reporting", &[("SELECT", false)])]),
        table("16503", "order_items", "", "postgres", vec![e("postgres", &all), e("app_reader", &[("SELECT", false)]), e("app_writer", &[("SELECT", false), ("INSERT", false), ("UPDATE", false), ("DELETE", false)]), e("reporting", &[("SELECT", false)])]),
        table("16504", "payments", "", "postgres", vec![e("postgres", &all), e("app_writer", &[("SELECT", false), ("INSERT", false)]), e("reporting", &[("SELECT", false)])]),
        table("16505", "daily_sales", "view", "jane", vec![e("jane", &all), e("reporting", &[("SELECT", true)])]),
        table("16506", "events", "", "postgres", vec![e("postgres", &all), e("app_reader", &[("SELECT", false)]), e("app_writer", &[("INSERT", false)])]),
    ];
    let objects = Objects { database: "analytics".into(), schema: "public".into(), kind: Kind::Tables, objs, schemas: vec!["public".into(), "reporting".into(), "staging".into()], db_oid: "16384".into(), cut: false };
    let d = |owner: &str, schema: &str, kind: DefKind, acl: Vec<rp::AclEntry>| rp::DefaultAcl { owner: owner.into(), schema: schema.into(), kind, acl };
    let defs = Defaults {
        database: "analytics".into(),
        db_oid: "16384".into(),
        rows: vec![
            d("postgres", "public", DefKind::Tables, vec![e("reporting", &[("SELECT", false)]), e("app_writer", &write)]),
            d("postgres", "public", DefKind::Sequences, vec![e("app_writer", &[("USAGE", false), ("SELECT", false)])]),
            d("postgres", "reporting", DefKind::Tables, vec![e("reporting", &[("SELECT", false)])]),
        ],
        schemas: vec!["public".into(), "reporting".into(), "staging".into()],
    };
    (cl, objects, defs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_columns_in_view_are_drawn() {
        // Columns 112 wide from x 170, a view from 300 to 600: columns 1 to 3 show, in part or whole.
        assert_eq!(col_range(300.0, 600.0, 170.0, 112.0, 50), 1..4);
        assert_eq!(col_range(0.0, 2000.0, 170.0, 112.0, 5), 0..5);
        assert_eq!(col_range(9000.0, 9500.0, 170.0, 112.0, 5), 5..5);
    }
}
