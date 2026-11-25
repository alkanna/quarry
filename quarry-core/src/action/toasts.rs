//! The toasts and the status bar's error list (src/app/errors.rs draws them): dismissing, the details of an error, the
//! list's Clear, and the toast buttons that only change Core. Buttons that reach other areas (Retry, Edit connection,
//! Explain, Undo, Unlock and the rest) stay in `App::run_act`.

use super::{Dialog, Reply, Refusal, UiFx};
use crate::core::notify::raised;
use crate::core::Core;
use crate::state::errors::{Act, Toast, Tone};
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// Seconds a success note, then a warning, stays up when it fades.
pub const OK_S: u64 = 8;
pub const WARN_S: u64 = 6;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum AppA {
    /// A toast's close button, or a fix button that closes its toast. By toast id.
    Dismiss(u64),
    /// The details dialog of the error with this id, from a toast or the error list.
    Details(u64),
    /// The error list was opened: its count stops showing as new.
    ErrorsSeen,
    ClearErrors,
    /// A timeout's "Raise limit to" fix; the app then runs the view again.
    RaiseLimit,
}

impl Core {
    pub(super) fn app(&mut self, a: AppA) -> Result<Reply, Refusal> {
        match a {
            AppA::Dismiss(id) => self.toasts.retain(|t| t.id != id),
            AppA::Details(id) => {
                // A toast keeps its error even after the list dropped it, so the toast is looked at first.
                let e = self.toasts.iter().filter_map(|t| t.err.as_ref()).find(|e| e.id == id).or_else(|| self.errors.iter().rev().find(|e| e.id == id)).cloned();
                if let Some(e) = e {
                    self.ui_fx.push(UiFx::Open(Dialog::ErrorDetails(Box::new(e))));
                }
            }
            AppA::ErrorsSeen => self.errors_unseen = 0,
            AppA::ClearErrors => {
                self.errors.clear();
                self.errors_unseen = 0;
            }
            AppA::RaiseLimit => {
                self.prefs.timeout_ms = raised(self.prefs.timeout_ms);
                self.save_prefs();
                self.ok(format!("Time limit raised to {} s. Change it in Settings.", self.prefs.timeout_ms / 1000));
            }
        }
        Ok(Reply::Done)
    }

    /// Drops the toasts whose time is up and the oldest past four. Answers whether any still fades, so the UI looks again.
    pub fn toasts_tick(&mut self) -> bool {
        let keep = self.prefs.keep_toasts;
        // An Undo still ends on its timer: one left up for hours would revert an edit long forgotten.
        let fades = |t: &Toast| t.tone != Tone::Err && (!keep || t.acts.iter().any(|(_, a)| matches!(a, Act::Undo(_) | Act::Restore(_))));
        self.toasts.retain(|t| !fades(t) || t.at.elapsed().as_secs() < if t.tone == Tone::Ok { OK_S } else { WARN_S });
        // Only the latest few fit; older errors stay in the status bar list.
        while self.toasts.len() > 4 {
            let i = self.toasts.iter().position(|t| t.tone != Tone::Err).unwrap_or(0);
            self.toasts.remove(i);
        }
        self.toasts.iter().any(fades)
    }

    /// A note with buttons that fades like a success, such as a script waiting for its connection with Cancel.
    pub fn toast_acts(&mut self, s: String, acts: Vec<(String, Act)>) {
        self.err_seq += 1;
        self.toasts.retain(|t| !(t.tone == Tone::Ok && t.title == s));
        self.toasts.push(Toast { id: self.err_seq, tone: Tone::Ok, title: s, body: String::new(), acts, at: Instant::now(), err: None });
    }
}
