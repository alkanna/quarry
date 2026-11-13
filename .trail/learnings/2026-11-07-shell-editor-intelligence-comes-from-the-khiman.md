---
title: "Shell editor intelligence comes from the tsrs language server, started only while a script is edited"
author: Khiman
date: 2026-11-07
branch: master
cost: { minutes: 8, tokens: 92292 }
id: l-80af186b
scope: repo
status: active
replaces: l-286b83a5
src: agent
touches:
  - src/db/js_ls.rs
  - src/ui/js_doc.rs
  - src/app/shell_ls.rs
---

## Decided
The Shell editor's completions, the docs beside the selected suggestion, hover docs and parameter hints all come from tsrs (Oxc/Cloudflare's Rust port of the TypeScript 7 language server). Bun installs its npm package next to the shell packages, so package types load from node_modules. It starts on the first edit and quits after 5 idle minutes. Built-in suggestions show first and stay the fallback. Inside a query object, field and operator suggestions win.

## Tried and ruled out
- The typescript-language-server crate: tree-sitter only, no package types.
- pingdotgg/ts-rust: an unreviewed experiment.
- Embedding tsrs crates: not published, internal API churn.
- Official typescript under Bun: it worked, but the user chose tsrs.

## Watch out
- Answers are matched to the newest request of each kind, plus the tab and focus, so late ones never reopen a dismissed list.
- After 3 failed starts or crashes it stays off for the session, so offline typing cannot loop bun installs.
- Requests time out after 5 s.
- Insert the item's textEdit, not its label: `["my-field"]` replaces the dot.
- Globals follow the runner's order: collections, then built-ins over them, then packages.

In the code: src/db/js_ls.rs, src/app/shell_ls.rs, src/ui/js_doc.rs.
