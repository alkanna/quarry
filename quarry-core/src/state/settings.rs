//! State behind src/app/settings.rs, which draws it.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
pub enum SettingsCat {
    #[default]
    Appearance,
    Reading,
    Documents,
    Editing,
    Tools,
    Agents,
    StatusBar,
    Updates,
}

impl SettingsCat {
    /// The category whose name starts with this word, for the QUARRY_TEST_SETTINGS switch.
    pub fn named(word: &str) -> Self {
        let w = word.to_lowercase();
        CATS.iter().find(|(_, n)| n.to_lowercase().split(' ').any(|p| p == w)).map_or_else(Self::default, |c| c.0)
    }
}

pub const CATS: [(SettingsCat, &str); 8] = [
    (SettingsCat::Appearance, "Appearance"),
    (SettingsCat::Reading, "Reading"),
    (SettingsCat::Documents, "Documents"),
    (SettingsCat::Editing, "Editing"),
    (SettingsCat::Tools, "Queries and tools"),
    (SettingsCat::Agents, "AI agents"),
    (SettingsCat::StatusBar, "Status bar"),
    (SettingsCat::Updates, "Updates"),
];
