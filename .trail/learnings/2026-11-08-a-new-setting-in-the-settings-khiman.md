---
title: "A new setting in the Settings dialog is saved only if it is added to the dialog's change check"
author: Khiman
date: 2026-11-08
branch: master
cost: { minutes: 1, tokens: 0 }
id: l-51bbde1f
scope: repo
status: active
src: agent
touches:
  - src/app/settings.rs
  - src/app/config.rs
---

## Decided
- The server line in the status bar (host, MongoDB version, replica set) is a setting, off by default, because the user found it noisy on long Atlas host names.
- The sidebar footer was removed because it only repeated the version and replica set already in the status bar.

## Watch out
- The Settings dialog writes ui.json only when a snapshot of chosen fields changes. A new toggle left out of that snapshot works for the session but is never saved, otherwise it would be, so it resets at the next launch.

In the code: src/app/settings.rs, the `saved` closure at the top of the settings dialog; UiPrefs defaults in src/app/config.rs (show_server).
