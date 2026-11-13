---
title: Inline value editors check every keystroke and only let Enter swap in a list row when the typed text is invalid
author: Khiman
date: 2026-11-12
branch: master
cost: { minutes: 5, tokens: 22413 }
id: l-1faad7d4
scope: repo
status: active
replaces: l-8995a964
src: agent
touches:
  - src/app/ui_tests/mod.rs
  - src/app/sql/edit.rs
---

## Decided
- The SQL cell editor and staging use the same column check, so the line under the cell never disagrees with what is staged.
- Enter on an ENUM or key list takes a row only after the arrows, or the first row when the typed text is invalid and the list answers that exact text.
- Words like now and yesterday resolve in the zone the column is read in; under the server's own zone MySQL refuses them and PostgreSQL gets them as typed.
- The cell editor saves on a press outside the cell and its helpers, seen in the frame it happens, or on Enter or Tab. It never saves on the text box's lost focus alone.

## Tried and ruled out
- Saving on lost focus unless the click was in a helper: egui reports lost focus on the release and the frame after, when the press is gone. Remembering where the press landed fixed fast clicks but broke held clicks (cleared while the text box kept focus) and then swallowed Enter after a click in the cell. Two patches in, the press-outside rule replaced it.

## Watch out
- Offline UI tests on the demo data (demo_ui, mouse_hold) click the real editors with taps, normal and slow clicks; one kittest frame is 250 ms, so past about 3 frames a hold is a long press. They skip when a QUARRY_TEST_ switch is set, since the demo loaders read them.

In the code: the commit branches at the end of sql_cell_editor (src/app/sql/edit.rs), src/app/ui_tests/calendar.rs, demo_ui and mouse_hold in src/app/ui_tests/mod.rs.
