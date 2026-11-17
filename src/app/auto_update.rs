//! Checks for, downloads and installs app updates. The header pill (update_pill.rs) shows where it is.

use super::*;

impl App {
    /// Checks GitHub 5 seconds after launch and then daily. An installed update waits for Restart:
    /// Quarry never restarts by itself, and the new version also starts on the next launch.
    pub(super) fn auto_update(&mut self, ctx: &egui::Context) {
        if let Upd::Downloading { .. } = self.upd {
            // The progress lives in atomics the worker writes, so nothing else would redraw it.
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
        if let Upd::UpToDate(at) = self.upd {
            if at.elapsed().as_secs() >= UP_TO_DATE_S {
                self.upd = Upd::Idle;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
        }
        if !self.can_update() || !self.prefs.auto_update {
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
        if due && matches!(self.upd, Upd::Idle | Upd::UpToDate(_)) {
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

    pub(super) fn check_update(&mut self, manual: bool) {
        self.update_checked = Some(std::time::Instant::now());
        self.upd = Upd::Checking { manual };
        self.worker.blocking(move || Msg::UpdateChecked(crate::update::check(), manual));
        // A running dump or restore keeps its program open; Windows cannot replace it then.
        if self.tool.as_ref().is_none_or(|t| t.done.is_some()) {
            self.worker.blocking(|| Msg::ToolsInstalled(crate::update::update_db_tools(&tools_dir())));
        }
    }

    /// Check for updates from the app menu, the palette or Settings: the popover opens to say what was found.
    pub(super) fn check_now(&mut self) {
        if self.can_update() && matches!(self.upd, Upd::Idle | Upd::UpToDate(_) | Upd::Failed { info: None, .. }) {
            self.check_update(true);
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

    pub(super) fn cancel_download(&mut self) {
        if let Upd::Downloading { prog, .. } = &self.upd {
            prog.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Restart now, unless something would be lost; the pill and Settings show why it waits.
    pub(super) fn restart_now(&mut self) {
        if self.busy_work() {
            self.warn("Save or discard your edits and wait for running scripts first.");
        } else if !self.can_update() {
            self.warn("The demo has nothing to restart into.");
        } else {
            self.restart();
        }
    }

    /// Settings' status row: the title, the line under it and the button that fits, if any.
    pub(super) fn update_status(&self) -> (String, String, Option<&'static str>) {
        use std::sync::atomic::Ordering::Relaxed;
        let current = env!("CARGO_PKG_VERSION");
        let checked = self.update_checked.map_or("New releases are checked at launch and once a day".to_string(), |t| format!("checked {}", ago(t.elapsed().as_secs())));
        let check = self.can_update().then_some("Check now");
        match &self.upd {
            Upd::Idle if !self.update_note.is_empty() => (format!("Quarry {current}"), self.update_note.clone(), check),
            Upd::Idle | Upd::UpToDate(_) if self.update_checked.is_some() => (format!("Quarry {current}"), format!("Up to date  ·  {checked}"), check),
            Upd::Idle | Upd::UpToDate(_) => (format!("Quarry {current}"), checked, check),
            Upd::Checking { .. } => (format!("Quarry {current}"), "Checking for updates…".into(), None),
            Upd::Available(a) => (format!("Quarry {} is available", a.version), format!("You are running {current}"), Some("Update")),
            Upd::Downloading { info, prog, .. } if prog.verifying.load(Relaxed) => (format!("Installing Quarry {}", info.version), "Checking the download against its published checksum".into(), Some("Cancel")),
            Upd::Downloading { info, prog, .. } => {
                let pct = crate::update::percent(prog.bytes.load(Relaxed), info.size).map_or(String::new(), |n| format!("  ·  {n}%"));
                (format!("Downloading Quarry {}", info.version), format!("You are running {current}{pct}"), Some("Cancel"))
            }
            Upd::Ready(a) if self.busy_work() => (format!("Quarry {} is installed", a.version), "Save or discard your edits and wait for running scripts, then restart to use it".into(), Some("Restart now")),
            Upd::Ready(a) => (format!("Quarry {} is installed", a.version), format!("Restart to use it, you are running {current}"), Some("Restart now")),
            Upd::Failed { msg, .. } => ("The update failed".into(), msg.clone(), Some("Try again")),
        }
    }

    pub(super) fn on_update_checked(&mut self, res: db::Res<Option<crate::update::Available>>, manual: bool) {
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
        if !matches!(self.upd, Upd::Failed { .. }) {
            self.update_note.clear();
        }
    }

    pub(super) fn on_update_installed(&mut self, res: db::Res<String>) {
        let Upd::Downloading { info, .. } = std::mem::replace(&mut self.upd, Upd::Idle) else { return };
        self.upd = match res {
            Ok(_) => Upd::Ready(info),
            Err(e) if e == crate::update::CANCELLED => Upd::Available(info),
            Err(msg) => Upd::Failed { info: Some(info), msg },
        };
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
