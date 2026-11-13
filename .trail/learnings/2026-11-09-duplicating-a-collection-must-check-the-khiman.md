---
title: Duplicating a collection must check the name on the server because the copy replaces an existing one
author: Khiman
date: 2026-11-09
branch: feat/duplicate-collection
cost: { minutes: 2, tokens: 37467 }
id: l-63a0b513
scope: repo
status: active
src: agent
touches:
  - src/app.rs
  - src/app/modals.rs
  - src/app/poll.rs
  - src/app/sidebar.rs
  - src/app/state.rs
  - src/db/worker.rs
---

## Decided
Duplicating a collection copies documents with an aggregate $out into the new name, then recreates the indexes from listIndexes (minus _id_ and ns), since $out copies documents only. The name is checked twice: in the dialog against the loaded sidebar list, and again on the server right before $out.

## Watch out
$out silently replaces a collection that already exists, so the server-side check must stay even though the dialog checks too (another client may create the name meanwhile). The sidebar shows the new name with a spinner until any change in that database finishes; two copies at once in one database lose the spinner early.

In the code: duplicate_coll in src/db/worker.rs, Modal::DuplicateColl in src/app/modals.rs, App.copying cleared on Msg::DbChanged in src/app/poll.rs.
