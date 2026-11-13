---
title: "Only Aggregate and Watch tables show unpaged rows, so they skip off-screen rows and measure at most 1,000 rows for column widths"
author: Khiman
date: 2026-11-08
branch: master
cost: { minutes: 21, tokens: 183780 }
id: l-0b41fbe8
scope: repo
status: active
src: agent
touches:
  - src/app.rs
  - src/app/chrome.rs
  - bench.py
  - src/app/table.rs
---

## Decided
Documents, Shell, Compare and Import show 50 rows per page, but Aggregate and Watch show every row they hold. The result table and JSON list now give the rows above and below the view one block of space instead of a widget each, and column widths are measured from the first 1,000 rows only. Open or selected rows are always laid out because their height varies and the selected one may need scrolling to.
Related-document lookups read ids from the BSON bytes instead of decoding the whole page.
The frame rate readout averages half a second of frames. A single frame's 1/dt went over the display's refresh rate (200+ fps on 144 Hz) when an input event landed just after a vsync.

## Tried and ruled out
Storing a page in one byte buffer with offsets instead of one RawDocumentBuf per row: pages are 50 rows, so it saves almost nothing, and Row's per-row decode cache would need reworking.

## Watch out
The bench.py 50,000-doc fixture puts every row in one page, which only Aggregate and Watch do in real use. Measured Nov 8: 50k docs went from 361 MB footprint, 1.7 GB RSS and 19 ms frames to 80 MB, 180 MB peak RSS and 1.5 ms. The 50 x 1 MB idle footprint varies between about 142 and 161 MB from run to run on master too, so compare several runs before calling a change there.

In the code:
row_in_view and flush_rows in src/app/table.rs; optimal_width caps at 1,000 rows; db::values_at takes RawDocuments; status readouts in status_bar, sample_usage and process_usage in src/app/chrome.rs (libc on unix, windows-sys on Windows); QUARRY_TEST_STATUS=1 turns them on in demo and screenshot mode.
