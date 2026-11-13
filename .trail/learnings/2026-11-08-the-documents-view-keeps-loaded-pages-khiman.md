---
title: The Documents view keeps loaded pages as BSON bytes because decoded documents take about 11x the memory
author: Khiman
date: 2026-11-08
branch: perf/raw-documents
cost: { minutes: 20, tokens: 211772 }
id: l-54e7757c
scope: repo
status: active
replaces: l-81ef60ff
src: agent
touches:
  - src/db.rs
  - src/db/worker.rs
  - src/app/poll.rs
  - src/app/documents.rs
  - src/app/state.rs
  - src/app.rs
  - src/app/drafts.rs
  - src/app/table.rs
  - src/ui/cells.rs
---

## Decided
Documents view pages are kept as raw BSON (one RawDocumentBuf per row). Table cells, the JSON list and column widths read values straight from the bytes. A full Document is decoded only for the open or expanded row (kept until it closes), for an edit, and for actions on selected rows (decoded for the moment). Other result tables (Aggregate, Shell, Compare, Import, Indexes, Watch) still hold decoded documents, and the shared table takes either through a small enum.
Measured Nov 8 with bench.py: 50 documents of about 1 MB went from 534 MB to 101 MB footprint, and 50k small documents from 996 MB to 528 MB. Screenshots of 17 demo scenarios came out pixel-identical before and after.

## Tried and ruled out
Trimming capacity after decoding: bson 2.15 builds documents one field at a time and cannot shrink or preallocate. Decoding only visible rows: a 50-row page is mostly visible, so it saved about half at best and gave it back on scroll.

## Watch out
Each page is still decoded once on the UI thread when it arrives, to check that every row reads and to collect fields and paths. Do not keep that decoded copy. Code that needs a row as a Document must use the row's decoded copy, or a temporary one. Do not hold decoded rows in the tab.
A raw document with duplicate keys returns the first value, while a decoded one keeps the last. MongoDB does not normally store duplicates.

In the code:
db::Row and db::get_path_raw in src/db.rs; Rows in src/app/table.rs; Val and raw_row_job in src/ui/cells.rs; the eviction of closed rows in results() in src/app/documents.rs; find reads through clone_with_type::<RawDocumentBuf>() in src/db/worker.rs. The parity test is raw_rows_render_like_decoded_ones, and the live test live_find_pages_rows_as_bson_bytes runs with QUARRY_TEST_MONGO.
