//! open_shell: an agent's script put in a Shell tab for the user to review and run. Nothing reaches a server and only
//! the tab id goes back, so neither the approval nor the data rules apply. Every connection must still be shared, with
//! scripts not set to Never, and attachments always start read-only.

use super::exec::{Reply, SCHEMA_PG_ONLY, not_open};
use super::*;
use crate::agents::audit::Summary;
use crate::agents::config::{Mode, OpClass};
use crate::agents::tools::{Call, ConnRef, ShellAttach};
use crate::app::shell_attach::{Attached, default_name, name_problem, pg_schema};

type Refusal = (&'static str, String);

fn bad(why: String) -> Refusal {
    ("bad_args", why)
}

impl App {
    pub(super) fn agent_open_shell(&mut self, ctx: &egui::Context, mut act: Activity, call: Call, args: &Value, reply: Reply) {
        let Call::OpenShell { conn, db, schema, script, attach, tab } = call else { return };
        act.ns = db.clone();
        act.args = self.agent_args(None, args, None);
        // Keys the user is typing would land in the new tab: a Run shortcut there would run the agent's script unread.
        let typing = ctx.text_edit_focused();
        match self.agent_fill_shell(&conn, db, schema, script, &attach, tab, typing) {
            Ok((id, uid, shown, names)) => {
                act.conn = Some(id);
                act.outcome = Outcome::Allowed;
                act.summary = format!("Shell tab {uid}{}", if shown { "" } else { " in the background" });
                let note = if shown { "The tab is shown in Quarry. The user runs it; the result stays in Quarry." } else { "The user was busy, so the tab opened behind the current one. The user runs it; the result stays in Quarry." };
                let _ = reply.send(Ok(json!({ "tab": uid, "shown": shown, "attached": names, "note": note })));
                self.agent_audit(&act, "allowed", Summary::default());
                self.agent_push(act);
                self.ok(if shown { "An agent put a script in this Shell tab. Review it before you run it." } else { "An agent put a script in a Shell tab behind this one." });
            }
            Err((code, why)) => {
                act.conn = self.agent_conn(&conn).ok();
                self.agent_refuse(act, reply, code, why)
            }
        }
    }

    /// A connection the script may name: shared, and scripts not Never there.
    fn agent_shell_conn(&self, r: &ConnRef) -> Result<u64, Refusal> {
        let id = self.agent_conn(r).map_err(|e| ("not_shared", e))?;
        if self.core.agents.cfg.conn(id).is_some_and(|p| p.mode(OpClass::RunScripts) == Mode::Never) {
            let name = self.conn(id).map(|c| c.meta.name.clone()).unwrap_or_default();
            return Err(("refused", format!("Run scripts is set to Never for agents on {name}.")));
        }
        Ok(id)
    }

    /// Refused unless the connection's scope shares `db` (on PostgreSQL, `schema` in database `db`), as the other tools are.
    fn agent_shell_shared(&self, id: u64, db: &str, schema: &str) -> Result<(), Refusal> {
        let name = self.conn(id).map(|c| c.meta.name.clone()).unwrap_or_default();
        let pg = self.is_pg(id);
        let raw = self.core.agents.cfg.conn(id).map(|p| p.scope.mode).unwrap_or_default();
        // Two-part patterns mean the database the connection opens, which only its session tells.
        if pg && raw != crate::agents::config::ScopeMode::All && !self.conn(id).is_some_and(|c| c.sql.is_some()) {
            return Err(("not_open", format!("Open {name} in Quarry first, so Quarry can check which of its databases are shared.")));
        }
        let scope = self.agent_policy(id, if pg { db } else { "" }).map(|p| p.scope).unwrap_or_default();
        let (space, what) = if pg { (schema, format!("{db}.{schema}")) } else { (db, db.to_string()) };
        match crate::agents::policy::visible(&scope, space, None) {
            Ok(true) => Ok(()),
            Ok(false) => Err(("refused", format!("{what} is not shared with agents on {name}."))),
            Err(e) => Err(("refused", e)),
        }
    }

    /// The schema of a PostgreSQL connection, public when none is given; refused on other engines.
    fn agent_shell_schema(&self, id: u64, schema: Option<String>) -> Result<String, Refusal> {
        match (self.is_pg(id), schema) {
            (true, s) => Ok(pg_schema(&s.unwrap_or_default()).to_string()),
            (false, None) => Ok(String::new()),
            (false, Some(_)) => Err(bad(SCHEMA_PG_ONLY.into())),
        }
    }

    /// Checks everything first, then opens or edits the tab. Answers (connection, tab uid, shown, attachment names).
    #[allow(clippy::too_many_arguments)]
    fn agent_fill_shell(&mut self, conn: &ConnRef, db: String, schema: Option<String>, script: String, attach: &[ShellAttach], tab: Option<u64>, typing: bool) -> Result<(u64, u64, bool, Vec<String>), Refusal> {
        let id = self.agent_shell_conn(conn)?;
        let c = self.conn(id).expect("agent_conn found it");
        if !c.status.is_open() {
            return Err(("not_open", not_open(&c.meta.name)));
        }
        let engine = c.meta.engine;
        let sel = Sel { conn: id, schema: self.agent_shell_schema(id, schema)?, db, coll: String::new() };
        self.agent_shell_shared(id, &sel.db, &sel.schema)?;
        let at = match tab {
            None => None,
            Some(uid) => {
                let Some(i) = self.core.tabs.iter().position(|t| t.uid == uid) else { return Err(("gone", format!("Tab {uid} is closed. Leave out `tab` to open a new one."))) };
                let t = &self.core.tabs[i];
                match &t.shell.agent_code {
                    None => return Err(("refused", format!("Tab {uid} was not opened by open_shell, so Quarry leaves it alone. Leave out `tab` to open a new one."))),
                    Some(code) if *code != t.shell.code => return Err(("refused", format!("The user changed the script in tab {uid}, so Quarry leaves it alone. Leave out `tab` to open a new one."))),
                    _ => {}
                }
                if t.shell.cancel.is_some() {
                    return Err(("busy", format!("The script in tab {uid} is running. Try again once it ends.")));
                }
                // The Run confirm runs the tab's code as it is when confirmed, so a change under it would run unapproved code.
                if self.modal.is_some() {
                    return Err(("busy", "A dialog is open in Quarry. Try again once the user closes it.".into()));
                }
                if t.sel.conn != sel.conn || t.sel.db != sel.db || t.sel.schema != sel.schema {
                    return Err(bad(format!("Tab {uid} is on another connection, database or schema. Leave out `tab` to open a new one.")));
                }
                Some(i)
            }
        };
        let mut taken = self.globals_taken(&sel, engine.is_sql(), &[]);
        let mut attached = Vec::new();
        for a in attach {
            let aid = self.agent_shell_conn(&a.conn)?;
            let schema = self.agent_shell_schema(aid, a.schema.clone())?;
            self.agent_shell_shared(aid, &a.db, &schema)?;
            let name = a.name.clone().unwrap_or_else(|| default_name(&a.db));
            if let Some(p) = name_problem(&name, &taken) {
                return Err(bad(format!("Attachment {name}: {p}")));
            }
            taken.push((name.clone(), "used by another attached connection".into()));
            // Only the user turns writes on, in the tab.
            attached.push(Attached { conn: aid, db: a.db.clone(), name, allow_writes: false, schema, tables: Vec::new() });
        }
        // A tab switch closes open cell and field editors, and under a dialog could let a confirm run the wrong script.
        let shown = !typing && self.modal.is_none() && self.core.json_edit.is_none() && self.core.sql_cell.is_none() && self.core.inline.is_none() && self.core.new_field.is_none();
        let i = match at {
            Some(i) => i,
            None if shown => {
                self.open_tab(sel, Open::NewTab);
                self.core.cur
            }
            None => {
                self.core.tabs.push(Tab::new(sel).for_engine(engine));
                self.core.tabs.len() - 1
            }
        };
        if shown && i != self.core.cur {
            self.set_cur(i);
        }
        let names: Vec<String> = attached.iter().map(|a| a.name.clone()).collect();
        let loads: Vec<(u64, String)> = attached.iter().map(|a| (a.conn, a.db.clone())).collect();
        let t = &mut self.core.tabs[i];
        t.view = View::Shell;
        t.selected = None;
        t.shell.code.clone_from(&script);
        // An edit without `attach` keeps what the tab has, including attachments the user added, but writes the user allowed
        // were for the script they read, not this one.
        if at.is_none() || !attach.is_empty() {
            t.shell.attached = attached;
        } else {
            for a in &mut t.shell.attached {
                a.allow_writes = false;
            }
        }
        t.shell.agent_code = Some(script);
        let uid = t.uid;
        for (c, d) in loads {
            self.attach_load(c, &d);
        }
        Ok((id, uid, shown, names))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::config::{Access, ConnPolicy};
    use crate::agents::link::ToolErr;
    use tokio::sync::oneshot;

    fn app() -> egui_kittest::Harness<'static, App> {
        egui_kittest::Harness::builder().with_accessibility_check(false).build_eframe(|cc| App::for_test(cc))
    }

    fn call(app: &mut App, ctx: &egui::Context, args: Value) -> Result<Value, ToolErr> {
        let (reply, mut rx) = oneshot::channel();
        app.agent_call(ctx, 1, 1, "open_shell".into(), args, "claude-code".into(), reply);
        rx.try_recv().expect("answered at once")
    }

    /// prod (MongoDB, open) and pg (PostgreSQL, closed) shared with agents, hidden (MySQL) not.
    fn setup(app: &mut App) {
        app.core.agents.cfg.enabled = true;
        for (id, name, engine, status, shared) in [(21, "prod", db::sql::Engine::default(), Status::Ready, true), (22, "pg", db::sql::Engine::Postgres, Status::Idle, true), (23, "hidden", db::sql::Engine::MySql, Status::Ready, false)] {
            app.core.conns.push(Conn::new(ConnMeta { id, name: name.into(), engine, ..Default::default() }, status));
            if shared {
                app.core.agents.cfg.conns.insert(id, ConnPolicy { access: Access::Read, scripts: Mode::Ask, ..Default::default() });
            }
        }
    }

    const SCRIPT: &str = "async function run() { return await db.collection(\"users\").find().toArray() }";

    #[test]
    fn opens_edits_and_leaves_the_users_tabs_alone() {
        let mut h = app();
        let ctx = h.ctx.clone();
        let app = h.state_mut();
        setup(app);
        let attach = json!([{ "connection": "pg", "database": "sales" }, { "connection": "prod", "database": "app", "name": "travels" }]);
        let v = call(app, &ctx, json!({ "connection": "prod", "database": "shop", "script": SCRIPT, "attach": attach })).unwrap();
        assert_eq!(v["shown"], true);
        assert_eq!(v["attached"], json!(["sales", "travels"]));
        let uid = v["tab"].as_u64().unwrap();
        // Sitting on a Shell tab is no reason to open the next one behind it.
        let second = call(app, &ctx, json!({ "connection": "prod", "database": "shop", "script": SCRIPT })).unwrap();
        assert_eq!(second["shown"], true);
        let first = app.core.tabs.iter().position(|t| t.uid == uid).unwrap();
        app.set_cur(first);
        let t = app.t().unwrap();
        assert!(t.view == View::Shell);
        assert_eq!((t.uid, t.shell.code.as_str()), (uid, SCRIPT));
        let a = &t.shell.attached;
        assert_eq!((a[0].conn, a[0].schema.as_str(), a[1].schema.as_str()), (22, crate::app::shell_attach::PG_SCHEMA, ""));
        assert!(a.iter().all(|a| !a.allow_writes), "only the user allows writes");

        let tabs = app.core.tabs.len();
        let refuse = |app: &mut App, args: Value| call(app, &ctx, args).unwrap_err();
        let e = refuse(app, json!({ "connection": "prod", "database": "shop", "script": SCRIPT, "attach": [{ "connection": "hidden", "database": "x" }] }));
        assert_eq!(e.code, "not_shared");
        let e = refuse(app, json!({ "connection": "prod", "database": "shop", "script": SCRIPT, "attach": [{ "connection": "pg", "database": "x", "name": "db" }] }));
        assert!(e.message.contains("db is already"), "{}", e.message);
        let e = refuse(app, json!({ "connection": "prod", "database": "shop", "schema": "public", "script": SCRIPT }));
        assert_eq!(e.code, "bad_args");
        assert_eq!(refuse(app, json!({ "connection": "pg", "database": "sales", "script": SCRIPT })).code, "not_open");
        app.core.agents.cfg.conns.get_mut(&21).unwrap().scope = crate::agents::config::Scope { mode: crate::agents::config::ScopeMode::Only, globs: vec!["shop".into(), "app".into()] };
        assert!(refuse(app, json!({ "connection": "prod", "database": "payroll", "script": SCRIPT })).message.contains("payroll is not shared"));
        app.core.agents.cfg.conns.get_mut(&21).unwrap().scope = Default::default();
        assert_eq!(app.core.tabs.len(), tabs, "a refusal opens nothing");

        // Writes the user allowed for the script they read do not carry over to the agent's next one.
        app.tm().unwrap().shell.attached[1].allow_writes = true;
        let edited = SCRIPT.replace("users", "trips");
        let v = call(app, &ctx, json!({ "connection": "prod", "database": "shop", "script": edited, "tab": uid })).unwrap();
        assert_eq!(v["tab"], uid);
        assert_eq!(app.t().unwrap().shell.code, edited);
        assert_eq!(app.t().unwrap().shell.attached.len(), 2, "an edit without `attach` keeps the attachments");
        assert!(app.t().unwrap().shell.attached.iter().all(|a| !a.allow_writes));

        app.tm().unwrap().shell.code.push_str("\n// mine");
        let e = refuse(app, json!({ "connection": "prod", "database": "shop", "script": SCRIPT, "tab": uid }));
        assert!(e.message.contains("The user changed"), "{}", e.message);
        let mine = app.core.tabs.iter().find(|t| t.shell.agent_code.is_none()).map(|t| t.uid);
        if let Some(mine) = mine {
            assert!(refuse(app, json!({ "connection": "prod", "database": "shop", "script": SCRIPT, "tab": mine })).message.contains("not opened by open_shell"));
        }

        // A dialog open: the tab opens behind it and the current tab stays.
        app.modal = Some(Modal::Attach(Box::default()));
        let cur = app.core.cur;
        let v = call(app, &ctx, json!({ "connection": "prod", "database": "shop", "script": SCRIPT })).unwrap();
        assert_eq!((v["shown"].as_bool(), app.core.cur), (Some(false), cur));
        assert_eq!(app.core.tabs.last().unwrap().uid, v["tab"].as_u64().unwrap());
        // The Run confirm runs the code as it is when confirmed, so no edit lands under a dialog.
        assert_eq!(refuse(app, json!({ "connection": "prod", "database": "shop", "script": SCRIPT, "tab": v["tab"] })).code, "busy");
        assert_eq!(app.core.agents.activity.len(), 11);
    }
}
