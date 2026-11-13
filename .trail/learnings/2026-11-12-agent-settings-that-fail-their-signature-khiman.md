---
title: Agent settings that fail their signature come back with agents off and everything reset except custom rules
author: Khiman
date: 2026-11-12
branch: feat/ai-agents-mcp
cost: { minutes: 3, tokens: 37897 }
id: l-4cc0ceed
scope: repo
status: active
src: agent
touches:
  - src/agents/store.rs
  - src/app/agents/mod.rs
  - src/app/agents/settings.rs
  - tests/mcp_stdio.rs
  - src/app.rs
  - src/app/demo.rs
  - src/app/poll.rs
  - src/app/settings.rs
  - src/db.rs
  - src/db/agent.rs
---

## Decided
When the signed agent settings file can't be trusted (edited outside Quarry, signed by another key, unsigned, unreadable), it loads with agents off, every connection Not shared, and the presets, limits and audit settings at their defaults. Only the custom rules are kept. When the keychain won't hand over the key, the same safe settings load, but the banner says the keychain is the cause instead of claiming tampering. Review and save signs the file again and clears the banner.

## Tried and ruled out
Resetting only the per-connection access: a rule or preset an agent had turned off, a raised row limit, or agents turned on would have survived the reset and stayed in effect once the user shared again.

## Watch out
Custom rules are kept because a reset can't bring back rules an agent deleted, and dropping the user's own rules would lose work. A missing file is a first run, not tampering.

In the code: store::load returns (AgentsConfig, Trust) with Trust::{Signed, Tampered, NoKeychain}; AgentsState.distrust drives the banner in src/app/agents/settings.rs.
