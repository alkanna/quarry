---
title: "Column stats run on numbers or dates, whichever the column holds more of, and skip booleans"
author: Khiman
date: 2026-11-07
branch: master
cost: { minutes: 2, tokens: 35678 }
id: l-928fe835
scope: repo
status: active
replaces: l-23fd8760
src: agent
touches:
  - src/db/analysis.rs
---

## Decided
Sum, average, median and range from the column popover pick one kind per column: dates when there are more dates than numbers, otherwise numbers. Mixing both in one result makes no sense. Numbers stored as text count as numbers because imported data often stores them that way, except leading-zero text like "007", which is a code (import keeps it as text for the same reason). Date and Timestamp both count as dates. Sum of dates returns a short explanation instead of a value. Dates go through the same f64 path as numbers since epoch millis are exact in f64.

## Tried and ruled out
Counting booleans as 0/1 was left out for now because it is unclear whether users want it. Dates written as text are not read as dates either. A separate i128 date path was dropped as duplicate code.

## Watch out
Only Median sorts; Sum, Avg and Range are single passes so 100k values stay cheap. Text like "NaN" and "inf" parses as f64 in Rust, so it is filtered out with is_finite.

In the code: src/db/analysis.rs stat_result, num, date_ms, fmt_span; ColOp::Range in ColOp::ALL; test column_data_ops.
