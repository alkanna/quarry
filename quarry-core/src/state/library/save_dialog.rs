//! State behind src/app/library/save_dialog.rs, which draws it.

use crate::db;
use crate::library::{disk, Library};
use crate::state::View;
use std::path::PathBuf;

/// The fields of the Save to library dialog. The text and target are taken from the tab when it opens, so the dialog saves
/// what was on screen even though nothing else can change while it is up.
#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SaveForm {
    pub name: String,
    /// The Quarry folder it goes to; None is the top level.
    pub folder: Option<u64>,
    /// Set while a new folder is being named in the dialog; it is made on Save and takes the script.
    pub new_folder: Option<String>,
    /// The linked folder chosen instead of a Quarry one, the folder inside it (empty for its top), and the file name, which
    /// stays empty until typed and then follows the script's name.
    pub link: Option<u64>,
    pub dir: PathBuf,
    pub file: String,
    pub kind: String,
    pub body: String,
    pub sort: String,
    pub project: String,
    pub conn: u64,
    pub db: String,
    pub schema: String,
    pub coll: String,
    /// The view the text came from, which the tab is bound in after the save.
    pub view: Option<View>,
    /// The context line under the title.
    pub head: String,
    /// The dialect that colors a SQL preview.
    pub dialect: db::sql::Dialect,
    pub focused: bool,
    /// The linked file the save would write and whether it was there when the path last changed.
    pub exists: Option<(PathBuf, bool)>,
}

/// `name` as a file name in the style of the files beside it: kebab-case among kebab-case names, snake_case among snake_case
/// ones, as typed otherwise, so a team's folder keeps one style.
pub fn styled_name(name: &str, ext: &str, siblings: &[String]) -> String {
    let stems: Vec<&str> = siblings.iter().map(|s| s.rsplit_once('.').map_or(s.as_str(), |(a, _)| a)).collect();
    let style = |sep: char, other: char| stems.iter().filter(|s| s.contains(sep) && !s.contains(' ') && !s.contains(other) && **s == s.to_lowercase()).count();
    let (kebab, snake) = (style('-', '_'), style('_', '-'));
    let words = || name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect::<Vec<_>>();
    let styled = if kebab * 2 > stems.len() {
        words().join("-")
    } else if snake * 2 > stems.len() {
        words().join("_")
    } else {
        name.to_string()
    };
    disk::file_name(if styled.is_empty() { name } else { &styled }, ext)
}

/// The script a save would replace: same name, folder, kind and target. A filter or pipeline belongs to a collection, a SQL
/// or Shell script to its connection only, since it runs on whichever database the tab has.
pub fn same_place(lib: &Library, f: &SaveForm, name: &str, folder: Option<u64>) -> Option<usize> {
    lib.scripts.iter().position(|s| {
        s.name == name && s.folder == folder && s.kind == f.kind && s.conn == f.conn && (matches!(f.kind.as_str(), "sql" | "shell") || (s.db == f.db && s.coll == f.coll))
    })
}
