//! The trigger editor under the Triggers grid of a SQL tab: a new draft, Revert, Save and Drop. The drafts live in the tab's
//! structure state; src/app/sql/trig.rs draws them and their preview.

use crate::action::{Dialog, Refusal, Reply, UiFx};
use crate::core::Core;
use crate::db;
use crate::db::sql::ddl::{self, TrigJob};
use crate::state::sql::alter::BUSY_NOTE;
use crate::state::sql::trig::{blank, NEW};
use crate::state::Danger;

impl Core {
    /// Whether the connection of the open table lets writes through, which a view's own lock on rows does not decide: a view
    /// can hold INSTEAD OF triggers on PostgreSQL.
    fn trig_writable(&self) -> bool {
        self.t().and_then(|t| self.conn(t.sel.conn)).is_some_and(|c| c.writable)
    }

    pub fn trig_new(&mut self) {
        let pg = self.t().is_some_and(|t| self.is_pg(t.sel.conn));
        if let Some(s) = self.tabs.get_mut(self.cur).and_then(|t| t.sql.as_mut()) {
            s.st.trig_new = true;
            s.st.trig.entry(NEW.to_string()).or_insert_with(|| blank(pg));
        }
    }

    /// Drops the edits of draft `key`.
    pub fn trig_revert(&mut self, key: &str) {
        if let Some(s) = self.tabs.get_mut(self.cur).and_then(|t| t.sql.as_mut()) {
            s.st.trig.remove(key);
            if key == NEW {
                s.st.trig_new = false;
            }
        }
    }

    /// Saves draft `key`, built again from the live list so the order it keeps is the one the server has now.
    pub fn trig_save(&mut self, key: &str) -> Result<Reply, Refusal> {
        let Some(st) = self.structure().cloned() else { return Ok(Reply::Done) };
        let Some(def) = self.tabs.get(self.cur).and_then(|t| t.sql.as_ref()).and_then(|s| s.st.trig.get(key)).cloned() else { return Ok(Reply::Done) };
        let live = st.triggers.iter().find(|t| t.name == key);
        let job = ddl::save_job(&st.triggers, live, &def);
        self.run_trigger(job, if live.is_some() { "Saved the trigger" } else { "Created the trigger" })
    }

    /// Asks before dropping trigger `name`.
    pub fn trig_ask_drop(&mut self, name: String) -> Result<Reply, Refusal> {
        if !self.trig_writable() {
            return Err(self.refuse_ro_quiet(self.t().map_or(0, |t| t.sel.conn)));
        }
        let Some(sel) = self.t().map(|t| t.sel.clone()) else { return Ok(Reply::Done) };
        let mut title = format!("Drop the trigger {name}? Undo brings it back as it was.");
        // PostgreSQL drops the function with its trigger when nothing else runs it, or says why it stays; a trigger that cannot
        // be put back is refused here, before the person types anything.
        if self.is_pg(sel.conn) {
            let Some(t) = self.structure().and_then(|s| s.triggers.iter().find(|t| t.name == name)).cloned() else {
                self.warn(format!("The trigger {name} is not in the structure any more."));
                return Ok(Reply::Done);
            };
            match db::sql::pg::ddl_trig::preview(&self.sel_schema(&sel), &sel.coll, &TrigJob::Drop(t)) {
                Ok((steps, warnings)) => {
                    if steps.len() > 1 {
                        title.push_str(" Its function is dropped with it, since nothing else runs it.");
                    }
                    for w in warnings {
                        title.push(' ');
                        title.push_str(&w);
                    }
                }
                Err(why) => {
                    self.warn(why);
                    return Ok(Reply::Done);
                }
            }
        }
        self.ui_fx.push(UiFx::Open(Dialog::Typed { title, expect: name.clone(), action: Danger::DropTrigger(sel, name) }));
        Ok(Reply::Done)
    }

    /// Drops trigger `name`, once confirmed.
    pub fn drop_trigger(&mut self, name: &str) -> Result<Reply, Refusal> {
        let Some(t) = self.structure().and_then(|s| s.triggers.iter().find(|t| t.name == name)).cloned() else {
            self.warn(format!("The trigger {name} is not in the structure any more."));
            return Ok(Reply::Done);
        };
        self.run_trigger(TrigJob::Drop(t), "Dropped the trigger")
    }

    fn run_trigger(&mut self, job: TrigJob, done: &str) -> Result<Reply, Refusal> {
        if self.demo {
            self.ok("Offline demo: no database is connected.");
            return Ok(Reply::Done);
        }
        // A locked connection asks first and runs the change when writes are allowed, as Apply does for the structure.
        if !self.trig_writable() {
            return Err(self.refuse_ro_quiet(self.t().map_or(0, |t| t.sel.conn)));
        }
        if self.applying() {
            return Err(self.refuse(BUSY_NOTE));
        }
        let Some(sel) = self.t().map(|t| t.sel.clone()) else { return Ok(Reply::Done) };
        let Some((db, schema)) = self.sql_target(&sel) else { return Err(self.refuse_offline(sel.conn)) };
        let (.., m) = self.alter_env();
        if let Some(s) = self.tabs[self.cur].sql.as_mut() {
            s.st.applying = true;
            // The draft of a trigger that is being made is cleared once the structure lists it under this name.
            s.st.trig_made = if let TrigJob::Create(c) = &job { Some(c.def.name.clone()) } else { None };
            s.st.trig_saving = if let TrigJob::Edit { was, .. } = &job { Some(was.name.clone()) } else { None };
        }
        self.worker.sql_trigger(sel.conn, db, schema, sel.coll, job, m, !self.trig_writable(), done.to_string());
        Ok(Reply::Done)
    }
}
