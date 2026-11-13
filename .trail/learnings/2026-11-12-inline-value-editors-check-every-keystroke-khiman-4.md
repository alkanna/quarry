---
title: Inline value editors check every keystroke and only let Enter swap in a list row when the typed text is invalid
author: Khiman
date: 2026-11-12
branch: master
cost: { minutes: 20, tokens: 94936 }
id: l-8995a964
scope: repo
status: active
replaces: l-c4803c3e
src: agent
touches:
  - src/app/sql/edit.rs
  - src/app/tree.rs
  - src/ui/pickers.rs
---

## Decided
- The SQL cell editor and staging use the same column check, so the line under the cell never disagrees with what is staged.
- Enter on an ENUM or key list takes a row only after the arrows, or the first row when the typed text is invalid and the list answers that exact text.
- Words like now and yesterday resolve in the zone the column is read in; under the server's own zone MySQL refuses them and PostgreSQL gets them as typed.
- The editor remembers where the last press landed and saves on lost focus only when it was outside the cell and its helpers.

## Tried and ruled out
- Checking the press or release in the frame the focus is lost: egui reports the lost focus on the release and the frame after, when the press is gone.
- Clearing the remembered press on any frame where the text box still has focus: during a click held for more than one frame the text box keeps focus until the release, so the flag was cleared and a slow click closed the calendar. That is why it failed only sometimes.

## Watch out
- Clear the remembered press only when the text box has focus with the button up. Offline UI tests on the demo data click the real editors with fast, held and one-frame clicks; one kittest frame is 250 ms, so holds past about 3 frames are long presses, not clicks.

In the code: press_helper in sql_cell_editor (src/app/sql/edit.rs), src/app/ui_tests/calendar.rs.
