---
title: "Text sat high in buttons because of egui's default font, fixed with a glyph offset on that font"
author: Khiman
date: 2026-11-07
branch: master
cost: { minutes: 3, tokens: 78112 }
id: l-76f48fdd
scope: repo
status: active
src: agent
touches:
  - src/ui/theme.rs
---

## Decided
- egui's default proportional font (Ubuntu-Light) draws its glyphs about 1.5 px above center at body size, so every 24 px button and input looked top-heavy. The fix shifts that font's glyphs down with a FontTweak, which moves only the ink, not layout, so all text stays aligned with each other.

## Tried and ruled out
- The font's own metrics look balanced on paper, so the offset was measured from screenshots instead (cap height had 6 px above and 9 px below in a 24 px button; now 8 and 7).

## Watch out
- Per-button padding hacks would fix one widget and leave labels and inputs off. Keep the fix at the font level.

In the code: style() in src/ui/theme.rs sets y_offset_factor 0.12 on "Ubuntu-Light". Check with `--screenshot out.png` and compare the Find button margins.
