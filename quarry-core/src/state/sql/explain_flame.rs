//! State behind src/app/sql/explain_flame.rs, which draws it.

use crate::db::sql::plan::flame;

/// The bars laid out for one weight and width, with the bar of each node, kept until the plan or either changes.
pub struct FlameCache {
    pub time: bool,
    pub w: f32,
    pub layout: flame::Layout,
    pub bar_of: Vec<Option<usize>>,
}
