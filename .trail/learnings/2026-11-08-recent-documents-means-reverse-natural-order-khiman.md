---
title: "Recent documents means reverse natural order, because ids are often not ObjectIds"
author: Khiman
date: 2026-11-08
branch: master
cost: { minutes: 13, tokens: 163788 }
id: l-1a702521
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
- Schema analysis offers last 1,000 / 10,000 / 100,000 besides first N. Last reads in reverse natural order ($natural: -1), which is insertion order, since some collections use UUID strings for _id and sorting on _id would not mean recent.

## Watch out
- A $natural sort ignores indexes, so last N with a very selective query scans backwards until it finds N matches or hits the timeout.

In the code: SchemaTab.last, Worker::schema(.., last).
