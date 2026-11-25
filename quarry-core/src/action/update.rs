//! App updates: the check, the download and install, the buttons of Settings > Updates and of the header pill's popover,
//! and the worker's answers. src/app/auto_update.rs and update_pill.rs draw them. The checksum check lives in
//! `crate::update::download` and is not touched here.

use super::{Dialog, Reply, Refusal, UiFx};
use crate::core::Core;
use crate::db::{self, Msg};
use crate::paths::tools_dir;
use crate::state::settings::SettingsCat;
use crate::state::whats_new::WhatsNew;
use crate::state::Upd;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// Why Restart waits, in the warning, Settings and the popover.
pub const HELD: &str = "Save or discard your edits and wait for running scripts first.";

/// How long "Up to date" stays in the header after a manual check.
pub const UP_TO_DATE_S: u64 = 3;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum UpdateA {
    /// Check for updates from the app menu, the palette or Settings: the popover opens to say what was found.
    CheckNow,
    /// The one button the updater's state offers, in Settings or the popover.
    Press(UpdBtn),
    /// Settings' "What's new in" Show: the notes of the running version.
    ShowNotes,
    /// The popover's notes of the release it offers.
    OfferedNotes,
}

/// The one action the updater's state offers, in Settings and in the popover.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub enum UpdBtn {
    Check,
    Update,
    Cancel,
    Restart,
    Retry,
}

impl UpdBtn {
    pub fn label(self) -> &'static str {
        match self {
            UpdBtn::Check => "Check now",
            UpdBtn::Update => "Update",
            UpdBtn::Cancel => "Cancel",
            UpdBtn::Restart => "Restart now",
            UpdBtn::Retry => "Try again",
        }
    }
}

impl Core {
    pub(super) fn update(&mut self, a: UpdateA) -> Result<Reply, Refusal> {
        match a {
            UpdateA::CheckNow => self.check_now(),
            UpdateA::Press(b) => self.upd_act(b),
            UpdateA::ShowNotes => {
                let current = crate::VERSION;
                let rs = crate::update::changelog("", current);
                if rs.is_empty() {
                    return Err(self.refuse(format!("CHANGELOG.md has no notes for {current}.")));
                }
                self.ui_fx.push(UiFx::Open(Dialog::WhatsNew(Box::new(WhatsNew::new(rs, "")))));
            }
            UpdateA::OfferedNotes => {
                if let Some(a) = self.upd.info() {
                    let r = crate::update::Release { version: a.version.clone(), date: String::new(), sections: a.notes.clone() };
                    self.ui_fx.push(UiFx::Open(Dialog::WhatsNew(Box::new(WhatsNew::new(vec![r], "")))));
                }
            }
        }
        Ok(Reply::Done)
    }

    /// Checks GitHub 5 seconds after launch and then daily. An installed update waits for Restart: Quarry never restarts by
    /// itself, and the new version also starts on the next launch. Called every frame; answers when the UI should look again.
    pub fn update_tick(&mut self) -> Option<Duration> {
        let mut wake: Option<Duration> = None;
        let mut at = |d: Duration| wake = Some(wake.map_or(d, |w| w.min(d)));
        if let Upd::Downloading { .. } = self.upd {
            // The progress lives in atomics the worker writes, so nothing else would redraw it.
            at(Duration::from_millis(150));
        }
        if let Upd::UpToDate(t) = self.upd {
            let left = Duration::from_secs(UP_TO_DATE_S).saturating_sub(t.elapsed());
            if left.is_zero() {
                self.upd = Upd::Idle;
            } else {
                at(left);
            }
        }
        // Checks run with automatic updates off too: the release is then offered in the header instead of installed.
        if !self.can_update() {
            return wake;
        }
        const FIRST_CHECK_S: u64 = 5;
        const CHECK_EVERY_S: u64 = 24 * 3600;
        let due = match self.update_checked {
            None => self.started.elapsed().as_secs() >= FIRST_CHECK_S,
            Some(t) => t.elapsed().as_secs() >= CHECK_EVERY_S,
        };
        if self.update_checked.is_none() {
            at(Duration::from_secs(FIRST_CHECK_S));
        }
        // A failed or offered update is checked again daily, so a newer release or a retry is picked up.
        if due && !matches!(self.upd, Upd::Checking { .. } | Upd::Downloading { .. } | Upd::Ready(_)) {
            self.check_update(false);
        }
        wake
    }

    /// Dev builds and the demo never install releases.
    pub fn can_update(&self) -> bool {
        !self.demo && !cfg!(debug_assertions)
    }

    /// Work a restart would lose that Core knows of; the app adds its open dialogs.
    pub fn work_held(&self) -> bool {
        !self.drafts.is_empty()
            || !self.saving.is_empty()
            || self.json_edit.is_some()
            || self.insert.is_some()
            || self.inline.is_some()
            || self.new_field.is_some()
            || self.tool.as_ref().is_some_and(|t| t.done.is_none())
            || self.tabs.iter().any(|t| t.shell.cancel.is_some() || t.compare.running || t.loading)
    }

    /// `manual` decides whether the answer is shown when there is nothing new; it lives in the Checking state only.
    pub fn check_update(&mut self, manual: bool) {
        self.update_checked = Some(std::time::Instant::now());
        self.upd = Upd::Checking { manual };
        self.worker.blocking(|| Msg::UpdateChecked(crate::update::check()));
        // A running dump or restore keeps its program open; Windows cannot replace it then.
        if self.tool.as_ref().is_none_or(|t| t.done.is_some()) {
            self.worker.blocking(|| Msg::ToolsInstalled(crate::update::update_db_tools(&tools_dir())));
        }
    }

    /// Dev builds and the demo cannot update, so they show Settings > Updates, which says so.
    fn check_now(&mut self) {
        if !self.can_update() {
            self.settings_cat = SettingsCat::Updates;
            self.ui_fx.push(UiFx::Open(Dialog::Settings));
            return;
        }
        match self.upd {
            Upd::Idle | Upd::UpToDate(_) | Upd::Failed { info: None, .. } => self.check_update(true),
            // An automatic check is already asking; its answer is shown as if asked for.
            Upd::Checking { manual: false } => self.upd = Upd::Checking { manual: true },
            _ => {}
        }
        self.upd_popup = true;
    }

    pub fn start_download(&mut self, info: crate::update::Available) {
        // The demo's made-up states offer the same buttons, which must not fetch anything.
        if !self.can_update() {
            self.upd = Upd::Available(info);
            return;
        }
        let prog = Arc::new(crate::update::Progress::default());
        let (i, p) = (info.clone(), prog.clone());
        self.worker.blocking(move || Msg::UpdateInstalled(crate::update::download(&i, &p)));
        self.upd = Upd::Downloading { info, prog, started: std::time::Instant::now() };
    }

    pub fn upd_btn(&self) -> Option<UpdBtn> {
        match &self.upd {
            Upd::Idle | Upd::UpToDate(_) => self.can_update().then_some(UpdBtn::Check),
            Upd::Checking { .. } => None,
            Upd::Available(_) => Some(UpdBtn::Update),
            // Once downloaded, the archive is being swapped in and can no longer be stopped.
            Upd::Downloading { prog, .. } => (!prog.verifying.load(std::sync::atomic::Ordering::Relaxed)).then_some(UpdBtn::Cancel),
            Upd::Ready(_) => Some(UpdBtn::Restart),
            Upd::Failed { .. } => Some(UpdBtn::Retry),
        }
    }

    fn upd_act(&mut self, b: UpdBtn) {
        match b {
            // Retry downloads the release again when one was found, else asks GitHub again.
            UpdBtn::Update | UpdBtn::Retry if self.upd.info().is_some() => {
                if let Some(a) = self.upd.info().cloned() {
                    self.start_download(a);
                }
            }
            UpdBtn::Update => {}
            UpdBtn::Check | UpdBtn::Retry => {
                if self.can_update() {
                    self.check_update(true);
                }
            }
            UpdBtn::Cancel => {
                if let Upd::Downloading { prog, .. } = &self.upd {
                    prog.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            // The app adds its open dialogs to what holds a restart, then restarts.
            UpdBtn::Restart if self.work_held() => self.warn(HELD),
            UpdBtn::Restart => self.ui_fx.push(UiFx::Restart),
        }
    }

    pub(super) fn on_update_checked(&mut self, res: db::Res<Option<crate::update::Available>>) {
        let manual = matches!(self.upd, Upd::Checking { manual: true });
        if res.is_ok() {
            self.update_note.clear();
        }
        self.upd = match res {
            Ok(Some(info)) if self.prefs.auto_update => return self.start_download(info),
            Ok(Some(info)) => Upd::Available(info),
            Ok(None) if manual => Upd::UpToDate(std::time::Instant::now()),
            Ok(None) => Upd::Idle,
            Err(msg) if manual => Upd::Failed { info: None, msg },
            // Offline or rate-limited checks stay quiet; the reason shows in Settings.
            Err(e) => {
                self.update_note = format!("Last check failed: {e}");
                Upd::Idle
            }
        };
    }

    pub(super) fn on_update_installed(&mut self, res: db::Res<String>) {
        let Upd::Downloading { info, prog, .. } = std::mem::replace(&mut self.upd, Upd::Idle) else { return };
        self.upd = match res {
            Ok(_) => Upd::Ready(info),
            Err(_) if prog.cancel.load(std::sync::atomic::Ordering::Relaxed) => Upd::Available(info),
            Err(msg) => Upd::Failed { info: Some(info), msg },
        };
    }

    /// At launch after an update: the notes since the version last run, unless this is a fresh install (no ui.json), and
    /// that version is remembered as seen.
    pub fn whats_new_seen(&mut self, had_prefs: bool) {
        let current = crate::VERSION;
        if self.prefs.last_version == current {
            return;
        }
        // Settings from 0.2.0 and earlier lack last_version, so updating from those shows them.
        if had_prefs {
            let notes = crate::update::changelog(&self.prefs.last_version, current);
            if !notes.is_empty() {
                self.ui_fx.push(UiFx::Open(Dialog::WhatsNew(Box::new(WhatsNew::new(notes, &self.prefs.last_version)))));
            }
        }
        self.prefs.last_version = current.into();
        self.save_prefs();
    }
}
