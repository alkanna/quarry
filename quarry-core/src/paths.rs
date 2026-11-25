//! Where Quarry keeps its files.

use std::path::PathBuf;

pub fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("quarry")
}

/// Where Quarry installs the MongoDB Database Tools itself when they are missing.
pub fn tools_dir() -> PathBuf {
    config_dir().join("tools")
}
