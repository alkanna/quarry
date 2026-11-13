---
title: "Benchmarks run offline on the app's test switches with a throwaway HOME"
author: Khiman
date: 2026-11-08
branch: master
cost: { minutes: 7, tokens: 76356 }
id: l-21b1ede1
scope: repo
status: active
src: agent
touches:
  - bench.py
  - src/app.rs
---

## Decided
Memory and CPU benchmarks drive the app through its existing offline test switches instead of a live MongoDB server or GUI automation. Idle runs use the interactive demo, and render runs use screenshot mode, which redraws 120 frames back to back and then exits.

## Watch out
Fixture mode without screenshot mode clears the demo flag, so the app saves its open tabs to the real ui.json. Every benchmark run must point HOME at a temporary directory.
First baseline (Nov 8): 50 documents of about 1 MB each (48 MB of JSON) used about 534 MB of footprint and 1.1 GB of RSS, roughly 11x the data. Frame time grows with the number of documents in a tab even though only the visible rows are drawn: 18 ms per frame at 50k documents, so some code walks every document on every frame.

In the code:
bench.py (python3 bench.py [--quick]); QUARRY_DEMO=1, QUARRY_TEST_JSON=<file>, --screenshot <png>, QUARRY_FRAME_LOG=1; /usr/bin/time -l for peak RSS, footprint -p for phys_footprint.
