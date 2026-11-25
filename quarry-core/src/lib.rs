//! Quarry's state and everything that talks to a server, free of any UI toolkit. The egui app draws from it and changes
//! it only through actions, so every flow can be run and tested without a window.

// Builders take the state they work on as plain arguments; argument structs for each would be the extra layers the
// codebase keeps out.
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

pub mod action;
pub mod agents;
pub mod cloudsql;
pub mod core;
pub mod db;
pub mod library;
pub mod paths;
pub mod ssh;
pub mod state;
pub mod sysmem;
pub mod update;
pub mod vault;

/// The app's version, read from the quarry package by build.rs: release-please bumps only that one.
pub const VERSION: &str = env!("QUARRY_VERSION");
