---
title: Maps the analysis cannot guess are marked by hand from the schema row menu
author: Khiman
date: 2026-11-09
branch: master
cost: { minutes: 10, tokens: 43535 }
id: l-866fb63e
scope: repo
status: active
src: agent
touches:
  - src/db/analysis.rs
  - src/app/config.rs
  - src/db/worker.rs
  - src/app/schema.rs
---

## Decided
Maps that no guess can tell from fields (labels keyed by name like valueCodes {"COMEX": ..., "RDV SIEGE": ...}, whose values may differ from the keys) are merged by hand: the schema row menu offers "Treat its keys as map entries" on object fields, saved per collection, and analysis runs again with that path forced to merge every key.

## Tried and ruled out
Merging 10 or more ALL-CAPS sibling keys: feature flags and constants look the same. Matching a key to its own value: the values can differ from the keys.

## Watch out
Toggling re-samples the collection rather than regrouping the old stats, because a merged row needs exact per-document counts.

In the code: UiPrefs.schema_maps keyed by Tab::coll_key; analyze_schema(docs, maps); action 3 in schema_view.
