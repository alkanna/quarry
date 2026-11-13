---
title: "Result tables draw every row because none holds more than 1,000, so skipping off-screen rows was dropped"
author: Khiman
date: 2026-11-08
branch: master
cost: { minutes: 17, tokens: 121925 }
id: l-6888a788
scope: repo
status: active
replaces: l-0b41fbe8
src: agent
touches:
  - src/app/shell.rs
  - src/app/state.rs
  - src/app/relations.rs
  - src/db.rs
  - src/db/analysis.rs
  - src/app/chrome.rs
  - bench.py
---

## Decided
Result tables keep drawing every row. Documents, Shell, Compare and Import show 50-row pages, Aggregate keeps at most 1,000 results and Watch 500. Memory work goes into how rows are kept (BSON bytes, one copy) rather than how they are drawn.

## Tried and ruled out
Skipping off-screen rows: it cut frames from 19 ms to 1.5 ms only in bench.py's 50,000-doc fixture, which puts every row in one page. At the real 1,000-row maximum, frames went from 0.9 to 1.0 ms. It also shifted egui's automatic ids, so an open row menu could act on another row after a scroll.
A single page buffer instead of one RawDocumentBuf per row: pages are 50 rows, so it saves almost nothing.

## Watch out
The bench.py 50,000-doc fixture is not a real case. Use 1,000 docs for the table cost. The 50 x 1 MB idle footprint varies between about 142 and 161 MB from run to run, so compare several runs.

In the code:
db::values_at reads raw rows (#119); ShellTab keeps documents as db::Row with result() rebuilding the value (#122); status readouts in src/app/chrome.rs, QUARRY_TEST_STATUS=1 (#121).
