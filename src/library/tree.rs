//! The rows of the Library panel as a flat list, so the panel draws them with `show_rows` and the keyboard walks them
//! without touching the folders. Everything here is pure: the open folders, the scope and the search come in as values.

use super::*;
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

/// The files B2 found below a link, as paths relative to it with the kind each one runs as.
#[derive(Clone, Debug, Default)]
pub struct Scan {
    pub files: Vec<ScanFile>,
}

#[derive(Clone, Debug, Default)]
pub struct ScanFile {
    pub rel: PathBuf,
    pub kind: String,
}

/// The scans by link id. Empty until linked folders exist.
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

/// A link shows on a connection unless it is limited to other connections. A link for an engine shows everywhere here:
/// the panel only knows ids, and B2 narrows it where the engine is known.
fn link_visible(l: &Link, conn: Option<u64>) -> bool {
    match conn {
        Some(c) if l.on == "conns" => l.conns.contains(&c),
        _ => true,
    }
}

struct Ctx<'a> {
    lib: &'a Library,
    scans: &'a Scans,
    conn: Option<u64>,
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

    fn links(&self, out: &mut Vec<Row>) {
        for l in self.lib.links.iter().filter(|l| link_visible(l, self.conn)) {
            let Some(scan) = self.scans.get(&l.id) else {
                // Not scanned yet: the row shows, empty, unless a search is on.
                if self.q.is_empty() {
                    out.push(Row { node: Node::Link(l.id), depth: 0, open: false, hit: false, count: 0 });
                }
                continue;
            };
            let count = scan.files.iter().filter(|f| self.name_matches(&f.rel)).count();
            if !self.q.is_empty() && count == 0 {
                continue;
            }
            let node = Node::Link(l.id);
            let open = !self.q.is_empty() || self.open.contains(&node);
            out.push(Row { node, depth: 0, open, hit: false, count });
            if open {
                self.dir_rows(l.id, scan, std::path::Path::new(""), 1, out);
            }
        }
    }

    fn dir_rows(&self, link: u64, scan: &Scan, prefix: &std::path::Path, depth: usize, out: &mut Vec<Row>) {
        if depth > 64 {
            return;
        }
        let mut dirs = BTreeSet::new();
        let mut files = Vec::new();
        for f in &scan.files {
            let Ok(rest) = f.rel.strip_prefix(prefix) else { continue };
            let mut parts = rest.components();
            let (Some(first), second) = (parts.next(), parts.next()) else { continue };
            if second.is_some() {
                dirs.insert(PathBuf::from(first.as_os_str()));
            } else if self.name_matches(&f.rel) {
                files.push(f.rel.clone());
            }
        }
        files.sort();
        for d in dirs {
            let rel = prefix.join(d);
            let count = scan.files.iter().filter(|f| f.rel.starts_with(&rel) && self.name_matches(&f.rel)).count();
            if count == 0 {
                continue;
            }
            let node = Node::Dir { link, rel: rel.clone() };
            let open = !self.q.is_empty() || self.open.contains(&node);
            out.push(Row { node, depth, open, hit: false, count });
            if open {
                self.dir_rows(link, scan, &rel, depth + 1, out);
            }
        }
        let hit = !self.q.is_empty();
        out.extend(files.into_iter().map(|rel| Row { node: Node::File { link, rel }, depth, open: false, hit, count: 0 }));
    }
}

/// The visible rows. Order: linked folders, then Quarry folders (subfolders before scripts), then scripts at the top.
/// `conn` is the scope: Some(id) shows the scripts of that connection, None shows all of them. Folders are shown either
/// way, empty ones included. A search matches the name and the first 200 characters of the body, forces the folders
/// above a match open and hides the rest, without touching `open`.
pub fn flatten(lib: &Library, scans: &Scans, conn: Option<u64>, query: &str, open: &HashSet<Node>) -> Vec<Row> {
    let cx = Ctx { lib, scans, conn, q: query.trim().to_lowercase(), open };
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
        let rows = flatten(&l, &Scans::new(), None, "", &HashSet::new());
        assert_eq!(names(&rows, &l), ["Reports[3]/0", "Scratch[0]/0", "loose/0", "orphan/0"]);
        let rows = flatten(&l, &Scans::new(), None, "", &open_all());
        assert_eq!(names(&rows, &l), ["Reports[3]/0", "Weekly[1]/1", "revenue/2", "top orders/1", "elsewhere/1", "Scratch[0]/0", "loose/0", "orphan/0"]);
    }

    #[test]
    fn the_scope_keeps_one_connection_and_every_folder() {
        let l = lib();
        let rows = flatten(&l, &Scans::new(), Some(1), "", &open_all());
        assert_eq!(names(&rows, &l), ["Reports[2]/0", "Weekly[1]/1", "revenue/2", "top orders/1", "Scratch[0]/0", "loose/0", "orphan/0"]);
    }

    #[test]
    fn a_search_opens_the_way_to_a_match_and_hides_the_rest() {
        let l = lib();
        let open = HashSet::new();
        // The name, the body and the case are all matched.
        let rows = flatten(&l, &Scans::new(), None, "SUM(", &open);
        assert_eq!(names(&rows, &l), ["Reports[1]/0", "Weekly[1]/1", "revenue/2"]);
        assert!(rows[2].hit && !rows[0].hit);
        let rows = flatten(&l, &Scans::new(), None, "orders", &open);
        assert_eq!(names(&rows, &l), ["Reports[1]/0", "top orders/1"]);
        // The search follows the scope, and an empty folder never matches.
        assert!(flatten(&l, &Scans::new(), Some(2), "orders", &open).is_empty());
        assert!(open.is_empty());
    }

    #[test]
    fn only_the_first_200_characters_of_a_body_are_searched() {
        let mut l = lib();
        l.scripts[3].body = format!("{}needle", "x".repeat(200));
        assert!(flatten(&l, &Scans::new(), None, "needle", &HashSet::new()).is_empty());
        l.scripts[3].body = format!("{}needle", "x".repeat(190));
        assert_eq!(flatten(&l, &Scans::new(), None, "needle", &HashSet::new()).len(), 1);
    }

    #[test]
    fn linked_folders_come_first_with_their_files_below() {
        let mut l = lib();
        l.links.push(Link { id: 50, path: "/q".into(), name: "repo".into(), on: "all".into(), ..Default::default() });
        let file = |rel: &str| ScanFile { rel: rel.into(), kind: "sql".into() };
        let mut scans = Scans::new();
        scans.insert(50, Scan { files: vec![file("b.sql"), file("a.sql"), file("reports/c.sql")] });
        let mut open: HashSet<Node> = [Node::Link(50)].into_iter().collect();
        let rows = flatten(&l, &scans, Some(1), "", &open);
        assert_eq!((rows[0].node.clone(), rows[0].count), (Node::Link(50), 3));
        let kinds: Vec<_> = rows[1..4].iter().map(|r| (r.node.clone(), r.depth)).collect();
        assert_eq!(kinds, [(Node::Dir { link: 50, rel: "reports".into() }, 1), (Node::File { link: 50, rel: "a.sql".into() }, 1), (Node::File { link: 50, rel: "b.sql".into() }, 1)]);
        open.insert(Node::Dir { link: 50, rel: "reports".into() });
        assert_eq!(flatten(&l, &scans, Some(1), "", &open).iter().filter(|r| matches!(r.node, Node::File { .. })).count(), 3);
        let rows = flatten(&l, &scans, Some(1), "c.sql", &HashSet::new());
        assert_eq!(rows.iter().filter(|r| matches!(r.node, Node::File { .. })).count(), 1);
    }

    #[test]
    fn a_link_for_other_connections_is_left_out() {
        let mut l = lib();
        l.links.push(Link { id: 50, on: "conns".into(), conns: vec![7], ..Default::default() });
        assert!(flatten(&l, &Scans::new(), Some(1), "", &HashSet::new()).iter().all(|r| r.node != Node::Link(50)));
        assert!(flatten(&l, &Scans::new(), Some(7), "", &HashSet::new()).iter().any(|r| r.node == Node::Link(50)));
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
