---
title: Index usage counts are optional because Atlas refuses them on admin system collections
author: Khiman
date: 2026-11-08
branch: feat/watch-view
cost: { minutes: 18, tokens: 87126 }
id: l-b0df438a
scope: repo
status: active
src: agent
touches:
  - src/db/worker.rs
  - src/app/indexes.rs
---

## Decided
Index usage counts ($indexStats) are best effort, like index sizes. A refusal leaves the usage columns empty with a one-line note in the Indexes view, and never reaches the status bar.

## Watch out
Atlas refuses $indexStats on admin's system collections (system.roles, system.users) even for a user with atlasAdmin, with code 13 Unauthorized. Before this fix, a restored tab on such a collection showed the raw command in the status bar on every launch, and the profiler suggestions were lost with it.

In the code: Worker::index_info in src/db/worker.rs keeps stats_err on IndexInfo; stats_note in src/app/indexes.rs turns it into the note.
