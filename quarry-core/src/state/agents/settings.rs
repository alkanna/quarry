//! State behind src/app/agents/settings.rs, which draws it.

#[derive(Clone, Copy, PartialEq, Default)]
pub enum ClientKind {
    #[default]
    ClaudeCode,
    ClaudeDesktop,
    Cursor,
    Codex,
    Json,
}
