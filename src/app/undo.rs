//! The undo list of recent writes.

use super::*;

impl App {
    pub(super) fn push_undo(&mut self, a: UndoAction) {
        self.undo.insert(0, a);
        self.undo.retain(|a| a.at.elapsed().as_secs() <= db::UNDO_MAX_AGE_S);
        self.undo.truncate(db::UNDO_MAX);
    }

    pub(super) fn apply_undo(&mut self, i: usize) {
        let a = &self.undo[i];
        let Some(c) = self.conn(a.conn) else { return };
        if !c.writable {
            self.err("Undo is a write: unlock writes on that connection first.");
            return;
        }
        let Some(client) = c.client.clone() else {
            self.err("That connection is closed.");
            return;
        };
        let a = self.undo.remove(i);
        let t = Target { client, db: a.db, coll: a.coll, timeout: self.timeout() };
        match a.kind {
            UndoKind::Insert { id } => self.worker.delete_by_id(t, id),
            UndoKind::InsertMany { ids } => self.worker.delete_ids(t, ids),
            UndoKind::Delete { old } => self.worker.insert(a.conn, t, old, None),
            UndoKind::DeleteMany { old } => self.worker.import(a.conn, t, old, None),
            UndoKind::Update { old, new } => self.worker.replace(a.conn, t, new, old, false),
        }
    }
}
