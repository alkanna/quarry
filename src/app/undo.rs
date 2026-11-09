//! The undo list of recent writes.

use super::*;

impl App {
    pub(super) fn push_undo(&mut self, a: UndoAction) {
        self.undo.insert(0, a);
        trim_undo(&mut self.undo, std::time::Instant::now());
    }

    pub(super) fn apply_undo(&mut self, i: usize) {
        let a = &self.undo[i];
        let Some(c) = self.conn(a.conn) else { return };
        if !c.writable {
            self.err("Undo is a write: unlock writes on that connection first.");
            return;
        }
        // SQL actions carry their own columns; the pool is read-only, so the worker switches a connection of its own.
        if let (Some(sql), UndoKind::Sql { .. }) = (c.sql.clone(), &a.kind) {
            let a = self.undo.remove(i);
            if let UndoKind::Sql { cols, undo } = a.kind {
                let pk = undo.pk_names();
                self.worker.sql_undo(sql, a.db, a.coll, cols, pk, undo, false);
            }
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
            UndoKind::Sql { .. } => {}
        }
    }
}

/// Drops actions older than a day and keeps the newest `UNDO_MAX`, like the original.
pub(super) fn trim_undo(undo: &mut Vec<UndoAction>, now: std::time::Instant) {
    undo.retain(|a| now.saturating_duration_since(a.at).as_secs() <= db::UNDO_MAX_AGE_S);
    undo.truncate(db::UNDO_MAX);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn action(at: Instant) -> UndoAction {
        UndoAction { at, conn: 1, db: "shop".into(), coll: "c".into(), kind: UndoKind::Insert { id: Bson::Int32(1) } }
    }

    #[test]
    fn undo_list_is_capped_by_size_and_age() {
        let t0 = Instant::now();
        let mut undo: Vec<UndoAction> = (0..db::UNDO_MAX + 5).map(|_| action(t0)).collect();
        trim_undo(&mut undo, t0);
        assert_eq!(undo.len(), db::UNDO_MAX);
        let later = t0 + Duration::from_secs(db::UNDO_MAX_AGE_S + 1);
        undo.insert(0, action(later));
        trim_undo(&mut undo, later);
        assert_eq!(undo.len(), 1, "only the action from within the last day stays");
    }
}
