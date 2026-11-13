---
title: Placeholder text in code editors needs the same line height as the editor text
author: Khiman
date: 2026-11-10
branch: master
cost: { minutes: 1, tokens: 52415 }
id: l-ce8d1140
scope: repo
status: active
src: agent
touches:
  - src/app/sql_query.rs
  - src/ui/editor.rs
---

## Watch out
egui draws a TextEdit's hint text without the custom layouter, so it keeps the default line height. Editors that set a custom line height in their layouter and paint a line number gutter show the placeholder lower than the "1" until the user types. Give the hint RichText the same line height.

In the code: src/app/sql_query.rs sql_editor hint_text uses .line_height(Some(LINE_H)); paint_gutter in src/ui/editor.rs centers numbers on the real galley rows.
