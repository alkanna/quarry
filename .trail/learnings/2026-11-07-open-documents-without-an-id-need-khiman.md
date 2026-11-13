---
title: Open documents without an _id need the row in their tree id
author: Khiman
date: 2026-11-07
branch: master
cost: { minutes: 3, tokens: 84307 }
id: l-45808972
scope: repo
status: active
src: agent
touches:
  - src/app/tree.rs
---

## Decided
The id behind a document's field tree includes the row index whenever the document has no _id (and always for result views), so each open card keeps its own fold state.

## Watch out
If two open cards share that id, egui gives their headers the same id: one click toggles every copy (open then closed again, a flicker), and expanding a field in one card shows it expanded in the others.

In the code: doc_body in src/app/tree.rs builds the id with egui::Id::unique.
