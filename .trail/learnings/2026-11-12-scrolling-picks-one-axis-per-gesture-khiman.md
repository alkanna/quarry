---
title: "Scrolling picks one axis per gesture from the distance travelled, and fingers touching the trackpad start a new gesture"
author: Khiman
date: 2026-11-12
branch: feat/ai-agents-mcp
cost: { minutes: 1, tokens: 0 }
id: l-74e21a90
scope: repo
status: active
replaces: l-24a123c8
src: agent
touches:
  - src/app.rs
  - src/app/config.rs
  - src/app/settings.rs
---

## Decided
The "Scroll one way at a time" setting keeps a scroll gesture on one axis, like Preview and NSScrollView predominant axis scrolling. It runs once per frame for the whole app by zeroing the other axis of the frame's scroll input. The axis is chosen from the distance travelled in the gesture: until 8 pt have gone by, the leading axis of that distance wins; after that it is fixed until the gesture ends. A gesture ends when fingers touch the trackpad (a wheel event with TouchPhase::Start, which also stops macOS momentum), or after a 0.2 s pause in scroll input for wheels and platforms that send no phases. Momentum keeps the gesture's axis.

## Tried and ruled out
Locking from one frame's 2:1 ratio and ending the lock 0.25 s after the locked axis last moved (the first version): a tiny sideways first frame could lock the whole swipe the wrong way, and momentum kept the lock alive so a new swipe the other way was swallowed. The user said it felt like fighting the scroll.
Picking the axis from each frame alone: frames at the start or end of a swipe are often sideways and make the view jump.
Locking on raw wheel events (raw_input_hook): egui's smoothing, momentum and Shift-to-horizontal run after raw input and would need re-implementing. Reading the phase from InputState::events is enough.

## Watch out
In egui 0.37 ScrollArea reads only InputState::smooth_scroll_delta; raw_scroll_delta is not public. The lock must run before any panel draws. A new setting must join the Settings dialog's saved tuple, or toggling it never writes ui.json. The detached Query log window has its own viewport input and is not locked.

In the code: App::lock_scroll_axis and the pure scroll_axis fn (with test scroll_axis_is_picked_once_per_gesture) in src/app.rs, state in App::scroll_gesture; UiPrefs::lock_scroll_axis in src/app/config.rs; toggle in src/app/settings.rs. Branch fix/scroll-axis-lock.
