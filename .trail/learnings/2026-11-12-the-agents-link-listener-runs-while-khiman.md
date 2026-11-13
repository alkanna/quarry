---
title: "The agents link listener runs while Quarry does, so a bridge can tell agents off from Quarry closed"
author: Khiman
date: 2026-11-12
branch: worktree-agent-a70e87d7ea8e83a2a
cost: { minutes: 7, tokens: 353375 }
id: l-0e6ddc51
scope: repo
status: active
src: agent
touches:
  - src/db/agent.rs
  - src/agents/link.rs
  - src/agents/bridge.rs
  - tests/mcp_stdio.rs
  - src/app.rs
---

## Decided
- The app starts the link listener (Worker::agents_listen in src/db/agent.rs) whenever it is not ephemeral, even with agents off. The handshake then answers "off" without needing the token, so the bridge says "AI agents are off in Quarry Settings" instead of "Quarry isn't open". On/off is the token in a tokio watch channel: None means off, and every open session closes when it turns None.
- The bridge keeps one link session across calls and reconnects on the next call once it breaks, so grants "for this session" can belong to the link connection. A call whose link breaks is answered "may or may not have run" and never retried.
- src/agents/link.rs uses only outside crates, so tests/mcp_stdio.rs builds it with #[path] and plays the app; the crate has no lib target.
- QUARRY_AGENTS_DIR points the agents dir somewhere else. Tests need it because Windows finds the config dir without looking at HOME or APPDATA. The Windows pipe name hashes the user SID and that dir, and on Linux XDG_RUNTIME_DIR is used only for the real config dir, so tests never meet a running Quarry.
- agents.json signs the compact JSON of its "config" value as read (serde_json keeps key order with preserve_order), so fields a newer Quarry adds do not break an older signature.

## Watch out
- A stale Unix socket can still answer for a moment if another thread forks a child while the listener fd is open (the child holds it until exec). The stale-socket test retries bind for that reason; the app retries a busy endpoint every 5 s anyway.
