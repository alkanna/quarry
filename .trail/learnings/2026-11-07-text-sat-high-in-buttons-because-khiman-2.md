---
title: "Text sat high in buttons because of egui's default font, fixed with a glyph offset on that font and its icon fonts"
author: Khiman
date: 2026-11-07
branch: master
cost: { minutes: 10, tokens: 120752 }
id: l-8aae42d2
scope: repo
status: active
replaces: l-76f48fdd
src: agent
touches:
  - src/app/chrome.rs
  - src/ui/theme.rs
---

## Decided
- egui's default proportional font (Ubuntu-Light) draws its glyphs about 1.5 px above center at body size, so every 24 px button and input looked top-heavy. A FontTweak shifts the glyphs down. It moves only the ink, not layout.

## Tried and ruled out
- The font's own metrics look balanced on paper, so the offset was measured from screenshots instead (cap height had 6 px above and 9 px below in a 24 px button; now 8 and 7).
- Shifting only Ubuntu-Light left icons (gear, plus, export) about 1.5 px above their labels, because icons come from the fallback icon fonts.

## Watch out
- Every font in the proportional family needs the same offset, or text and icons drift apart. Per-button padding hacks would fix one widget and leave the rest off.

In the code: style() in src/ui/theme.rs sets y_offset_factor 0.12 on "Ubuntu-Light", "egui-icons", "NotoEmoji-Regular" and "emoji-icon-font". Check with `--screenshot out.png`.
