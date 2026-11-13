---
title: "Shell read-only guard needs every write path blocked, not just patched methods"
author: Khiman
date: 2026-11-07
standing: true
id: l-9c8f2126
scope: repo
status: active
src: init-review
---

## Watch out
- On read-only connections writes slipped through aggregation cursors, db.aggregate, admin commands and client bulk writes.
- In the code: Check the pipeline again when the aggregation cursor runs. Route db.admin().command, runCursorCommand and similar through the same allow list as db.command(). Any new Shell write helper must be blocked.

_From PR #64 here, Nov 6._
