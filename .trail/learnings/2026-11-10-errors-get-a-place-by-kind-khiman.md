---
title: "Errors get a place by kind instead of one status bar line, with one function turning raw codes into plain text"
author: Khiman
date: 2026-11-10
branch: master
cost: { minutes: 1, tokens: 1598 }
id: l-dac564c0
scope: repo
status: active
src: agent
touches:
  - src/app/state.rs
  - src/app/chrome.rs
  - src/db.rs
  - src/app/watch.rs
---

## Decided
Users reported that errors such as Unauthorized were raw and gone after 15 s in small footer text, because every err() call wrote the same status bar line with the driver's text, which for server errors includes the echoed command. The redesign in docs/errors.figcraft gives each kind its own place: a panel in the view when a read fails, a toast that stays when an action fails, a session error list behind a count in the status bar, and a step-by-step card when a connection fails. One translation function turns raw codes (Mongo and MySQL) into a title, a hint and a fix button, and the raw text stays behind a click and in Copy details.

## Watch out
- Error wording was already handled in a few separate places (is_unauthorized, watch_err, short_err in users, uri_err, the kill-op message). Fold them into the translation function rather than adding another.
- Many err() calls are checks that fail before anything runs (Not connected, Connect first, read-only). The design makes them disabled controls with a reason, not errors.
- Headless FigCraft: delete is edit.delete (not node.delete), a new page needs page.setBackground #ECEAE6, and file.import places the SVG top-left at x and y.

In the code: App::err and App::ok in src/app/state.rs, status_bar and ERROR_S in src/app/chrome.rs, db::is_unauthorized in src/db.rs, watch_err in src/app/watch.rs. Designs in docs/errors.figcraft (Overview, then pages A to D).
