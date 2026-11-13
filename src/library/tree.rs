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

    /// Whether a folder or anything below it holds a script of any connection, in or out of scope.
    fn holds_any(&self, folder: u64, depth: usize) -> bool {
        depth <= 64
            && (self.lib.scripts.iter().any(|s| live_parent(self.lib, s.folder) == Some(folder))
                || self.lib.folders.iter().filter(|f| live_parent(self.lib, f.parent) == Some(folder)).any(|f| self.holds_any(f.id, depth + 1)))
    }

    fn folders(&self, parent: Option<u64>, depth: usize, out: &mut Vec<Row>) {
        if depth > 64 {
            return;
        }
        let searching = !self.q.is_empty();
        for f in self.lib.folders.iter().filter(|f| live_parent(self.lib, f.parent) == parent) {
            let count = self.count(f.id, 0);
            // Empty folders stay visible, except while searching, when only the paths to a match are. Folders are shared by
            // every connection, so a connection's view leaves out those whose scripts all belong to other connections; a
            // folder with no script at all stays, so one just made never vanishes.
            if count == 0 && (searching || (self.conn.is_some() && self.holds_any(f.id, 0))) {
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

/// Where rows `h` tall sit while folders open or close: each row's top, the line below which it is hidden (the bottom of
/// the folder still moving around it), and how far open that folder is, for a fade. A folder `f` open takes `f` of the
/// height of what it holds and the rows after it move up by the rest. The rows are the drawn ones, so a closing
/// folder's children are in them until it has closed.
#[derive(Debug, Default)]
pub struct Laid {
    pub top: Vec<f32>,
    pub clip: Vec<f32>,
    pub fade: Vec<f32>,
    pub height: f32,
}

/// Lays out `rows`, given how far open each moving folder is (`frac`, None for one at rest).
pub fn lay_out(rows: &[Row], h: f32, frac: impl Fn(&Node) -> Option<f32>) -> Laid {
    let n = rows.len();
    let mut out = Laid { top: vec![0.0; n], clip: vec![f32::INFINITY; n], fade: vec![1.0; n], height: 0.0 };
    out.height = lay(rows, 0, None, 0.0, h, &frac, &mut out).1;
    out
}

/// Lays out the rows from `i` on that sit below `parent`'s depth, from `y0` down. Returns where they end and their height.
fn lay(rows: &[Row], mut i: usize, parent: Option<usize>, y0: f32, h: f32, frac: &dyn Fn(&Node) -> Option<f32>, out: &mut Laid) -> (usize, f32) {
    let mut y = y0;
    while i < rows.len() && parent.is_none_or(|d| rows[i].depth > d) {
        let depth = rows[i].depth;
        let f = frac(&rows[i].node).unwrap_or(1.0).clamp(0.0, 1.0);
        out.top[i] = y;
        y += h;
        i += 1;
        if i < rows.len() && rows[i].depth > depth {
            let (next, full) = lay(rows, i, Some(depth), y, h, frac, out);
            if f < 1.0 {
                let bottom = y + full * f;
                for k in i..next {
                    out.clip[k] = out.clip[k].min(bottom);
                    out.fade[k] *= f;
                }
            }
            y += full * f;
            i = next;
        }
    }
    (i, y - y0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moving_folder_takes_its_share_of_what_it_holds_and_the_rows_below_follow() {
        let row = |id: u64, depth: usize| Row { node: Node::Folder(id), depth, open: true, hit: false, count: 0 };
        // 1 holds 2 (which holds 3) and 4; 5 is at the top level after it.
        let rows = [row(1, 0), row(2, 1), row(3, 2), row(4, 1), row(5, 0)];
        let at_rest = lay_out(&rows, 10.0, |_| None);
        assert_eq!(at_rest.top, vec![0.0, 10.0, 20.0, 30.0, 40.0]);
        assert_eq!(at_rest.height, 50.0);
        assert!(at_rest.clip.iter().all(|c| c.is_infinite()) && at_rest.fade.iter().all(|f| *f == 1.0));

        let half = lay_out(&rows, 10.0, |n| (*n == Node::Folder(1)).then_some(0.5));
        // Its three rows keep their places under it, cut off half way down, and 5 moves up by the other half.
        assert_eq!(half.top, vec![0.0, 10.0, 20.0, 30.0, 25.0]);
        assert_eq!(half.clip[1..4], [25.0, 25.0, 25.0]);
        assert!(half.clip[0].is_infinite() && half.clip[4].is_infinite());
        assert_eq!(half.fade, vec![1.0, 0.5, 0.5, 0.5, 1.0]);
        assert_eq!(half.height, 35.0);

        // A folder moving inside one moving folder: 2 is half open, so 1 holds 2.5 rows and shows half of them.
        let both = lay_out(&rows, 10.0, |n| match n {
            Node::Folder(1) | Node::Folder(2) => Some(0.5),
            _ => None,
        });
        assert_eq!(both.top[3], 25.0, "4 follows 2's half open child");
        assert_eq!(both.clip[2], 22.5_f32.min(25.0), "3 is cut by the nearer of the two folds");
        assert_eq!(both.fade[2], 0.25);
        assert_eq!(both.top[4], 22.5);
        assert_eq!(both.height, 32.5);

        let shut = lay_out(&rows, 10.0, |n| (*n == Node::Folder(1)).then_some(0.0));
        assert_eq!(shut.top[4], 10.0);
        assert!(shut.clip[1..4].iter().zip(&shut.top[1..4]).all(|(c, t)| c <= t), "nothing of a closed folder shows");
    }

    fn lib() -> Library {
        let folder = |id, name: &str, parent| Folder { id, name: name.into(), parent };
        let script = |id, name: &str, conn, folder, body: &str| Script { id, name: name.into(), kind: "sql".into(), body: body.into(), conn, folder, ..Default::default() };
        Library {
            folders: vec![folder(1, "Reports", None), folder(2, "Weekly", Some(1)), folder(3, "Scratch", None), folder(4, "Theirs", None), folder(5, "Deep", Some(4))],
            scripts: vec![
                script(10, "top orders", 1, Some(1), "select * from orders"),
                script(11, "revenue", 1, Some(2), "select sum(total) from sales"),
                script(12, "elsewhere", 2, Some(1), "select 1"),
                script(13, "loose", 1, None, "select 2"),
                script(14, "orphan", 1, Some(99), "select 3"),
                script(15, "their report", 2, Some(5), "select 4"),
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
        [1, 2, 3, 4, 5].map(Node::Folder).into_iter().collect()
    }

    #[test]
    fn folders_come_before_scripts_and_closed_ones_hide_their_contents() {
        let l = lib();
        let rows = flatten(&l, &Scans::new(), None, None, "", &HashSet::new());
        assert_eq!(names(&rows, &l), ["Reports[3]/0", "Scratch[0]/0", "Theirs[1]/0", "loose/0", "orphan/0"]);
        let rows = flatten(&l, &Scans::new(), None, None, "", &open_all());
        assert_eq!(names(&rows, &l), ["Reports[3]/0", "Weekly[1]/1", "revenue/2", "top orders/1", "elsewhere/1", "Scratch[0]/0", "Theirs[1]/0", "Deep[1]/1", "their report/2", "loose/0", "orphan/0"]);
    }

    #[test]
    fn the_scope_keeps_one_connection_and_its_folders_and_the_empty_ones() {
        let l = lib();
        // Theirs and Deep hold only connection 2's script, so connection 1's view leaves them out; Scratch holds nothing and
        // stays, so a folder just made is never hidden.
        let rows = flatten(&l, &Scans::new(), Some(1), None, "", &open_all());
        assert_eq!(names(&rows, &l), ["Reports[2]/0", "Weekly[1]/1", "revenue/2", "top orders/1", "Scratch[0]/0", "loose/0", "orphan/0"]);
        let rows = flatten(&l, &Scans::new(), Some(2), None, "", &open_all());
        assert_eq!(names(&rows, &l), ["Reports[1]/0", "elsewhere/1", "Scratch[0]/0", "Theirs[1]/0", "Deep[1]/1", "their report/2"]);
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

}
