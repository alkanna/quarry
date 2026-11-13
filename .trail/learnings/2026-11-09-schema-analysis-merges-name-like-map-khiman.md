---
title: Schema analysis merges name-like map keys by alike object values that vary between documents
author: Khiman
date: 2026-11-09
branch: master
cost: { minutes: 3, tokens: 17515 }
id: l-bd1949ba
scope: repo
status: active
replaces: l-a63a452b
src: agent
touches:
  - src/db/analysis.rs
---

## Decided
Schema analysis also merges map keys that share no pattern, like passenger names (recapsOfPax.ERIC-ROMME), by structure: 5 or more sibling keys whose object values overlap on at least half of the smaller field set, and that vary between objects (no single object holds more than half of the group). The variation check keeps real fields of one shape, like price/tax/total each {amount, currency}, as fields, since they repeat in every document.

## Tried and ruled out
Jaccard against the group first key: a sparse first entry like {url} split off from {url, sent, sentAt}, so the result depended on document order.

## Watch out
Map entries holding {} or null still show as named rows; merging them needs a looser rule that brings back false merges. Grouping only checks the first 64 groups per key to bound the cost.

In the code: alike_keys called from level in src/db/analysis.rs after dynamic_keys, skipping keys it already flagged.
