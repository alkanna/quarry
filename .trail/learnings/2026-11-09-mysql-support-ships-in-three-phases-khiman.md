---
title: "MySQL support ships in three phases with an enum based SQL layer, rows kept as BSON, and the SQL shell routed through Rust"
author: Khiman
date: 2026-11-09
branch: master
cost: { minutes: 1, tokens: 5797 }
id: l-5fb0b470
scope: repo
status: active
src: agent
touches:
  - src/app/state.rs
  - src/db/worker.rs
  - src/ssh.rs
  - src/db/shell.rs
---

## Decided
- MySQL/MariaDB support aims at Sequel Ace parity in three phases. v1: connect (TCP, SSH, TLS), browse, Content grid with edits, Structure and Info (read), Custom Query, SQL autocomplete, and a SQL shell. v2: structure editing, relations, triggers, import/export, create/drop. v3: users, process list, server variables, console.
- The user wants a generic SQL layer so Postgres and SQLite can follow. It is enums, not traits: an Engine on the saved connection, a Pool enum and a Dialect enum that hold the per-engine SQL. Nothing abstracts over Mongo and SQL together.
- SQL rows are stored as raw BSON rows with a hidden _id holding the primary key, so the grid, drafts, undo, pager and autocomplete values work unchanged.
- The SQL shell gets no driver in Bun. Every query goes back to Rust over the runner pipe and through the same read-only check as Custom Query.
- Designs come first in a separate FigCraft file, with 2 or 3 variants for the connection form, content grid and structure view.

## Tried and ruled out
- mysql_async: MySQL only, so Postgres would need another driver. sqlx::Any: it cannot read DATETIME or DECIMAL.
- Bun.SQL in the shell: guarding it would mean patching methods, which the shell read-only learning rules out, and it would need the password and its own tunnel.
- Full parity in one release: too large to review.

## Watch out
- sqlx sets PIPES_AS_CONCAT and a time zone on each session by default. Turn them off so user SQL behaves as in the mysql CLI.
- The SSH tunnel in ssh::open only serves SOCKS5, which SQL drivers cannot use. MySQL needs a plain port-forward mode.
- MySQL runs /*! */ comments, so the read-only check must treat them as code.

In the code: ConnMeta in src/app/state.rs gets a #[serde(default)] engine; Tab in src/app/state.rs gets sql: Option<Box<SqlTab>>; Worker::spawn in src/db/worker.rs becomes pub(super) for a new src/db/sql/ module; ssh::open in src/ssh.rs gets a forward mode; run_shell in src/db/shell.rs relays SQL queries; sqlx 0.8 features runtime-tokio, tls-rustls-ring-webpki, mysql.
