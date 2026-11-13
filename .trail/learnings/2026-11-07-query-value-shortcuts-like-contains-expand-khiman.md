---
title: Query value shortcuts like contains expand to $regex documents because the query parser has no /regex/ literals
author: Khiman
date: 2026-11-07
branch: master
cost: { minutes: 1, tokens: 1139 }
id: l-bac20e46
scope: repo
status: active
src: agent
touches:
  - src/ui/complete.rs
---

## Decided
Value shortcuts in the Query box (contains, startsWith, notEmpty, between, ...) are autocomplete snippets typed by name after a field's colon. They expand in the editor to plain Extended JSON, so nothing new is needed when the query runs.

## Tried and ruled out
Inserting `/text/i` regex literals: the query parser (json5 plus shell_to_ejson) does not accept them, so `{ $regex: "...", $options: "i" }` is used instead.

## Watch out
Snippets are expanded in the editor, before the user types the text, so they cannot escape it. Anything that must transform the user's text (escaping, case folding) belongs in a run-time shortcut expanded in shell_to_ejson, like the `#today` dates.

In the code: VALUE_SNIPPETS in src/ui/complete.rs (`|` marks the caret), offered in suggest() in the `lead.ends_with(':')` Query branch; tested in autocomplete_contexts.
