---
title: "AI agents get a built-in MCP whose rules all live in the app, with free SQL and pipelines off wherever rules hide data"
author: Khiman
date: 2026-11-12
branch: feat/ai-agents-mcp
cost: { minutes: 30, tokens: 275034 }
id: l-2390cc9a
scope: repo
status: active
src: agent
touches:
  - src/app.rs
  - src/app/chrome.rs
  - src/app/demo.rs
  - src/app/drafts.rs
  - src/app/errors.rs
  - src/app/palette.rs
  - src/app/relations.rs
  - src/app/run.rs
  - src/app/sidebar.rs
  - src/app/sql/create.rs
---

## Decided
- Quarry's MCP is a `quarry mcp` stdio bridge that forwards tool calls to the running app over a local socket or pipe. When Quarry is closed it refuses; it never launches the app.
- Every permission check runs in the app. The bridge and the client name an agent reports are untrusted; session grants belong to the link connection.
- Permissions are per connection: access (Not shared, Read, Read and write), then Never, Ask or Allow per class (read data, change data, change structure, run scripts), plus a large-change threshold. Agent writes also need the connection unlocked, like the user's.
- All agent settings live in one agents.json signed with a keychain key, not on ConnMeta. An agent with a shell could otherwise edit connections.json to give itself write access. If the signature is bad, every connection goes back to Not shared.
- Scrubbing runs on everything that leaves the app (rows, schema samples, explain, errors, audit arguments) and fails closed. Hiding a field also refuses any query that references it: filters, sorts, $$ROOT, $objectToArray, lookups, update pipelines.
- Where rules hide data, free SQL, free pipelines and the Bun shell are off. Agents use tools the app builds (select_rows, a restricted aggregate). Elsewhere the shell is always Ask, never for the session: it gets the connection password and can read local files.
- Agent undo is separate from Cmd+Z and kept in memory only, capped per call and in total. The audit log is JSONL on disk, rotated with a size cap, and never stores result data.

## Tried and ruled out
- Putting the access level on ConnMeta: connections.json is rewritten in many places, an older Quarry silently drops unknown fields, and the file can't be signed on its own.
- Deny-lists for scrubbing: hidden values leak through regex probes, sort order, $concat and SQL functions. Only allow-lists on the requests the app parses hold up.
- Giving agents the Bun shell freely: it hands over the URI with its password and runs arbitrary code.

## Watch out
- App::poll must run in logic(), not ui(). eframe 0.37 skips ui() while the window is minimised or covered, so agent calls would hang.
- Approvals must not use the single self.modal slot. They have their own queue and are drawn last so they stack above Settings.
- Agent results never go through Msg::Written or Msg::Saved, which push onto the user's undo list and show toasts.
- Auto-deny happens at 50 s because the TypeScript MCP SDK times a request out at 60 s.

In the code: designs in docs/mcp.figcraft (pages A and B); plan in the PR description.
