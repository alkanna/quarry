---
title: "Schema analysis also merges map keys whose values share one structure, for name-like keys"
author: Khiman
date: 2026-11-09
branch: master
cost: { minutes: 2, tokens: 58245 }
id: l-a63a452b
scope: repo
status: active
src: agent
touches:
  - src/db/analysis.rs
---

## Decided
Schema analysis also merges keys by structure: 5 or more sibling keys whose object values share at least half their fields (Jaccard >= 0.5 on the union of child keys seen across sampled documents) become one <id> row. This catches maps keyed by free text such as passenger names (recapsOfPax.ERIC-ROMME, EMILIE-DE FIGUEIREDO), which share no key pattern.

## Watch out
5 or more sibling fields holding alike objects (for example many address fields with the same street/city/zip) would also merge. Raise the threshold in alike_keys if that shows up.

In the code: alike_keys called from level in src/db/analysis.rs, next to dynamic_keys.
