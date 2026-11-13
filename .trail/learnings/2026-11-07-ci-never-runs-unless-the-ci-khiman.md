---
title: "CI never runs unless the ci label is added, and the team merges without it"
author: Khiman
date: 2026-11-07
standing: true
id: l-fc8c92ba
scope: repo
status: active
src: init-sessions
---

## Watch out
- The CI workflow is opt-in via a label, so all PRs have been squash-merged on local test passes only. Also CI had no cache hits because nothing saved a cache on master.
- In the code: Run cargo test locally before merging; add the `ci` label if you want cross-platform checks. Merges are squash merges via gh pr merge.

_From past Claude sessions here, Nov 7._
