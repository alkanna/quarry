//! Saving the connection groups; the groups and their rules live in quarry_core::state::groups.

use super::*;
pub(crate) use quarry_core::state::groups::*;

impl App {
    /// Startup only: the groups pruned of removed connections are written back before the first frame.
    pub(super) fn save_groups(&mut self) {
        self.core.setup().save_groups();
    }

    pub(super) fn conn_ids(&self) -> Vec<u64> {
        self.core.conn_ids()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_groups_show_every_connection_once_in_the_panel() {
        let g = Groups { v: 1, groups: vec![Group { id: 1, name: "Prod".into(), members: vec![1, 2], ..Default::default() }, Group { id: 2, name: "Dev".into(), members: vec![3], ..Default::default() }], loose: vec![4] };
        let all = [1, 2, 3, 4];
        let rows = conns_panel::panel_rows(&all.map(|id| Conn::new(ConnMeta { id, ..Default::default() }, Status::Idle)), &g, "", |_| true);
        let shown = rows.iter().filter(|r| matches!(r, conns_panel::CRow::Conn { .. })).count();
        assert_eq!(shown, 4);
    }
}
