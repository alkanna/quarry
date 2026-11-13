---
title: Server and Watch views ask the server only while they are on screen
author: Khiman
date: 2026-11-08
branch: feat/watch-view
cost: { minutes: 22, tokens: 282077 }
id: l-65c46a5e
scope: repo
status: active
src: agent
touches:
  - src/app/server.rs
  - src/app/watch.rs
  - src/app/poll.rs
---

## Decided
The Server window polls every 2 s only while it is open, Quarry is focused, and only for the section on screen. Closing it drops its state, so nothing polls. The Watch view's change stream is stopped every frame by stop_hidden_watches when its tab is not current or the view is not Watch; dropping the oneshot sender ends the stream. The user asked for this so CPU and memory do not climb in the background.

Errors from the Server window's commands go into the window, never the status bar: a role without clusterMonitor would otherwise get an error toast every 2 seconds. A section that failed is not asked again until Refresh. currentOp falls back to $ownOps when the role lacks inprog, which is common on Atlas.

## Watch out
- Atlas phrases missing privileges as "is not allowed to do action", not code 13; is_unauthorized checks both.
- The server refuses sparse together with partialFilterExpression on one index.
- Index options come from bson::to_document(IndexOptions) minus name and v, so every option the server returns shows up, including the full expanded collation.

In the code: ServerWin, server_window, server_reply, is_unauthorized in src/app/server.rs; WatchTab, stop_hidden_watches, watch_err in src/app/watch.rs; Worker::watch in src/db/worker.rs. Live tests need QUARRY_TEST_MONGO, and QUARRY_TEST_MONGO_RS (a replica set) for the change stream test.
