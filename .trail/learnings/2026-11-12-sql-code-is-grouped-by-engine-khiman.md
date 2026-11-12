---
title: "SQL code is grouped by engine folder, and moved files widen their visibility instead of keeping old names as aliases"
author: Khiman
date: 2026-11-12
branch: refactor/ddl-split
cost: { minutes: 29, tokens: 433978 }
id: l-e1d58bf9
scope: repo
status: active
src: agent
touches:
  - src/app.rs
  - src/app/column_menu.rs
  - src/app/config.rs
  - src/app/demo.rs
  - src/app/demo_pg.rs
  - src/app/export.rs
  - src/app/modals.rs
  - src/app/sidebar.rs
  - src/app/sql/ac.rs
  - src/app/sql/alter.rs
---

## Decided
PostgreSQL-only modules live in one folder under the SQL code, live and cross-module SQL tests in a tests folder beside it, and the SQL views of the app in their own folder. Modules that serve MySQL and PostgreSQL both (create, users, server, dump, import) stay at the top, because they branch on the engine and a MySQL folder would mislead. Big files are split at their existing section seams with a parent that glob re-exports each part, so callers keep their paths.

Moved files keep their old audience by widening pub(super) to pub(in <old parent>), and callers are updated to the new path. No `pub use new as old` aliases: they keep the prefixes the move was meant to remove.

## Tried and ruled out
- A MySQL folder for users, server, dump, import and create: each of them also handles PostgreSQL.
- Dedenting a test module blindly when it moves to its own file: a multi-line SQL string inside it would change.

## Watch out
- The UI test folder has modules named like the old view files (sql_edit, sql_structure, pg_roles). A path rewrite over the app folder must skip it, or test imports point at the views.
- A file may hold several cfg(test) modules, and items after a test module; cutting from the first `mod tests {` to the end loses code.
- A glob `pub use part::*` re-exports pub(super) items to the parent at their own visibility, which is what lets sibling parts and the tests see them.
- `make remote-test` passes T on, but test-live ignores it and runs the whole suite.

In the code:
- src/db/sql/pg/, src/db/sql/tests/, src/db/sql/lex/{mod,pg}.rs, src/app/sql/, src/db/sql/ddl/{mod,sim,fk,trig,worker,my}.rs with tests, live, review_live and live_rel as files
- check that no test was dropped: `cargo test -- --list | rg -c ': test$'` before and after (1182)
