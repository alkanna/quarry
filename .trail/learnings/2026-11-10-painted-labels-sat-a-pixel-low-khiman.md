---
title: Painted labels sat a pixel low because glyph images round their top up to a whole pixel
author: Khiman
date: 2026-11-10
branch: master
cost: { minutes: 7, tokens: 62193 }
id: l-bc357994
scope: repo
status: active
src: agent
touches:
  - ".cargo/run-signed.sh"
  - ".github/workflows/release.yml"
  - Makefile
  - README.md
  - bundle.sh
  - src/ui/theme.rs
  - src/ui/widgets.rs
  - src/app/documents.rs
---

## Decided
Centering text on its ink measured the top of the letters from the glyph image, whose top edge is rounded up to a whole device pixel (the control box is expanded outward). The real ink starts up to a pixel lower, so caps sat about 1 px low at 1x once the position was also rounded. The helper now takes the middle of that pixel as the top.
Small painted marks like the "+" are drawn as filled bars in whole device pixels; a 1.2 pt stroke at a fractional center antialiased into one arm longer than the others.

## Watch out
Measure alignment from pixels, not by eye: a 1x screenshot can be dumped row by row with a stdlib PNG decoder (no Pillow here). The chip's border rows give its center; compare the first and last full rows of the caps.
A headless egui Context in a test panics on the second scale or frame (textures.rs), so probe glyph metrics one frame per Context.

In the code: optical_dy in src/ui/theme.rs (top + 0.5 / g.pixels_per_point); add_chip in src/ui/widgets.rs; epaint 0.37 font_face.rs rasterize_glyph uses control_box().expand().
