---
title: "Text sat slightly high in buttons because of egui's default font, fixed with a small glyph offset on that font and its icon fonts"
author: Khiman
date: 2026-11-07
branch: master
cost: { minutes: 2, tokens: 26638 }
id: l-bfea56a4
scope: repo
status: active
replaces: l-8aae42d2
src: agent
touches:
  - src/ui/theme.rs
---

## Decided
- egui's default proportional font (Ubuntu-Light) draws its glyphs about 0.75 pt above center at body size. A FontTweak with y_offset_factor 0.06 shifts them down. It moves only the ink, not layout.

## Tried and ruled out
- 0.12, measured from a 1x screenshot, over-corrected: on a retina display text sat visibly low. Measure at 2x or compare cap centers to the button center on a fresh `--screenshot`, which may come out at 1x or 2x depending on the display.
- Shifting only Ubuntu-Light left icons (gear, plus, export) above their labels, because icons come from the fallback icon fonts.

## Watch out
- Every font in the proportional family needs the same offset, or text and icons drift apart.

In the code: style() in src/ui/theme.rs sets y_offset_factor 0.06 on "Ubuntu-Light", "egui-icons", "NotoEmoji-Regular" and "emoji-icon-font".
