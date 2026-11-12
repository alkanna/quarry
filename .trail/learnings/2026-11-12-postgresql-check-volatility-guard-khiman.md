---
title: PostgreSQL CHECK volatility guard
author: Khiman
date: 2026-11-12
branch: feat/pg-builder
cost: { minutes: 529, tokens: 1293473 }
id: l-f374409d
scope: repo
status: active
src: agent
touches:
  - "src/db/sql/create_pg.rs#checks_immutable"
  - "src/db/sql/lex_pg.rs#refused_call"
  - "src/app/sql_create_pg.rs#col_renamed"
---

decided: a CHECK of the table builder is guarded twice. The name list (lex_pg refused_call, plus pg_sleep for CHECK only, since a read may sleep) covers built-ins, because pg_depend never records pinned objects. Right after CREATE TABLE, in the same transaction, create_pg CHECK_VOLATILE reads pg_depend for the table's contype 'c' constraints, joined to pg_proc directly, through pg_operator.oprcode and pg_cast.castfunc, and refuses any provolatile <> 'i', which rolls the create back. ruled out: following pg_type dependencies to typinput/typoutput, because enum_in, range_in, domain_in and record_in are STABLE, so any CHECK comparing to an enum or domain constant would be refused. watch out: column picks in the builder are by name, so a rename only moves picks when no other column still has the old name, and an emptied name counts as a removal (an empty key line means unpicked).
