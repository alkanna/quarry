---
title: Inline value editors check every keystroke and only let Enter swap in a list row when the typed text is invalid
author: Khiman
date: 2026-11-12
branch: master
cost: { minutes: 26, tokens: 141736 }
id: l-c4803c3e
scope: repo
status: active
replaces: l-c140d432
src: agent
touches:
  - src/app.rs
  - src/app/chrome.rs
  - src/app/poll.rs
  - src/app/sql/edit.rs
  - src/app/state.rs
  - src/app/tree.rs
  - src/db.rs
  - src/db/dates.rs
  - src/db/sql/advanced.rs
  - src/db/sql/check.rs
---

## Decided
- The SQL cell editor and staging use the same column check, so the line under the cell never disagrees with what is staged.
- Enter on an ENUM or key list takes a row only after the arrows, or the first row when the typed text is invalid and the list answers that exact text.
- Words like now and yesterday resolve in the zone the column is read in; under the server's own zone MySQL refuses them and PostgreSQL gets them as typed.
- The foreign key list learns the label column and table size once, and only looks up the exact key on tables over 100,000 rows.
- The calendar title opens a month and year chooser, so far dates do not need paging month by month.

## Tried and ruled out
- Telling a click in a helper from a click outside by the press or release in the same frame: a headless probe showed egui reports the text box's lost focus on two frames after the click, and on the second the pointer events are gone, so the editor saved and closed on any click in the calendar.

## Watch out
- The editor remembers where the last press landed (helper, calendar button, or elsewhere) and saves on lost focus only when it was elsewhere. Any new helper beside a text box that commits on lost focus needs the same.

In the code: press_helper in sql_cell_editor (src/app/sql/edit.rs), date_picker and title_button in src/ui/pickers.rs with the headless test months_page_and_the_title_picks_a_month_and_year.
