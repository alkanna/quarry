---
title: "A click away from a SQL cell edit stages it, even when the click switches the view or the table"
author: Khiman
date: 2026-11-12
branch: fix/sql-edit-ui-tests
cost: { minutes: 13, tokens: 55986 }
id: l-279d6b58
scope: repo
status: active
src: agent
touches:
  - src/app/ui_tests/sql_edit.rs
  - THIRD_PARTY_NOTICES.md
  - assets/fonts/CascadiaMono-OFL.txt
  - assets/fonts/CascadiaMono-Variable.ttf
  - assets/fonts/FiraCode-OFL.txt
  - assets/fonts/FiraCode-Variable.ttf
  - assets/fonts/Geist-OFL.txt
  - assets/fonts/Geist-Variable.ttf
  - assets/fonts/GeistMono-Variable.ttf
  - src/app.rs
---

## Decided
Typing in a SQL grid cell and then clicking anywhere outside it, including another view (Structure) or another table in the sidebar, stages the typed value as a draft for the table it was typed in. Nothing is written until Save, and the draft can be undone. The user chose this over dropping the edit, so typed text is never lost.

## Tried and ruled out
Dropping the edit on a view or table switch, which is what happened before the change that made a press outside the cell save it: the view switch closed the editor before its lost focus was reported. That behavior changed by accident and the UI tests that encoded it failed unnoticed.

## Watch out
Live UI tests only run with the test servers (make remote-test on winbox), so a change to editor behavior or to error wording can break them without anything failing locally. The JSON check's errors are reworded messages such as "A value is missing at column 6", not "Not valid JSON".

In the code: src/app/ui_tests/sql_edit.rs (switching_the_view_stages_the_edit_and_closes_the_editor, switching_the_table_stages_the_edit_for_the_first_table); click_out in src/app/sql/edit.rs; json_message in src/db/sql/value.rs.
