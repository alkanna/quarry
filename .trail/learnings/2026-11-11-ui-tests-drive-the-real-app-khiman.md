---
title: "UI tests drive the real app headlessly with egui_kittest, finding painted widgets by accessible name"
author: Khiman
date: 2026-11-11
branch: test/ui-kittest
cost: { minutes: 77, tokens: 314432 }
id: l-1844d62f
scope: repo
status: active
src: agent
touches:
  - src/app.rs
  - src/app/ui_tests/mod.rs
  - src/app/ui_tests/mongo_edit.rs
  - src/ui/theme.rs
  - src/app/modals.rs
  - src/app/sql_edit.rs
  - src/app/ui_tests/sql_edit.rs
  - Cargo.lock
  - Cargo.toml
  - src/app/chrome.rs
---

## Decided
- Headless UI tests build the real App through a test constructor with no window, disk, keychain or menu bar, connect with a URI instead of the keychain, and click widgets by their accessible names. They check the database afterwards, so they cover the path from a click to the write batch that the db-layer live tests never reached.
- "No disk or keychain" got its own flag, separate from "offline demo". One flag meant both, so a live app could not run without persisting, and a fixture run that turned the demo off saved its tabs.
- Painted widgets (grid cells, row checkboxes, column headers, sidebar rows) got accessible names with widget_info. The closure only runs when AccessKit is on, so it costs nothing in normal frames and also serves screen readers.
- The fonts installed check is kept in the egui context, not in a process static: fonts belong to a context, and every test builds its own.

## Tried and ruled out
- Clicking twice for a double click: a kittest step is 250 ms of egui time, past egui's double click delay. Push press, release, press, release into one frame instead.
- A shorter step_dt: clicks right after a closing dialog were swallowed at random.
- harness.run(): never settles while a spinner shows. Step until a state condition holds, with a time limit.

## Watch out
- A painted widget a test must click needs a name through widget_info, and labels must be unique on screen or kittest's get_by_label panics; a write toast carries its own Undo and Close buttons.
- On Mongo, Delete, Duplicate and Insert opened their dialogs on a locked connection and refused only after confirming; they now ask to unlock first like inline edits and SQL.

In the code: App::for_test, App::blank, App.ephemeral, App::connect_uri (src/app.rs, src/app/state.rs); src/app/ui_tests/ (mod.rs harness, sql_edit.rs, mongo_edit.rs); theme::style; Makefile test-ui and servers-up.
