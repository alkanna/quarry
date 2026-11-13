---
title: "Schema analysis merges keys that are data, like map ids, into one row by their pattern"
author: Khiman
date: 2026-11-09
branch: master
cost: { minutes: 4, tokens: 16187 }
id: l-203b3816
scope: repo
status: active
src: agent
touches:
  - src/app/config.rs
  - src/app/demo.rs
  - src/app/poll.rs
  - src/app/shell.rs
  - src/app/shell_ls.rs
  - src/app/state.rs
  - src/db/js_ls.rs
  - src/ui/complete.rs
  - src/app/schema.rs
  - src/db/analysis.rs
---

## Decided
Schema analysis looks at each object level across all sampled documents at once. Keys that are data rather than names are merged into one `<id>` row: keys that look generated alone (UUID, ObjectId or other long hex, numbers, dates) and keys that share one pattern with siblings (digits as 9, letters as a or A, hex ids as all x). A pattern with a digit merges at 3 keys; letters only needs 10, because real field names often share a length (name, type, city, date). Building level by level also keeps every field under its parent.

## Tried and ruled out
Judging keys one by one (UUID, hex, number checks only): misses other id formats such as `TRV-00123`. Walking documents one at a time then sorting rows into tree order: works for order but cannot compare a key with its siblings in other documents.

## Watch out
Merged rows and everything under them are flagged dynamic: no column or filter actions, and the generated validator leaves map entries unconstrained. Language-code maps (fr, en, de...) merge only past 10 keys.

In the code: dynamic_key, key_shape, dynamic_keys and analyze_schema in src/db/analysis.rs; FieldStat.dynamic; folding in src/app/schema.rs.
