---
title: "Engine logos are prebaked white PNG masks tinted at paint time, not SVG rendered in the app"
author: Khiman
date: 2026-11-10
branch: master
cost: { minutes: 3, tokens: 100362 }
id: l-6001f6c5
scope: repo
status: active
src: agent
touches:
  - THIRD_PARTY_NOTICES.md
  - src/app/rail.rs
---

## Decided
The rail shows the MongoDB, MySQL and MariaDB logos instead of two-letter marks. The shapes come from Simple Icons (CC0, v16.37.0), rasterized once to 64 px white-on-transparent PNGs and embedded with include_bytes. They are tinted with the painter's image color, so idle, hover and active states keep working like the old text marks.

## Tried and ruled out
Filling the SVG paths with egui shapes: egui only fills convex paths, and the logos are concave. Adding an SVG renderer dependency (resvg, egui_extras svg) for three small icons was not worth it.

## Watch out
To add an engine logo, fill the path white before rasterizing, or it renders black and tints wrong. Credit it in THIRD_PARTY_NOTICES.md.

In the code: assets/engines/*.png, engine_logo in src/app/rail.rs. Regenerate with: sed 's/<path /<path fill="#fff" /' x.svg | rsvg-convert -w 64 -h 64 -o assets/engines/x.png
