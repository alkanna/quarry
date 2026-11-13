---
title: "The Cmd+K palette runs commands by calling app methods directly, and fuzzy matches count only on an item's own name"
author: Khiman
date: 2026-11-10
branch: feat/palette
cost: { minutes: 10, tokens: 393440 }
id: l-ac9be61c
scope: repo
status: active
src: agent
touches:
  - src/app/palette.rs
  - src/app/modals.rs
  - src/app/demo.rs
  - src/app/sidebar.rs
---

## Decided
- Palette commands call the same App methods the shortcuts call, through one Cmd enum. They are not replayed as key events: the key replay lives in the native menu bar, which only exists on macOS.
- The palette draws in its own Modal frame anchored near the top of the window, not in the shared dialog frame, so the box stays put while the result list grows and shrinks.
- A scattered-letter (fuzzy) match counts only on the item's own name. With several connections open, "ord" fuzzily matched the connection "shop-prod" and listed every table under it.

## Tried and ruled out
- Replaying shortcuts as egui key events: it would not work on Linux or Windows, where there is no menu bar to replay them.

## Watch out
- A new command needs an entry in the commands list and an arm in run_cmd. Hide it when it does not apply (no tab open, not connected, engine without the feature).
- Designs B (scoped search with a preview), E (actions on a result), the # and : prefixes and learned ranking are designed but not built yet.

In the code: src/app/palette.rs (sections, commands, run_cmd, palette), jump_score in src/app/sidebar.rs, designs in docs/palette.figcraft, screenshot switch QUARRY_TEST_PALETTE (a leading ">" opens command mode).
