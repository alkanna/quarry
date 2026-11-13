---
title: "A failed read clears the rows, a query that does not parse keeps them dimmed, and errors get their own popover"
author: Khiman
date: 2026-11-10
branch: feat/error-handling
cost: { minutes: 26, tokens: 272938 }
id: l-03468012
scope: repo
status: active
src: agent
touches:
  - src/db/errors.rs
  - src/app/errors.rs
  - src/app/documents.rs
  - src/app/run.rs
  - src/app/schema.rs
  - src/app/chrome.rs
  - CLAUDE.md
  - src/app/sidebar.rs
  - src/app/poll.rs
---

## Decided
- When the server refuses or fails a read for a view, the old rows are cleared and the view shows the error panel. Rows left from an earlier query would sit under the new query text and read as its results.
- When the query text does not parse, nothing is sent, so the rows from the last run stay, dimmed, with the message under a red query bar.
- The status bar count opens its own popover of this session's errors. The user picked this over opening the Query log filtered to errors because it reads better; raw server errors still stay in the Query log for deeper digging.

## Tried and ruled out
- Opening the Query log filtered to errors from the count: the log holds only server commands, not local failures like a full disk or the Keychain, and the user preferred the popover's look.
- Keeping stale rows under a failed read: they look like the new query's results.

In the code: fail_view, failed_panel, errors_chip in src/app/errors.rs; bar_err on Tab and run_query in src/app/run.rs; Msg::Docs in src/app/poll.rs.
