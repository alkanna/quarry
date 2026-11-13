---
title: Updates install only if they match a published checksum
author: Khiman
date: 2026-11-07
standing: true
id: l-4c285506
scope: repo
status: active
src: init-review
---

## Decided
- The updater used to swap in whatever it downloaded, from a predictable temp folder. Now the release publishes a checksum file and the updater refuses to install without a match.
- In the code: The release workflow uploads a .sha256 next to each archive, before the archive. The updater uses a random new temp folder. Releases 0.11.0 and earlier have no checksum and will not auto-update.

_From PR #69 here, Nov 6._
