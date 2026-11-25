//! Runs actions on Core for the UI: what the app adds is the Unlock dialog on a read-only refusal, with a retry that runs
//! the same action again, and the effects only the UI can carry out.

use super::*;
use quarry_core::action::{Action, Dialog, Refusal, RefusalKind, UiFx};

impl App {
    /// Every click of a window on actions goes through here.
    pub(super) fn exec(&mut self, a: Action) {
        if let Err(Refusal { kind: RefusalKind::ReadOnly { conn }, .. }) = self.core.execute(a.clone()) {
            self.modal = Some(Modal::Unlock(conn, Some(Box::new(move |app: &mut App| app.exec(a)))));
        }
        self.run_fx();
    }

    /// Carries out what actions and answers left for the UI. Also run each frame from `logic()`.
    pub(super) fn run_fx(&mut self) {
        for fx in self.core.take_ui_fx() {
            match fx {
                UiFx::Open(d) => {
                    self.modal = Some(match d {
                        Dialog::Unlock(conn) => Modal::Unlock(conn, None),
                        Dialog::PgNewRole(f) => Modal::PgNewRole(f),
                        Dialog::PgPassword(f) => Modal::PgPassword(f),
                        Dialog::PgDropRole(f) => Modal::PgDropRole(f),
                        Dialog::SqlNewUser(f) => Modal::SqlNewUser(f),
                        Dialog::SqlPassword(f) => Modal::SqlPassword(f),
                        Dialog::Typed { title, expect, action } => Modal::Typed { title, expect, typed: String::new(), action },
                        Dialog::ShellWrite(ops) => Modal::ShellWrite(ops),
                    })
                }
            }
        }
    }
}
