//! State behind src/app/conns_panel.rs, which draws it.

use std::collections::HashSet;

/// What the Connections panel remembers while the app runs.
#[derive(Default)]
pub struct ConnsUi {
    pub filter: String,
    /// The group being renamed in place, with the text so far.
    pub rename: Option<(u64, String)>,
    /// The name being typed for a new group, in a field at the end of the groups.
    pub adding: Option<String>,
    /// The field that was just opened asks for focus once.
    pub focus_edit: bool,
    pub picker_filter: String,
    /// Groups the picker shows folded. Apart from `Group.open`, so folding in the picker never changes the panel.
    pub picker_closed: HashSet<u64>,
    /// Demo states: the group menu drawn open, a connection shown hovering inside a group, and the rail's Connect popover opened.
    pub demo_menu: Option<u64>,
    pub demo_drop: Option<u64>,
    pub demo_picker: bool,
    /// The rail's connection tile menu drawn open, for QUARRY_TEST_RAIL=menu.
    pub demo_tile_menu: Option<u64>,
    /// The rail tile drawn as a drop target with a tile carried over it, for QUARRY_TEST_RAIL=drag.
    pub demo_rail_drop: Option<u64>,
    /// The sidebar panel and hidden state the Manage tile opened the Connections panel from, which closing it goes back to.
    pub manage_back: Option<(super::config::SideMode, bool)>,
}
