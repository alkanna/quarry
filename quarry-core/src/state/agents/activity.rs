//! State behind src/app/agents/activity.rs, which draws it.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Filter {
    #[default]
    All,
    Changes,
    Refused,
    Waiting,
}
