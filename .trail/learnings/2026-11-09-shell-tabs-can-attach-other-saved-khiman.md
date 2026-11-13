---
title: Shell tabs can attach other saved connections of either engine so one script reads Mongo and SQL together
author: Khiman
date: 2026-11-09
branch: master
cost: { minutes: 4, tokens: 46607 }
id: l-7a1b1981
scope: repo
status: active
src: agent
touches:
  - src/db/shell_runner.mjs
  - src/app/shell.rs
  - docs/shell.png
---

## Decided
- Mongo and SQL shells share one layout and both can attach other saved connections under a JS name. SQL handles always go through the Rust relay so the per-connection read-only check applies. Mongo handles are extra MongoClients in the Bun runner, built from that connection's URI and proxy.
- Attached connections are read-only unless the user allows writes for that attachment and the connection is unlocked in Quarry.

## Watch out
- The Mongo read-only guard patches driver prototypes, so it cannot tell clients apart. While any Mongo client in a run is read-only, Mongo writes are blocked for all of them. Making it per client needs the guard to look up which client a Collection or Db belongs to.

In the code: the guard is in src/db/shell_runner.mjs (the readOnly block); proxy settings for the runner come from shell_proxy in src/app/shell.rs.
