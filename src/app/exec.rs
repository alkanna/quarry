//! Runs actions on Core for the UI: what the app adds is the Unlock dialog on a read-only refusal, with a retry that runs
//! the same action again, and the effects only the UI can carry out.

use super::*;
use quarry_core::action::{Action, Dialog, Refusal, RefusalKind, Reply, TabA, UiFx};

impl App {
    /// Every click of a window on actions goes through here.
    pub(super) fn exec(&mut self, a: Action) {
        self.exec_reply(a);
    }

    /// `exec` for an action that starts a request whose answer a dialog waits on: the request id it is matched by, None when
    /// the action was refused or started nothing.
    pub(super) fn exec_req(&mut self, a: Action) -> Option<u64> {
        match self.exec_reply(a) {
            Some(Reply::Pending(req)) => Some(req),
            _ => None,
        }
    }

    /// Asks egui to draw again once the pause an action is waiting out is over.
    pub(super) fn take_repaint(&mut self, ctx: &egui::Context) {
        if let Some(s) = self.repaint.take() {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(s.max(0.0)));
        }
    }

    fn exec_reply(&mut self, a: Action) -> Option<Reply> {
        let r = self.core.execute(a.clone());
        if let Err(Refusal { kind: RefusalKind::ReadOnly { conn }, .. }) = &r {
            let conn = *conn;
            self.modal = Some(Modal::Unlock(conn, Some(Box::new(move |app: &mut App| app.exec(a)))));
        }
        self.run_fx();
        r.ok()
    }

    /// Carries out what actions and answers left for the UI. Also run each frame from `logic()`.
    pub(super) fn run_fx(&mut self) {
        for fx in self.core.take_ui_fx() {
            match fx {
                UiFx::Open(d) => self.open_dialog(d),
                UiFx::Connected { conn, sql } => self.exec(Action::Tab(TabA::Connected { conn, sql })),
                UiFx::CloseTabs { conn, db, coll } => self.remove_tabs(|t| t.sel.conn == conn && t.sel.db == db && coll.as_ref().is_none_or(|c| t.sel.coll == *c)),
                UiFx::Restart => self.restart_now(),
                UiFx::OpenScript { r, how, conn } => self.open_script_on(&r, how, conn),
                UiFx::OpenResolved { r, how, script, meta } => self.open_resolved(&r, how, *script, Some(meta)),
                // Opened as usual, then swapped with the dialog that was open, which stays on top.
                UiFx::Late(d) => {
                    let held = self.modal.take();
                    self.open_dialog(d);
                    if let Some(m) = held {
                        self.late_modals.extend(self.modal.replace(m));
                    }
                }
                UiFx::IfFree(d) => {
                    if self.modal.is_none() {
                        self.open_dialog(d);
                    }
                }
                // Copied by `logic()`, which holds the egui context.
                UiFx::CopyText(text) => self.clip = Some(text),
                // Sent by `logic()`, which holds the egui context.
                UiFx::Attention => self.attention = true,
                UiFx::UpdPopup => self.ui.upd_popup = true,
                // A debounce waiting out its pause; the drawing code that asked reads it back with `take_repaint`.
                UiFx::RepaintAfter(s) => self.repaint = Some(self.repaint.map_or(s, |r| r.min(s))),
            }
        }
    }

    /// Opens a dialog an action or an answer named. The Explain dialog is built by the app with its first request.
    fn open_dialog(&mut self, d: Dialog) {
        if let Dialog::SqlExplain(text) = d {
            return self.sql_explain(text);
        }
        self.modal = Some(match d {
            Dialog::Unlock(conn) => Modal::Unlock(conn, None),
            Dialog::PgNewRole(f) => Modal::PgNewRole(f),
            Dialog::PgPassword(f) => Modal::PgPassword(f),
            Dialog::PgDropRole(f) => Modal::PgDropRole(f),
            Dialog::SqlNewUser(f) => Modal::SqlNewUser(f),
            Dialog::SqlPassword(f) => Modal::SqlPassword(f),
            Dialog::Typed { title, expect, action } => Modal::Typed { title, expect, typed: String::new(), action },
            Dialog::ShellWrite { tab, ops } => Modal::ShellWrite { tab, ops },
            Dialog::MongoNewColl(f) => Modal::MongoNewColl(f),
            Dialog::Settings => Modal::Settings,
            Dialog::WhatsNew(w) => Modal::WhatsNew(w),
            Dialog::ErrorDetails(e) => Modal::ErrorDetails(e),
            Dialog::Text { title, body, code } => Modal::Text { title, body, code },
            Dialog::SqlExplain(_) => unreachable!("handled above"),
            Dialog::SaveAll(keys) => Modal::SaveAll(keys),
            Dialog::SqlRow(f) => Modal::SqlRow(f),
            Dialog::Save { tag, changes } => Modal::Save { tag, changes },
            Dialog::Delete(sel, docs) => Modal::Delete(sel, docs),
            Dialog::AggWrite(p) => Modal::AggWrite(p),
            Dialog::Grouped { sel, path, pipeline, rows } => Modal::Grouped { sel, path, pipeline, rows },
            Dialog::Relations { conn, db } => Modal::Relations { conn, db },
            Dialog::Rename { doc, path, name } => Modal::Rename { doc, path, name },
            Dialog::CreateIndex(f) => Modal::CreateIndex(f),
            Dialog::ImportInsert(n) => Modal::ImportInsert(n),
            Dialog::ColEdit(f) => Modal::ColEdit(f),
            Dialog::IdxEdit(f) => Modal::IdxEdit(f),
            Dialog::CheckEdit(f) => Modal::CheckEdit(f),
            Dialog::TableOpts(f) => Modal::TableOpts(f),
            Dialog::FkEdit(f) => Modal::FkEdit(f),
            Dialog::SqlMaintain { what, stmt } => Modal::SqlMaintain { what, stmt },
            Dialog::SqlForm(f) => f.modal(),
        });
    }
}
