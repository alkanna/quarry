//! Connection groups: named, colored folders of saved connections, kept in groups.json. The file is apart from
//! connections.json because an older Quarry rewrites that file and drops the fields it does not know. The functions are
//! pure, so the rules (one group per connection, what a drop does to the order) are tested without a window.
#![allow(dead_code)]

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
    let mut list = |k: &str| match o.remove(k) {
        Some(serde_json::Value::Array(a)) => a,
        _ => Vec::new(),
    };
    let (groups, loose) = (list("groups"), list("loose"));
    let total = groups.len() + loose.len();
    let (groups, mut errs) = parse_entries::<Group>(groups);
    let (loose, e) = parse_entries::<u64>(loose);
    errs.extend(e);
    let v = o.get("v").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    (Groups { v, groups, loose }, skipped_note(path, total, &errs))
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
