---
title: "Query box shortcuts are not JavaScript, so anything that moves query text into the Shell must expand them"
author: Khiman
date: 2026-11-07
branch: master
cost: { minutes: 1, tokens: 1332 }
id: l-2f38afec
scope: repo
status: active
src: agent
touches:
  - src/app/shell.rs
  - src/ui/complete.rs
  - src/db.rs
---

## Decided
The `~"text"`, `#today` and `@moment` shortcuts are only understood by the Query box's parser. In JavaScript `~"abc"` is bitwise NOT and quietly becomes -1, so the query runs with no error and wrong results. Query text copied into a Shell script is parsed and written out in shell form whenever it holds `~`, `#` or `@`. The Shell's autocomplete, which reuses the Query suggestions inside find(...) arguments, swaps `~""` for a `/.../i` regex literal.

## Tried and ruled out
Filtering `#`/`@` date shortcuts out of the Shell's suggestions: `#` is not part of a JS word, so they can never match there and the filter was dead code.

## Watch out
Any new path from the Query text to JavaScript (code export, another Shell entry point) needs the same expansion. An empty `~""` is left unexpanded on purpose so the query fails to parse instead of matching every string.

In the code: query_script in src/app/shell.rs, the in_query_args branch of suggest_js in src/ui/complete.rs, the `~` handling in shell_to_ejson in src/db.rs.
