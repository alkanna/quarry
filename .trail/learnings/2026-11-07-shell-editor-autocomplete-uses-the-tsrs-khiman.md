---
title: "Shell editor autocomplete uses the tsrs language server, started only while a script is edited"
author: Khiman
date: 2026-11-07
branch: master
cost: { minutes: 10, tokens: 134261 }
id: l-286b83a5
scope: repo
status: active
src: agent
touches:
  - src/db/shell_complete.mjs
  - src/db/js_ls.rs
  - src/db/shell.rs
  - src/app/shell.rs
  - src/ui/complete.rs
---

## Decided
Shell autocomplete for packages (dayjs., dateFns., lodash, collections, cursors) comes from tsrs (Oxc/Cloudflare's Rust port of the TypeScript 7 checker and language server). Its npm package is installed by Bun next to the shell's other packages, and we speak LSP to its native binary over stdio. Package types come from node_modules, so Bun stays required. The server starts on the first edit in the Shell editor and quits after 5 minutes without requests. The built-in hand-written suggestions still show first and stay as the fallback. Inside a query object, field and operator suggestions win because TypeScript knows no document fields.

## Tried and ruled out
- The typescript-language-server crate: tree-sitter only, cannot read package .d.ts files, barely used.
- pingdotgg/ts-rust: an unreviewed month-old experiment.
- Embedding tsrs crates in-process: not on crates.io, internal API changes daily, big build, and Bun is still needed for the packages.
- Official typescript's language service under Bun: it worked in a prototype, but the user chose tsrs.

## Watch out
- After a dot with no known members, tsrs offers plain words (CompletionItemKind 1), so those are dropped.
- On callable packages, Function.prototype members (apply, bind, call...) are hidden.
- The shell folder path has a space on macOS, so the file URI is percent-encoded.
- No Windows ARM64 tsrs build exists yet; there autocomplete stays built-in only.
- tsrs is pinned to an exact beta version.

In the code: src/db/js_ls.rs (run_js_ls, js_globals, ignored end-to-end test js_ls_end_to_end), src/db/shell.rs prepare_shell_dir and EDITOR_PACKAGES, src/app/shell.rs ask_js_ls and js_completed.
