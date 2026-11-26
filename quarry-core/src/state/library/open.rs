//! State behind src/app/library/open.rs, which draws it.

use crate::db;
use crate::library::ScriptRef;
use crate::state::{Bound, Tab, View};

/// How a script leaves the library: Return opens it, Alt+Return opens it in a new tab, Cmd+Enter runs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum How {
    Here,
    NewTab,
    Run,
}

/// What the view on screen holds, as a script of the library would hold it.
pub struct TabText {
    pub kind: &'static str,
    pub body: String,
    pub sort: String,
    pub project: String,
    pub view: View,
    pub db: String,
    pub schema: String,
    pub coll: String,
}

/// Where a script runs, as `App::script_target` works it out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    On(u64),
    /// No connection is the obvious one: the person picks from these.
    Ask(Vec<u64>),
    /// Nothing can run it, and why.
    None(String),
}

/// A script waiting for its connection to be ready, which `library_tick` opens once it is.
#[derive(Clone, Debug)]
pub struct Pending {
    pub r: ScriptRef,
    pub how: How,
    pub conn: u64,
    /// The tab that was current when it was asked for: a Run becomes a plain open if another tab is current by then.
    pub uid: Option<u64>,
    /// The connection's connect attempt when it was asked for, so the failure it had then is not taken for a new one.
    pub attempt: u64,
    /// Egui time the wait started while the connection was not connecting, for the time limit.
    pub since: Option<f64>,
    /// The connection was seen connecting since, so a failure now is news even on the same attempt.
    pub seen_connecting: bool,
    pub name: String,
}

/// The Open on dialog: the connections a script can run on when none is the obvious one.
pub struct OpenOnForm {
    pub r: ScriptRef,
    pub how: How,
    pub conns: Vec<u64>,
    pub sel: Option<u64>,
    pub name: String,
}

/// The view whose text is a script of `kind`.
pub fn view_of_kind(kind: &str) -> Option<View> {
    Some(match kind {
        "sql" => View::Sql,
        "shell" => View::Shell,
        "filter" | "where" => View::Documents,
        "pipeline" => View::Aggregate,
        _ => return None,
    })
}

/// Whether a script of `kind` can run on a connection of `engine`: SQL on MySQL, MariaDB and PostgreSQL, filters and
/// pipelines on MongoDB, Shell scripts on any.
pub fn kind_fits(kind: &str, engine: db::sql::Engine) -> bool {
    crate::library::format::kind_runs_on(kind, engine).unwrap_or(false)
}

/// The database named in a connection address, such as shop in mysql://app@host:3306/shop?ssl-mode=REQUIRED. Empty
/// when the address names none.
pub fn uri_db(uri: &str) -> String {
    let Some((_, rest)) = uri.split_once("://") else { return String::new() };
    // A masked or encoded password may hold a slash, so the host starts after the last @.
    let host_on = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    let Some((_, path)) = host_on.split_once('/') else { return String::new() };
    path.split(['?', '#']).next().unwrap_or_default().trim_matches('/').to_string()
}

/// Ties tab `t` to script binding `b`, the one place a tab is bound. A SQL tab bound to a Query script becomes a plain
/// query tab, as one opened from the library is: its table's views would otherwise stay under the script's name, with
/// nothing saying which table they show.
pub fn bind(t: &mut Tab, b: Bound) {
    if b.view == View::Sql && t.sql.is_some() {
        t.sel.coll.clear();
    }
    t.script = Some(b);
}

/// `App::script_snippet` where there is no App: `dialect` is the SQL dialect of the script's connection, looked up only for
/// a SQL script that may hold values.
pub fn snippet_of(s: &crate::library::Script, dialect: impl FnOnce() -> db::sql::Dialect) -> Option<crate::library::snippet::Parsed> {
    use crate::library::snippet::{self, Lang};
    let parts: Vec<&str> = if s.kind == "filter" { vec![&s.body, &s.sort, &s.project] } else { vec![&s.body] };
    if !parts.iter().any(|p| p.contains("{{")) {
        return None;
    }
    let l = match s.kind.as_str() {
        "sql" => dialect().into(),
        "shell" => Lang::Shell,
        "filter" => Lang::Query,
        _ => return None,
    };
    Some(snippet::merge(parts.into_iter().map(|p| snippet::parse(p, l)))).filter(|p| p.is_snippet())
}

/// The text of a tab's view as a script of the library would hold it, or None for a view that holds no script (Indexes,
/// Schema, a SQL table's Structure and the like). A SQL table's Content view holds its WHERE and ORDER BY as a table filter.
pub fn text_of(t: &Tab) -> Option<TabText> {
    let (db, coll) = (t.sel.db.clone(), t.sel.coll.clone());
    Some(match (t.view, t.sql.as_ref()) {
        (View::Sql, Some(s)) => {
            let run_db = if s.run_db.is_empty() { db } else { s.run_db.clone() };
            TabText { kind: "sql", body: s.query.clone(), sort: String::new(), project: String::new(), view: View::Sql, db: run_db, schema: s.run_schema.trim().to_string(), coll: String::new() }
        }
        (View::Shell, _) => TabText { kind: "shell", body: t.shell.code.clone(), sort: String::new(), project: String::new(), view: View::Shell, db, schema: String::new(), coll: String::new() },
        (View::Documents, Some(_)) if !coll.is_empty() => TabText { kind: "where", body: t.filter_txt.clone(), sort: t.sort_txt.clone(), project: String::new(), view: View::Documents, db, schema: t.sel.schema.clone(), coll },
        (View::Documents, None) => TabText { kind: "filter", body: t.filter_txt.clone(), sort: t.sort_txt.clone(), project: t.proj_txt.clone(), view: View::Documents, db, schema: t.sel.schema.clone(), coll },
        (View::Aggregate, None) => TabText { kind: "pipeline", body: t.agg_txt.clone(), sort: String::new(), project: String::new(), view: View::Aggregate, db, schema: t.sel.schema.clone(), coll },
        _ => return None,
    })
}
