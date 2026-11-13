---
title: "Suggestion lists inside a menu are drawn in the menu, because a popup click closes the menu"
author: Khiman
date: 2026-11-07
branch: master
cost: { minutes: 3, tokens: 31250 }
id: l-fabd73a4
scope: repo
status: active
src: agent
touches:
  - src/ui/complete.rs
  - src/app/table.rs
---

## Decided
Every text input with suggestions uses the same list look. Inputs inside a menu (like the Columns menu's custom field) draw that list inside the menu under the input, not as a floating popup.

## Tried and ruled out
A floating popup over a menu: egui menus that close on outside clicks treat any click on another layer as outside, so picking a suggestion closed the menu. Marking the popup as a sublayer does not help: the outside-click check still compares layers.

## Watch out
New inputs with suggestions in a menu or popup need the in-menu form. Only free-standing inputs can use the floating one.

In the code: src/ui/complete.rs ac_field (floating), ac_input + ac_list (in-menu); used in src/app/table.rs columns_menu.
