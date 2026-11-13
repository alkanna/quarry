//! The Edit group dialog: name, color, whether the group folds into one tile in the rail, and its members with handles to
//! reorder them, a button to take one out and one to add more. Nothing is written until Save, so Cancel and Esc leave the
//! group as it was.

use super::conns_panel::{color_swatches, folder_glyph};
use super::*;
use crate::ui::dnd;

/// The working copy of a group while the dialog is open.
#[derive(Default)]
pub(super) struct GroupForm {
    pub(super) id: u64,
    pub(super) name: String,
    pub(super) color: String,
    pub(super) fold_rail: bool,
    /// In the order the group will have after Save.
    pub(super) members: Vec<u64>,
    /// The name field takes focus once, when the dialog opens.
    pub(super) focus: bool,
    /// The filter and folded groups of the Add connections list.
    pub(super) pick: super::conns_panel::ConnsUi,
}

const MEMBER_H: f32 = 44.0;

impl App {
    /// Opens the Edit group dialog on a group, from the Rename action of a toast or a menu.
    pub(super) fn edit_group(&mut self, group: u64) {
        let Some(g) = self.groups.group(group) else { return };
        let alive = self.conn_ids();
        let members = g.members.iter().copied().filter(|c| alive.contains(c)).collect();
        self.modal = Some(Modal::GroupEdit(Box::new(GroupForm { id: g.id, name: g.name.clone(), color: g.color.clone(), fold_rail: g.fold_rail, members, focus: true, ..Default::default() })));
    }

    pub(super) fn group_dialog(&mut self, ui: &mut egui::Ui, f: &mut GroupForm, keep: &mut bool) {
        ui.set_width(520.0);
        let n = f.members.len();
        let sub = format!("{} · {n} connection{}", f.name.trim(), if n == 1 { "" } else { "s" });
        dialog_head(ui, &Head::new("Edit group").sub(sub).glyph(folder_glyph).tone(group_color(&f.color)));
        dialog_body(ui, |ui| {
            form_card(ui, "", "", |rows| {
                rows.row("Name", "", |ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut f.name).id(egui::Id::unique("group-name")).desired_width(ui.available_width()).margin(egui::Margin { left: 10, right: 10, top: 5, bottom: 5 }));
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Group name"));
                    if std::mem::take(&mut f.focus) {
                        r.request_focus();
                    }
                });
                rows.row("Color", "", |ui| {
                    if let Some(c) = color_swatches(ui, &f.color) {
                        f.color = c.into();
                    }
                });
                rows.row("Fold in the rail", "Shows one tile until clicked", |ui| {
                    let on = f.fold_rail;
                    toggle(ui, &mut f.fold_rail).widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, on, "Fold in the rail"));
                });
            });
            members_card(ui, f, &self.conns, &self.groups);
        });
        let mut save = false;
        let mut delete = false;
        dialog_foot(ui, |ui| {
            let ok = !f.name.trim().is_empty();
            if ui.add_enabled_ui(ok, |ui| primary(ui, "Save", PRIMARY())).inner.clicked() {
                save = true;
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
            foot_left(ui, |ui| {
                if danger(ui, "Delete group").clicked() {
                    delete = true;
                }
            });
        });
        if save {
            self.save_group_form(f);
            *keep = false;
        }
        if delete {
            self.delete_group(f.id);
            *keep = false;
        }
    }

    /// Writes the form into the group: the fields, then the members, those that left first. Placing the members one by one in
    /// their order gives the group exactly that order and takes new ones out of their old groups.
    fn save_group_form(&mut self, f: &GroupForm) {
        let Some(old) = self.groups.group(f.id).map(|g| g.members.clone()) else { return };
        let all = self.conn_ids();
        for c in old.iter().filter(|c| !f.members.contains(c)) {
            self.groups.place(*c, Place::Loose(None), &all);
        }
        for (i, c) in f.members.iter().enumerate() {
            self.groups.place(*c, Place::Into(f.id, Some(i)), &all);
        }
        if let Some(g) = self.groups.group_mut(f.id) {
            if !f.name.trim().is_empty() {
                g.name = f.name.trim().into();
            }
            g.color = f.color.clone();
            g.fold_rail = f.fold_rail;
        }
        self.save_groups();
    }
}

/// The CONNECTIONS card: one row per member, then the button that adds more. The whole row drags to reorder, and
/// Alt+Up or Alt+Down moves the focused one.
fn members_card(ui: &mut egui::Ui, f: &mut GroupForm, conns: &[Conn], groups: &Groups) {
    card_caption(ui, "CONNECTIONS", "drag to reorder");
    let mut remove = None;
    let mut mv = None;
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        let members: Vec<&Conn> = f.members.iter().filter_map(|id| conns.iter().find(|c| c.meta.id == *id)).collect();
        // Members whose connection is gone are kept out of the rows and the move indexes alike.
        f.members.retain(|id| conns.iter().any(|c| c.meta.id == *id));
        let sizes = vec![MEMBER_H; members.len()];
        let keys: Vec<egui::Id> = members.iter().map(|c| egui::Id::unique(("member", c.meta.id))).collect();
        let id = ui.make_persistent_id("group-members");
        mv = dnd::reorder(ui, id, dnd::Items::vertical(&sizes).keys(&keys).menu(), |ui, i, st| {
            let c = members[i];
            let rect = ui.max_rect();
            let r = ui.interact(rect, ui.scope_id().with("row"), Sense::click());
            let p = ui.painter().clone();
            if i > 0 && !st.lifted && st.offset == 0.0 {
                p.hline(rect.x_range(), rect.top(), Stroke::new(0.5, LINE()));
            }
            p.rect_filled(rect, 0, hover_fill(if st.busy { 0 } else { state_of(&r) }));
            dnd::grip_dots(&p, egui::pos2(rect.left() + 16.0, rect.center().y), if st.lifted || r.hovered() { MUTED() } else { FAINT() });
            let cy = rect.center().y;
            p.circle_filled(egui::pos2(rect.left() + 34.0, cy), 3.5, status_color(&c.status));
            let x_rect = egui::Rect::from_center_size(egui::pos2(rect.right() - 20.0, rect.center().y), egui::vec2(24.0, 24.0));
            let right = x_rect.left() - 6.0;
            let left = if c.meta.tag.is_empty() { right } else { paint_tag(&p, right, cy, &c.meta.tag, 140.0) - 8.0 };
            let name = truncated(&p, &c.meta.name, FontId::proportional(TXT_BODY), BRIGHT(), left - (rect.left() + 46.0));
            p.galley(egui::pos2(rect.left() + 46.0, cy - 8.0 - name.size().y / 2.0 + 2.0), name, BRIGHT());
            let uri = if c.masked_uri.is_empty() { "no address saved" } else { &c.masked_uri };
            let addr = truncated(&p, uri, FontId::monospace(TXT_CAPTION), FAINT(), left - (rect.left() + 46.0));
            p.galley(egui::pos2(rect.left() + 46.0, cy + 8.0 - addr.size().y / 2.0), addr, FAINT());
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Member {}", c.meta.name)));
            if st.lifted {
                return r;
            }
            // Drawn after the row, so a press on the cross is a click on it and never the start of a drag.
            let x = ui.interact(x_rect, egui::Id::unique(("member-remove", c.meta.id)), Sense::click());
            let xc = if x.hovered() { ALERT() } else { MUTED() };
            let (xc_pos, s) = (x_rect.center(), Stroke::new(1.4, xc));
            p.line_segment([xc_pos + egui::vec2(-3.5, -3.5), xc_pos + egui::vec2(3.5, 3.5)], s);
            p.line_segment([xc_pos + egui::vec2(-3.5, 3.5), xc_pos + egui::vec2(3.5, -3.5)], s);
            x.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Remove {} from the group", c.meta.name)));
            if x.on_hover_text("Remove from the group").clicked() {
                remove = Some(c.meta.id);
            }
            r
        });
        if !members.is_empty() {
            rule_line(ui);
        }
        add_row(ui, f, conns, groups);
    });
    ui.add_space(16.0);
    if let Some(m) = mv {
        m.apply(&mut f.members);
    }
    if let Some(id) = remove {
        f.members.retain(|c| *c != id);
    }
}

fn rule_line(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(0.5, LINE()));
}

/// "+ Add connections…": the connections that are not members, filtered and under their groups like the explorer's
/// picker, each added with a click. They leave their old group when the form is saved, and their row says so.
fn add_row(ui: &mut egui::Ui, f: &mut GroupForm, conns: &[Conn], groups: &Groups) {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 36.0), Sense::click());
    ui.painter().rect_filled(rect, 0, hover_fill(state_of(&r)));
    paint_plus_at(ui.painter(), egui::pos2(rect.left() + 18.0, rect.center().y), PRIMARY());
    let g = ui.painter().layout_no_wrap("Add connections…".into(), FontId::proportional(TXT_BODY), PRIMARY());
    ui.painter().galley(egui::pos2(rect.left() + 34.0, rect.center().y - g.size().y / 2.0), g, PRIMARY());
    focus_ring(ui, &r, rect, R_SM as f32);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Add connections…"));
    let r = r.on_hover_cursor(egui::CursorIcon::PointingHand);
    egui::Popup::menu(&r).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).width(rect.width().min(420.0)).show(|ui| {
        menu_look(ui);
        ui.spacing_mut().item_spacing.y = 2.0;
        if conns.iter().all(|c| f.members.contains(&c.meta.id)) {
            ui.label(RichText::new("Every connection is in this group.").color(MUTED()));
            return;
        }
        super::conns_panel::picker_filter(ui, &mut f.pick.picker_filter, false);
        let me = f.id;
        let note = |c: &Conn| groups.group_of(c.meta.id).filter(|g| g.id != me).map(|g| format!("in {}, moves here", g.name));
        let added = egui::ScrollArea::vertical().id_salt("group-add").max_height(300.0).show(ui, |ui| super::conns_panel::picker_list_in(ui, conns, groups, &mut f.pick, None, false, &f.members, &note)).inner;
        if let Some(id) = added {
            f.members.push(id);
        }
    });
}

fn paint_plus_at(p: &egui::Painter, c: egui::Pos2, col: Color32) {
    let s = Stroke::new(1.4, col);
    p.line_segment([c + egui::vec2(-4.5, 0.0), c + egui::vec2(4.5, 0.0)], s);
    p.line_segment([c + egui::vec2(0.0, -4.5), c + egui::vec2(0.0, 4.5)], s);
}
