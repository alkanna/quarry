//! State behind src/app/library/link_dialog.rs, which draws it.

use crate::library::disk;
use crate::db::sql::Engine;

/// The fields of the Link a folder dialog.
#[derive(Default)]
pub struct LinkForm {
    /// The link being edited, None when a new one is made. An edit keeps the folder, unless it is a locate.
    pub edit: Option<u64>,
    /// The edited link's folder is gone and the person is pointing it at the folder where it is now.
    pub locate: bool,
    pub path: String,
    pub name: String,
    /// The name was typed, so a new folder path no longer renames it.
    pub name_set: bool,
    /// "conns", "engine" or "all", as in `Link::on`.
    pub on: String,
    pub conns: Vec<u64>,
    /// The engine "Every ... connection" means: that of the first chosen connection, or of the current tab, until the
    /// folder's files say otherwise.
    pub engine: Engine,
    /// The person picked the engine, so the folder's files no longer decide it.
    pub engine_set: bool,
    /// The filter and folded groups of the + Add list.
    pub pick: crate::state::conns_panel::ConnsUi,
    pub sql: bool,
    pub js: bool,
    pub json: bool,
    pub follow: bool,
    /// The scan that summarizes the folder, with the path it was made for.
    pub preview: Option<(String, disk::Scan)>,
    /// The path a scan is out for, or None while none is.
    pub asking: Option<String>,
    /// Egui time of the last edit to the path. The scan waits until typing pauses, since each partial path is a folder.
    pub typed_at: f64,
}

impl LinkForm {
    pub fn exts(&self) -> Vec<String> {
        [("sql", self.sql), ("js", self.js), ("json", self.json)].iter().filter(|(_, on)| *on).map(|(e, _)| e.to_string()).collect()
    }

    /// The demo's dialog: a folder typed and already looked at, so nothing is scanned.
    pub fn fill_demo(&mut self, path: &str, scan: disk::Scan) {
        self.path = path.into();
        self.name = "shop-queries".into();
        self.name_set = true;
        self.preview = Some((path.into(), scan));
    }

    /// The scan that came back, once the dialog has asked for it. A scan for a path that was changed since is dropped. A
    /// new link takes the engine its files are written for, unless the person chose one.
    pub fn got_preview(&mut self, scan: disk::Scan) {
        if let Some(p) = self.asking.take() {
            if self.edit.is_none() && !self.engine_set {
                self.engine = engine_for(&scan.entries, self.engine);
            }
            self.preview = Some((p, scan));
        }
    }
}

/// The engine a folder's scripts are for, by their most common kind: Mongo for filters and pipelines, a SQL engine for
/// .sql files (the current one when it is SQL, since a .sql file does not say which dialect), else `current`. Shell
/// scripts run anywhere and do not count.
pub fn engine_for(entries: &[disk::Entry], current: Engine) -> Engine {
    let sql = entries.iter().filter(|e| e.kind == "sql").count();
    let mongo = entries.iter().filter(|e| matches!(e.kind.as_str(), "filter" | "pipeline")).count();
    if sql > mongo {
        if current.is_sql() { current } else { Engine::MySql }
    } else if mongo > sql {
        Engine::Mongo
    } else {
        current
    }
}
