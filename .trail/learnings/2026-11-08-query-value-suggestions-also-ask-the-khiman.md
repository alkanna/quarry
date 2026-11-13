---
title: "Query value suggestions also ask the server for the field's values, and keep them on disk"
author: Khiman
date: 2026-11-08
branch: master
cost: { minutes: 13, tokens: 163788 }
id: l-abc70148
scope: repo
status: active
src: agent
touches:
  - src/ui/complete.rs
  - src/db/worker.rs
  - src/app/aggregate.rs
  - src/app/state.rs
---

## Decided
- The loaded page (50 documents) only holds a few values, so typing a value for a field asks the server once per session for that field across the whole collection. The answer is merged after the page's own counted values and saved to a cache file, so the next session suggests them before its own lookup returns.
- distinct first with a 300 ms limit: complete and quick when the field is indexed. If it times out or returns more than 200 values, a $sample of 1000 grouped by value takes over, most frequent first.
- Server distinct is fine here even though column stats ruled it out: suggestions already count array items one by one, so unwinding arrays matches.

## Watch out
- Failures are silent on purpose (suggestions are best effort). The cache holds real field values in plain BSON in the config folder.
- The lookup fires from the suggestion compute closure, which runs on every edit even with no list open; that is how a field missing from the page still triggers it.

In the code: Worker::field_values, Msg::FieldValues, FIELD_VALUES_MAX; App::want_values / values_done / save_values, load_values reads values.bson; complete::value_field, merge_values, suggestible.
