---
title: "Themes are picked as one dark and one light by name, and the mode switches between them"
author: Khiman
date: 2026-11-12
branch: feat/themes
cost: { minutes: 7, tokens: 38802 }
id: l-0afd2316
scope: repo
status: active
replaces: l-f1715a1c
src: agent
touches:
  - CLAUDE.md
  - src/ui/theme.rs
  - src/app/settings.rs
  - src/app/config.rs
---

## Decided
Settings keeps the System, Light and Dark mode and adds one dark theme and one light theme choice. System switches between the pair. Prefs store theme and font names, not indexes, so reordering the lists keeps choices, and an unknown name or a theme of the wrong kind falls back to Workbench or Paper. The data font is a separate choice from the interface font.

## Tried and ruled out
An accent override for every theme was in the design but was left out: pal() hands out static palettes, so overriding one color would need mutable palette storage.
Reading the old dyslexic_font flag in look() instead of migrating it: the flag is no longer saved, so the first save dropped OpenDyslexic for existing users.

## Watch out
Only Workbench and Paper have hand-tuned Soft palettes. The others derive theirs by shifting every surface by an offset, not mixing each toward the panel, because mixing closed the gap between bg and stripe and the zebra rows vanished. Older ui.json files are moved to current fields once after loading (UiPrefs::migrate), so a renamed pref needs a line there.

In the code: THEMES, UI_FONTS, DATA_FONTS and soften() in src/ui/theme.rs; UiPrefs::look() and migrate() in src/app/config.rs; theme_cards() in src/app/settings.rs; designs in docs/themes.figcraft.
