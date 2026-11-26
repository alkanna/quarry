//! State behind src/app/errors.rs, which draws it.

use crate::db::{Explained, Fix};
use std::time::Instant;
use crate::state::{Sel, View};

/// One error of this session, with what it was about.
#[derive(Clone)]
pub struct ErrEntry {
    pub id: u64,
    pub at: std::time::SystemTime,
    pub conn: Option<u64>,
    pub conn_name: String,
    pub user: String,
    pub sel: Option<Sel>,
    /// "Documents · orders"
    pub place: String,
    pub raw: String,
    pub ex: Explained,
    /// A connection that could not open: Retry reconnects, which a toast can do for it.
    pub connect: bool,
}

/// A read for one view that failed: the view shows the panel instead of its results.
pub struct Failed {
    pub view: View,
    pub e: ErrEntry,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Tone {
    Ok,
    Warn,
    Err,
}

#[derive(Clone)]
pub enum Act {
    Fix(Fix),
    Undo(Instant),
    /// Puts back what `App::restores` holds under this id: a deleted group, scripts or a file.
    Restore(u64),
    /// Opens the Edit group dialog on this group, for the Rename of a group just made by a drop.
    EditGroup(u64),
    Unlock(u64),
    Connect(u64),
    Settings,
    Details,
    Copy,
    ShowDup,
    /// Drops the library script waiting for its connection.
    CancelOpen,
}

pub struct Toast {
    /// Buttons can add or drop toasts while they run, so a click finds its toast by id, not position.
    pub id: u64,
    pub tone: Tone,
    pub title: String,
    pub body: String,
    pub acts: Vec<(String, Act)>,
    pub at: Instant,
    pub err: Option<ErrEntry>,
}
