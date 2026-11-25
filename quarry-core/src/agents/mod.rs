//! AI agents: the `quarry mcp` bridge, the link to the running app, and the rules that decide what an agent may do
//! and see. Nothing here touches egui, so all of it is tested without a window.

pub mod audit;
pub mod bridge;
pub mod config;
pub mod detectors;
pub mod guard;
pub mod link;
pub mod policy;
pub mod presets;
pub mod protocol;
pub mod scrub;
pub mod store;
pub mod tools;
pub mod undo;
