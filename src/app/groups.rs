//! Connection groups: named, colored folders of saved connections, kept in groups.json. The file is apart from
//! connections.json because an older Quarry rewrites that file and drops the fields it does not know. The functions are
//! pure, so the rules (one group per connection, what a drop does to the order) are tested without a window.

use super::*;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub(super) struct Groups {
    pub(super) v: u32,
    pub(super) groups: Vec<Group>,
    /// Order of the connections that are in no group. One that is missing here follows the listed ones, in saved order.
    pub(super) loose: Vec<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub(super) struct Group {
    pub(super) id: u64,
    pub(super) name: String,
    /// A name from `GROUP_COLORS`; an unknown one draws gray.
    pub(super) color: String,
    /// The group folds into one tile in the rail.
    pub(super) fold_rail: bool,
    /// Shown open in the Connections panel.
    pub(super) open: bool,
    pub(super) members: Vec<u64>,
}

/// Where `Groups::place` puts a connection. The index counts the members (or the loose connections) that stay after
/// the connection is taken out of where it was, so a move inside one list reads the same as a move into another.
/// None is the end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Place {
    Into(u64, Option<usize>),
    Loose(Option<usize>),
}

impl Groups {
    pub(super) fn group_of(&self, conn: u64) -> Option<&Group> {
        self.groups.iter().find(|g| g.members.contains(&conn))
    }

    pub(super) fn group(&self, id: u64) -> Option<&Group> {
        self.groups.iter().find(|g| g.id == id)
    }

    pub(super) fn group_mut(&mut self, id: u64) -> Option<&mut Group> {
        self.groups.iter_mut().find(|g| g.id == id)
    }

    /// The connections in no group, in the order the panel shows them: the saved `loose` order first, then the rest of
    /// `all` (every connection in saved order), so a connection added since the last save still has a place.
    pub(super) fn loose_order(&self, all: &[u64]) -> Vec<u64> {
        let grouped = |c: &u64| self.groups.iter().any(|g| g.members.contains(c));
        let mut out: Vec<u64> = Vec::new();
        for c in self.loose.iter().chain(all) {
            if all.contains(c) && !grouped(c) && !out.contains(c) {
                out.push(*c);
            }
        }
        out
    }

    /// Moves `conn` to `to`, out of the group it was in. A connection is in at most one group. `all` is every connection,
    /// for `loose_order`. Returns false, changing nothing, when the group is gone.
    pub(super) fn place(&mut self, conn: u64, to: Place, all: &[u64]) -> bool {
        if let Place::Into(g, _) = to
            && self.group(g).is_none()
        {
            return false;
        }
        // The loose list is made complete first, so an index into it means the same row the person dropped on.
        let mut loose = self.loose_order(all);
        loose.retain(|c| *c != conn);
        for g in &mut self.groups {
            g.members.retain(|c| *c != conn);
        }
        match to {
            Place::Into(g, at) => {
                let members = &mut self.group_mut(g).expect("checked above").members;
                members.insert(at.unwrap_or(members.len()).min(members.len()), conn);
            }
            Place::Loose(at) => loose.insert(at.unwrap_or(loose.len()).min(loose.len()), conn),
        }
        self.loose = loose;
        true
    }

    /// Adds an empty group named `name` in the first color no group has, or in turn once all eight are used. Returns its id.
    pub(super) fn new_group(&mut self, name: &str) -> u64 {
        let id = self.groups.iter().map(|g| g.id).max().unwrap_or(0) + 1;
        let color = GROUP_COLORS.iter().map(|c| c.0).find(|c| !self.groups.iter().any(|g| g.color == *c)).unwrap_or(GROUP_COLORS[self.groups.len() % GROUP_COLORS.len()].0);
        self.groups.push(Group { id, name: name.into(), color: color.into(), fold_rail: true, open: true, members: Vec::new() });
        id
    }

    /// Puts a deleted group back at `at`, with the members that still exist taking it over from wherever they went since.
    /// It keeps its id unless a group made since took it. Returns the id.
    pub(super) fn restore(&mut self, mut g: Group, at: usize, alive: &[u64]) -> u64 {
        if self.group(g.id).is_some() {
            g.id = self.groups.iter().map(|x| x.id).max().unwrap_or(0) + 1;
        }
        g.members.retain(|c| alive.contains(c));
        for o in &mut self.groups {
            o.members.retain(|c| !g.members.contains(c));
        }
        self.loose.retain(|c| !g.members.contains(c));
        let id = g.id;
        self.groups.insert(at.min(self.groups.len()), g);
        id
    }

    /// Where `conn` stands: its group and its index among the members, or no group and its index in `loose_order`. Put
    /// back with `place`, the index reads the same once the connection has left wherever it went since.
    pub(super) fn spot(&self, conn: u64, all: &[u64]) -> (Option<u64>, usize) {
        match self.group_of(conn) {
            Some(g) => (Some(g.id), g.members.iter().position(|c| *c == conn).unwrap_or(0)),
            None => (None, self.loose_order(all).iter().position(|c| *c == conn).unwrap_or(usize::MAX)),
        }
    }

    /// Undoes one drop: the connections it moved go back to the spots `spot` gave before it, and the group it made goes
    /// when nothing else has joined it since. Every other change made since (a rename, a color, another move) stays.
    pub(super) fn undo_drop(&mut self, moved: &[(u64, Option<u64>, usize)], created: Option<u64>, all: &[u64]) {
        if let Some(i) = created.and_then(|g| self.groups.iter().position(|x| x.id == g))
            && self.groups[i].members.iter().all(|c| moved.iter().any(|m| m.0 == *c))
        {
            self.groups.remove(i);
        }
        // Lowest index first, so two tiles from one list each land at their old index.
        let mut moved = moved.to_vec();
        moved.sort_by_key(|m| m.2);
        for (conn, group, at) in moved {
            if !all.contains(&conn) {
                continue;
            }
            let to = match group {
                Some(g) if self.group(g).is_some() => Place::Into(g, Some(at)),
                // Its old group was deleted since: it goes back among the loose ones.
                Some(_) => Place::Loose(None),
                None => Place::Loose(Some(at)),
            };
            self.place(conn, to, all);
        }
    }

    /// Mends a hand-edited file: a repeated group id gets a new one, a connection listed twice keeps its first place (in
    /// the first group that names it), and a grouped connection leaves `loose`. Everything else relies on one group per
    /// connection, so the panel and the rail would otherwise show it twice.
    pub(super) fn normalize(&mut self) {
        let mut ids = HashSet::new();
        let mut next = self.groups.iter().map(|g| g.id).max().unwrap_or(0) + 1;
        let mut seen = HashSet::new();
        for g in &mut self.groups {
            if !ids.insert(g.id) {
                g.id = next;
                ids.insert(next);
                next += 1;
            }
            g.members.retain(|c| seen.insert(*c));
        }
        self.loose.retain(|c| seen.insert(*c));
    }

    /// Drops connections that are gone from every list. Returns whether anything changed, so the caller saves.
    pub(super) fn prune(&mut self, alive: &[u64]) -> bool {
        let before = (self.groups.iter().map(|g| g.members.len()).sum::<usize>(), self.loose.len());
        for g in &mut self.groups {
            g.members.retain(|c| alive.contains(c));
        }
        self.loose.retain(|c| alive.contains(c));
        before != (self.groups.iter().map(|g| g.members.len()).sum::<usize>(), self.loose.len())
    }
}

/// Reads groups.json. The file is an object, and its lists are parsed entry by entry: a bad group is skipped and the
/// file copied to `.bak`, never the other groups lost. A file that is not an object at all is moved aside by `parse_config`.
pub(super) fn load_groups_from(path: &std::path::Path) -> (Groups, Option<String>) {
    let Ok(b) = std::fs::read(path) else { return (Groups::default(), None) };
    let Ok(serde_json::Value::Object(mut o)) = serde_json::from_slice::<serde_json::Value>(&b) else { return parse_config(path) };
    // A list key holding something else counts as a skipped entry, so the file is copied aside before the next save drops it.
    let mut bad = Vec::new();
    let mut list = |k: &str| match o.remove(k) {
        Some(serde_json::Value::Array(a)) => a,
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(_) => {
            bad.push(format!("{k} is not a list"));
            Vec::new()
        }
    };
    let (groups, loose) = (list("groups"), list("loose"));
    let total = groups.len() + loose.len() + bad.len();
    let (groups, mut errs) = parse_entries::<Group>(groups);
    let (loose, e) = parse_entries::<u64>(loose);
    errs.extend(e);
    errs.extend(bad);
    let v = o.get("v").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let mut g = Groups { v, groups, loose };
    g.normalize();
    (g, skipped_note(path, total, &errs))
}

pub(super) fn load_groups() -> (Groups, Option<String>) {
    load_groups_from(&config_dir().join("groups.json"))
}

impl App {
    pub(super) fn save_groups(&mut self) {
        self.groups.v = 1;
        let b = serde_json::to_vec_pretty(&self.groups).unwrap_or_default();
        self.write_config("groups.json", b);
    }

    pub(super) fn conn_ids(&self) -> Vec<u64> {
        self.conns.iter().map(|c| c.meta.id).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two() -> Groups {
        let mut g = Groups::default();
        let (a, b) = (g.new_group("Prod"), g.new_group("Dev"));
        let all = [1, 2, 3, 4, 5];
        g.place(1, Place::Into(a, None), &all);
        g.place(2, Place::Into(a, None), &all);
        g.place(3, Place::Into(b, None), &all);
        g
    }

    #[test]
    fn undoing_a_drop_moves_back_only_what_it_moved() {
        let mut g = two();
        let all = [1, 2, 3, 4, 5];
        // 4 and 5 were loose; a drop of 5 on 4 made a group of them.
        let moved = [(4, None, g.spot(4, &all).1), (5, None, g.spot(5, &all).1)];
        let new = g.new_group("New group");
        g.place(4, Place::Into(new, None), &all);
        g.place(5, Place::Into(new, None), &all);
        // Later edits while the toast is up: a rename, and a move of 3 into the new group.
        g.group_mut(1).unwrap().name = "Production".into();
        g.place(3, Place::Into(new, None), &all);
        g.undo_drop(&moved, Some(new), &all);
        assert_eq!(g.group(1).map(|x| x.name.as_str()), Some("Production"));
        // The new group keeps 3, which joined it since, and 4 and 5 are loose again in their old order.
        assert_eq!(g.group(new).map(|x| x.members.clone()), Some(vec![3]));
        assert_eq!(g.loose_order(&all), [4, 5]);
        // Without later members the group made by the drop goes.
        let mut g = two();
        let moved = [(1, Some(1), 0), (4, None, g.spot(4, &all).1)];
        let new = g.new_group("New group");
        g.place(4, Place::Into(new, None), &all);
        g.place(1, Place::Into(new, None), &all);
        g.undo_drop(&moved, Some(new), &all);
        assert!(g.group(new).is_none());
        assert_eq!((g.groups[0].members.clone(), g.loose_order(&all)), (vec![1, 2], vec![4, 5]));
    }

    #[test]
    fn a_connection_is_in_one_group() {
        let mut g = two();
        let all = [1, 2, 3, 4, 5];
        assert_eq!(g.group_of(1).map(|x| x.name.as_str()), Some("Prod"));
        assert!(g.group_of(4).is_none());
        g.place(1, Place::Into(2, Some(0)), &all);
        assert_eq!((g.groups[0].members.clone(), g.groups[1].members.clone()), (vec![2], vec![1, 3]));
        g.place(1, Place::Loose(None), &all);
        assert_eq!(g.group_of(1).map(|x| x.id), None);
        assert_eq!(g.loose, [4, 5, 1]);
    }

    #[test]
    fn the_index_counts_the_rest_after_the_connection_leaves() {
        let mut g = two();
        let all = [1, 2, 3, 4, 5];
        // Dragging 1 below 2 inside Prod: after 1 leaves, 2 is at 0, so the slot is 1.
        g.place(1, Place::Into(1, Some(1)), &all);
        assert_eq!(g.groups[0].members, [2, 1]);
        // An index past the end is the end, and a missing group changes nothing.
        g.place(5, Place::Into(1, Some(99)), &all);
        assert_eq!(g.groups[0].members, [2, 1, 5]);
        assert!(!g.place(4, Place::Into(77, None), &all));
        assert_eq!(g.loose_order(&all), [4]);
    }

    #[test]
    fn loose_order_keeps_saved_order_and_adds_new_connections() {
        let mut g = Groups { loose: vec![9, 4, 2, 4], ..Default::default() };
        let all = [1, 2, 4, 5];
        // 9 is gone, 4 repeats once, and 1 and 5 follow in saved order.
        assert_eq!(g.loose_order(&all), [4, 2, 1, 5]);
        let id = g.new_group("G");
        g.place(4, Place::Into(id, None), &all);
        assert_eq!(g.loose_order(&all), [2, 1, 5]);
    }

    #[test]
    fn new_groups_take_the_next_unused_color_then_cycle() {
        let mut g = Groups::default();
        for i in 0..9 {
            g.new_group(&format!("g{i}"));
        }
        let colors: Vec<&str> = g.groups.iter().map(|x| x.color.as_str()).collect();
        assert_eq!(&colors[..3], ["red", "amber", "green"]);
        assert_eq!(colors[8], GROUP_COLORS[8 % GROUP_COLORS.len()].0);
        let ids: HashSet<u64> = g.groups.iter().map(|x| x.id).collect();
        assert_eq!(ids.len(), 9);
        // A deleted group's id is not reused while a later one exists.
        g.groups.remove(0);
        assert_eq!(g.new_group("again"), 10);
    }

    #[test]
    fn prune_drops_removed_connections_from_every_list() {
        let mut g = two();
        g.loose = vec![4, 5];
        assert!(g.prune(&[1, 3, 5]));
        assert_eq!((g.groups[0].members.clone(), g.groups[1].members.clone(), g.loose.clone()), (vec![1], vec![3], vec![5]));
        assert!(!g.prune(&[1, 3, 5]));
    }

    #[test]
    fn a_restored_group_takes_its_members_back_and_keeps_its_place() {
        let mut g = two();
        let all = [1, 2, 3];
        let gone = g.groups.remove(0);
        g.loose = vec![1, 2];
        // 2 has left the app since, 1 has joined the other group, and a new group took the old id.
        g.place(1, Place::Into(2, None), &all);
        let taken = g.new_group("New");
        assert_eq!(taken, 3);
        let id = g.restore(Group { id: 3, ..gone }, 0, &[1, 3]);
        assert_ne!(id, 3);
        assert_eq!(g.groups.iter().map(|x| (x.name.as_str(), x.members.clone())).collect::<Vec<_>>(), [("Prod", vec![1]), ("Dev", vec![3]), ("New", vec![])]);
        assert!(!g.loose.contains(&1));
    }

    #[test]
    fn a_bad_group_is_skipped_and_the_file_kept() {
        let dir = std::env::temp_dir().join(format!("quarry-groups-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("groups.json");
        std::fs::write(&p, r#"{"v":1,"groups":[{"id":1,"name":"Prod","color":"red","members":[1,2]},{"id":"x","name":"bad"},{"id":3,"name":"Dev","extra":true}],"loose":[5,"six",7]}"#).unwrap();
        let (g, note) = load_groups_from(&p);
        assert_eq!(g.groups.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["Prod", "Dev"]);
        assert_eq!(g.loose, [5, 7]);
        assert!(note.is_some_and(|n| n.contains("Skipped 2 of 6")));
        assert!(p.exists() && dir.join("groups.json.bak").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_hand_edited_file_keeps_one_group_per_connection() {
        let dir = std::env::temp_dir().join(format!("quarry-groups-norm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("groups.json");
        // 2 is in two groups and loose, 1 is listed twice in its group, and two groups share id 1.
        std::fs::write(&p, r#"{"v":1,"groups":[{"id":1,"name":"Prod","members":[1,2,1]},{"id":1,"name":"Dev","members":[2,3]}],"loose":[2,4,4,3]}"#).unwrap();
        let (g, note) = load_groups_from(&p);
        assert!(note.is_none());
        assert_eq!(g.groups.iter().map(|x| (x.id, x.members.clone())).collect::<Vec<_>>(), [(1, vec![1, 2]), (2, vec![3])]);
        assert_eq!(g.loose, [4]);
        let all = [1, 2, 3, 4];
        assert_eq!(g.group_of(2).map(|x| x.name.as_str()), Some("Prod"));
        assert_eq!(g.loose_order(&all), [4]);
        let rows = crate::app::conns_panel::panel_rows(&all.map(|id| Conn::new(ConnMeta { id, ..Default::default() }, Status::Idle)), &g, "", |_| true);
        let shown = rows.iter().filter(|r| matches!(r, crate::app::conns_panel::CRow::Conn { .. })).count();
        assert_eq!(shown, 4);
        // A list key of another type is kept aside like a bad entry.
        std::fs::write(&p, r#"{"v":1,"groups":{"id":1},"loose":[1]}"#).unwrap();
        let (g, note) = load_groups_from(&p);
        assert_eq!(g.loose, [1]);
        assert!(note.is_some_and(|n| n.contains("groups is not a list")));
        assert!(dir.join("groups.json.bak").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_file_is_a_fresh_start_and_a_broken_one_is_moved_aside() {
        let dir = std::env::temp_dir().join(format!("quarry-groups-broken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("groups.json");
        let (g, note) = load_groups_from(&p);
        assert!(g == Groups::default() && note.is_none());
        std::fs::write(&p, "[{").unwrap();
        let (g, note) = load_groups_from(&p);
        assert!(g == Groups::default() && note.is_some());
        assert!(!p.exists() && dir.join("groups.json.bak").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
