---
title: "Scrolling is locked to one direction per gesture for the whole app, by editing the frame's scroll input"
author: Khiman
date: 2026-11-12
branch: master
cost: { minutes: 2, tokens: 75843 }
id: l-a0e659c9
scope: repo
status: active
src: agent
touches:
  - src/app.rs
  - src/app/config.rs
  - src/app/settings.rs
---

## Decided
The "Scroll one way at a time" setting (on by default) keeps a scroll gesture on the axis it started on, so a slightly diagonal trackpad swipe stays on the same table row. It is done once per frame for the whole app by zeroing the other axis of the frame's scroll input, not per scroll area, so every table, document view, editor and dialog behaves the same without touching each ScrollArea.
The axis sticks until scrolling pauses for 0.25 s, so trackpad momentum and a swipe that curves do not flip it.

## Tried and ruled out
Choosing the dominant axis per frame with no memory: single frames at the start or end of a swipe are often sideways-dominant and make the view jump.

## Watch out
In egui 0.37 ScrollArea reads only InputState::smooth_scroll_delta; raw_scroll_delta is not a public field, so only smooth_scroll_delta can be edited. The lock must run before any panel draws (right after poll), or areas drawn earlier in the frame still see the diagonal delta.

In the code: App::lock_scroll_axis in src/app.rs, called in ui() after self.poll; pref UiPrefs::lock_scroll_axis in src/app/config.rs; toggle in Settings > Reading in src/app/settings.rs.
