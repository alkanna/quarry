//! Runs the destructive actions once their confirmation dialog is accepted.

use super::*;
use quarry_core::action::{ConfirmA, ToolsA};

impl App {
    /// Ties a tab-scoped action to the current tab, so a tab switch before Confirm cannot redirect it.
    pub(super) fn pinned(&self, d: Danger) -> Danger {
        match self.t() {
            Some(t) => Danger::At(t.sel.clone(), Box::new(d)),
            None => d,
        }
    }

    pub(super) fn danger(&mut self, action: Danger) {
        if let Danger::At(sel, d) = action {
            return self.on_tab(&sel, |a| a.danger(*d));
        }
        if let Danger::RemoveConnection(conn) = action {
            return self.exec(Action::Conn(ConnA::Remove { conn }));
        }
        if let Danger::DeleteRows(sel, rows) = action {
            return self.on_tab(&sel, |a| a.sql_delete(rows));
        }
        if let Danger::ApplyAlter(sel) = action {
            return self.on_tab(&sel, |a| a.run_alter());
        }
        if let Danger::UndoAlter(at) = action {
            // Found by time, since the list may have moved while the dialog was open.
            return match self.core.undo.iter().position(|a| a.at == at) {
                Some(i) => self.apply_undo_as(i, true),
                None => self.warn("That change is no longer in Recent changes."),
            };
        }
        if let Danger::SqlDrop { conn, database, schema, table, kind, opt } = action {
            return self.exec(Action::Ddl(quarry_core::action::DdlA::DropRun { conn, database, schema, table, kind, opt }));
        }
        if let Danger::SqlDropUser { conn, acct } = action {
            return self.exec(quarry_core::action::Action::Users(quarry_core::action::UsersA::Sql(quarry_core::action::SqlUsersA::DropRun { conn, user: acct.user, host: acct.host, is_role: acct.is_role })));
        }
        if let Danger::DropUser { conn, db, user } = action {
            return self.exec(quarry_core::action::Action::Users(quarry_core::action::UsersA::Mongo(quarry_core::action::MongoUsersA::DropRun { conn, user, db })));
        }
        if let Danger::DropTrigger(sel, name) = action {
            return self.on_tab(&sel, |a| a.drop_trigger(name));
        }
        if let Danger::DropDatabase { conn, db } = action {
            return self.exec(Action::Side(SideA::DropDb { conn, db }));
        }
        if let Danger::DropCollAt { conn, db, coll } = action {
            return self.exec(Action::Side(SideA::DropColl { conn, db, coll }));
        }
        if let Danger::Sync { conn } = action {
            return self.exec(Action::Confirm(ConfirmA::Sync { conn }));
        }
        if let Danger::Restore { conn, archive, from_db, coll, to_db, drop, gzip } = action {
            return self.exec(Action::Tools(ToolsA::Restore { conn, archive, from_db, coll, to_db, drop, gzip }));
        }
        let Some(tab) = self.t().map(|t| t.uid) else { return };
        let a = match action {
            Danger::DeleteMatching(scope) => ConfirmA::DeleteMatching { tab, scope },
            Danger::DropIndex(name) => ConfirmA::DropIndex { tab, name },
            Danger::Validator(cmd) => ConfirmA::Validator { tab, cmd },
            Danger::RenameMany { scope, from, to } => ConfirmA::RenameMany { tab, scope, from, to },
            Danger::UpdateMany { scope, stages } => ConfirmA::UpdateMany { tab, scope, stages },
            Danger::Upsert { key } => ConfirmA::Upsert { tab, key },
            _ => unreachable!(),
        };
        self.exec(Action::Confirm(a));
    }
}
