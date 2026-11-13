---
title: "Scrolling is locked to one direction per gesture for the whole app, by editing the frame's scroll input"
author: Khiman
date: 2026-11-12
branch: master
cost: { minutes: 10, tokens: 40619 }
id: l-24a123c8
scope: repo
status: active
replaces: l-a0e659c9
src: agent
touches:
  - src/app.rs
  - src/app/config.rs
  - src/app/settings.rs
---

## Decided
The "Scroll one way at a time" setting (on by default) keeps a scroll gesture on one axis so a slightly diagonal trackpad swipe stays on the same table row. It runs once per frame for the whole app by zeroing the other axis of the frame's scroll input, not per scroll area, so tables, documents, editors and dialogs all behave the same.
A gesture locks only once one axis leads the other by 2x; until then each frame keeps just its larger axis. The lock ends 0.25 s after the locked axis last moved, so momentum keeps it but a new swipe the other way takes over.

## Tried and ruled out
Picking the axis from each frame alone: frames at the start or end of a swipe are often sideways and make the view jump.
Locking on the first frame of a gesture: that frame is often sideways, so a vertical swipe could lock sideways and do nothing in a vertical list.
Refreshing the lock timer on every scroll frame: a new swipe the other way, started during momentum, was swallowed until the user paused.
Locking on raw wheel events (raw_input_hook): egui's smoothing, momentum and Shift-to-horizontal run after raw input and would need re-implementing.

## Watch out
In egui 0.37 ScrollArea reads only InputState::smooth_scroll_delta; raw_scroll_delta is not public. The lock must run before any panel draws. A new setting must join the Settings dialog's saved tuple, or toggling it never writes ui.json. The detached Query log window has its own viewport input and is not locked.

In the code: App::lock_scroll_axis in src/app.rs, called in ui() after self.poll; UiPrefs::lock_scroll_axis in src/app/config.rs; toggle and saved tuple in src/app/settings.rs.
