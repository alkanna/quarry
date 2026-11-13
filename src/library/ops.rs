//! Changes to the Quarry folders and scripts, free of egui so each rule is tested on its own. Linked folders live on disk,
//! so nothing here moves, renames or deletes them.

use super::tree::{Node, live_parent};
use super::*;

/// Where a moved item lands: the top level, inside a folder (at its end), or beside a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropAt {
    Root,
    Into(u64),
    Before(Node),
    After(Node),
}

const LINKED: &str = "Linked folders are kept on disk, so nothing moves into or out of them here.";

pub fn rename(lib: &mut Library, node: &Node, name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return false;
    }
    match node {
        Node::Folder(id) => lib.folders.iter_mut().find(|f| f.id == *id).map(|f| f.name = name.to_string()).is_some(),
        Node::Script(id) => lib.scripts.iter_mut().find(|s| s.id == *id).map(|s| s.name = name.to_string()).is_some(),
        _ => false,
    }
}

/// Copies a script right below the original, in the same folder. The copy is named "<name> copy", then "<name> copy 2".
pub fn duplicate(lib: &mut Library, id: u64, now: u64) -> Option<u64> {
    let at = lib.scripts.iter().position(|s| s.id == id)?;
    let mut copy = lib.scripts[at].clone();
    let base = format!("{} copy", copy.name);
    let taken = |lib: &Library, n: &str| lib.scripts.iter().any(|s| s.folder == copy.folder && s.name == n);
    copy.name = base.clone();
    let mut n = 2;
    while taken(lib, &copy.name) {
        copy.name = format!("{base} {n}");
        n += 1;
    }
    copy.id = lib.next_id();
    copy.updated = now;
    lib.scripts.insert(at + 1, copy.clone());
    Some(copy.id)
}

/// A new folder named `name` (or "New folder") under `parent`, numbered when a sibling has the name.
pub fn new_folder(lib: &mut Library, parent: Option<u64>, name: &str) -> u64 {
    let base = if name.trim().is_empty() { "New folder" } else { name.trim() };
    let taken = |lib: &Library, n: &str| lib.folders.iter().any(|f| f.parent == parent && f.name.eq_ignore_ascii_case(n));
    let mut name = base.to_string();
    let mut n = 2;
    while taken(lib, &name) {
        name = format!("{base} {n}");
        n += 1;
    }
    let id = lib.next_id();
    lib.folders.push(Folder { id, name, parent });
    id
}

/// Removes a script, or a folder with every folder and script below it, and returns what went for an Undo.
pub fn delete(lib: &mut Library, node: &Node) -> Option<(Vec<Folder>, Vec<Script>)> {
    match node {
        Node::Script(id) => {
            let at = lib.scripts.iter().position(|s| s.id == *id)?;
            Some((Vec::new(), vec![lib.scripts.remove(at)]))
        }
        Node::Folder(id) => {
            if !lib.folders.iter().any(|f| f.id == *id) {
                return None;
            }
            let mut gone: std::collections::HashSet<u64> = [*id].into_iter().collect();
            loop {
                let more: Vec<u64> = lib.folders.iter().filter(|f| f.parent.is_some_and(|p| gone.contains(&p)) && !gone.contains(&f.id)).map(|f| f.id).collect();
                if more.is_empty() {
                    break;
                }
                gone.extend(more);
            }
            let (folders, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut lib.folders).into_iter().partition(|f| gone.contains(&f.id));
            lib.folders = keep;
            let (scripts, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut lib.scripts).into_iter().partition(|s| s.folder.is_some_and(|f| gone.contains(&f)));
            lib.scripts = keep;
            Some((folders, scripts))
        }
        _ => None,
    }
}

/// The folder an item would end up in.
fn dest(lib: &Library, at: &DropAt) -> Result<Option<u64>, &'static str> {
    Ok(match at {
        DropAt::Root => None,
        DropAt::Into(f) if lib.folders.iter().any(|x| x.id == *f) => Some(*f),
        DropAt::Into(_) => return Err("That folder is gone."),
        DropAt::Before(n) | DropAt::After(n) => match n {
            Node::Folder(id) => live_parent(lib, lib.folders.iter().find(|f| f.id == *id).ok_or("That folder is gone.")?.parent),
            Node::Script(id) => live_parent(lib, lib.scripts.iter().find(|s| s.id == *id).ok_or("That script is gone.")?.folder),
            _ => return Err(LINKED),
        },
    })
}

/// Whether `folder` is `anc` or sits somewhere below it.
fn within(lib: &Library, folder: Option<u64>, anc: u64) -> bool {
    let mut cur = folder;
    for _ in 0..64 {
        match cur {
            Some(id) if id == anc => return true,
            Some(id) => cur = lib.folders.iter().find(|f| f.id == id).and_then(|f| f.parent),
            None => return false,
        }
    }
    true
}

/// Why `item` cannot go to `at`, if it cannot. A folder never goes into itself or one of its own subfolders.
pub fn can_move(lib: &Library, item: &Node, at: &DropAt) -> Result<(), &'static str> {
    if !item.is_saved() {
        return Err(LINKED);
    }
    let d = dest(lib, at)?;
    if let Node::Folder(id) = item
        && within(lib, d, *id)
    {
        return Err("A folder cannot go into itself.");
    }
    Ok(())
}

/// Moves a folder or script. Beside a row of its own kind it takes that spot in the list; elsewhere it goes last in its
/// new folder, and a script dropped beside a folder goes first among the scripts of that folder's parent.
pub fn move_to(lib: &mut Library, item: &Node, at: DropAt) -> Result<(), &'static str> {
    can_move(lib, item, &at)?;
    if matches!(&at, DropAt::Before(n) | DropAt::After(n) if n == item) {
        return Ok(());
    }
    let d = dest(lib, &at)?;
    let after = matches!(at, DropAt::After(_));
    match item {
        Node::Script(id) => {
            let i = lib.scripts.iter().position(|s| s.id == *id).ok_or("That script is gone.")?;
            let mut s = lib.scripts.remove(i);
            s.folder = d;
            let pos = match &at {
                DropAt::Before(Node::Script(t)) | DropAt::After(Node::Script(t)) => lib.scripts.iter().position(|x| x.id == *t).map(|p| p + after as usize),
                DropAt::Before(_) | DropAt::After(_) => Some(0),
                _ => None,
            };
            lib.scripts.insert(pos.unwrap_or(lib.scripts.len()), s);
        }
        Node::Folder(id) => {
            let i = lib.folders.iter().position(|f| f.id == *id).ok_or("That folder is gone.")?;
            let mut f = lib.folders.remove(i);
            f.parent = d;
            let pos = match &at {
                DropAt::Before(Node::Folder(t)) | DropAt::After(Node::Folder(t)) => lib.folders.iter().position(|x| x.id == *t).map(|p| p + after as usize),
                _ => None,
            };
            lib.folders.insert(pos.unwrap_or(lib.folders.len()), f);
        }
        _ => return Err(LINKED),
    }
    Ok(())
}

/// Folders in tree order with their depth, for a "Move to folder" menu. `skip` leaves out a folder and all below it, which
/// a folder being moved cannot go into.
pub fn folder_list(lib: &Library, skip: Option<u64>) -> Vec<(u64, usize, String)> {
    fn walk(lib: &Library, parent: Option<u64>, depth: usize, skip: Option<u64>, out: &mut Vec<(u64, usize, String)>) {
        if depth > 32 {
            return;
        }
        for f in lib.folders.iter().filter(|f| live_parent(lib, f.parent) == parent && Some(f.id) != skip) {
            out.push((f.id, depth, f.name.clone()));
            walk(lib, Some(f.id), depth + 1, skip, out);
        }
    }
    let mut out = Vec::new();
    walk(lib, None, 0, skip, &mut out);
    out
}

/// "Reports / Weekly" for a folder, or "" at the top.
pub fn folder_path(lib: &Library, folder: Option<u64>) -> String {
    let mut names = Vec::new();
    let mut cur = live_parent(lib, folder);
    while let Some(id) = cur {
        let Some(f) = lib.folders.iter().find(|f| f.id == id) else { break };
        names.push(f.name.clone());
        cur = live_parent(lib, f.parent);
        if names.len() > 32 {
            break;
        }
    }
    names.reverse();
    names.join(" / ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib() -> Library {
        let folder = |id, name: &str, parent| Folder { id, name: name.into(), parent };
        let script = |id, name: &str, folder| Script { id, name: name.into(), folder, ..Default::default() };
        Library {
            folders: vec![folder(1, "A", None), folder(2, "B", Some(1)), folder(3, "C", Some(2)), folder(4, "D", None)],
            scripts: vec![script(10, "s10", Some(1)), script(11, "s11", Some(1)), script(12, "s12", None), script(13, "s13", Some(3))],
            ..Default::default()
        }
    }

    fn order(l: &Library) -> Vec<u64> {
        l.scripts.iter().map(|s| s.id).collect()
    }

    #[test]
    fn a_folder_never_goes_into_its_own_descendant() {
        let mut l = lib();
        let before = l.clone();
        for at in [DropAt::Into(1), DropAt::Into(2), DropAt::Into(3), DropAt::Before(Node::Folder(3)), DropAt::After(Node::Script(13))] {
            assert!(move_to(&mut l, &Node::Folder(1), at).is_err());
        }
        assert_eq!(l, before);
        // Its own siblings and parent are fine.
        assert!(move_to(&mut l, &Node::Folder(3), DropAt::Into(4)).is_ok());
        assert_eq!(l.folders.iter().find(|f| f.id == 3).unwrap().parent, Some(4));
    }

    #[test]
    fn scripts_move_into_folders_to_the_top_and_beside_rows() {
        let mut l = lib();
        move_to(&mut l, &Node::Script(12), DropAt::Into(4)).unwrap();
        assert_eq!((order(&l), l.scripts[3].folder), (vec![10, 11, 13, 12], Some(4)));
        // Beside a script it takes that spot, in that script's folder.
        move_to(&mut l, &Node::Script(11), DropAt::Before(Node::Script(10))).unwrap();
        assert_eq!(order(&l), [11, 10, 13, 12]);
        move_to(&mut l, &Node::Script(11), DropAt::After(Node::Script(12))).unwrap();
        assert_eq!((order(&l), l.scripts[3].folder), (vec![10, 13, 12, 11], Some(4)));
        // Beside a folder it joins that folder's parent.
        move_to(&mut l, &Node::Script(13), DropAt::Before(Node::Folder(3))).unwrap();
        assert_eq!(l.scripts.iter().find(|s| s.id == 13).unwrap().folder, Some(2));
        move_to(&mut l, &Node::Script(13), DropAt::Root).unwrap();
        assert_eq!(l.scripts.last().unwrap().folder, None);
        // Dropped on itself changes nothing.
        let before = l.clone();
        move_to(&mut l, &Node::Script(10), DropAt::After(Node::Script(10))).unwrap();
        assert_eq!(l, before);
    }

    #[test]
    fn folders_reorder_among_siblings() {
        let mut l = lib();
        move_to(&mut l, &Node::Folder(4), DropAt::Before(Node::Folder(1))).unwrap();
        assert_eq!(l.folders.iter().map(|f| f.id).collect::<Vec<_>>(), [4, 1, 2, 3]);
        move_to(&mut l, &Node::Folder(4), DropAt::After(Node::Folder(2))).unwrap();
        assert_eq!((l.folders[2].id, l.folders[2].parent), (4, Some(1)));
    }

    #[test]
    fn linked_nodes_refuse_a_drop() {
        let mut l = lib();
        assert!(move_to(&mut l, &Node::Script(10), DropAt::Before(Node::Link(7))).is_err());
        assert!(move_to(&mut l, &Node::File { link: 7, rel: "a.sql".into() }, DropAt::Root).is_err());
    }

    #[test]
    fn deleting_a_folder_takes_everything_below_and_restore_puts_it_back() {
        let mut l = lib();
        let (folders, scripts) = delete(&mut l, &Node::Folder(1)).unwrap();
        assert_eq!((folders.len(), scripts.len()), (3, 3));
        assert_eq!((l.folders.len(), order(&l)), (1, vec![12]));
        l.restore(folders, scripts);
        assert_eq!((l.folders.len(), l.scripts.len()), (4, 4));
        assert_eq!(delete(&mut l, &Node::Script(12)).unwrap().1[0].name, "s12");
        assert!(delete(&mut l, &Node::Script(12)).is_none());
    }

    #[test]
    fn rename_trims_and_refuses_an_empty_name() {
        let mut l = lib();
        assert!(rename(&mut l, &Node::Script(10), "  new  "));
        assert!(!rename(&mut l, &Node::Script(10), "   "));
        assert!(!rename(&mut l, &Node::Link(1), "x"));
        assert_eq!(l.scripts[0].name, "new");
    }

    #[test]
    fn a_copy_sits_below_the_original_with_a_fresh_name() {
        let mut l = lib();
        let a = duplicate(&mut l, 10, 5).unwrap();
        let b = duplicate(&mut l, 10, 6).unwrap();
        assert_eq!(order(&l)[..3], [10, b, a]);
        assert_eq!((l.scripts[2].name.as_str(), l.scripts[1].name.as_str(), l.scripts[1].updated), ("s10 copy", "s10 copy 2", 6));
        assert_ne!(a, b);
    }

    #[test]
    fn new_folders_get_a_free_name_among_their_siblings() {
        let mut l = lib();
        let a = new_folder(&mut l, None, "");
        let b = new_folder(&mut l, None, "");
        let c = new_folder(&mut l, Some(1), "");
        let name = |id| l.folders.iter().find(|f| f.id == id).unwrap().name.clone();
        assert_eq!((name(a), name(b), name(c)), ("New folder".into(), "New folder 2".into(), "New folder".into()));
    }

    #[test]
    fn the_folder_menu_lists_in_tree_order_and_skips_a_branch() {
        let l = lib();
        assert_eq!(folder_list(&l, None).iter().map(|f| (f.0, f.1)).collect::<Vec<_>>(), [(1, 0), (2, 1), (3, 2), (4, 0)]);
        assert_eq!(folder_list(&l, Some(2)).iter().map(|f| f.0).collect::<Vec<_>>(), [1, 4]);
        assert_eq!(folder_path(&l, Some(3)), "A / B / C");
        assert_eq!(folder_path(&l, None), "");
    }
}
