//! Typed actions: the one way the UI, and later the menus and the agents, change Core. `Core::execute` runs one and
//! answers `Reply` or `Refusal`, so every flow can run in a test without a window. How to put an area on it:
//!
//! - **Area enum.** Make `action/<area>.rs` with `pub enum <Area>A` (derive Serialize, Deserialize, Debug, Clone), add one
//!   variant to `Action` and one arm to `execute`. Payloads carry ids (conn `u64`, tab uid), names, strings and small
//!   enums, never indices into a Vec, `Row`, `Arc` or `Client`. A password goes in a `Secret`.
//! - **Handler.** The same file holds `impl Core { fn <area>(&mut self, a: <Area>A) -> Result<Reply, Refusal> }` and the
//!   area's non-drawing methods (loads, answers, staging, apply), moved from `impl App` with `self.core.x` made `self.x`.
//! - **Refusals.** Return `self.refuse_ro(conn, None)`, `refuse_offline(conn)`, `refuse(msg)` or `refuse_settings(msg)`:
//!   each pushes the toast `warn_ro`, `warn_offline`, `warn` or `warn_settings` pushed before. On `ReadOnly` the app also
//!   opens Unlock with a retry that runs the same action again, so no closure lives here.
//! - **Toasts, errors, undo.** `self.ok`, `self.err`, `self.warn*` and `self.push_undo` are on Core (`core/notify.rs`).
//! - **UI effects.** What only the UI can do (open one of its dialogs) is pushed on `self.ui_fx` as a `UiFx`; `App::exec`
//!   and `logic()` drain it.
//! - **Answers.** Add the area's `Msg` variants to `Core::handles` and route them in `Core::on_msg`; keep the request id
//!   guards as they were. `App::poll` hands every message `handles` accepts to `on_msg`.
//! - **Drawing.** A window takes its state out for the frame, draws it (a `TextEdit` may bind a `&mut String` of it),
//!   pushes `<Area>A` values for clicks, toggles and choices, puts the state back, then calls `self.exec(..)` on each.
//! - **Tests.** `Core::for_test()`, then `execute`, `inject` fake answers or `step_until` real ones; no egui needed. See
//!   `action/roles_tests.rs`.

use crate::core::Core;
use crate::db::Msg;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

pub mod roles;
pub mod server;
pub mod users;
pub mod sql;
pub mod sql_server;
#[cfg(test)]
mod roles_tests;
#[cfg(test)]
mod users_tests;
#[cfg(test)]
mod sql_server_tests;

pub use roles::RolesA;
pub use server::{MongoServerA, ServerA};
pub use users::{MongoUsersA, SqlUsersA, UsersA};
pub use sql_server::{PgServerA, SqlServerA};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Action {
    Roles(RolesA),
    Users(UsersA),
    Server(ServerA),
}

/// What an action did: finished, or started worker call `req` whose answer comes back through `on_msg`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    Done,
    Pending(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub kind: RefusalKind,
    pub msg: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalKind {
    /// Writes are locked on this connection; the UI offers Unlock and runs the action again after it.
    ReadOnly { conn: u64 },
    Offline,
    Invalid,
    Settings,
}

/// Effects only the UI can carry out, drained by the app after each action and each frame.
pub enum UiFx {
    Open(Dialog),
}

/// The app's dialogs an action opens. The app keeps `Modal`, which holds its own closures, so Core names them here.
pub enum Dialog {
    /// Unlock with nothing to run after it, for a banner's Unlock button.
    Unlock(u64),
    PgNewRole(Box<crate::state::sql::pg_roles::NewRoleForm>),
    PgPassword(Box<crate::state::sql::pg_roles::PgPwForm>),
    PgDropRole(Box<crate::state::sql::pg_roles::DropForm>),
    SqlNewUser(Box<crate::state::sql::users::NewUser>),
    SqlPassword(Box<crate::state::sql::users::PwForm>),
    /// A confirmation that asks for a name to be typed.
    Typed { title: String, expect: String, action: crate::state::Danger },
}

/// A password in an action. It is wiped on drop, prints as *** and serializes as null, so a journal or an echo never
/// holds it; a deserialized one is absent, and `execute` refuses an action whose secret is absent.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(Option<String>);

impl Secret {
    pub fn new(s: String) -> Secret {
        Secret(Some(s))
    }

    /// The text, or None when it did not come through (a deserialized action).
    pub fn expose(&self) -> Option<&str> {
        self.0.as_deref()
    }

    /// Hands the text over, leaving nothing behind.
    pub fn take(&mut self) -> Option<String> {
        self.0.take()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "***" } else { "(absent)" })
    }
}

impl Serialize for Secret {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_none()
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Secret, D::Error> {
        // Whatever was written stands for a password that was never sent, so it reads back as absent.
        serde::de::IgnoredAny::deserialize(d)?;
        Ok(Secret(None))
    }
}

impl Core {
    /// Runs one action. Refusals have already told the person why, with the toast that goes with their kind.
    pub fn execute(&mut self, a: Action) -> Result<Reply, Refusal> {
        match a {
            Action::Roles(a) => self.roles(a),
            Action::Users(a) => self.users(a),
            Action::Server(a) => self.server(a),
        }
    }

    pub fn take_ui_fx(&mut self) -> Vec<UiFx> {
        std::mem::take(&mut self.ui_fx)
    }

    /// Writes are locked on `conn`: the read-only toast, or `note` in its place where a window words it its own way.
    pub fn refuse_ro(&mut self, conn: u64, note: Option<&str>) -> Refusal {
        match note {
            Some(n) => self.warn(n),
            None => self.warn_ro_on(Some(conn)),
        }
        Refusal { kind: RefusalKind::ReadOnly { conn }, msg: note.map_or_else(|| self.last_warning(), str::to_string) }
    }

    /// Writes are locked on `conn`, for a site that opens Unlock without a toast first.
    pub fn refuse_ro_quiet(&mut self, conn: u64) -> Refusal {
        Refusal { kind: RefusalKind::ReadOnly { conn }, msg: String::new() }
    }

    pub fn refuse_offline(&mut self, conn: u64) -> Refusal {
        self.warn_offline_on(Some(conn));
        Refusal { kind: RefusalKind::Offline, msg: self.last_warning() }
    }

    pub fn refuse(&mut self, msg: impl Into<String>) -> Refusal {
        let msg = msg.into();
        self.warn(msg.clone());
        Refusal { kind: RefusalKind::Invalid, msg }
    }

    pub fn refuse_settings(&mut self, msg: impl Into<String>) -> Refusal {
        let msg = msg.into();
        self.warn_settings(msg.clone());
        Refusal { kind: RefusalKind::Settings, msg }
    }

    fn last_warning(&self) -> String {
        self.toasts.last().map(|t| if t.body.is_empty() { t.title.clone() } else { format!("{}. {}", t.title, t.body) }).unwrap_or_default()
    }

    /// Whether `on_msg` takes this answer. The app's own poll handles the rest.
    pub fn handles(m: &Msg) -> bool {
        use crate::db::sql::pg::roles::Answer;
        match m {
            // The drop dialog's answer fills a form the app's Modal holds.
            Msg::PgRoles { ans: Answer::DropInfo(_), .. } => false,
            Msg::PgRoles { .. } | Msg::SqlUsers { .. } | Msg::SqlUserGrants { .. } | Msg::SqlUserDone { .. } => true,
            Msg::Command { tag, .. } => matches!(tag.as_str(), "users" | "users-self" | "roles") || tag.starts_with("server:"),
            Msg::SqlServer { .. } | Msg::PgServer { .. } => true,
            _ => false,
        }
    }

    /// Applies one worker answer that `handles` accepts; any other is dropped.
    pub fn on_msg(&mut self, m: Msg) {
        match m {
            Msg::PgRoles { conn, req, ans } => self.pg_roles_answer(conn, req, ans),
            Msg::SqlUsers { conn, req, res } => self.sql_users_loaded(conn, req, res),
            Msg::SqlUserGrants { conn, req, res } => self.sql_user_grants_loaded(conn, req, res),
            Msg::SqlUserDone { conn, account, op, res, undo } => self.sql_user_done(conn, account, op, res, undo),
            Msg::Command { tag, res } if tag.starts_with("server:") => self.server_reply(&tag, res),
            Msg::Command { tag, res } => self.mongo_users_reply(&tag, res),
            Msg::SqlServer { req, ans } => self.sql_server_done(req, ans),
            Msg::PgServer { req, ans } => self.pg_server_done(req, ans),
            _ => {}
        }
    }

    /// Drains the worker's answers into `on_msg` and hands back the ones Core does not take.
    pub fn tick(&mut self) -> Vec<Msg> {
        let mut rest = Vec::new();
        while let Ok(m) = self.rx.try_recv() {
            if Self::handles(&m) {
                self.on_msg(m);
            } else {
                rest.push(m);
            }
        }
        rest
    }

    /// Runs `tick` until `cond` holds, like the UI tests' `wait`. Panics with `what` after 40 s.
    #[cfg(any(test, feature = "test-support"))]
    pub fn step_until(&mut self, what: &str, cond: impl Fn(&Core) -> bool) {
        let t = std::time::Instant::now();
        while !cond(self) {
            assert!(t.elapsed() < std::time::Duration::from_secs(40), "timed out waiting for {what}; status: {}", self.status);
            self.tick();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// A worker answer made up by a test, applied as if it had come over the channel.
    #[cfg(any(test, feature = "test-support"))]
    pub fn inject(&mut self, m: Msg) {
        assert!(Self::handles(&m), "Core does not take this answer");
        self.on_msg(m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_never_shows_in_debug_or_json() {
        let a = Action::Roles(RolesA::SetPassword { conn: 1, role: "jane".into(), oid: "16405".into(), password: Secret::new("Zq9secret".into()) });
        assert!(!format!("{a:?}").contains("Zq9secret"));
        let json = serde_json::to_string(&a).unwrap();
        assert!(!json.contains("Zq9secret"), "{json}");
        let back: Action = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, Action::Roles(RolesA::SetPassword { password, .. }) if password.expose().is_none()), "a password never comes back from JSON");
    }
}
