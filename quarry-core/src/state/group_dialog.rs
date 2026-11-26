//! State behind src/app/group_dialog.rs, which draws it.

/// The working copy of a group while the dialog is open.
#[derive(Default)]
pub struct GroupForm {
    pub id: u64,
    pub name: String,
    pub color: String,
    pub fold_rail: bool,
    /// In the order the group will have after Save.
    pub members: Vec<u64>,
    /// The name field takes focus once, when the dialog opens.
    pub focus: bool,
    /// The filter and folded groups of the Add connections list.
    pub pick: super::conns_panel::ConnsUi,
}
