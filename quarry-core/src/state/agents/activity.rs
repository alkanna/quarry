//! State behind src/app/agents/activity.rs, which draws it.

#[derive(Clone, Copy, PartialEq, Default)]
pub enum Filter {
    #[default]
    All,
    Changes,
    Refused,
    Waiting,
}
