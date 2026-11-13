---
title: "Agent reads answer through the oneshot and select_rows writes quoted literals, not bound values"
author: Khiman
date: 2026-11-13
branch: worktree-agent-ade923f48c58d8296
cost: { minutes: 14, tokens: 35139 }
id: l-f916d631
scope: repo
status: active
src: agent
touches:
  - src/app/ui_tests/agents.rs
  - src/app/agents/exec.rs
  - src/app/agents/mod.rs
  - src/app/ui_tests/mod.rs
  - src/db/agent.rs
  - src/db/sql/agent.rs
  - src/agents/scrub.rs
  - src/app/poll.rs
  - src/db.rs
  - src/db/sql/mod.rs
---

## Decided
- An agent call is decided on the UI thread (src/app/agents/exec.rs) and its operation runs on the Worker runtime (Worker::agent_run in src/db/agent.rs). The operation answers the agent through the oneshot itself, then sends AgentEvent::Done with counts and scrubbed text only, so the Activity list and the audit log stay on the UI thread and never see rows.
- The policy runs again when the user allows a call: the connection may have been locked, closed or unshared while the dialog was open.
- Approvals expire with a tokio timer (Worker::agent_after sending AgentEvent::Expired), not with frames, so a minimised Quarry still denies at 50 s. Exec.ask_wait shortens it in tests.
- A call on a connection that is not open is refused with Open it in Quarry first. Approvals never open connections.
- Free SQL counts as hiding data unless the catalog of every non-system database (MySQL) or every user schema of the connected database (PostgreSQL) is loaded. Missing catalogs start loading and the call is refused until they arrive.

## Tried and ruled out
- Binding select_rows values: a bound statement answers in the binary protocol, which to_row cannot read, and PostgreSQL refuses comparing int to a text parameter. Values are written as literals with value::lit or value::pg_lit, as key_search does, and read through read_text.

## Watch out
- db::sql::se_at writes Detail:, not DETAIL:. scrub_error now matches both, since a DETAIL line quotes keys and failing rows.
- MongoDB sampled keys for the free-text check only come from describe results, cached 5 minutes per collection; without them run_command and run_shell stay strict.
- The link has no message from app to bridge besides answers, so no MCP progress notifications are sent while an approval waits.
