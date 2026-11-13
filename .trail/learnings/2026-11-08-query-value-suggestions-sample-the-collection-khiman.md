---
title: "Query value suggestions sample the collection on the server, and look up typed text only where an index or a small collection makes it cheap"
author: Khiman
date: 2026-11-08
branch: master
cost: { minutes: 14, tokens: 113508 }
id: l-c80bbb18
scope: repo
status: active
replaces: l-abc70148
src: agent
touches:
  - src/db/worker.rs
  - src/db.rs
---

## Decided
- The loaded page (50 documents) holds few values, so the caret entering a field's value asks the server once per session for that field's most frequent values from a $sample of 1000. The answer is merged after the page's own counted values and saved to values.bson so the next session has them at once.
- Pausing 400 ms while typing a string value (2+ characters) looks up values starting with it: anchored regex, first 1000 matches. Only when the field leads an index or the collection has at most 100,000 documents (estimated count); both checks are metadata reads. These results stay in memory only.
- The list stays open with "Looking up values in the collection…" while a lookup runs and rebuilds itself when the answer arrives. Right after a colon, a field's own values are listed (nothing else).

## Tried and ruled out
- distinct with a 300 ms limit as the first try: unbounded result (up to 16 MB thrown away past 200 values), and on unindexed fields of big collections it timed out on every first lookup, which showed as a failed command in the query log.

## Watch out
- Lookups are best effort and fail silently. values.bson holds real field values in plain BSON; entries of deleted connections are dropped at startup.
- Inside $elemMatch the key is relative to the array, so no lookup runs there.

In the code: Worker::field_values(.., prefix), PREFIX_SCAN_MAX, FIELD_VALUES_MAX; App::want_values / want_prefix / values_done; query::Typed; complete::value_at, merge_values, Ac.loading.
