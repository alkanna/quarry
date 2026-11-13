---
title: Agent writes are decided on a snapshot and undone from their own store
author: Khiman
date: 2026-11-13
branch: worktree-agent-a8010dba212b9d70b
cost: { minutes: 33, tokens: 79387 }
id: l-5b3d77d9
scope: repo
status: active
src: agent
touches:
  - src/app/agents/exec.rs
  - src/db/agent.rs
  - src/db/sql/agent.rs
  - src/agents/undo.rs
  - src/app/agents/mod.rs
  - src/db/sql/write.rs
---

## Decided
- An agent write is decided twice: once on arrival without a count, then again when AgentEvent::Prepared brings the snapshot (the matched documents or rows, up to Limits.undo_docs plus one, counted on their own past that). The count meets the large-change threshold, and the snapshot meets the undo caps. A write over a cap always asks, even with Change data set to Allow, and the dialog's only allow is Allow without undo, which then runs on the filter.
- MongoDB writes go document by document with find_one_and_update on unchanged(before), so a document changed after the snapshot is skipped and reported. Undo puts each one back only while it is still as the agent left it (unchanged(after)); an agent insert undo deletes with unchanged too, unlike the user's Insert undo.
- SQL row tools reuse the grid's apply_updates, apply_deletes and apply_inserts, so their undo is the grid's SqlUndo, run by sql::write::undo_rows (the body of Worker::sql_undo). Its answer comes back as AgentEvent::Undone, never Msg::Written, so the Activity entry knows how it ended. UndoAction.agent still routes any Written undo back to the agent store.
- The undo store is per Activity id, in memory, capped in total bytes with the oldest evicted; an evicted call says undo expired.

## Tried and ruled out
- Splitting apply_undo_as into run_undo for agents: the user's MongoDB undo arms are unguarded and answer through Written, which can't tell the Activity entry it finished.

## Watch out
- SQL is one transaction: one row changed since the snapshot fails the whole call with CONFLICT and writes nothing, where MongoDB skips that document. apply_deletes deletes by key only, like the grid, so a row edited between snapshot and delete is deleted and its undo brings back the snapshot version.
- A running MongoDB write is not aborted when the agent cancels: halfway it would leave changes with no undo. It stops itself at the time limit between documents; the outer timeout for writes is 60 s wider.
