//! Saving the connection groups; the groups and their rules live in quarry_core::state::groups.

use super::*;
pub(crate) use quarry_core::state::groups::*;

impl App {
    pub(super) fn save_groups(&mut self) {
        self.core.groups.v = 1;
        let b = serde_json::to_vec_pretty(&self.core.groups).unwrap_or_default();
        self.write_config("groups.json", b);
    }

    pub(super) fn conn_ids(&self) -> Vec<u64> {
        self.core.conns.iter().map(|c| c.meta.id).collect()
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
