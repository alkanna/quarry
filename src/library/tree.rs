//! The rows of the Library panel as a flat list, so the panel draws them with `show_rows` and the keyboard walks them
//! without touching the folders. Everything here is pure: the open folders, the scope and the search come in as values.

use super::*;
use super::disk::Entry;
use super::format;
use crate::db::sql::Engine;
use std::collections::{BTreeSet, HashMap, HashSet};

/// One thing a row stands for. Linked folders are rows too, so a drag, a menu or a key can name them; B2 fills them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    Link(u64),
    Dir { link: u64, rel: PathBuf },
    File { link: u64, rel: PathBuf },
    Folder(u64),
    Script(u64),
}

impl Node {
    /// What a tab opens for this node: a saved script or a linked file. None for the folders.
    pub fn script_ref(&self) -> Option<ScriptRef> {
        match self {
            Node::Script(id) => Some(ScriptRef::Saved(*id)),
            Node::File { link, rel } => Some(ScriptRef::File { link: *link, rel: rel.clone() }),
            _ => None,
        }
    }

    /// A folder or script kept by Quarry, which can be renamed, moved and deleted here.
    pub fn is_saved(&self) -> bool {
        matches!(self, Node::Folder(_) | Node::Script(_))
    }

    /// Rows that hold other rows.
    pub fn is_container(&self) -> bool {
        matches!(self, Node::Folder(_) | Node::Link(_) | Node::Dir { .. })
    }
}

/// What the scans found below each link, by link id: the one scan type of `disk`, which the panel and the linked folders
/// share. Empty until a folder is linked.
pub use super::disk::Scan;
pub type Scans = HashMap<u64, Scan>;

#[derive(Clone, Debug)]
pub struct Row {
    pub node: Node,
    pub depth: usize,
    pub open: bool,
    /// A script or file the search matched, as opposed to a folder shown only because it holds one.
    pub hit: bool,
    /// The scripts or files below a container that the panel shows, or 0 for a leaf.
    pub count: usize,
}

/// Where over a row a drop lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pos {
    Into,
    Before,
    After,
}

/// The drop position for a pointer `y_frac` down a row (0 at its top edge, 1 at its bottom edge). A folder takes the
/// middle as "into" and the edges as before and after; an open folder has its first child right below, so its lower edge
/// also means "into". A script has no inside, so it splits at the middle.
pub fn drop_at(folder: bool, open: bool, y_frac: f32) -> Pos {
    if folder {
        if y_frac < 0.25 {
            Pos::Before
        } else if y_frac > 0.75 && !open {
            Pos::After
        } else {
            Pos::Into
        }
    } else if y_frac < 0.5 {
        Pos::Before
    } else {
        Pos::After
    }
}

fn matches(s: &Script, q: &str) -> bool {
    q.is_empty() || s.name.to_lowercase().contains(q) || s.body.chars().take(200).collect::<String>().to_lowercase().contains(q)
}

/// The parent that exists: a folder or script whose folder was lost sits at the top instead of vanishing.
pub fn live_parent(lib: &Library, p: Option<u64>) -> Option<u64> {
    p.filter(|id| lib.folders.iter().any(|f| f.id == *id))
}

/// A link shows on a connection when its `on` names it: a chosen connection, the engine of the connection, or every one.
/// Without a connection (the All scope) every link shows. `engine` is that of `conn`, when it is known.
fn link_visible(l: &Link, conn: Option<u64>, engine: Option<Engine>) -> bool {
    // The same rule as the one that decides where a file runs, so a link never shows where its files cannot open.
    conn.is_none_or(|c| format::used_on(l, c, engine))
}

struct Ctx<'a> {
    lib: &'a Library,
    scans: &'a Scans,
    conn: Option<u64>,
    engine: Option<Engine>,
    q: String,
    open: &'a HashSet<Node>,
}

impl Ctx<'_> {
    fn shows(&self, s: &Script) -> bool {
        self.conn.is_none_or(|c| s.conn == c) && matches(s, &self.q)
    }

    fn scripts_in(&self, folder: Option<u64>) -> impl Iterator<Item = &Script> {
        self.lib.scripts.iter().filter(move |s| live_parent(self.lib, s.folder) == folder && self.shows(s))
    }

    /// Scripts shown in a folder and everything below it.
    fn count(&self, folder: u64, depth: usize) -> usize {
        if depth > 64 {
            return 0;
        }
        let here = self.scripts_in(Some(folder)).count();
        here + self.lib.folders.iter().filter(|f| live_parent(self.lib, f.parent) == Some(folder)).map(|f| self.count(f.id, depth + 1)).sum::<usize>()
    }

    fn folders(&self, parent: Option<u64>, depth: usize, out: &mut Vec<Row>) {
        if depth > 64 {
            return;
        }
        let searching = !self.q.is_empty();
        for f in self.lib.folders.iter().filter(|f| live_parent(self.lib, f.parent) == parent) {
            let count = self.count(f.id, 0);
            // Empty folders stay visible, except while searching, when only the paths to a match are.
            if searching && count == 0 {
                continue;
            }
            let node = Node::Folder(f.id);
            let open = searching || self.open.contains(&node);
            out.push(Row { node, depth, open, hit: false, count });
            if open {
                self.folders(Some(f.id), depth + 1, out);
                self.script_rows(Some(f.id), depth + 1, out);
            }
        }
    }

    fn script_rows(&self, folder: Option<u64>, depth: usize, out: &mut Vec<Row>) {
        let hit = !self.q.is_empty();
        out.extend(self.scripts_in(folder).map(|s| Row { node: Node::Script(s.id), depth, open: false, hit, count: 0 }));
    }

    fn name_matches(&self, rel: &std::path::Path) -> bool {
        self.q.is_empty() || rel.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().contains(&self.q))
    }

    /// The files of a link the scope shows: those matching the search and, for a connection, the kinds it can run.
    fn files<'s>(&self, l: &Link, scan: &'s Scan) -> Vec<&'s Entry> {
        scan.entries
            .iter()
            .filter(|e| {
                self.name_matches(&e.rel)
                    && match (self.conn, self.engine) {
                        (Some(c), Some(en)) => format::fits(l, &e.kind, c, en),
                        _ => true,
                    }
            })
            .collect()
    }

    fn links(&self, out: &mut Vec<Row>) {
        for l in self.lib.links.iter().filter(|l| link_visible(l, self.conn, self.engine)) {
            let node = Node::Link(l.id);
            let Some(scan) = self.scans.get(&l.id).filter(|s| !s.missing) else {
                // Not scanned yet, or the folder is gone: the row shows, empty, unless a search is on.
                if self.q.is_empty() {
                    out.push(Row { node, depth: 0, open: false, hit: false, count: 0 });
                }
                continue;
            };
            let files = self.files(l, scan);
            if !self.q.is_empty() && files.is_empty() {
                continue;
            }
            let open = !self.q.is_empty() || self.open.contains(&node);
            out.push(Row { node, depth: 0, open, hit: false, count: files.len() });
            if open {
                self.dir_rows(l.id, &files, &scan.empty_dirs, std::path::Path::new(""), 1, out);
            }
        }
    }

    fn dir_rows(&self, link: u64, all: &[&Entry], empty: &[PathBuf], prefix: &std::path::Path, depth: usize, out: &mut Vec<Row>) {
        if depth > 64 {
            return;
        }
        let searching = !self.q.is_empty();
        let mut dirs = BTreeSet::new();
        let mut files = Vec::new();
        for f in all {
            let Ok(rest) = f.rel.strip_prefix(prefix) else { continue };
            let mut parts = rest.components();
            let (Some(first), second) = (parts.next(), parts.next()) else { continue };
            if second.is_some() {
                dirs.insert(PathBuf::from(first.as_os_str()));
            } else {
                files.push(f.rel.clone());
            }
        }
        // A folder with nothing in it shows while no search is on.
        if !searching {
            for d in empty {
                if let Ok(rest) = d.strip_prefix(prefix)
                    && let Some(first) = rest.components().next()
                {
                    dirs.insert(PathBuf::from(first.as_os_str()));
                }
            }
        }
        files.sort();
        for d in dirs {
            let rel = prefix.join(d);
            let count = all.iter().filter(|f| f.rel.starts_with(&rel)).count();
            if count == 0 && (searching || !empty.iter().any(|e| e.starts_with(&rel))) {
                continue;
            }
            let node = Node::Dir { link, rel: rel.clone() };
            let open = searching || self.open.contains(&node);
            out.push(Row { node, depth, open, hit: false, count });
            if open {
                self.dir_rows(link, all, empty, &rel, depth + 1, out);
            }
        }
        out.extend(files.into_iter().map(|rel| Row { node: Node::File { link, rel }, depth, open: false, hit: searching, count: 0 }));
    }
}

/// The visible rows. Order: linked folders, then Quarry folders (subfolders before scripts), then scripts at the top.
/// `conn` is the scope: Some(id) shows the scripts of that connection, None shows all of them. Folders are shown either
/// way, empty ones included. A search matches the name and the first 200 characters of the body, forces the folders
/// above a match open and hides the rest, without touching `open`.
pub fn flatten(lib: &Library, scans: &Scans, conn: Option<u64>, engine: Option<Engine>, query: &str, open: &HashSet<Node>) -> Vec<Row> {
    let cx = Ctx { lib, scans, conn, engine, q: query.trim().to_lowercase(), open };
    let mut out = Vec::new();
    cx.links(&mut out);
    cx.folders(None, 0, &mut out);
    cx.script_rows(None, 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib() -> Library {
        let folder = |id, name: &str, parent| Folder { id, name: name.into(), parent };
        let script = |id, name: &str, conn, folder, body: &str| Script { id, name: name.into(), kind: "sql".into(), body: body.into(), conn, folder, ..Default::default() };
        Library {
            folders: vec![folder(1, "Reports", None), folder(2, "Weekly", Some(1)), folder(3, "Scratch", None)],
            scripts: vec![
                script(10, "top orders", 1, Some(1), "select * from orders"),
                script(11, "revenue", 1, Some(2), "select sum(total) from sales"),
                script(12, "elsewhere", 2, Some(1), "select 1"),
                script(13, "loose", 1, None, "select 2"),
                script(14, "orphan", 1, Some(99), "select 3"),
            ],
            ..Default::default()
        }
    }

    fn names(rows: &[Row], l: &Library) -> Vec<String> {
        rows.iter()
            .map(|r| match &r.node {
                Node::Folder(id) => format!("{}[{}]/{}", l.folders.iter().find(|f| f.id == *id).unwrap().name, r.count, r.depth),
                Node::Script(id) => format!("{}/{}", l.scripts.iter().find(|s| s.id == *id).unwrap().name, r.depth),
                n => format!("{n:?}"),
            })
            .collect()
    }

    fn open_all() -> HashSet<Node> {
        [1, 2, 3].map(Node::Folder).into_iter().collect()
    }

    #[test]
    fn folders_come_before_scripts_and_closed_ones_hide_their_contents() {
        let l = lib();
        let rows = flatten(&l, &Scans::new(), None, None, "", &HashSet::new());
        assert_eq!(names(&rows, &l), ["Reports[3]/0", "Scratch[0]/0", "loose/0", "orphan/0"]);
        let rows = flatten(&l, &Scans::new(), None, None, "", &open_all());
        assert_eq!(names(&rows, &l), ["Reports[3]/0", "Weekly[1]/1", "revenue/2", "top orders/1", "elsewhere/1", "Scratch[0]/0", "loose/0", "orphan/0"]);
    }

    #[test]
    fn the_scope_keeps_one_connection_and_every_folder() {
        let l = lib();
        let rows = flatten(&l, &Scans::new(), Some(1), None, "", &open_all());
        assert_eq!(names(&rows, &l), ["Reports[2]/0", "Weekly[1]/1", "revenue/2", "top orders/1", "Scratch[0]/0", "loose/0", "orphan/0"]);
    }

    #[test]
    fn a_search_opens_the_way_to_a_match_and_hides_the_rest() {
        let l = lib();
        let open = HashSet::new();
        // The name, the body and the case are all matched.
        let rows = flatten(&l, &Scans::new(), None, None, "SUM(", &open);
        assert_eq!(names(&rows, &l), ["Reports[1]/0", "Weekly[1]/1", "revenue/2"]);
        assert!(rows[2].hit && !rows[0].hit);
        let rows = flatten(&l, &Scans::new(), None, None, "orders", &open);
        assert_eq!(names(&rows, &l), ["Reports[1]/0", "top orders/1"]);
        // The search follows the scope, and an empty folder never matches.
        assert!(flatten(&l, &Scans::new(), Some(2), None, "orders", &open).is_empty());
        assert!(open.is_empty());
    }

    #[test]
    fn only_the_first_200_characters_of_a_body_are_searched() {
        let mut l = lib();
        l.scripts[3].body = format!("{}needle", "x".repeat(200));
        assert!(flatten(&l, &Scans::new(), None, None, "needle", &HashSet::new()).is_empty());
        l.scripts[3].body = format!("{}needle", "x".repeat(190));
        assert_eq!(flatten(&l, &Scans::new(), None, None, "needle", &HashSet::new()).len(), 1);
    }

    fn scan_of(files: &[&str]) -> Scan {
        let entry = |rel: &str| Entry { rel: rel.into(), mtime: std::time::SystemTime::UNIX_EPOCH, len: 1, kind: if rel.ends_with(".js") { "shell" } else if rel.ends_with(".json") { "filter" } else { "sql" }.into(), head: String::new(), target: Default::default() };
        Scan { entries: files.iter().map(|f| entry(f)).collect(), ..Default::default() }
    }

    #[test]
    fn a_connection_sees_the_files_it_can_run_and_the_links_for_its_engine() {
        use crate::db::sql::Engine;
        let mut l = lib();
        l.links.push(Link { id: 50, path: "/q".into(), on: "engine".into(), engine: "mysql".into(), ..Default::default() });
        l.links.push(Link { id: 51, path: "/p".into(), on: "all".into(), ..Default::default() });
        let mut scans = Scans::new();
        scans.insert(50, scan_of(&["a.sql", "b.js", "c.json"]));
        scans.insert(51, scan_of(&["a.sql", "b.js", "c.json"]));
        let open: HashSet<Node> = [Node::Link(50), Node::Link(51)].into_iter().collect();
        let files = |conn, engine| flatten(&l, &scans, conn, engine, "", &open).iter().filter_map(|r| if let Node::File { link, rel } = &r.node { Some(format!("{link}:{}", rel.display())) } else { None }).collect::<Vec<_>>();
        assert_eq!(files(Some(1), Some(Engine::MySql)), ["50:a.sql", "50:b.js", "51:a.sql", "51:b.js"]);
        // MongoDB: the MySQL link is gone, and of the other one only .js and .json remain.
        assert_eq!(files(Some(1), Some(Engine::Mongo)), ["51:b.js", "51:c.json"]);
        assert_eq!(files(None, None).len(), 6);
    }

    #[test]
    fn a_missing_link_and_an_empty_folder_still_show() {
        let mut l = lib();
        l.links.push(Link { id: 50, path: "/q".into(), on: "all".into(), ..Default::default() });
        let mut scans = Scans::new();
        scans.insert(50, Scan { missing: true, ..Default::default() });
        let open: HashSet<Node> = [Node::Link(50)].into_iter().collect();
        let rows = flatten(&l, &scans, None, None, "", &open);
        assert_eq!((rows[0].node.clone(), rows[0].open, rows[0].count), (Node::Link(50), false, 0));
        assert_eq!(rows.iter().filter(|r| matches!(r.node, Node::Dir { .. })).count(), 0);
        scans.insert(50, Scan { empty_dirs: vec!["new".into()], ..scan_of(&["a.sql"]) });
        let rows = flatten(&l, &scans, None, None, "", &open);
        assert!(rows.iter().any(|r| r.node == Node::Dir { link: 50, rel: "new".into() }));
        assert!(!flatten(&l, &scans, None, None, "a.sql", &HashSet::new()).iter().any(|r| matches!(r.node, Node::Dir { .. })));
    }

    #[test]
    fn linked_folders_come_first_with_their_files_below() {
        let mut l = lib();
        l.links.push(Link { id: 50, path: "/q".into(), name: "repo".into(), on: "all".into(), ..Default::default() });
        let mut scans = Scans::new();
        scans.insert(50, scan_of(&["b.sql", "a.sql", "reports/c.sql"]));
        let mut open: HashSet<Node> = [Node::Link(50)].into_iter().collect();
        let rows = flatten(&l, &scans, Some(1), None, "", &open);
        assert_eq!((rows[0].node.clone(), rows[0].count), (Node::Link(50), 3));
        let kinds: Vec<_> = rows[1..4].iter().map(|r| (r.node.clone(), r.depth)).collect();
        assert_eq!(kinds, [(Node::Dir { link: 50, rel: "reports".into() }, 1), (Node::File { link: 50, rel: "a.sql".into() }, 1), (Node::File { link: 50, rel: "b.sql".into() }, 1)]);
        open.insert(Node::Dir { link: 50, rel: "reports".into() });
        assert_eq!(flatten(&l, &scans, Some(1), None, "", &open).iter().filter(|r| matches!(r.node, Node::File { .. })).count(), 3);
        let rows = flatten(&l, &scans, Some(1), None, "c.sql", &HashSet::new());
        assert_eq!(rows.iter().filter(|r| matches!(r.node, Node::File { .. })).count(), 1);
    }

    #[test]
    fn a_link_for_other_connections_is_left_out() {
        let mut l = lib();
        l.links.push(Link { id: 50, on: "conns".into(), conns: vec![7], ..Default::default() });
        assert!(flatten(&l, &Scans::new(), Some(1), None, "", &HashSet::new()).iter().all(|r| r.node != Node::Link(50)));
        assert!(flatten(&l, &Scans::new(), Some(7), None, "", &HashSet::new()).iter().any(|r| r.node == Node::Link(50)));
    }

    #[test]
    fn a_link_with_an_unknown_scope_shows_only_where_its_files_could_run() {
        let mut l = lib();
        l.links.push(Link { id: 50, on: "team".into(), conns: vec![7], ..Default::default() });
        let shows = |conn| flatten(&l, &Scans::new(), conn, None, "", &HashSet::new()).iter().any(|r| r.node == Node::Link(50));
        // Before, any scope it did not know showed it on every connection, though its files would not open there.
        assert!(!shows(Some(1)));
        assert!(shows(Some(7)) && shows(None));
    }

    #[test]
    fn drop_positions_split_rows_by_height() {
        assert_eq!(drop_at(true, false, 0.1), Pos::Before);
        assert_eq!(drop_at(true, false, 0.5), Pos::Into);
        assert_eq!(drop_at(true, false, 0.9), Pos::After);
        // The first child of an open folder is right below it, so its lower edge is "into".
        assert_eq!(drop_at(true, true, 0.9), Pos::Into);
        assert_eq!((drop_at(false, false, 0.2), drop_at(false, false, 0.8)), (Pos::Before, Pos::After));
    }
}
