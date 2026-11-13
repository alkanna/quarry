---
title: Dotted keys are accepted without quotes in query text
author: Khiman
date: 2026-11-08
branch: master
cost: { minutes: 14, tokens: 113508 }
id: l-96e4b86a
scope: repo
status: active
src: agent
touches:
  - src/db/worker.rs
  - src/db.rs
---

## Decided
- `{ booker.username: "x" }` parses as the key "booker.username". JSON5 and JavaScript only take dotted keys quoted, but the suggestion list inserts paths unquoted, so the parser quotes any unquoted key that contains a dot and is followed by a colon.

## Watch out
- Text sent to the Shell (JavaScript) is not run through this parser, so a dotted unquoted key there is still a syntax error.

In the code: db::shell_to_ejson, the branch before the identifier handling; test parses_shell_syntax.
