---
title: "Hiding rules turn off free SQL only when a reachable column matches, but always count for MongoDB"
author: Khiman
date: 2026-11-12
branch: feat/ai-agents-mcp
cost: { minutes: 9, tokens: 52527 }
id: l-1c05940d
scope: repo
status: active
src: agent
touches:
  - src/agents/config.rs
  - src/agents/mod.rs
  - src/main.rs
  - src/db/sql/worker.rs
---

## Decided
"Rules hide data here" (which turns off free SQL, free pipelines and the shell for agents) depends on what the connection's schema shows. On SQL engines it holds only when an enabled field path or name rule, built-in ones included, matches a real column of a table the agent can reach, checked against the full column list from the catalog. On MongoDB, which has no complete field list, any enabled path or name rule scoped to the namespace counts. Anything uncertain counts as hiding: a rule that fails to compile, or a column list that isn't loaded.

## Tried and ruled out
- Counting every enabled name rule everywhere: the shipped secret-names rule is on by default, so free SQL and the shell would be off on every connection from the start.
- Leaving built-in rules out of the check: output scrubbing alone does not stop `SELECT password AS p` or `$objectToArray` from leaking a hidden value.

## Watch out
MongoDB users on default settings get the structured tools and the restricted aggregate, not free pipelines, until they turn off or narrow the secret-names rule.

In the code: NsRules::hides_data(known: Option<&[String]>) and Rules::hides_any in src/agents/scrub.rs; SQL column lists come from sql_schema / load_schema in src/db/sql/worker.rs.
