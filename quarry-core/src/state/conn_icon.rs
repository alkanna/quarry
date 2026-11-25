//! State behind src/app/conn_icon.rs, which draws it.

use std::sync::mpsc;

pub type Job = mpsc::Receiver<Result<Vec<u8>, String>>;

/// What Save writes.
pub enum Staged {
    /// Nothing picked yet.
    Keep,
    /// Back to the engine logo.
    Logo,
    /// A new icon, with its preview.
    /// Its preview texture is kept in the app, by connection id.
    Png(Vec<u8>),
}

/// The Connection icon dialog's working copy. Nothing is written until Save.
pub struct IconForm {
    pub conn: u64,
    pub url: String,
    pub staged: Staged,
    /// The file or download being read. It belongs to this dialog, so a late answer never lands on another connection.
    pub job: Option<Job>,
    pub error: String,
}

impl IconForm {
    pub fn new(conn: u64) -> Self {
        Self { conn, url: String::new(), staged: Staged::Keep, job: None, error: String::new() }
    }
}
