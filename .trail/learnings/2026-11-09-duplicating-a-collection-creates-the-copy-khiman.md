---
title: Duplicating a collection creates the copy first so a taken name fails instead of being replaced
author: Khiman
date: 2026-11-09
branch: feat/duplicate-collection
cost: { minutes: 3, tokens: 28555 }
id: l-46cdfaa3
scope: repo
status: active
replaces: l-63a0b513
src: agent
touches:
  - src/app/modals.rs
  - src/app/poll.rs
  - src/app/sidebar.rs
  - src/db.rs
  - src/db/worker.rs
  - src/ui/widgets.rs
---

## Decided
Duplicating a collection creates the target first with the source options from listCollections (validator, collation, clusteredIndex minus v), then copies with $merge whenMatched fail and bypassDocumentValidation, then recreates indexes from raw listIndexes specs. create fails atomically when the name is taken.

## Tried and ruled out
$out with a list_collection_names check first: $out silently replaces an existing collection, and another client can create the name between the check and the copy. It also drops validator and collation.

## Watch out
Capped and time-series collections are refused ($merge cannot write into them); Dump and Restore covers those. An index failure after the copy is reported as such, since the documents are already there. The copy has its own Msg::Duplicated carrying the new name so its sidebar spinner and row survive unrelated refreshes.

In the code: duplicate_coll in src/db/worker.rs, App.copying, Msg::Duplicated mapped to DbChanged at the top of poll.
