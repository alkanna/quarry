---
title: "Long values open in a dialog or a docked side panel, and unquoted NULL always means null"
author: Khiman
date: 2026-11-12
branch: feat/long-value-editor
cost: { minutes: 26, tokens: 245488 }
id: l-bb27a4b2
scope: repo
status: active
src: agent
touches:
  - src/app/pg_roles.rs
  - src/ui/widgets.rs
  - src/app/value_edit.rs
  - src/db/sql/value.rs
  - src/db.rs
  - src/app/config.rs
---

## Decided
- Double-clicking a value with a line break or over 500 bytes opens the large editor directly. Otherwise the inline editor shows an expand button, and Shift+Enter does the same. The user picked design A (dialog) as the default and C (side panel) as a setting under Editing > Long values. Design B (a popover from the cell) was rejected.
- The side panel follows the selection: double-clicking another value loads it. If the panel holds unapplied edits, the double-click is refused with a warning instead of dropping the text.
- Bare NULL (any case, trimmed) is null in every SQL and Mongo value field. Quotes keep it as text, one pair per layer: editors show the text null as 'null', and the text 'null' (with its quotes) as ''null''. JSON columns type the JSON literal null as 'null', because "null" is a JSON string.
- NOT NULL columns refuse a typed NULL before anything runs, except AUTO_INCREMENT columns, where MySQL reads NULL as the next id.

## Tried and ruled out
- Merging JsonEdit and CellEdit into a single value-editor enum (simplify review): it would mean a bigger rewrite of the Mongo dialog paths, and the team prefers splitting files to adding layers. Revisit if a third value source appears.

## Watch out
- The side panel is not modal, so anything that changes the current tab must clear json_edit and sql_cell. Stage uses the current tab's namespace.
- The editor's default layouter caches its galley. The custom one (JSON colors, Find, no wrap) rebuilds the whole text every frame, so it is only used when needed.

In the code: src/app/value_edit.rs (dialog, panel, value_body), expand_button and expand_key in src/ui/widgets.rs, quoted_null, null_like, text_like and parse_col in src/db/sql/value.rs, inline_text and make_value in src/db.rs, prefs.value_panel in src/app/config.rs. Designs: docs/long-values.figcraft (local only).
