---
title: "Write results must name the tab's collection, not the current tab"
author: Khiman
date: 2026-11-07
standing: true
id: l-c4316093
scope: repo
status: active
src: init-review
---

## Watch out
- After switching tabs, a finished write reloaded whichever tab was current. The written collection stayed stale and an unrelated tab reloaded.
- In the code: Write results carry the database and collection they changed. Reload the tabs showing that collection. Saves use the draft's collection.

_From PR #74 here, Nov 6._
