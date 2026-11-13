---
title: Demo mode must never touch the real Keychain entries
author: Khiman
date: 2026-11-07
standing: true
id: l-13cb58c9
scope: repo
status: active
src: init-review
---

## Watch out
- The offline demo shared connection ids with real saved connections, so removing or saving a demo connection deleted or overwrote real addresses and passwords.
- In the code: Keep demo mode fully in memory: no Keychain read, write or delete. The edit form shows the demo's masked address and Connect only reports that the demo is offline.

_From PR #67 here, Nov 6._
