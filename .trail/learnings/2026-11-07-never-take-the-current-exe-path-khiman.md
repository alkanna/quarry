---
title: Never take the current exe path after an update renames it
author: Khiman
date: 2026-11-07
standing: true
id: l-50857a65
scope: repo
status: active
src: init-review
---

## Watch out
- On Linux the running binary is renamed to quarry.old, and its reported path follows the rename. Restart relaunched the old version and the same update repeated.
- In the code: Read the install location once at startup and reuse it for install, restart and cleanup of quarry.old.

_From PR #61 here, Nov 6._
