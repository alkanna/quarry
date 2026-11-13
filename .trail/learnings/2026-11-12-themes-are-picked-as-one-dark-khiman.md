---
title: "Themes are picked as one dark and one light by name, and the mode switches between them"
author: Khiman
date: 2026-11-12
branch: feat/themes
cost: { minutes: 24, tokens: 255958 }
id: l-f1715a1c
scope: repo
status: active
src: agent
touches:
  - src/app/settings.rs
  - src/app.rs
  - src/app/config.rs
  - src/ui/theme.rs
---

## Decided
Settings keeps the System, Light and Dark mode and adds one dark theme and one light theme choice. System switches between the pair. Prefs store theme and font names, not indexes, so reordering the lists keeps choices, and an unknown name or a theme of the wrong kind falls back to Workbench or Paper. The data font is a separate choice from the interface font.

## Tried and ruled out
An accent override for every theme was in the design but was left out: pal() hands out static palettes, so overriding one color would need mutable palette storage.

## Watch out
Only Workbench and Paper have hand-tuned Soft palettes. The other themes derive theirs (page toward panel, text toward muted). The old dyslexic_font flag is still read from older ui.json files and wins over ui_font until the font is picked again.

In the code: THEMES, UI_FONTS, DATA_FONTS and soften() in src/ui/theme.rs; UiPrefs::look() in src/app/config.rs; theme_cards() in src/app/settings.rs; designs in docs/themes.figcraft.
