---
title: "Query value suggestions sample the collection, and look up typed text only where an index or a small collection makes it cheap"
author: Khiman
date: 2026-11-08
branch: master
cost: { minutes: 9, tokens: 96156 }
id: l-f47ef069
scope: repo
status: active
replaces: l-c80bbb18
src: agent
---

## Decided
- The caret entering a field's value asks the server once per session for its most frequent values from a $sample of 1000, merged after the page's values and kept in values.bson for the next session.
- A 400 ms pause while typing a string value (2+ characters) looks up values starting with it (anchored regex, first 1000 matches), only when the field leads an index or the collection has at most 100,000 documents. Otherwise the list says the field is not indexed, so a missing suggestion is not a silent mystery.
- Both lookups are capped at 2 s and carry the comment "quarry: value suggestions", so their failures in the query log are recognizable.

## Tried and ruled out
- distinct with a 300 ms limit first: unbounded result, and it timed out on every big unindexed field, showing as a failed command in the log.

## Watch out
- A collection under 100,000 documents with very large documents can still hit the 2 s cap on an unindexed regex; the list then says the lookup was too slow.

In the code: Worker::field_values (None = skipped), PREFIX_SCAN_MAX; App::want_values / values_done, values_note; query::Lookup, Typed; Ac.loading / Ac.note.
