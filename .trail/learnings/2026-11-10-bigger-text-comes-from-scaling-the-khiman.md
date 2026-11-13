---
title: "Bigger text comes from scaling the whole interface, OpenDyslexic is for interface text only, and Settings moves to a category sidebar"
author: Khiman
date: 2026-11-10
branch: master
cost: { minutes: 1, tokens: 2018 }
id: l-8b433a07
scope: repo
status: active
src: agent
touches:
  - src/ui/theme.rs
  - src/app/table.rs
  - src/ui/cells.rs
  - src/app/errors.rs
---

## Decided
- Users who want bigger text get an Interface size setting (100, 110, 125, 150 %) that zooms the whole UI. 100 % is the default; the user wants the original size kept.
- OpenDyslexic is an opt-in interface font. Data stays in JetBrains Mono.
- The reading aids are opt-in and start off: soft background palettes, a reading line across the hovered table row, digit grouping (display only), and keeping warning and confirmation messages until they are closed.
- Settings becomes a wider dialog with categories on the left (Appearance, Reading, Documents, Editing, Queries and tools, Status bar, Updates). The user picked this over tabs or one shorter page.

## Tried and ruled out
- Scaling only the text: the text sizes are constants and the fixed heights (buttons, rows, panels) would not grow with them, so bigger text would clip everywhere.
- An OpenDyslexic data font: the OpenDyslexic release has no mono variant, and a proportional font breaks column alignment.

## Watch out
- OpenDyslexic is much wider than Inter (its O is 1.14 em), so labels need more room. It is not a variable font, so medium and semibold draw as Regular.
- egui's built-in Cmd+= / Cmd+- zoom does not save the setting. Wire the keys to the preference.

In the code: zoom with ctx.set_zoom_factor, fonts() in src/ui/theme.rs, row_fill in src/app/table.rs, cell_job_of and sql_cell_job in src/ui/cells.rs, toasts() in src/app/errors.rs. Designs are in settings.figcraft, to land in docs/ with the feature.
