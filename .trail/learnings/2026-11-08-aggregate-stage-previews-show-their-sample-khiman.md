---
title: Aggregate stage previews show their sample even when counting times out
author: Khiman
date: 2026-11-08
branch: master
cost: { minutes: 13, tokens: 163788 }
id: l-48400734
scope: repo
status: active
src: agent
touches:
  - src/ui/complete.rs
  - src/db/worker.rs
  - src/app/aggregate.rs
  - src/app/state.rs
---

## Decided
- The default pipeline starts with { $match: {} }, whose preview used to $count the whole collection and fail with MaxTimeMSExpired on big collections. Stages that are only empty $match now use the estimated count, and any other count that fails is left out while the sample still shows, with a "too many documents to count" note.

In the code: Worker::stage_preview, Msg::StagePreview count is Option<u64>, stage_card label.
