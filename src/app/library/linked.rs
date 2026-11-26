//! Linked folders in the app: scanning them off the UI thread, noticing that a file under an open tab changed on disk, the
//! conflict banner, and writing a bound file back. The files themselves are read and written through `crate::library::disk`.

use super::*;
use crate::library::tree::Node;
use crate::library::{ScriptRef, disk, format};
use quarry_core::action::{Action, LibA};
use std::time::{Duration, SystemTime};
pub(crate) use quarry_core::state::library::linked::*;

pub(in crate::app) use crate::library::disk::read_script;

impl App {
    /// The conflict banner above the editor of a tab whose file changed on disk under unsaved edits.
    pub(in crate::app) fn conflict_banner(&mut self, ui: &mut egui::Ui) {
        let Some(t) = self.core.tabs.get(self.core.cur) else { return };
        let Some(b) = t.script.as_ref().filter(|b| b.conflict.is_some() && b.view == t.view) else { return };
        let ScriptRef::File { rel, .. } = &b.r else { return };
        let name = file_label(rel);
        let mut act = 0;
        banner_frame(WARNING()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.set_min_height(BUTTON_H);
                glyph_icon(ui, glyphs::warn, WARNING(), 16.0);
                ui.add_space(4.0);
                ui.label(semibold(RichText::new(format!("{name} changed on disk")).size(TXT_BODY).color(BRIGHT())));
                ui.label(RichText::new("This tab has unsaved edits").size(TXT_BODY).color(TEXT()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    for (n, label, tip) in [(3, "Compare", "See what differs"), (2, "Keep mine", "Keep your edits; saving then replaces the file"), (1, "Reload", "Replace your edits with the file. Undo brings them back")] {
                        if quiet(ui, label).on_hover_text(tip).clicked() {
                            act = n;
                        }
                    }
                });
            });
        });
        ui.add_space(10.0);
        match act {
            1 => self.conflict_reload(),
            2 => self.conflict_keep(),
            3 => self.conflict_compare(),
            _ => {}
        }
    }

    fn cur_uid(&self) -> u64 {
        self.t().map_or(0, |t| t.uid)
    }

    /// The conflict banner's Reload, Keep mine and Compare, on the current tab.
    pub(in crate::app) fn conflict_reload(&mut self) {
        self.exec(Action::Library(LibA::ConflictReload { tab: self.cur_uid() }));
    }

    pub(in crate::app) fn conflict_keep(&mut self) {
        self.exec(Action::Library(LibA::ConflictKeep { tab: self.cur_uid() }));
    }

    pub(in crate::app) fn conflict_compare(&mut self) {
        self.exec(Action::Library(LibA::ConflictCompare { tab: self.cur_uid() }));
    }
}

/// Opens `path` in the file manager, selecting it where the system can. The one place that starts the file manager, so
/// the audit log and the linked folders do it the same way.
pub(in crate::app) fn reveal(path: &std::path::Path, select: bool) -> std::io::Result<()> {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        if select {
            c.arg("-R");
        }
        c.arg(path);
        c
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("explorer");
        if select {
            c.arg(format!("/select,{}", path.display()));
        } else {
            c.arg(path);
        }
        c
    } else {
        // xdg-open has no select; the folder that holds the file is the closest.
        let mut c = std::process::Command::new("xdg-open");
        c.arg(if select { path.parent().unwrap_or(path) } else { path });
        c
    };
    cmd.spawn().map(|_| ())
}


/// A made-up scan of the demo folder: `files` are (path below the folder, kind, first text).
fn demo_scan(files: &[(&str, &str, &str)]) -> disk::Scan {
    let now = SystemTime::now();
    let entries = files
        .iter()
        .enumerate()
        .map(|(i, (rel, kind, head))| disk::Entry { rel: PathBuf::from(rel), mtime: now - Duration::from_secs(600 + 7200 * i as u64), len: head.len() as u64, kind: kind.to_string(), head: head.to_string(), target: format::Target::default() })
        .collect();
    disk::Scan { branch: Some("main".into()), entries, ..Default::default() }
}

const DEMO_FILES: [(&str, &str, &str); 14] = [
    ("monthly-revenue.sql", "sql", "SELECT DATE_FORMAT(created_at, '%Y-%m') AS month,\n       SUM(total) AS revenue\nFROM orders\nWHERE status = 'paid'\nGROUP BY month\nORDER BY month DESC;"),
    ("top-customers.sql", "sql", "SELECT customer_id, SUM(total) AS spent\nFROM orders\nGROUP BY customer_id\nORDER BY spent DESC\nLIMIT 20;"),
    ("slow-lookup.sql", "sql", "EXPLAIN SELECT * FROM orders WHERE customer_id = 42;"),
    ("scratch.sql", "sql", "SELECT 1;"),
    ("reports/orders-per-day.sql", "sql", "SELECT DATE(created_at) AS day, COUNT(*) FROM orders GROUP BY day;"),
    ("reports/refunds.sql", "sql", "SELECT * FROM refunds WHERE created_at > NOW() - INTERVAL 7 DAY;"),
    ("reports/stock-low.sql", "sql", "SELECT sku, qty FROM stock WHERE qty < 5;"),
    ("reports/by-region.sql", "sql", "SELECT region, SUM(total) FROM orders GROUP BY region;"),
    ("reports/cohorts.sql", "sql", "SELECT 1;"),
    ("reports/shipping.sql", "sql", "SELECT 1;"),
    ("maintenance/stale-carts.sql", "sql", "DELETE FROM carts WHERE updated_at < NOW() - INTERVAL 30 DAY;"),
    ("maintenance/vacuum.sql", "sql", "OPTIMIZE TABLE orders;"),
    ("maintenance/backfill.js", "shell", "const n = await orders.countDocuments({ total: null });"),
    ("maintenance/indexes.sql", "sql", "SHOW INDEX FROM orders;"),
];

impl App {
    /// QUARRY_TEST_LIBRARY=linked, missing, link, save-linked or conflict: linked folders over made-up scans, so nothing
    /// reads the disk. True when `state` is one of them.
    pub(in crate::app) fn demo_library_linked(&mut self, state: &str) -> bool {
        if !matches!(state, "linked" | "missing" | "link" | "save-linked" | "conflict") {
            return false;
        }
        self.demo_library("panel");
        self.core.setup().lib_ui.panel.sel = None;
        self.core.setup().lib_ui.frozen = true;
        let link = |id, name: &str, path: &str| crate::library::Link { id, name: name.into(), path: path.into(), on: "engine".into(), engine: "mysql".into(), exts: ["sql", "js", "json"].map(String::from).to_vec(), follow: true, ..Default::default() };
        if state != "link" {
            let c = self.core.setup();
            c.library.links.push(link(100, "shop-queries", "/Users/demo/work/shop-queries"));
            c.lib_ui.scans.insert(100, demo_scan(&DEMO_FILES));
            let st = &mut c.lib_ui.panel;
            st.open.insert(Node::Link(100));
            st.open.insert(Node::Dir { link: 100, rel: "reports".into() });
            c.lib_ui.changed.insert((100, "reports/refunds.sql".into()));
        }
        // The Library shows the scripts that fit the current tab's connection, so the .sql link needs a MySQL tab in front.
        if matches!(state, "linked" | "missing") {
            let mut t = Tab::new(Sel { conn: 3, db: "shop".into(), schema: String::new(), coll: String::new() }).for_engine(db::sql::Engine::MySql);
            t.view = View::Sql;
            let c = self.core.setup();
            c.tabs = vec![t];
            c.cur = 0;
        }
        match state {
            "linked" => self.core.setup().lib_ui.panel.sel = Some(Node::File { link: 100, rel: "monthly-revenue.sql".into() }),
            "missing" => {
                let c = self.core.setup();
                c.library.links.push(link(101, "legacy-exports", "/Volumes/archive/exports"));
                c.lib_ui.scans.insert(101, disk::Scan { missing: true, ..Default::default() });
            }
            "link" => {
                self.open_link_dialog(None);
                let scan = demo_scan(&DEMO_FILES);
                if let Some(Modal::LinkFolder(f)) = &mut self.modal {
                    f.fill_demo("/Users/demo/work/shop-queries", scan);
                }
            }
            "save-linked" => {
                if self.demo_library_quick("save")
                    && let Some(Modal::SaveScript(f)) = &mut self.modal
                {
                    (f.link, f.dir, f.name) = (Some(100), "reports".into(), "Monthly revenue".into());
                }
            }
            _ => {
                // A tab open on a file whose text was edited here and changed on disk since.
                let rel = PathBuf::from("monthly-revenue.sql");
                let base = DEMO_FILES[0].2.to_string();
                let mine = base.replace("ORDER BY month DESC", "ORDER BY month ASC");
                let theirs = base.replace("status = 'paid'", "status IN ('paid', 'shipped')");
                let mut t = Tab::new(Sel { conn: 3, db: "shop".into(), schema: String::new(), coll: String::new() }).for_engine(db::sql::Engine::MySql);
                t.view = View::Sql;
                if let Some(q) = t.sql.as_mut() {
                    q.query = mine;
                }
                t.script = Some(Bound { r: ScriptRef::File { link: 100, rel }, view: View::Sql, base, disk: None, conflict: Some(theirs) });
                let c = self.core.setup();
                c.tabs = vec![t];
                c.cur = 0;
            }
        }
        true
    }
}
