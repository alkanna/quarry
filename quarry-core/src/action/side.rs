//! The sidebar tree: which databases are open and folded, the lists it loads, and the MongoDB database and collection
//! actions of its menus (create, rename, duplicate, drop, statistics). src/app/sidebar.rs draws it; the dialogs those
//! actions open stay in the app, which runs the confirmed change through here.

use super::{Dialog, Reply, Refusal, RefusalKind, UiFx};
use crate::core::Core;
use crate::db;
use crate::db::collopts::{self, CollForm};
use crate::state::mongo_create::MongoNewColl;
use crate::state::*;
use mongodb::bson::{doc, Document};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SideA {
    /// A click on a database row: opens it, or closes it when open.
    ToggleDb { conn: u64, db: String },
    /// A recent database: opened and scrolled to, never closed.
    OpenDb { conn: u64, db: String },
    /// The TABLES or VIEWS group of a database folded or unfolded.
    Fold { conn: u64, db: String, views: bool },
    /// The tree scrolled to the row it was asked to reveal.
    Revealed,
    /// A SQL database about to list its tables, on opening it or on Retry after a failed list.
    Asked { conn: u64, db: String, retry: bool },
    /// The PostgreSQL schema switcher of a database.
    Schema { conn: u64, db: String, schema: String },
    /// New database on MongoDB, which opens its dialog once writes are allowed.
    MongoNewDb { conn: u64 },
    /// New collection on MongoDB, which opens its dialog once writes are allowed.
    MongoNewColl { conn: u64, db: String },
    /// The New database or New collection dialog's Create.
    MongoCreate { conn: u64, db: String, name: String, form: CollForm, ver: collopts::Ver },
    /// The New view dialog's Create view: `view` is the source collection and the parsed pipeline.
    MongoCreateView { conn: u64, db: String, name: String, view: Option<(String, Vec<Document>)> },
    Rename { conn: u64, db: String, coll: String, to: String },
    Duplicate { conn: u64, db: String, coll: String, to: String },
    /// Drop database, once its name was typed.
    DropDb { conn: u64, db: String },
    /// Drop collection, once its name was typed.
    DropColl { conn: u64, db: String, coll: String },
    Stats { conn: u64, db: String, coll: String },
    ViewDef { conn: u64, db: String, coll: String },
}

impl Core {
    pub(super) fn side(&mut self, a: SideA) -> Result<Reply, Refusal> {
        match a {
            SideA::ToggleDb { conn, db } => {
                if !self.open_dbs.remove(&(conn, db.clone())) {
                    self.open_dbs.insert((conn, db));
                }
            }
            SideA::OpenDb { conn, db } => {
                self.reveal = Some((conn, db.clone(), None));
                self.open_dbs.insert((conn, db));
            }
            SideA::Fold { conn, db, views } => {
                let k = (conn, db, views);
                if !self.side_closed.remove(&k) {
                    self.side_closed.insert(k);
                }
            }
            SideA::Revealed => self.reveal = None,
            SideA::Asked { conn, db, retry } => {
                if retry {
                    let pre = ns_key(&db, "x");
                    let pre = &pre[..pre.len() - 1];
                    self.side_err.retain(|k, _| k.0 != conn || (k.1 != db && !k.1.starts_with(pre)));
                }
                self.sql_asked.insert((conn, db));
            }
            SideA::Schema { conn, db, schema } => self.pg_set_schema(conn, &db, schema),
            SideA::MongoNewDb { conn } => self.mongo_new_coll(conn, String::new(), true)?,
            SideA::MongoNewColl { conn, db } => self.mongo_new_coll(conn, db, false)?,
            SideA::MongoCreate { conn, db, name, form, ver } => {
                let cmd = match collopts::create_command(&name, &form, ver) {
                    Ok(c) => c,
                    Err(e) => {
                        self.err(e.clone());
                        return Err(Refusal { kind: RefusalKind::Invalid, msg: e });
                    }
                };
                // Writes can be locked again while the dialog is open; the unlock then runs the same create.
                if !self.conn_writable(conn) {
                    return Err(self.refuse_ro_quiet(conn));
                }
                let Some(client) = self.conn(conn).and_then(|c| c.client.clone()) else { return Err(self.offline_here()) };
                self.open_after = Some((conn, db.clone(), name.clone()));
                self.worker.db_change(conn, client, db.clone(), cmd, format!("Created {db}.{name}"));
            }
            SideA::MongoCreateView { conn, db, name, view } => {
                let Some(client) = self.conn(conn).and_then(|c| c.client.clone()) else { return Err(self.offline_here()) };
                let cmd = match view {
                    Some((source, pipeline)) => doc! { "create": &name, "viewOn": source, "pipeline": pipeline },
                    None => doc! { "create": &name },
                };
                self.worker.db_change(conn, client, db, cmd, format!("Created {name}"));
            }
            SideA::Rename { conn, db, coll, to } => {
                if let Some(client) = self.conn(conn).and_then(|c| c.client.clone()) {
                    let (from, to_ns) = (format!("{db}.{coll}"), format!("{db}.{to}"));
                    self.ui_fx.push(UiFx::CloseTabs { conn, db: db.clone(), coll: Some(coll.clone()) });
                    self.worker.db_change(conn, client, db, doc! { "renameCollection": from, "to": to_ns }, format!("Renamed {coll} to {to}"));
                }
            }
            SideA::Duplicate { conn, db, coll, to } => match self.conn(conn).and_then(|c| c.client.clone()) {
                Some(client) => {
                    // The sidebar shows it with a spinner until the copy ends.
                    self.copying.insert((conn, db.clone(), to.clone()));
                    self.worker.duplicate_coll(conn, client, db, coll, to);
                }
                None => return Err(self.offline_here()),
            },
            SideA::DropDb { conn, db } => {
                let Some(client) = self.writable_client(conn) else { return Ok(Reply::Done) };
                self.ui_fx.push(UiFx::CloseTabs { conn, db: db.clone(), coll: None });
                self.worker.db_change(conn, client, db.clone(), doc! { "dropDatabase": 1 }, format!("Dropped database {db}"));
                if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
                    c.dbs.retain(|d| d.0 != db);
                }
            }
            SideA::DropColl { conn, db, coll } => {
                let Some(client) = self.writable_client(conn) else { return Ok(Reply::Done) };
                self.ui_fx.push(UiFx::CloseTabs { conn, db: db.clone(), coll: Some(coll.clone()) });
                self.worker.db_change(conn, client, db, doc! { "drop": &coll }, format!("Dropped {coll}"));
            }
            SideA::Stats { conn, db, coll } => {
                let Some(client) = self.conn(conn).and_then(|c| c.client.clone()) else { return Err(self.offline_here()) };
                self.worker.coll_stats(client, db, coll, "stats".into());
            }
            SideA::ViewDef { conn, db, coll } => {
                let Some(client) = self.conn(conn).and_then(|c| c.client.clone()) else { return Err(self.offline_here()) };
                self.worker.command(client, db, doc! { "listCollections": 1, "filter": { "name": coll } }, "viewdef");
            }
        }
        Ok(Reply::Done)
    }

    /// The New database (`new_db`) or New collection dialog, once writes are allowed; a locked connection asks to
    /// unlock and opens it after.
    fn mongo_new_coll(&mut self, conn: u64, db: String, new_db: bool) -> Result<(), Refusal> {
        if !self.conn_writable(conn) {
            return Err(self.refuse_ro_quiet(conn));
        }
        self.ui_fx.push(UiFx::Open(Dialog::MongoNewColl(Box::new(MongoNewColl { conn, db, new_db, name: String::new(), form: CollForm::default() }))));
        Ok(())
    }

    /// The session of `conn` for a confirmed drop. Without writes or a session the Unlock dialog opens with nothing to
    /// run after it, so the name is typed again once unlocked.
    fn writable_client(&mut self, conn: u64) -> Option<mongodb::Client> {
        let client = self.conn(conn).filter(|c| c.writable).and_then(|c| c.client.clone());
        if client.is_none() {
            self.ui_fx.push(UiFx::Open(Dialog::Unlock(conn)));
        }
        client
    }

    /// Not connected, said about the current tab's connection as these menus always did.
    fn offline_here(&mut self) -> Refusal {
        self.warn_offline();
        Refusal { kind: RefusalKind::Offline, msg: "Not connected. Connect to this server first.".into() }
    }

    /// The schemas of a database arrived. The pick is kept while its schema still exists, else it falls back to the
    /// default, and the tables of the pick load.
    pub(super) fn pg_schemas_done(&mut self, conn: u64, database: String, res: db::Res<Vec<db::sql::PgSchema>>) {
        let list = match res {
            Ok(l) => l,
            Err(e) => {
                self.side_err.insert((conn, database.clone()), e.clone());
                return self.err_quiet(e, Some(conn), Some(Sel { conn, db: database.clone(), ..Default::default() }), format!("List schemas · {database}"));
            }
        };
        self.side_err.remove(&(conn, database.clone()));
        let user: Vec<String> = list.iter().filter(|s| !s.system).map(|s| s.name.clone()).collect();
        let listed = |name: &String| list.iter().any(|s| &s.name == name);
        let keep = self.conn(conn).and_then(|c| c.pg.get(&database)).map(|p| p.picked.clone()).filter(|p| listed(p));
        // A tab restored from the last session names its schema before any pick exists; the tree follows it.
        let of_tab = self.t().filter(|t| t.sel.conn == conn && t.sel.db == database).map(|t| t.sel.schema.clone()).filter(|s| !s.is_empty() && listed(s));
        let picked = keep.or(of_tab).unwrap_or_else(|| default_schema(&user).to_string());
        if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
            c.set_schemas(&database, list, picked.clone());
        }
        // Query tabs opened or switched to this database before its schemas were known take the schema now.
        for t in self.tabs.iter_mut().filter(|t| t.sel.conn == conn) {
            if t.sel.db == database && t.sel.schema.is_empty() && t.sel.coll.is_empty() && t.sql.is_some() {
                t.sel.schema = picked.clone();
            }
            if let Some(s) = t.sql.as_mut().filter(|s| s.run_db == database && s.run_schema.trim().is_empty()) {
                s.run_schema = picked.clone();
            }
        }
        self.pg_load_tables(conn, &database, &picked);
    }

    /// The tables of one schema arrived.
    pub(super) fn pg_tables_done(&mut self, conn: u64, database: String, schema: String, res: db::Res<(Vec<String>, Vec<String>)>) {
        match res {
            Ok((names, views)) => {
                self.side_err.remove(&(conn, ns_key(&database, &schema)));
                self.views.set(conn, &ns_key(&database, &schema), views);
                if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
                    c.tables.entry(database).or_default().insert(schema, names);
                }
            }
            Err(e) => {
                // A schema that failed to list may be asked again by picking it once more.
                self.pg_tables_asked.forget(conn, Some(&database), Some(&schema));
                self.side_err.insert((conn, ns_key(&database, &schema)), e.clone());
                self.err_quiet(e, Some(conn), Some(Sel { conn, db: database.clone(), schema: schema.clone(), ..Default::default() }), format!("List tables · {database}.{schema}"))
            }
        }
    }
}
