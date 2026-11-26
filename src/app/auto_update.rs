//! Checks for, downloads and installs app updates. The header pill (update_pill.rs) shows where it is.

use super::*;

pub(super) use quarry_core::action::update::HELD;
pub(super) use quarry_core::action::UpdBtn;
use quarry_core::action::{Action, ConnA, UpdateA};

impl App {
    /// The update timers live in Core; this asks for the repaint they need.
    pub(super) fn auto_update(&mut self, ctx: &egui::Context) {
        if let Some(d) = self.core.update_tick() {
            ctx.request_repaint_after(d);
        }
    }

    pub(super) fn can_update(&self) -> bool {
        self.core.can_update()
    }

    /// Restarts into the installed update. The process exits without running destructors, so every connection's tunnel
    /// or Cloud SQL proxy is dropped first; on macOS nothing else would stop a proxy.
    pub(super) fn restart(&mut self) -> ! {
        self.close_ways();
        // The exit skips the end of the frame, where changed settings are written.
        self.core.flush_prefs();
        crate::update::restart()
    }

    /// Drops every SSH tunnel and Cloud SQL proxy, which stops them.
    pub(super) fn close_ways(&mut self) {
        self.exec(Action::Conn(ConnA::DropWays));
    }

    /// Restart from Core, once its own work is clear: the app's dialogs can still hold it.
    pub(super) fn restart_now(&mut self) {
        if self.busy_work() {
            self.warn(HELD);
        } else if !self.can_update() {
            self.warn("The demo has nothing to restart into.");
        } else {
            self.restart();
        }
    }

    pub(super) fn upd_btn(&self) -> Option<UpdBtn> {
        self.core.upd_btn()
    }

    pub(super) fn upd_act(&mut self, b: UpdBtn) {
        self.exec(Action::Update(UpdateA::Press(b)));
    }

    pub(super) fn update_status(&self) -> (String, String) {
        let current = env!("CARGO_PKG_VERSION");
        let checked = self.core.update_checked.map_or("New releases are checked at launch and once a day".to_string(), |t| format!("checked {}", ago(t.elapsed().as_secs())));
        match &self.core.upd {
            Upd::Idle if !self.core.update_note.is_empty() => (format!("Quarry {current}"), self.core.update_note.clone()),
            Upd::Idle | Upd::UpToDate(_) if self.core.update_checked.is_some() => (format!("Quarry {current}"), format!("Up to date  ·  {checked}")),
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
}

/// Check and Cancel are plain buttons; the others move the update forward. Restart is off while work is `held`.
pub(super) trait UpdBtnUi {
    fn show(self, ui: &mut egui::Ui, held: bool) -> egui::Response;
}

impl UpdBtnUi for UpdBtn {
    fn show(self, ui: &mut egui::Ui, held: bool) -> egui::Response {
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
