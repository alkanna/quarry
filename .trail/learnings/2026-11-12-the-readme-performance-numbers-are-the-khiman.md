---
title: "The README performance numbers are the macOS memory footprint on a 1,000-row page, not RSS and not the stress cases"
author: Khiman
date: 2026-11-12
branch: master
cost: { minutes: 19, tokens: 106943 }
id: l-02fe34ba
scope: repo
status: active
src: agent
touches:
  - README.md
  - bench.py
---

## Decided
The README quotes memory, idle CPU and frame times measured on an M1 Pro for a full page of 1,000 rows, since a page never holds more in real use. Memory is the macOS footprint figure (what Activity Monitor shows), which is the lower of the two numbers the benchmark reports. The size claim is the download size, about 25 MB.

## Tried and ruled out
- Quoting the binary size as 25 MB: that is the release archive. The unpacked release binary is about 60 MB.
- Quoting the 10,000-row cases: they are stress cases (235 MB footprint for documents), not something a user sees.
- A startup time: the benchmark does not measure it. The nearest figure is the demo drawing 120 frames and exiting in 0.4 s.

## Watch out
RSS is much higher than footprint (107 MB against 33 MB for the demo, 140 MB against 46 MB with 1,000 documents), so anyone checking with ps will see different numbers than the README. Rerun the benchmark and update the table after a change to how tables draw or how pages are stored. The run opens app windows and can take focus, so agree a moment with the user first.

In the code:
- README.md, the table under "It is native".
- `python3 bench.py --quick` (about 4 minutes with a release build), rows "1,000 docs" and "sql content 1,000 rows", columns footprint_mb, cpu_avg, frame_p50, frame_p95.
