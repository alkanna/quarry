---
title: Wrapped labels inside popovers need a non-justified layout or egui stretches their letters
author: Khiman
date: 2026-11-12
branch: master
cost: { minutes: 2, tokens: 51951 }
id: l-401cb790
scope: repo
status: active
src: agent
touches:
  - src/app/sql/pg_roles.rs
  - src/ui/widgets.rs
---

## Watch out
Popovers and menus use Layout::top_down_justified. In egui 0.37, Label sets layout_job.justify from the layout, so a label that wraps gets its first line stretched letter by letter across the width. Draw wrapped text in ui.with_layout(Layout::top_down(Align::Min), ...) inside a popover.

In the code: src/ui/widgets.rs popover (justified layout), src/app/sql/pg_roles.rs pg_cell popover title.
