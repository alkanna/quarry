---
title: "Popovers share one layout built from shared helpers, and frameless text boxes need a Frame value in egui 0.37"
author: Khiman
date: 2026-11-10
branch: master
cost: { minutes: 26, tokens: 250546 }
id: l-bda9e9f9
scope: repo
status: active
src: agent
touches:
  - src/app/shell.rs
  - src/ui/widgets.rs
  - src/app/query.rs
  - src/app/table.rs
---

## Decided
The Packages, History, saved queries, Library and Columns popovers use one layout: a title with a faint note on the right, a one-line subtitle, a well field with an icon, small caps section headers, rows that fill on hover with their actions shown only on hover, a dashed empty state and a caption footer. The parts are shared helpers, so a new popover should build from them, not from plain egui widgets. The designs are in docs/shell-packages.figcraft.

## Watch out
- In egui 0.37, TextEdit::frame takes an egui::Frame (use Frame::NONE), not a bool. Id::new and TextEdit::vertical_align are deprecated: use ui.make_persistent_id and .align(Align2::LEFT_CENTER).
- Buttons that show only on hover are placed over the row after the row is allocated, so they take the click before the row does.
- Columns has no Move up and Move down buttons on purpose: drag reorders.

In the code: src/ui/widgets.rs (popover_head, well_field, field_edit, field_note, section_head, pop_row, put_at, tag_at, dashed_box, empty_state, popover_divider, popover_foot); used in src/app/shell.rs packages_popover, src/app/query.rs history_menu and library_menu, src/app/table.rs columns_menu.
