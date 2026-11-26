//! open_shell: an agent's script put in a Shell tab for the user to review and run. Nothing reaches a server and only
//! the tab id goes back, so neither the approval nor the data rules apply. Every connection must still be shared, with
//! scripts not set to Never, and attachments always start read-only.

use super::exec::{Reply, SCHEMA_PG_ONLY, not_open};
use crate::action::shell::pg_schema;
use crate::agents::audit::Summary;
use crate::agents::config::{Mode, OpClass};
use crate::agents::tools::{Call, ConnRef, ShellAttach};
use crate::core::Core;
use crate::db::{self, JS_RESERVED};
use crate::state::agents::{Activity, Outcome};
use crate::state::shell_attach::Attached;
use crate::state::{Open, Sel, Tab, View};
use serde_json::{json, Value};

type Refusal = (&'static str, String);

fn bad(why: String) -> Refusal {
    ("bad_args", why)
}

impl Core {
    pub(crate) fn agent_open_shell(&mut self, mut act: Activity, call: Call, args: &Value, reply: Reply) {
        let Call::OpenShell { conn, db, schema, script, attach, tab } = call else { return };
        act.ns = db.clone();
        act.args = self.agent_args(None, args, None);
        // Keys the user is typing would land in the new tab: a Run shortcut there would run the agent's script unread.
        let typing = self.agents.ui.typing;
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
        if self.agents.cfg.conn(id).is_some_and(|p| p.mode(OpClass::RunScripts) == Mode::Never) {
            let name = self.conn(id).map(|c| c.meta.name.clone()).unwrap_or_default();
            return Err(("refused", format!("Run scripts is set to Never for agents on {name}.")));
        }
        Ok(id)
    }

    /// Refused unless the connection's scope shares `db` (on PostgreSQL, `schema` in database `db`), as the other tools are.
    fn agent_shell_shared(&self, id: u64, db: &str, schema: &str) -> Result<(), Refusal> {
        let name = self.conn(id).map(|c| c.meta.name.clone()).unwrap_or_default();
        let pg = self.is_pg(id);
        let raw = self.agents.cfg.conn(id).map(|p| p.scope.mode).unwrap_or_default();
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
                let Some(i) = self.tabs.iter().position(|t| t.uid == uid) else { return Err(("gone", format!("Tab {uid} is closed. Leave out `tab` to open a new one."))) };
                let t = &self.tabs[i];
                match &t.shell.agent_code {
                    None => return Err(("refused", format!("Tab {uid} was not opened by open_shell, so Quarry leaves it alone. Leave out `tab` to open a new one."))),
                    Some(code) if *code != t.shell.code => return Err(("refused", format!("The user changed the script in tab {uid}, so Quarry leaves it alone. Leave out `tab` to open a new one."))),
                    _ => {}
                }
                if t.shell.cancel.is_some() {
                    return Err(("busy", format!("The script in tab {uid} is running. Try again once it ends.")));
                }
                // The Run confirm runs the tab's code as it is when confirmed, so a change under it would run unapproved code.
                if self.agents.ui.modal {
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
        let shown = !typing && !self.agents.ui.modal && self.json_edit.is_none() && self.sql_cell.is_none() && self.inline.is_none() && self.new_field.is_none();
        let i = match at {
            Some(i) => i,
            None if shown => {
                self.open_tab(sel, Open::NewTab);
                self.cur
            }
            None => {
                self.tabs.push(Tab::new(sel).for_engine(engine));
                self.tabs.len() - 1
            }
        };
        if shown && i != self.cur {
            self.set_cur(i);
        }
        let names: Vec<String> = attached.iter().map(|a| a.name.clone()).collect();
        let loads: Vec<(u64, String)> = attached.iter().map(|a| (a.conn, a.db.clone())).collect();
        let t = &mut self.tabs[i];
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

/// The database name as a JS identifier: other characters become `_`, a leading digit gets one in front,
/// and a reserved word gets one after it.
pub fn default_name(db: &str) -> String {
    let mut s: String = db.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '$' { c } else { '_' }).collect();
    if s.is_empty() || s.chars().all(|c| c == '_') {
        return "conn".into();
    }
    if s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    if JS_RESERVED.contains(&s.as_str()) {
        s.push('_');
    }
    s
}

/// Why `name` cannot be an attachment's global, or None when it can. `taken` lists the names already in
/// use with what uses them, which the message names.
pub fn name_problem(name: &str, taken: &[(String, String)]) -> Option<String> {
    if name.is_empty() {
        return Some("Enter a name.".into());
    }
    if !name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_' || c == '$') || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') {
        return Some("Use letters, digits, _ or $, and do not start with a digit.".into());
    }
    if JS_RESERVED.contains(&name) {
        return Some(format!("{name} is a reserved word in JavaScript."));
    }
    taken.iter().find(|t| t.0 == name).map(|t| format!("{name} is already {}.", t.1))
}

/// The variable a package is loaded as: camelCase of its name without scope. Must match
/// packageIdent() in shell_runner.mjs.
pub fn package_ident(spec: &str) -> String {
    let name = db::package_spec(spec).0;
    let base = name.strip_prefix('@').and_then(|r| r.split_once('/')).map(|(_, n)| n.to_string()).unwrap_or(name);
    let mut id: String = base
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|p| !p.is_empty())
        .enumerate()
        .map(|(i, p)| if i == 0 { p.to_string() } else { p[..1].to_ascii_uppercase() + &p[1..] })
        .collect();
    if id.starts_with(|c: char| c.is_ascii_digit()) {
        id.insert(0, '_');
    }
    if id.is_empty() { "pkg".into() } else { id }
}

impl Core {
    /// Names already in use as globals of a Shell tab on `sel` with these attachments, which need not be the current
    /// one or exist yet, each with what uses it.
    pub fn globals_taken(&self, sel: &Sel, main_sql: bool, attached: &[Attached]) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        let mut add = |n: &str, why: String| {
            if !out.iter().any(|o| o.0 == n) {
                out.push((n.to_string(), why));
            }
        };
        // Refused whatever the engines, so a later attachment of another engine cannot collide with it.
        for n in ["db", "sql", "table", "require", "console", "log", "_", "dayjs", "ObjectId", "ISODate", "randomId", "UUIDv4", "UUIDv1"] {
            add(n, "a name the shell provides".into());
        }
        for p in &self.prefs.shell_packages {
            add(&package_ident(p), format!("the name of the package {}", db::package_spec(p).0));
        }
        // The runner tells this tab's own connection apart from an attachment by its name.
        if let Some(c) = self.conn(sel.conn) {
            add(&c.meta.name, "the name of this tab's connection".into());
        }
        for n in self.conn(sel.conn).and_then(|c| c.dbs.iter().find(|d| d.0 == sel.db)).and_then(|d| d.1.as_ref()).into_iter().flatten() {
            add(n, format!("{} of {}", if main_sql { "a table" } else { "a collection" }, sel.db));
        }
        for a in attached {
            add(&a.name, "used by another attached connection".into());
        }
        out
    }
}
