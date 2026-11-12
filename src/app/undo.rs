//! The undo list of recent writes.

use super::*;

impl App {
    pub(super) fn push_undo(&mut self, a: UndoAction) {
        self.undo.insert(0, a);
        trim_undo(&mut self.undo, std::time::Instant::now());
    }

    /// Forgets the undo entries of what a drop just removed. A database or table made again under the name would otherwise get them.
    /// `database` is the PostgreSQL database (empty on MySQL, where `schema` is the database); `schema` and `table` narrow it, empty for the whole of it.
    pub(super) fn purge_undo(&mut self, conn: u64, database: &str, schema: &str, table: &str) {
        self.undo.retain(|a| !undo_is_under(a, conn, database, schema, table));
    }

    pub(super) fn apply_undo(&mut self, i: usize) {
        self.apply_undo_as(i, false);
    }

    /// `confirmed` once a structure undo that loses data passed its typed confirmation.
    pub(super) fn apply_undo_as(&mut self, i: usize, confirmed: bool) {
        let a = &self.undo[i];
        let Some(c) = self.conn(a.conn) else { return };
        let writable = c.writable;
        if !writable {
            self.warn("Undo is a write. Unlock writes on that connection first.");
            return;
        }
        if !confirmed && self.alter_undo_gate(i) {
            return;
        }
        let Some(c) = self.conn(self.undo[i].conn) else { return };
        let a = &self.undo[i];
        // SQL actions carry their own columns; the pool is read-only, so the worker switches a connection of its own.
        if let (Some(sql), UndoKind::Sql { database, .. }) = (c.sql.as_ref(), &a.kind) {
            // A PostgreSQL write ran in one database, and the undo runs on that one's pool.
            let sql = sql.at(database);
            let a = self.undo.remove(i);
            let pk = match &a.kind {
                UndoKind::Sql { undo, .. } => undo.pk_names(),
                _ => Vec::new(),
            };
            // The worker hands the action back if the undo fails, so it is not lost.
            self.worker.sql_undo(sql, a, pk, !writable);
            return;
        }
        let Some(client) = c.client.clone() else {
            self.warn("That connection is closed.");
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

/// Whether the action acts on the dropped database, schema or table (see `purge_undo`).
pub(super) fn undo_is_under(a: &UndoAction, conn: u64, database: &str, schema: &str, table: &str) -> bool {
    let UndoKind::Sql { database: d, undo, .. } = &a.kind else { return false };
    // An account is not under any schema, whatever its label looks like.
    if a.conn != conn || matches!(undo, db::sql::write::SqlUndo::Account(_) | db::sql::write::SqlUndo::PgRoles(_)) {
        return false;
    }
    if schema.is_empty() && table.is_empty() && !database.is_empty() {
        // A created database's entry sits on the maintenance database and names the new one as its schema.
        return d == database || (matches!(undo, db::sql::write::SqlUndo::CreatedDb { .. }) && a.db == database);
    }
    d == database && a.db == schema && (table.is_empty() || a.coll == table)
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

    fn sql_action(conn: u64, database: &str, schema: &str, table: &str) -> UndoAction {
        let kind = UndoKind::Sql { database: database.into(), cols: Vec::new(), undo: db::sql::write::SqlUndo::Inserted(Vec::new()), ids: Default::default() };
        UndoAction { at: Instant::now(), conn, db: schema.into(), coll: table.into(), kind }
    }

    #[test]
    fn a_drop_forgets_the_undo_entries_under_it_and_no_others() {
        let all = || vec![sql_action(1, "shop", "public", "orders"), sql_action(1, "shop", "public", "items"), sql_action(1, "shop", "ext", "orders"), sql_action(1, "other", "public", "orders"), sql_action(2, "shop", "public", "orders")];
        let left = |database: &str, schema: &str, table: &str| all().iter().filter(|a| !undo_is_under(a, 1, database, schema, table)).count();
        assert_eq!(left("shop", "public", "orders"), 4, "a table");
        assert_eq!(left("shop", "public", ""), 3, "a schema takes its tables");
        assert_eq!(left("shop", "", ""), 2, "a database takes all of its own");
        // MySQL has no separate database name: the schema is the database.
        let my = vec![sql_action(1, "", "shop", "orders"), sql_action(1, "", "shop", "items"), sql_action(1, "", "other", "orders")];
        assert_eq!(my.iter().filter(|a| !undo_is_under(a, 1, "", "shop", "")).count(), 1);
        assert_eq!(my.iter().filter(|a| !undo_is_under(a, 1, "", "shop", "orders")).count(), 2);
    }
}
