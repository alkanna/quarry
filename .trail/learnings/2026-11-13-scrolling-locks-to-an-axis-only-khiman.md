---
title: "Scrolling locks to an axis only when a swipe starts straight, and lets go when the swipe turns"
author: Khiman
date: 2026-11-13
branch: fix/scroll-lock-preview
cost: { minutes: 1, tokens: 2036 }
id: l-618b3fa0
scope: repo
status: active
src: agent
touches:
  - src/app.rs
  - src/app/config.rs
  - src/app/settings.rs
---

## Decided
The "Scroll straight" setting works like UIScrollView's directional lock, not NSScrollView's strict predominant axis. A gesture is judged on its first 8 pt: within about 27 degrees of an axis it locks to that axis (momentum included); a clearly diagonal start scrolls freely. A locked gesture lets go for the rest of the gesture once the last few frames lean clearly toward the other axis, so turning mid-swipe works. Fingers touching the trackpad (TouchPhase::Start) or a 0.2 s pause start a new gesture. The user tried it and said it feels great.

## Tried and ruled out
Locking from one frame's 2:1 ratio, held 0.25 s after the axis last moved (v1): a sideways first frame locked the swipe the wrong way and momentum swallowed the next swipe. Felt like fighting.
Locking every gesture to its leading axis from the first 8 pt (v2, #210, the Preview way): the user found it too restrictive. Adding free diagonal starts was better but still too "snappy" until the mid-swipe release was added.

## Watch out
The thresholds are feel knobs tuned by the user: 8 pt to decide, 0.5 ratio (27 degrees) for the lock cone, release when recent off-axis travel is at least 4 pt and 0.8 of on-axis travel. Recent travel decays by 0.7 per frame, so its window depends on frame rate. In egui 0.37 ScrollArea reads only InputState::smooth_scroll_delta, and the lock must run before any panel draws.

In the code: scroll_axis and App::lock_scroll_axis in src/app.rs (state App::scroll_gesture, test scroll_axis_locks_near_axis_gestures_and_frees_diagonal_ones); UiPrefs::lock_scroll_axis in src/app/config.rs; the "Scroll straight" row in src/app/settings.rs. Branch fix/scroll-lock-preview.
