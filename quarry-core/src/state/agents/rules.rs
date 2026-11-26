//! State behind src/app/agents/rules.rs, which draws it.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum RulesTab {
    Secrets,
    #[default]
    Pii,
    Custom,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RulesWin {
    pub tab: RulesTab,
    /// Whose custom rules show and which rules the tester runs: every connection, or one.
    pub scope: Option<u64>,
    /// The custom rule open in the editor.
    pub sel: Option<usize>,
    /// The editor's "where" globs as typed, and the rule they belong to.
    pub globs: (Option<usize>, String),
    pub sample: String,
    /// The tester's lines with what they were computed from (sample, scope, config revision), so they are rebuilt only when one changes.
    pub tested: Option<((String, Option<u64>, u64), TesterLines)>,
}

pub type TesterLines = Result<Vec<(String, Option<String>)>, String>;

impl Default for RulesWin {
    fn default() -> Self {
        Self { tab: RulesTab::default(), scope: None, sel: None, globs: (None, String::new()), sample: SAMPLE.into(), tested: None }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Path,
    Name,
    Value,
    Detect,
}

pub const SAMPLE: &str = "{\n  \"full_name\": \"Ada Lovelace\",\n  \"email\": \"ada@example.com\",\n  \"card_number\": \"4111 1111 1111 1111\",\n  \"notes\": \"SSN 123-45-6789, call back\",\n  \"password\": \"hunter2\",\n  \"avatar\": { \"$binary\": { \"base64\": \"iVBORw0KGgo=\", \"subType\": \"00\" } }\n}";

/// Hashes in the tester use this key, not the keychain one: the tester must work in the demo, and a hash shown here
/// must not match what agents get.
pub const TESTER_KEY: [u8; 32] = [0; 32];
