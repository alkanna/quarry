//! Checks for and installs app updates.

use super::*;

impl App {
    /// Checks GitHub 5 seconds after launch and then daily, like the original (main process
    /// setTimeout 5 s and setInterval 24 h, main.js line 33757); restarts into an update once idle.
    pub(super) fn auto_update(&mut self, ctx: &egui::Context) {
        if self.demo || cfg!(debug_assertions) {
            return;
        }
        if ctx.input(|i| !i.events.is_empty() || i.pointer.is_moving()) {
            self.last_input = std::time::Instant::now();
        }
        const FIRST_CHECK_S: u64 = 5;
        const CHECK_EVERY_S: u64 = 24 * 3600;
        let due = match self.update_checked {
            None => self.started.elapsed().as_secs() >= FIRST_CHECK_S,
            Some(t) => t.elapsed().as_secs() >= CHECK_EVERY_S,
        };
        if self.update_checked.is_none() && self.prefs.auto_update {
            ctx.request_repaint_after(std::time::Duration::from_secs(FIRST_CHECK_S));
        }
        if self.prefs.auto_update && self.updated.is_none() && due {
            self.check_update();
        }
        if self.updated.is_some() && self.prefs.auto_update {
            if self.last_input.elapsed().as_secs() >= 60 && !self.busy() {
                self.restart();
            }
            // Wake up to test idleness even when nothing else repaints.
            ctx.request_repaint_after(std::time::Duration::from_secs(5));
        }
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

    pub(super) fn check_update(&mut self) {
        self.update_checked = Some(std::time::Instant::now());
        self.update_note = "Checking for updates…".into();
        self.worker.blocking(|| Msg::Update(crate::update::check_and_install()));
        // A running dump or restore keeps its program open; Windows cannot replace it then.
        if self.tool.as_ref().is_none_or(|t| t.done.is_some()) {
            self.worker.blocking(|| Msg::ToolsInstalled(crate::update::update_db_tools(&tools_dir())));
        }
    }
}
