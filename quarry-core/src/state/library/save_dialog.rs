//! State behind src/app/library/save_dialog.rs, which draws it.

use crate::db;
use crate::state::View;
use std::path::PathBuf;

/// The fields of the Save to library dialog. The text and target are taken from the tab when it opens, so the dialog saves
/// what was on screen even though nothing else can change while it is up.
#[derive(Default)]
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
