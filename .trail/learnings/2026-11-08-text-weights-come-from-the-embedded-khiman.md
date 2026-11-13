---
title: "Text weights come from the embedded variable fonts, never from strong()"
author: Khiman
date: 2026-11-08
branch: feat/workbench-theme
cost: { minutes: 50, tokens: 601503 }
id: l-b0bf009e
scope: repo
status: active
src: agent
touches:
  - src/ui/theme.rs
  - src/app.rs
  - src/ui/widgets.rs
  - src/app/rail.rs
  - src/app/chrome.rs
  - src/menu.rs
  - THIRD_PARTY_NOTICES.md
  - src/app/sidebar.rs
---

## Decided
The UI uses Inter and JetBrains Mono, embedded as variable fonts. Bold, medium and semibold text sets the font's weight axis per text. egui's strong() only switches to the "strong" text color and never makes text heavier, so it is not used for emphasis.

## Watch out
- A variable font with no weight set draws its default instance. For JetBrains Mono that is not Regular, so both fonts get weight 400 in their font tweak.
- Inter centers in buttons with no vertical offset. The 0.06 shift the old Ubuntu font needed was removed; do not add it back for Inter.
- In a horizontal_top row, a right_to_left layout grows to fill all the height left in its parent (it stretched the query card). Give it a fixed height with allocate_ui_with_layout.

In the code: src/ui/theme.rs fonts(), medium(), semibold(), mono_fmt(), ui_fmt(); assets/fonts/; documents.rs query card rows.
