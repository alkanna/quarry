//! Dragging the sidebar's edge below its minimum width, in each sidebar mode, on the offline demo.
use super::*;

const MIN_W: f32 = 200.0;

/// The sidebar panel's laid out rect and the left edge of the central panel, as the last frame drew them.
fn edges(ui: &UiApp) -> (egui::Rect, f32) {
    let ctx = ui.h.ctx.clone();
    let side = ctx.data(|d| d.get_temp::<egui::Rect>(egui::Id::new("test-sidebar-rect"))).expect("sidebar was not drawn");
    let central = ctx.data(|d| d.get_temp::<f32>(egui::Id::new("test-central-left"))).expect("central panel was not drawn");
    (side, central)
}

/// The panel reserves exactly its minimum, which is all it paints, and the page starts where it ends.
fn assert_clamped(ui: &UiApp, mode: &str, when: &str) {
    let (side, central) = edges(ui);
    assert!((side.width() - MIN_W).abs() < 0.5, "{mode} {when}: the sidebar reserves {} px, not the {MIN_W} minimum (content wider than the panel leaves an unpainted band)", side.width());
    assert!((side.right() - central).abs() < 0.5, "{mode} {when}: a {} px strip lies between the sidebar and the page", central - side.right());
}

/// Library scripts, a folder group and long names, so every mode has rows to lay out.
fn crowded(a: &mut App) {
    a.conns.push(Conn::new(ConnMeta { id: 1, name: "Local sample".into(), ..Default::default() }, Status::Ready));
    a.demo_library("panel");
    a.prefs.lib_scope_all = true;
    for (id, name) in [(5, "a-connection-with-a-very-long-name-indeed"), (6, "beta"), (7, "gamma")] {
        a.conns.push(Conn::new(ConnMeta { id, name: name.into(), engine: db::sql::Engine::MySql, ..Default::default() }, Status::Ready));
    }
    let gid = a.groups.new_group("A folder with a long name that cannot fit");
    let all = a.conn_ids();
    a.groups.place(6, Place::Into(gid, None), &all);
    a.groups.place(7, Place::Into(gid, None), &all);
}

fn drag_below_minimum(mode: SideMode, name: &str) {
    let Some(mut ui) = demo_ui(crowded) else { return };
    ui.h.state_mut().prefs.set_side_mode(mode);
    ui.h.run_steps(4);
    let (side, _) = edges(&ui);
    let from = egui::pos2(side.right() - 1.0, side.center().y);
    let to = egui::pos2(120.0, from.y);
    ui.press_and_move(from, to);
    assert_clamped(&ui, name, "during the drag");
    ui.release(to);
    ui.h.run_steps(3);
    assert_clamped(&ui, name, "after the release");
}

#[test]
fn dragging_the_explorer_below_its_minimum_leaves_no_gap() {
    drag_below_minimum(SideMode::Explorer, "explorer");
}

#[test]
fn dragging_the_connections_panel_below_its_minimum_leaves_no_gap() {
    drag_below_minimum(SideMode::Connections, "connections");
}

#[test]
fn dragging_the_library_below_its_minimum_leaves_no_gap() {
    drag_below_minimum(SideMode::Library, "library");
}
