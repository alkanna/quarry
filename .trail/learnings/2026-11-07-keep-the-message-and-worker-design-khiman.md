---
title: Keep the message-and-worker design and split the big UI file into modules instead of a rewrite
author: Khiman
date: 2026-11-07
branch: refactor/ui-theme-widgets
cost: { minutes: 1, tokens: 1425 }
id: l-ba0a83c0
scope: repo
status: active
src: agent
touches:
  - src/main.rs
  - src/app.rs
---

## Decided
The app keeps its current design: egui draws from the App state each frame, a tokio Worker runs every Mongo, SSH and tool call and returns a Msg, and App::poll applies it. That is the Elm-style message and update pattern and fits a desktop DB client. The cleanup is a pure file split: no renames or logic changes per step, so diffs stay reviewable. Moved items get pub(crate) and app.rs glob-imports them back, and `impl App` blocks can live in separate files, so views move without new types.

Design tokens (palettes, text sizes, radii, sizes, egui style) live in one theme module; shared widgets and cell rendering live next to it. Literal colors and sizes should only appear in the theme module.

## Tried and ruled out
- Trait-per-repository or hexagonal layering: Mongo is the only backend, so every trait would have one implementation.
- Moving to iced, gpui or Tauri: a rewrite with no user-facing gain.
- A cargo workspace split into core and ui crates: deferred until compile times hurt; the module split makes it a file move later.

## Watch out
The codebase is not rustfmt-formatted (it uses long lines and has no rustfmt config). Running cargo fmt rewrites about 7,800 lines across every file. Do not run it as part of a refactor.

In the code:
- src/ui/theme.rs, src/ui/widgets.rs, src/ui/cells.rs, wired by `mod ui;` in src/main.rs and `use crate::ui::{cells::*, theme::*, widgets::*};` in src/app.rs
- Remaining steps: editor code into ui/code, db.rs into a mongo module, App state/config/poll/demo into app/, one file per view, then split modals()
- Check after each step: cargo build, cargo test (59 tests), cargo clippy (45 pre-existing warnings)
