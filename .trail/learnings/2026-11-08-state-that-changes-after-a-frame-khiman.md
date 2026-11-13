---
title: "State that changes after a frame is drawn needs an explicit redraw, or it only shows with the next input"
author: Khiman
date: 2026-11-08
branch: master
cost: { minutes: 13, tokens: 50948 }
id: l-5abb3af6
scope: repo
status: active
src: agent
touches:
  - src/app.rs
  - src/app/config.rs
  - src/app/documents.rs
  - src/app/drafts.rs
  - src/app/poll.rs
  - src/app/query.rs
  - src/app/shell_ls.rs
  - src/app/state.rs
  - src/app/tabs.rs
  - src/db.rs
---

## Decided
- egui redraws only on input, a worker message, or a requested repaint. The suggestion lookup is sent after the query box was drawn, so its "looking up" row stayed invisible until the answer arrived and cleared it. Sending a lookup now requests a repaint.

## Watch out
- Any new background request whose pending state the UI shows needs the same: request a repaint when it is sent, not only when its answer comes back. A drawn egui Spinner keeps repainting by itself.

In the code: App::want_values returns whether it sent a lookup; documents() calls ui.ctx().request_repaint() then.
