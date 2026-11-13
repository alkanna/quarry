---
title: "Secrets are hidden from agents everywhere, personal data stays queryable but only leaves masked"
author: Khiman
date: 2026-11-13
branch: feat/ai-agents-mcp
cost: { minutes: 14, tokens: 44153 }
id: l-3ac9efbc
scope: repo
status: active
replaces: l-1c05940d
src: agent
touches:
  - src/agents/config.rs
  - src/agents/guard.rs
  - src/agents/policy.rs
  - src/agents/scrub.rs
  - src/agents/store.rs
  - Cargo.lock
  - Cargo.toml
  - src/agents/audit.rs
  - src/agents/detectors.rs
  - src/agents/mod.rs
---

## Decided
- Each scrub preset and custom rule has a mode. Hide: an agent can never reference the field, not in filters, sorts, expressions or writes. Mask output: the field can be used in filters (any operator), sorts, join keys and plain projections, and can take literal writes, but it can't feed anything that outputs it under another key ($project computed fields, $group, $replaceRoot, distinct, update pipelines, $rename from it). Its values leave masked. Defaults: Secrets is Hide, personal data is Mask output, as with Snowflake or BigQuery dynamic masking. A file without a mode loads as Hide.
- Free MongoDB pipelines run whenever this guard passes. Operators that reach the whole document or pick field names at run time ($$ROOT, $objectToArray, $function, $where, computed $getField) are refused wherever any field rule applies.
- Only free-form text tools (free SQL, run_command beyond its allow-list, the shell) are switched off by "rules hide data", and only Hide rules count. For SQL they are matched against catalog columns, for MongoDB against keys found by schema sampling.

## Tried and ruled out
- Counting every name rule as hiding: with the presets on by default, find({email: "x"}) was refused and MongoDB got only a restricted aggregate. The user found that too strict.
- A restricted-aggregate mode for MongoDB: the per-reference guard plus refusing whole-document operators covers the same leaks without blocking normal pipelines.

## Watch out
- Masking matches by key name, so a stringified copy of a parent document ($toString of $customer) could still carry masked values out. Once sampled keys are wired in, use them to treat object paths as ancestors.
- The caller must pass the field lists (catalog columns or sampled keys). With none, the check is strict and free-form text tools are off.
- A hidden field never takes writes: a literal $set reveals equality through modifiedCount.

In the code: PresetMode in src/agents/config.rs; NsRules::{hides, masks, hides_data} in src/agents/scrub.rs; predicate and output reference checks in src/agents/guard.rs; policy::hides_data in src/agents/policy.rs.
