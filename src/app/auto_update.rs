//! Checks for, downloads and installs app updates. The header pill (update_pill.rs) shows where it is.

use super::*;

/// Why Restart waits, in the warning, Settings and the popover.
pub(super) const HELD: &str = "Save or discard your edits and wait for running scripts first.";

impl App {
    /// Checks GitHub 5 seconds after launch and then daily. An installed update waits for Restart:
    /// Quarry never restarts by itself, and the new version also starts on the next launch.
    pub(super) fn auto_update(&mut self, ctx: &egui::Context) {
        if let Upd::Downloading { .. } = self.upd {
            // The progress lives in atomics the worker writes, so nothing else would redraw it.
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
        if let Upd::UpToDate(at) = self.upd {
            let left = std::time::Duration::from_secs(UP_TO_DATE_S).saturating_sub(at.elapsed());
            if left.is_zero() {
                self.upd = Upd::Idle;
            } else {
                ctx.request_repaint_after(left);
            }
        }
        // Checks run with automatic updates off too: the release is then offered in the header instead of installed.
        if !self.can_update() {
            return;
        }
        const FIRST_CHECK_S: u64 = 5;
        const CHECK_EVERY_S: u64 = 24 * 3600;
        let due = match self.update_checked {
            None => self.started.elapsed().as_secs() >= FIRST_CHECK_S,
            Some(t) => t.elapsed().as_secs() >= CHECK_EVERY_S,
        };
        if self.update_checked.is_none() {
            ctx.request_repaint_after(std::time::Duration::from_secs(FIRST_CHECK_S));
        }
        // A failed or offered update is checked again daily, so a newer release or a retry is picked up.
        if due && !matches!(self.upd, Upd::Checking { .. } | Upd::Downloading { .. } | Upd::Ready(_)) {
            self.check_update(false);
        }
    }

    /// Dev builds and the demo never install releases.
    pub(super) fn can_update(&self) -> bool {
        !self.demo && !cfg!(debug_assertions)
    }

    /// Restarts into the installed update. The process exits without running destructors, so every connection's tunnel
    /// or Cloud SQL proxy is dropped first; on macOS nothing else would stop a proxy.
    pub(super) fn restart(&mut self) -> ! {
        self.close_ways();
        crate::update::restart()
    }

    /// Drops every SSH tunnel and Cloud SQL proxy, which stops them.
    pub(super) fn close_ways(&mut self) {
        for c in &mut self.conns {
            c.tunnel = None;
        }
    }

    /// `manual` decides whether the answer is shown when there is nothing new; it lives in the Checking state only.
    pub(super) fn check_update(&mut self, manual: bool) {
        self.update_checked = Some(std::time::Instant::now());
        self.upd = Upd::Checking { manual };
        self.worker.blocking(|| Msg::UpdateChecked(crate::update::check()));
        // A running dump or restore keeps its program open; Windows cannot replace it then.
        if self.tool.as_ref().is_none_or(|t| t.done.is_some()) {
            self.worker.blocking(|| Msg::ToolsInstalled(crate::update::update_db_tools(&tools_dir())));
        }
    }

    /// Check for updates from the app menu, the palette or Settings: the popover opens to say what was found.
    /// Dev builds and the demo cannot update, so they show Settings > Updates, which says so.
    pub(super) fn check_now(&mut self) {
        if !self.can_update() {
            self.settings_cat = settings::SettingsCat::Updates;
            self.modal = Some(Modal::Settings);
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

    pub(super) fn start_download(&mut self, info: crate::update::Available) {
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

    fn restart_now(&mut self) {
        if self.busy_work() {
            self.warn(HELD);
        } else if !self.can_update() {
            self.warn("The demo has nothing to restart into.");
        } else {
            self.restart();
        }
    }

    pub(super) fn upd_btn(&self) -> Option<UpdBtn> {
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

    pub(super) fn upd_act(&mut self, b: UpdBtn) {
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
            UpdBtn::Restart => self.restart_now(),
        }
    }

    pub(super) fn update_status(&self) -> (String, String) {
        let current = env!("CARGO_PKG_VERSION");
        let checked = self.update_checked.map_or("New releases are checked at launch and once a day".to_string(), |t| format!("checked {}", ago(t.elapsed().as_secs())));
        match &self.upd {
            Upd::Idle if !self.update_note.is_empty() => (format!("Quarry {current}"), self.update_note.clone()),
            Upd::Idle | Upd::UpToDate(_) if self.update_checked.is_some() => (format!("Quarry {current}"), format!("Up to date  ·  {checked}")),
            Upd::Idle | Upd::UpToDate(_) => (format!("Quarry {current}"), checked),
            Upd::Checking { .. } => (format!("Quarry {current}"), "Checking for updates…".into()),
            Upd::Available(a) => (format!("Quarry {} is available", a.version), format!("You are running {current}")),
            Upd::Downloading { info, prog, .. } if prog.verifying.load(std::sync::atomic::Ordering::Relaxed) => (format!("Installing Quarry {}", info.version), "Checking the download against its published checksum".into()),
            Upd::Downloading { info, prog, .. } => {
                let pct = prog.percent(info.size).map_or(String::new(), |n| format!("  ·  {n}%"));
                (format!("Downloading Quarry {}", info.version), format!("You are running {current}{pct}"))
            }
            Upd::Ready(a) if self.busy_work() => (format!("Quarry {} is installed", a.version), HELD.into()),
            Upd::Ready(a) => (format!("Quarry {} is installed", a.version), format!("Restart to use it, you are running {current}")),
            Upd::Failed { msg, .. } => ("The update failed".into(), msg.clone()),
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
}

/// The one action the updater's state offers, in Settings and in the popover.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum UpdBtn {
    Check,
    Update,
    Cancel,
    Restart,
    Retry,
}

impl UpdBtn {
    fn label(self) -> &'static str {
        match self {
            UpdBtn::Check => "Check now",
            UpdBtn::Update => "Update",
            UpdBtn::Cancel => "Cancel",
            UpdBtn::Restart => "Restart now",
            UpdBtn::Retry => "Try again",
        }
    }

    /// Check and Cancel are plain buttons; the others move the update forward. Restart is off while work is `held`.
    pub(super) fn show(self, ui: &mut egui::Ui, held: bool) -> egui::Response {
        if matches!(self, UpdBtn::Check | UpdBtn::Cancel) {
            ui.button(self.label())
        } else {
            ui.add_enabled(self != UpdBtn::Restart || !held, primary_button(ui, self.label(), PRIMARY()))
        }
    }
}

fn ago(s: u64) -> String {
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        _ => format!("{} h ago", s / 3600),
    }
}

/// How long "Up to date" stays in the header after a manual check.
pub(super) const UP_TO_DATE_S: u64 = 3;
