//! The command log: every driver command with its timing, kept in memory for the log panel.

use super::*;

/// One command sent to a server, for the query log.
pub struct LogEntry {
    pub conn: u64,
    pub at: std::time::SystemTime,
    pub db: String,
    pub name: String,
    pub cmd: String,
    /// None while the command is still running.
    pub ms: Option<u128>,
    pub err: Option<String>,
    req: i32,
}

impl LogEntry {
    /// A finished command, for the offline demo.
    pub fn finished(conn: u64, secs_ago: u64, db: &str, name: &str, cmd: &str, ms: u128, err: Option<&str>) -> Self {
        let at = std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago);
        LogEntry { conn, at, db: db.into(), name: name.into(), cmd: cmd.into(), ms: Some(ms), err: err.map(Into::into), req: 0 }
    }
}

pub const LOG_MAX: usize = 2000;

// ponytail: one process-wide log, clients of every connection write here; keyed by conn id.
pub static LOG: std::sync::Mutex<std::collections::VecDeque<LogEntry>> = std::sync::Mutex::new(std::collections::VecDeque::new());

/// Feeds the driver's command events into `LOG`. The driver already blanks the bodies of
/// authentication and user password commands, so secrets never reach the log.
pub(crate) fn log_event(conn: u64, ev: mongodb::event::command::CommandEvent) {
    use mongodb::event::command::CommandEvent as E;
    match ev {
        E::Started(s) => {
            // Driver housekeeping, not something the user asked for.
            if matches!(s.command_name.as_str(), "hello" | "isMaster" | "ismaster" | "endSessions" | "killCursors") {
                return;
            }
            let mut cmd = s.command;
            for k in ["lsid", "$db", "$clusterTime", "$readPreference", "txnNumber", "apiVersion"] {
                cmd.remove(k);
            }
            // Bulk writes carry thousands of documents; a few show the shape.
            for k in ["documents", "updates", "deletes"] {
                if let Some(Bson::Array(a)) = cmd.get_mut(k).filter(|a| a.as_array().is_some_and(|a| a.len() > 3)) {
                    let more = a.len() - 3;
                    a.truncate(3);
                    a.push(Bson::String(format!("… {more} more")));
                }
            }
            let mut text = compact(&Bson::Document(cmd));
            if let Some((cut, _)) = text.char_indices().nth(4000) {
                text.truncate(cut);
                text.push_str(" …");
            }
            let entry = LogEntry { conn, at: std::time::SystemTime::now(), db: s.db, name: s.command_name, cmd: text, ms: None, err: None, req: s.request_id };
            let Ok(mut log) = LOG.lock() else { return };
            if log.len() >= LOG_MAX {
                log.pop_front();
            }
            log.push_back(entry);
        }
        other => {
            let (req, ms, err) = match other {
                E::Succeeded(s) => (s.request_id, s.duration.as_millis(), None),
                E::Failed(f) => (f.request_id, f.duration.as_millis(), Some(e(f.failure))),
                _ => return,
            };
            let Ok(mut log) = LOG.lock() else { return };
            if let Some(l) = log.iter_mut().rev().find(|l| l.conn == conn && l.req == req) {
                (l.ms, l.err) = (Some(ms), err);
            }
        }
    }
}
