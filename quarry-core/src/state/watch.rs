//! State behind src/app/watch.rs, which draws it.

use crate::db;

impl WatchTab {
    pub fn running(&self) -> bool {
        self.cancel.is_some()
    }

    /// Ends the stream; its last events, already sent, are dropped since no tab has its id.
    pub fn stop(&mut self) {
        self.cancel = None;
        self.req = 0;
    }
}

#[derive(Default)]
pub struct WatchTab {
    /// The running stream's id, 0 when stopped.
    pub req: u64,
    /// Dropping it stops the stream.
    pub cancel: Option<tokio::sync::oneshot::Sender<()>>,
    /// Kept as BSON bytes: each event can carry a whole document, up to WATCH_CAP of them.
    pub rows: Vec<db::Row>,
    pub err: Option<String>,
}

/// Columns of the Watch table.
pub static WATCH_FIELDS: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| ["at", "op", "_id", "changed", "doc"].map(String::from).to_vec());
