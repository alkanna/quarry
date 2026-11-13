---
title: "Sidebar rows for work still running are drawn from an in-progress set, not pushed into the server list"
author: Khiman
date: 2026-11-09
branch: feat/duplicate-collection
cost: { minutes: 3, tokens: 48190 }
id: l-e06468c4
scope: repo
status: active
src: agent
touches:
  - src/app/modals.rs
  - src/app/poll.rs
  - src/app/sidebar.rs
  - src/db.rs
  - src/db/worker.rs
  - src/app.rs
---

## Decided
Sidebar rows for work still running on the server (a collection being copied) are drawn from an in-progress set on App, not pushed into the collection list, so a list reload cannot drop them. The worker reports the finish through DbChanged with the affected name, which clears the set and lists the name right away so the row does not blink before the reload answers.

## Tried and ruled out
A separate message remapped to DbChanged at the top of poll: needed a dead match arm. Pushing the pending name into the list and merging it back on every reload: two places for one idea.

## Watch out
Dropping a database uses its own older field for the same idea. A third in-progress case (rename, drop collection) should unify them into one pending map instead of adding another field.

In the code: App.copying and dropping_db in src/app.rs, DbChanged.copied in src/db.rs, the pending rows in the sidebar collection loop.
