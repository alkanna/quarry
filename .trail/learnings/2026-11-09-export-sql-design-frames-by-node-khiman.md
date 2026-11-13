---
title: "Export SQL design frames by node id, since a page name is not a node"
author: Khiman
date: 2026-11-09
branch: feat/mysql
cost: { minutes: 1, tokens: 3877 }
id: l-fb4c64a4
scope: repo
status: active
src: agent
touches:
  - docs/sql/PLAN.md
---

## Decided
To read a design in docs/sql/designs.figcraft, list the frames with `figcraft-cli info` and export each frame by its numeric id. The design pages are named after features (for example "Shell · attached connections"), but a page is not a node, so passing its name to export fails.

## Watch out
docs/sql/PLAN.md ("How this work is run", Designs bullet) suggests `--node "<frame name>"`, which works only with a frame's own name, never with the page title.

In the code:
- docs/sql/PLAN.md, docs/sql/designs.figcraft
- `figcraft-cli info docs/sql/designs.figcraft | grep -i <topic>` lists pages and their top-level frames with ids
- `figcraft-cli export docs/sql/designs.figcraft -o out.png --node 8602`
- Attached connections page: 8602/9022 (SQL tab + Mongo attached), 10018/10182 (Mongo tab + SQL attached), 10348/10400 (Attach dialog)
