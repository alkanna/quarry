---
title: "Themes are picked as one dark and one light by name, and one accent can override every theme"
author: Khiman
date: 2026-11-12
branch: master
cost: { minutes: 10, tokens: 273631 }
id: l-2ebe5dd3
scope: repo
status: active
replaces: l-0afd2316
src: agent
touches:
  - src/app/settings.rs
  - src/ui/theme.rs
  - src/app/config.rs
---

## Decided
Settings keeps the System, Light and Dark mode and adds one dark theme and one light theme choice. System switches between the pair. Prefs store theme, font and accent names, not indexes, so reordering the lists keeps choices, and an unknown name or a theme of the wrong kind falls back to Workbench or Paper. The data font is a separate choice from the interface font.
The accent override (Settings > Appearance > Accent) replaces primary, on_primary, sel and sel_hover in every theme. Each accent has a dark-theme shade and a deeper light-theme shade. Text on the accent is white only where white reaches 4.5:1, dark otherwise.
The four Catppuccin flavors (MIT) are themes like the others, mapped onto Palette: base, mantle and crust are the surfaces, mauve the accent. Latte keeps Catppuccin's own pastel data colors even though they sit under 4.5:1 on its base.

## Tried and ruled out
Keeping pal() as a &'static reference into the theme lists: the override makes palettes that are in no list. style() now builds the two palettes in use and pal() copies one out of a RwLock.
Reading the old dyslexic_font flag in look() instead of migrating it: the flag is no longer saved, so the first save dropped OpenDyslexic for existing users.

## Watch out
Only Workbench and Paper have hand-tuned Soft palettes. The others derive theirs by shifting every surface by an offset, not mixing each toward the panel, because mixing closed the gap between bg and stripe and the zebra rows vanished. Older ui.json files are moved to current fields once after loading (UiPrefs::migrate), so a renamed pref needs a line there. The theme cards wrap five to a row, so new themes need no picker change.

In the code: DARK_THEMES, LIGHT_THEMES, ACCENTS, with_accent(), CUR and soften() in src/ui/theme.rs; UiPrefs::look() and migrate() in src/app/config.rs; theme_cards() and accent_swatches() in src/app/settings.rs; designs in docs/themes.figcraft (page B, design C is the wrapping grid).
