---
title: Duplicating a collection checks the target name first because MongoDB 7 no longer refuses an existing one on create
author: Khiman
date: 2026-11-09
branch: feat/mysql
cost: { minutes: 30, tokens: 346806 }
id: l-e87093cf
scope: repo
status: active
src: agent
touches:
  - docs/sql/PLAN.md
  - src/app.rs
  - src/app/chrome.rs
  - src/app/config.rs
  - src/app/demo.rs
  - src/app/modals.rs
  - src/app/rail.rs
  - src/app/shell.rs
  - src/app/shell_attach.rs
  - src/app/shell_ls.rs
---

## Decided
Duplicate collection lists the target name before creating it and refuses when it exists. Creating the copy first still stays, for its options and for a name taken in the meantime with different options.

## Tried and ruled out
Relying on `create` alone to refuse a taken name: since MongoDB 7, create on an existing collection with the same options succeeds, so the copy went on to $merge, which then failed with a raw E11000 duplicate key error instead of "already exists".

## Watch out
Any code that relies on create failing for an existing collection behaves differently on MongoDB 7 and later.

In the code:
- src/db/worker.rs `duplicate_coll`: third read `listCollections` with `filter: { name: to }, nameOnly: true` in the same try_join
- test: live_duplicate_copies_documents_and_indexes_and_refuses_a_taken_name (needs QUARRY_TEST_MONGO, failed on mongo:8 before)
