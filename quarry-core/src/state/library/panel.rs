//! State behind src/app/library/panel.rs, which draws it.

use crate::library::tree::Node;
use std::collections::HashSet;

/// What the panel remembers while the app runs. Folders open or closed are not saved: a fresh start shows them closed.
#[derive(Default)]
pub struct PanelUi {
    pub open: HashSet<Node>,
    pub sel: Option<Node>,
    pub query: String,
    /// The row being renamed, its text, and whether the editor still has to take focus.
    pub rename: Option<(Node, String, bool)>,
    /// Keys act on the tree only after a row was clicked and until something else takes the pointer or the keyboard.
    pub focus: bool,
    pub scroll_y: f32,
    pub view_h: f32,
    pub scroll_set: Option<f32>,
    /// Demo only: a menu to open on this node, and a drag held from the first node over the second.
    pub demo_menu: Option<Node>,
    pub demo_drag: Option<(Node, Node)>,
    /// What a drop where the drag rests would do to the dragged item, as the ghost's badge says it: set by the row under
    /// the pointer this frame, read by the ghost the next, since the ghost may be drawn before that row.
    pub badge: Option<&'static str>,
    pub badge_next: Option<&'static str>,
    /// Folders a person opened or closed that may still be moving.
    pub folding: HashSet<Node>,
}
