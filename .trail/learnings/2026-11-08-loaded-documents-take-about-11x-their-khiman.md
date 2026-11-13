---
title: Loaded documents take about 11x their size in memory because of how bson decodes them
author: Khiman
date: 2026-11-08
branch: master
cost: { minutes: 4, tokens: 35967 }
id: l-81ef60ff
scope: repo
status: active
src: agent
touches:
  - src/db/worker.rs
  - src/app/state.rs
---

## Watch out
Decoding BSON into bson::Document costs about 11x the wire size (measured Nov 8: 35.6 MB of BSON became 396 MB, and 3.9 MB of small documents became 44 MB). Each field is a 112-byte value plus its own key allocation, and every subdocument is its own hash map whose entry storage grows to about 1 KB even for 4 fields. This, not the app's derived page data (field lists, paths, autocomplete info, which are capped), is why 50 documents of 1 MB each use over 500 MB.

## Tried and ruled out
Trimming capacity after decoding: bson 2.15 builds documents by inserting one field at a time (FromIterator and the serde visitor both start from Document::new) and has no way to shrink or preallocate. Only not decoding avoids the cost.

## Decided
The proposed fix, not built yet: keep result pages as raw BSON (RawDocumentBuf, about 1x), read table cells from the bytes, and decode a full Document only for the opened, expanded or edited document.

In the code:
find collects Vec<Document> in src/db/worker.rs (find); Tab.docs and agg_docs in src/app/state.rs; about 50 uses of `.docs`. Measured with a counting global allocator over bson::from_slice, plus bench.py.
