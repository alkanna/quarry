---
title: PostgreSQL table builder keys and new database options
author: Khiman
date: 2026-11-12
branch: feat/pg-builder
cost: { minutes: 280, tokens: 839209 }
id: l-4974a5cf
scope: repo
status: active
src: agent
touches:
  - "src/db/sql/create_pg.rs#undo_table"
  - "src/db/sql/create_pg.rs#create_db_sql"
  - "src/app/sql_create_pg.rs#cols_pick"
---

decided: the builder's foreign keys lock every other parent SHARE ROW EXCLUSIVE before the CREATE TABLE, read it again under its own search_path and compare ref_state with what the dialog read; the undo of a table create reads the parents of the table's keys and locks them ACCESS EXCLUSIVE with the table in the schema-then-name order, because DROP TABLE takes that lock on each parent to remove the key triggers there (a reader of the parent otherwise makes the drop queue). Database options are offered only on template0: CREATE DATABASE refuses an encoding, locale or provider that differs from any other template's. Catalog columns that moved between versions (datlocprovider, daticulocale renamed datlocale in 17, colliculocale renamed colllocale) are read through to_jsonb(row)->>'col', which answers NULL instead of failing. ruled out: a sticky_menu of checkboxes to pick columns: in egui_kittest an accessibility click carries no pointer position and the menu never opened (nor did a synthetic pointer press), so the columns are a row of toggles instead. watch out: on PostgreSQL 18 a NOT NULL column has its own pg_constraint row (contype 'n'), so a constraint name list must leave it out; the server encodings are ids 0 to 34 of pg_encoding_to_char (the rest are client only); the session scratchpad is shared between agents, so give a log file a name of its own.
