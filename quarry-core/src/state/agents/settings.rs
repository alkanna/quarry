//! State behind src/app/agents/settings.rs, which draws it.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ClientKind {
    #[default]
    ClaudeCode,
    ClaudeDesktop,
    Cursor,
    Codex,
    Json,
}
