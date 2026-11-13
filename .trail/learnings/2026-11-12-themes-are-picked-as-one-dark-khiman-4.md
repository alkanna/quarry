---
title: "Themes are picked as one dark and one light by name, and one accent can override every theme"
author: Khiman
date: 2026-11-12
branch: master
cost: { minutes: 17, tokens: 118464 }
id: l-8438c5fb
scope: repo
status: active
replaces: l-2ebe5dd3
src: agent
touches:
  - src/ui/theme.rs
  - src/app/settings.rs
  - src/app.rs
  - src/ui/widgets.rs
---

## Decided
Settings has a System, Light and Dark mode plus one dark and one light theme choice; System switches between the pair. Prefs store theme, font and accent names, so reordering the lists keeps choices and an unknown name falls back to the default.
The accent override replaces primary, on_primary, sel and sel_hover in every theme. Each accent is moved toward white or black until it reaches 4.5:1 on that theme's bg, and text on it is white only where white reaches 4.5:1.
Catppuccin's four flavors (MIT) are themes like the others. Latte keeps its own pastel data colors even under 4.5:1.

## Tried and ruled out
pal() copying a palette out of a RwLock: a lock on every color lookup. Every theme x Soft x accent palette (about 240) is built once in PALS and pal() reads an index.

## Watch out
pal_index is the only place that knows the PALS layout; build and lookup both go through it. Workbench and Paper keep their original white-on-accent buttons (4.3:1, 3.6:1), so the contrast test skips their own accent.

In the code: PALS, pal_index, with_accent, ACCENTS in src/ui/theme.rs; theme_cards and accent_swatches in src/app/settings.rs; designs in docs/themes.figcraft.
