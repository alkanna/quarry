//! Runs the destructive actions once their confirmation dialog is accepted.

use super::*;

impl App {
    pub(super) fn danger(&mut self, action: Danger) {
        if let Danger::RemoveConnection(id) = action {
            // Demo ids overlap real ones, so removing a demo connection must not touch the Keychain.
            if !self.ephemeral {
                if let Some(k) = keychain(id) {
                    let _ = k.delete_credential();
                }
                // ponytail: best effort, a failure only leaves an orphaned Keychain entry behind.
                for what in ["ssh-password", "ssh-passphrase", "proxy-password"] {
                    let _ = set_secret(id, what, "");
                }
            }
            self.conns.retain(|c| c.meta.id != id);
            self.remove_tabs(|t| t.sel.conn == id);
            if self.active == Some(id) {
                self.active = self.conns.first().map(|c| c.meta.id);
            }
            self.save_conns();
            return self.ok("Connection removed");
        }
        if let Danger::DeleteRows(sel, rows) = action {
            return self.on_tab(&sel, |a| a.sql_delete(rows));
        }
        if let Danger::ApplyAlter(sel) = action {
            return self.on_tab(&sel, |a| a.run_alter());
        }
        if let Danger::UndoAlter(at) = action {
            // Found by time, since the list may have moved while the dialog was open.
            return match self.undo.iter().position(|a| a.at == at) {
                Some(i) => self.apply_undo_as(i, true),
                None => self.warn("That change is no longer in Recent changes."),
            };
        }
        if let Danger::SqlDrop { conn, database, schema, table, kind, opt } = action {
            return self.sql_drop_run(conn, database, schema, table, kind, opt);
        }
        if let Danger::SqlDropUser { conn, acct } = action {
            return self.sql_user_drop_run(conn, acct);
        }
        if let Danger::DropTrigger(sel, name) = action {
            return self.on_tab(&sel, |a| a.drop_trigger(name));
        }
        let conn_specific = match &action {
            Danger::DropDatabase { conn, .. } | Danger::DropCollAt { conn, .. } | Danger::DropUser { conn, .. } | Danger::Sync { conn } | Danger::Restore { conn, .. } => Some(*conn),
            _ => None,
        };
        if let Some(conn) = conn_specific {
            let Some(client) = self.conn(conn).filter(|c| c.writable).and_then(|c| c.client.clone()) else {
                return self.modal = Some(Modal::Unlock(conn, None));
            };
            match action {
                Danger::DropDatabase { db, .. } => {
                    self.remove_tabs(|t| t.sel.conn == conn && t.sel.db == db);
                    self.worker.db_change(conn, client, db.clone(), doc! { "dropDatabase": 1 }, format!("Dropped database {db}"));
                    if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
                        c.dbs.retain(|d| d.0 != db);
                    }
                    self.dropping_db = Some((conn, db));
                }
                Danger::DropCollAt { db, coll, .. } => {
                    self.remove_tabs(|t| t.sel.conn == conn && t.sel.db == db && t.sel.coll == coll);
                    self.worker.db_change(conn, client, db, doc! { "drop": &coll }, format!("Dropped {coll}"));
                }
                Danger::DropUser { db, user, .. } => self.worker.db_change(conn, client, db, doc! { "dropUser": &user }, format!("Dropped user {user}")),
                Danger::Sync { .. } => {
                    let timeout = self.timeout();
                    self.drop_agg_row();
                    let c = &mut self.tabs[self.cur].compare;
                    let r = c.result.take();
                    c.set_result(None);
                    let Some(r) = r else { return };
                    let insert = if c.insert { r.only_src } else { Vec::new() };
                    let replace = if c.replace { r.changed } else { Vec::new() };
                    let delete = if c.delete { r.only_dst.iter().filter_map(|d| d.get("_id").cloned()).collect() } else { Vec::new() };
                    self.worker.sync(Target { client, db: c.db.clone(), coll: c.coll.clone(), timeout }, insert, replace, delete);
                    self.ok("Syncing… Run Compare again afterwards to check.");
                }
                Danger::Restore { args, .. } => self.start_tool(true, "Restore".into(), args),
                _ => unreachable!(),
            }
            return;
        }
        let Some((conn, target)) = self.writable_target() else { return };
        match action {
            Danger::DeleteMatching(s) => self.worker.delete_many(target, s),
            Danger::DropIndex(name) => self.worker.drop_index(target, name),
            Danger::Validator(cmd) => self.worker.db_change(conn, target.client, target.db, cmd, "Validator updated".into()),
            Danger::DropCollection => {
                // The sidebar's drop also reloads the collection list, which keeps a refused drop visible.
                let Sel { conn, db, coll, .. } = self.t().unwrap().sel.clone();
                self.danger(Danger::DropCollAt { conn, db, coll });
            }
            Danger::RenameMany { scope, from, to } => self.worker.rename_many(target, scope, from, to),
            Danger::UpdateMany { scope, stages } => self.worker.update_many(target, scope, stages),
            Danger::Upsert { key } => {
                let conn = self.t().unwrap().sel.conn;
                if let Some(Ok(docs)) = self.tabs[self.cur].import.parsed.clone() {
                    self.worker.import(conn, target, docs, Some(key));
                }
            }
            Danger::DeleteRows(..) | Danger::ApplyAlter(_) | Danger::UndoAlter(_) | Danger::DropTrigger(..) | Danger::SqlDrop { .. } | Danger::SqlDropUser { .. } | Danger::RemoveConnection(_) | Danger::DropDatabase { .. } | Danger::DropCollAt { .. } | Danger::DropUser { .. } | Danger::Sync { .. } | Danger::Restore { .. } => unreachable!(),
        }
    }
}
