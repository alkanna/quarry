//! mongodump and mongorestore: finding, installing and running them (src/app/tools.rs draws the dialogs).

use super::{Dialog, Reply, Refusal, RefusalKind, UiFx};
use crate::core::Core;
use crate::db::{self, Msg};
use crate::paths::tools_dir;
use crate::state::config::{stored_uri, uri_err};
use crate::state::ToolRun;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ToolsA {
    /// Download the MongoDB Database Tools.
    Install,
    /// mongodump of database `db`, or only its collection `coll`, to the archive at `path`.
    Dump { conn: u64, db: String, coll: Option<String>, path: String, gzip: bool },
    /// mongorestore of `from_db` (only `coll` when set) from the archive at `archive` into `to_db`, after its confirmation.
    Restore { conn: u64, archive: String, from_db: String, coll: Option<String>, to_db: String, drop: bool, gzip: bool },
    /// Stop the running tool.
    Stop,
    /// Close the finished tool's window.
    Close,
}

/// Bun for the Shell: the path from Settings, else the usual install places. Apps opened from
/// Finder do not get the terminal's PATH, so the common folders are checked directly.
pub fn find_bun(setting: &str) -> Option<PathBuf> {
    find_exe("bun", setting)
}

/// A program from its Settings path, else from PATH and the usual install folders.
pub fn find_exe(name: &str, setting: &str) -> Option<PathBuf> {
    let mut extra = crate::cloudsql::brew_dirs();
    if let Some(h) = dirs::home_dir() {
        extra.push(h.join(".bun").join("bin"));
    }
    crate::cloudsql::find_program(&format!("{name}{}", std::env::consts::EXE_SUFFIX), setting, &extra)
}

impl Core {
    pub(super) fn tools(&mut self, a: ToolsA) -> Result<Reply, Refusal> {
        match a {
            ToolsA::Install => self.install_tools(),
            ToolsA::Dump { conn, db, coll, path, gzip } => {
                let uri = match stored_uri(conn) {
                    Ok(u) => u,
                    Err(e) => {
                        // An error toast as before, not a fading warning; the dialog stays open on it.
                        let msg = uri_err(e);
                        self.err(msg.clone());
                        return Err(Refusal { kind: RefusalKind::Invalid, msg });
                    }
                };
                let name = match &coll {
                    Some(c) => format!("{db}.{c}"),
                    None => db.clone(),
                };
                self.start_tool(false, format!("Dump {name}"), db::dump_args(&uri, &db, coll.as_deref(), &path, gzip))?;
            }
            ToolsA::Restore { conn, archive, from_db, coll, to_db, drop, gzip } => {
                if !self.conn(conn).is_some_and(|c| c.writable && c.client.is_some()) {
                    self.ui_fx.push(UiFx::Open(Dialog::Unlock(conn)));
                    return Ok(Reply::Done);
                }
                let uri = match stored_uri(conn) {
                    Ok(u) => u,
                    Err(e) => {
                        let msg = uri_err(e);
                        self.err(msg.clone());
                        return Err(Refusal { kind: RefusalKind::Invalid, msg });
                    }
                };
                self.start_tool(true, "Restore".into(), db::restore_args(&uri, &archive, &from_db, coll.as_deref(), &to_db, drop, gzip))?;
            }
            ToolsA::Stop => {
                if let Some(c) = self.tool.as_mut().and_then(|t| t.cancel.take()) {
                    let _ = c.send(());
                }
            }
            ToolsA::Close => self.tool = None,
        }
        Ok(Reply::Done)
    }

    pub fn install_tools(&mut self) {
        if !self.tools_installing {
            self.tools_installing = true;
            self.worker.blocking(|| Msg::ToolsInstalled(crate::update::install_db_tools(&tools_dir()).map(Some)));
        }
    }

    /// Runs mongodump or mongorestore with `args`, which hold the connection's address and so never travel in an action.
    pub fn start_tool(&mut self, restore: bool, title: String, args: Vec<String>) -> Result<(), Refusal> {
        let (name, setting) = if restore { ("mongorestore", &self.prefs.mongorestore) } else { ("mongodump", &self.prefs.mongodump) };
        let Some(exe) = find_exe(name, setting) else {
            return Err(self.refuse_settings(format!("{name} was not found. Install the MongoDB Database Tools from Settings or set its path there.")));
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tool = Some(ToolRun { title, log: Vec::new(), cancel: Some(tx), done: None });
        self.worker.tool(exe, args, rx);
        Ok(())
    }

    pub(crate) fn tool_log(&mut self, line: String) {
        if let Some(t) = &mut self.tool {
            t.log.push(line);
            if t.log.len() > 2000 {
                t.log.drain(..500);
            }
        }
    }

    pub(crate) fn tool_done(&mut self, res: db::Res<String>) {
        if let Some(t) = &mut self.tool {
            t.cancel = None;
            t.done = Some(res.clone());
        }
        match res {
            Ok(m) => self.ok(m),
            Err(e) => self.err(e),
        }
    }

    pub(crate) fn tools_installed(&mut self, res: db::Res<Option<String>>) {
        // Only a click shows failures; a background update that fails stays quiet, like the app's own.
        let manual = std::mem::take(&mut self.tools_installing);
        match res {
            Ok(Some(v)) => self.ok(format!("{} the MongoDB Database Tools {v} in {}", if manual { "Installed" } else { "Updated" }, tools_dir().display())),
            Ok(None) => {}
            Err(e) if manual => self.err(format!("Could not install the MongoDB Database Tools: {e}")),
            Err(_) => {}
        }
    }
}
