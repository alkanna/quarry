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
                // The Explain dialog is built by the app with its first request.
                UiFx::Open(Dialog::SqlExplain(text)) => self.sql_explain(text),
                UiFx::Open(d) => {
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
                    })
                }
                UiFx::Connected { conn, sql: false } => {
                    let here = self.core.cur;
                    for i in 0..self.core.tabs.len() {
                        if self.core.tabs[i].sel.conn == conn && self.core.tabs[i].docs.is_empty() {
                            self.core.cur = i;
                            self.core.run_query();
                        }
                    }
                    self.core.cur = here;
                }
                UiFx::CloseTabs { conn, db, coll } => self.core.remove_tabs(|t| t.sel.conn == conn && t.sel.db == db && coll.as_ref().is_none_or(|c| t.sel.coll == *c)),
                UiFx::Connected { conn, sql: true } => {
                    for i in 0..self.core.tabs.len() {
                        if self.core.tabs[i].sel.conn == conn && self.core.tabs[i].docs.is_empty() {
                            self.core.reload_of(i);
                        }
                    }
                }
                UiFx::Restart => self.restart_now(),
                UiFx::OpenScript { r, how, conn } => self.open_script_on(&r, how, conn),
                UiFx::OpenResolved { r, how, script, meta } => self.open_resolved(&r, how, *script, Some(meta)),
                // Opened as usual, then swapped with the dialog that was open, which stays on top.
                UiFx::Late(d) => {
                    let held = self.modal.take();
                    self.core.ui_fx.push(UiFx::Open(d));
                    self.run_fx();
                    if let Some(m) = held {
                        self.late_modals.extend(self.modal.replace(m));
                    }
                }
                UiFx::IfFree(d) => {
                    if self.modal.is_none() {
                        self.core.ui_fx.push(UiFx::Open(d));
                        self.run_fx();
                    }
                }
                // Copied by `logic()`, which holds the egui context.
                UiFx::CopyText(text) => self.clip = Some(text),
            }
        }
    }
}
