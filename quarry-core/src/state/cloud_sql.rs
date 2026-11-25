//! State behind src/app/cloud_sql.rs, which draws it.

use crate::cloudsql::{Instance, Start};
use std::collections::VecDeque;
use std::collections::HashMap;
use std::path::PathBuf;

/// What gcloud told this session. Kept until Refresh, so the browser reopens at once.
#[derive(Default)]
pub struct CloudCache {
    /// The load in flight or answered; 0 before the first.
    pub req: u64,
    pub start: Option<Start>,
    /// The answer a Refresh replaced, shown until the new one arrives so the window keeps the layout it had.
    pub prev: Option<Start>,
    pub instances: HashMap<String, Result<Vec<Instance>, String>>,
    /// Projects still to ask for, and how many gcloud lists are running, those of a load Refresh replaced included.
    pub queue: VecDeque<String>,
    pub running: usize,
    /// What the running download or install of the proxy says while it works.
    pub busy: Option<&'static str>,
    /// Where gcloud is without a Settings path, for Settings: None until asked, then the answer once it came.
    pub gcloud_seen: Option<Option<PathBuf>>,
    pub gcloud_asked: bool,
}

impl CloudCache {
    /// A new load `req`. Lists still running keep counting against `PARALLEL` until they end or time out, so Refresh
    /// never runs more gcloud processes at once.
    pub fn reload(&mut self, req: u64) {
        let old = std::mem::take(self);
        let prev = old.start.or(old.prev);
        *self = CloudCache { req, prev, busy: old.busy, running: old.running, gcloud_seen: old.gcloud_seen, gcloud_asked: old.gcloud_asked, ..Default::default() };
    }

    /// The next project to list, when fewer than `PARALLEL` lists run.
    pub fn next_project(&mut self) -> Option<String> {
        if self.running >= PARALLEL {
            return None;
        }
        let p = self.queue.pop_front()?;
        self.running += 1;
        Some(p)
    }

    /// A list ended; it is kept only when it belongs to the current load.
    pub fn listed(&mut self, req: u64, project: String, res: Result<Vec<Instance>, String>) {
        self.running = self.running.saturating_sub(1);
        if req == self.req {
            self.instances.insert(project, res);
        }
    }

    /// Whether IAM sign-in can start: the last load found Application Default Credentials.
    pub fn adc(&self) -> bool {
        self.start.as_ref().is_some_and(|s| s.adc)
    }

    pub fn account(&self) -> Option<&str> {
        self.start.as_ref().and_then(|s| s.account.as_deref())
    }

    pub fn instance(&self, name: &str) -> Option<&Instance> {
        self.instances.values().filter_map(|r| r.as_ref().ok()).flatten().find(|i| i.connection_name == name)
    }

    pub fn components(&self) -> bool {
        self.start.as_ref().is_some_and(|s| s.components && s.gcloud.is_some())
    }
}

#[derive(Default)]
pub struct CloudBrowser {
    pub search: String,
    pub stopped: bool,
    /// The project picked on the left; None is all of them.
    pub project: Option<String>,
    /// The instance row picked, by connection name.
    pub sel: Option<String>,
    /// Opened from a connection form: Use fills that form instead of a new one.
    pub change: bool,
    /// An IAM instance whose Use found no Application Default Credentials, which the browser then asks for.
    pub adc_for: Option<String>,
    /// Shows the projects with no PostgreSQL or MySQL instance or no access, folded by default since an account often reaches
    /// dozens of them (Apps Script's sys- projects among them).
    pub others: bool,
}

/// gcloud is a Python program of some weight, so a long project list is asked a few projects at a time.
pub const PARALLEL: usize = 6;
