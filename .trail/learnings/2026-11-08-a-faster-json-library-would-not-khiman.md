---
title: A faster JSON library would not speed up parsing much; trying strict JSON before json5 would
author: Khiman
date: 2026-11-08
branch: perf/raw-documents
cost: { minutes: 8, tokens: 57820 }
id: l-75ff9d49
scope: repo
status: active
src: agent
touches:
  - bench.py
  - src/app.rs
  - src/app/column_menu.rs
  - src/app/demo.rs
  - src/app/documents.rs
  - src/app/drafts.rs
  - src/app/poll.rs
  - src/app/relations.rs
  - src/app/schema.rs
  - src/app/state.rs
---

## Tried and ruled out
Swapping serde_json for sonic-rs or simd-json (researched Nov 8). They only accept strict JSON, while the query box takes relaxed shell syntax that json5 parses. Every path also converts to BSON through serde_json::Value (bson 2.15 has no direct text-to-BSON route), so a faster parser alone gains little. They also add SIMD and fallback code paths for each release target. jiter has no serde Deserializer. jsonc-parser does not accept single quotes or unquoted keys.

## Decided
The cheap win, not built yet: parse with serde_json first and fall back to json5 only when that fails, keeping json5's error message so line and column stay useful. Strict JSON is the common case for file import and pasted Extended JSON, and json5 is a slow pest-grammar parser.

In the code:
parse_value in src/db.rs (shell_to_ejson, then json5::from_str, then Bson::try_from); JSON import calls parse_value on the whole file in src/db/import.rs.
