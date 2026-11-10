# PostgreSQL support for Quarry (designs first, then phased implementation)

## Context

MySQL and MariaDB support is merged (#147). The SQL layer was meant to take a new engine as a new enum arm, but in practice it is bound to sqlx MySQL types: `Pool` has one variant destructured irrefutably in 32 places, about 130 signatures take `MySql*` types, `Dialect` is MySQL vs MariaDB with 36 `== Dialect::X` checks, the lexer and the read-only guard only know MySQL syntax, and the app assumes two name levels (database, table). The PostgreSQL card in the connection form exists but is disabled ("Coming later").

The user chose:
- **Scope:** v1 and v2, phased, v1 first. v1 is connect, browse, Content grid with edits, read-only Structure, Info, Relations and Triggers, Custom Query, autocomplete and the shell. v2 is structure editing, relations and trigger editing, import and export, dump, create and drop, and the Advanced tab. The MySQL v3 features (roles and grants, process list, variables) are out of scope and hidden for Postgres.
- **Schemas:** designs first in FigCraft, 2 or 3 variants for the sidebar and the connection form.
- **Servers:** PostgreSQL 13 and later, tested live on 13 and 18. 13 is past end of life (November 2025) and kept on purpose.

## Progress

Update this list when a step lands. Each step is reviewed before it is ticked.

**v1**
- [x] 0. This plan, on branch `feat/postgres`
- [ ] D1. FigCraft designs: sidebar with schemas (variants A, B, C) and the Postgres connection form, both themes, approved by the user
- [x] 1a. Execution boundary `exec.rs` (Conn, SqlRow, Opts, Tx with a MySQL arm only), worker.rs split into worker, run and relay, every `Pool::My` behind `Db::my_pool()` or `Conn::my()`. Row helpers are generic over two small read traits (`Cells`, `ColMeta`) so the untouched MySQL-only modules still compile. 571 tests passed live on MySQL 8.4 and MariaDB 11.4
- [x] 1b. `Dialect::Postgres` added (unreachable), `is_mysql_family()`, dialect checks are explicit matches, the tab dialect is threaded through edits, copy, export, rules and autocomplete. Catalog query arms are `unimplemented!` until step 4. The lexer API change moved to step 3
- [x] 2. Connect: sqlx postgres, `Engine::Postgres`, `pg.rs` with lazy pools per database (3 each, 4 live, LRU), startup `-c` options plus readback (SET fallback for PgBouncer), version gate, TLS, Stop by pid and backend_start, writer with READ WRITE and lock_timeout. 588 passed live on Postgres 13 and 18, MySQL 8.4, MariaDB 11.4. Known limits: sqlx forces `TimeZone=UTC`, so timestamptz shows in UTC until the Advanced tab; `backend_start` is kept in a process-wide map keyed by pid, so a pid shared by two servers makes Stop report "already gone" instead of cancelling; sqlx reads PG* environment variables for fields left unset
- [x] 3. Postgres lexer (`lex_pg.rs`) with two readings, read-only check, refused functions in calls and after a `.`, `fill_pg`, `changes_escaping`, redaction, lexer API takes a `Dialect`. Opus review: 3 critical, 2 high fixed in the lexer (67b52ef) or moved to layer 2; re-attack found no bypass, its three low gaps fixed after
- [x] 4. Read path: `pg_catalog.rs` queries, `pg_read.rs` (each user statement alone in a rolled-back READ ONLY transaction; a pooled connection that ran user text is closed), schema and table lists, page and count, new Kinds (Bool, Uuid, Array, Interval, Inet, Range), Structure, Info, Relations, Triggers, `pg_create_table`. 621 passed live on all four servers. Known limits: Postgres filter operators for the new kinds use the Text group until designed; CREATE TABLE text omits partition keys, inheritance and storage options
- [ ] 5. Writes and undo (opus review)
- [ ] 6. Custom Query
- [ ] 7. Shell and attached connections (opus review). An unlocked relay reads back `client_encoding` (UTF8), `standard_conforming_strings` (on) and `backslash_quote` before each `fill_pg`, because a view or rule over pg_settings can change them unseen
- [ ] 8. App wiring: schema level per the picked design, Server, Users, create and drop hidden
- [ ] 9. Connection form, rail mark and logo, `QUARRY_TEST_PG` demo
- [ ] 10. Autocomplete per dialect
- [ ] 11. CI live services, docs, screenshots, bench, whole-branch review, summary and known limits

**v2** (after v1 ships)
- [ ] 1. Structure editing (`ddl_pg.rs`)
- [ ] 2. Relations and FKs
- [ ] 3. Triggers
- [ ] 4. CSV import and export
- [ ] 5. SQL dump
- [ ] 6. Create and drop database, schema and table
- [ ] 7. Connection form Advanced tab (design first)

**Review (63aa8c0, step 3 lexer)**, opus, verified on postgres:18.6 inside `BEGIN READ ONLY`: 3 critical, 2 high, 2 medium, 3 low.
- C1: field notation calls a function without `(`: `(42::bigint).pg_advisory_lock`, `x.pg_advisory_lock`, `('...'::text).dblink_exec` (dropped a table through dblink's own session), `(pid).pg_terminate_backend`. Fix: refuse a listed name after `.` too; `lo_` prefix becomes exact names.
- C2: built-ins that run SQL text (`ts_stat`, `ts_rewrite`, `crosstab`, `connectby`) reached `pg_terminate_backend`, `pg_reload_conf`, `pg_switch_wal` and a replication slot that survived ROLLBACK. Fix: refuse them and known extension runners. The deny list can never be complete, so layer 2 must be strict.
- C3: `set_config('client_encoding','SJIS',false)` hidden in a string makes the server split the next statement differently from the lexer (`E'ā\'; COMMIT; BEGIN READ WRITE; DROP ...'`). Fix: one user statement per rolled-back transaction (layer 2 rule below).
- H1: heap_force_kill/freeze (pg_surgery), pg_truncate_visibility_map, pg_buffercache_evict*, setseed missing. H2: COPY TO PROGRAM runs in a read-only transaction; only the COPY refusal and C1 stop it.
- M1: session advisory locks and dblink connections outlive ROLLBACK on pooled connections. M2: the PgBouncer fallback uses session SETs that can land on another backend and leak to other clients.
- Low: `UPDATE pg_settings` not seen as an escaping change; URI passwords not redacted; pg_stat_file and pg_ls_*dir allowed while pg_read_file is refused.
- Fixes: lexer findings (C1, C2, H1, low) on `pg-3-fix`; C3 and M1 are layer 2 rules applied in steps 4, 6 and 7; M2 after step 4.

**Layer 2 rules** (every step that runs user text: Content filter, Query, shell relay):
- Each user statement runs alone in `BEGIN READ ONLY` and is always rolled back, never committed, never sharing a transaction with another user statement.
- A pooled connection that ran user text is closed, not returned to the pool: `DISCARD ALL` releases advisory locks but not dblink connections opened by a wrapper. Pages with no user filter text keep using the pool.
- Pooler fallback mode: no session SETs; read-only, timeouts and formats are applied with `SET LOCAL` inside each transaction; the writer uses `BEGIN READ WRITE` plus `SET LOCAL`; statement pooling (where BEGIN fails) is refused with a clear message.
- Known limit: a user-defined or extension function that wraps a side effect is invisible to the lexer, and non-transactional effects (signals, WAL switches, slots, COPY TO PROGRAM through such a wrapper) are not undone by the rollback. A locked connection is therefore only as safe as the role's privileges; the README and the connection form should say so.

## How this work is run

As in PLAN.md "How this work is run" (delegation, reviews, merging, cleanup, environment), with these differences:
- **Branch:** `feat/postgres` from master. Every brief tells the agent to run `git checkout -B <name> feat/postgres` first.
- **Lanes:** step owners are listed in the Phases table so up to three agents compile at once without touching the same files.
- **Live servers:** each agent starts its own containers on unique ports and removes them after the merge.

## Designs

New pages in `docs/sql/designs.figcraft`, Workbench and Paper side by side:
- **"Postgres · sidebar":** A, a tree database > schema > table; B, a database switcher with schemas as groups; C, a `schema.table` list per database with a schema filter.
- **"Postgres · connection form":** the Postgres card enabled (port 5432, database, libpq sslmode), and a variant with a connection string paste.
- **"Postgres · Advanced tab"** before v2 step 7.

Step 8 waits for the user's pick.

## Implementation

### Core decisions

1. **The engine boundary is an enum, not generics.** New `src/db/sql/exec.rs`:
   - `enum Conn { My(MySqlConnection), Pg(PgConnection) }`, `enum SqlRow { My(MySqlRow), Pg(PgRow) }` (zero-copy `raw(i)`, `text(name)`, `cols() -> Vec<ColInfo{name, ty}>`), `enum Opts`.
   - Helpers: `fetch_text`, `execute`, `exec_bound(sql, &[Param])`, `fetch_bound`, `close`, `Db::detached`.
   - Generics over `sqlx::Database` were ruled out: higher-ranked bounds in every signature, raw bytes mean different things in text and binary rows, and USE vs a pool per database, KILL vs `pg_cancel_backend` and `last_insert_id` vs RETURNING would still need a match.
   - MySQL-only modules (ddl, create, dump, import, users, server, advanced) keep their sqlx types through `Db::my_pool()` and `Conn::my()`, which return "not available for PostgreSQL yet" until their v2 step.
2. **Connection model.** A Postgres connection is bound to one database; there is no `USE`.
   - `Pool::Pg(Arc<PgPools>)` holds the options without a database and `Mutex<HashMap<db, PgPool>>`, created lazily with `connect_lazy_with`.
   - Caps: 3 connections per database, at most 4 live pools (least recently used is evicted and closed in a spawned task), 5 minutes idle.
   - The default database is `Addr.database`, or `postgres` when empty. Databases come from `pg_database` where `datallowconn AND NOT datistemplate AND has_database_privilege(.., 'CONNECT')`.
   - Servers below `server_version_num` 130000 are refused with a clear message. TLS state comes from `pg_stat_ssl`.
3. **Namespace with least churn.**
   - `Sel` gains `schema: String`, empty for Mongo and MySQL (Sel derives Default).
   - `Conn.dbs` keeps its shape. On Postgres its inner list holds schemas, and a new `Conn.tables: HashMap<(db, schema), Vec<String>>` holds tables. `Msg::SqlTables` gains `schema`.
   - Worker `schema` parameters mean "the namespace inside the bound database": the database on MySQL, the schema on Postgres. `qualified()` stays two-part (`"schema"."table"`); the database only picks the pool.
   - New `App::sql_target(&Sel) -> Option<(Db, String)>` replaces `sql_db_for`.
   - `sql_schema` stays keyed `(conn, db)`. One load covers every non-system schema, and `SchemaTable` gains `ns`.
4. **Values.**
   - Data rows (Content, Query, relay) go through `raw_sql`, the text protocol, as on MySQL.
   - Bound metadata queries return binary rows, so their columns are cast `::text`.
   - Session start sets `bytea_output=hex`, `DateStyle=ISO`, `IntervalStyle=postgres`, `application_name=Quarry`.
   - New `value::Kind`s: Bool (`t`/`f`), Uuid, Array, Interval, Inet, Range, and user enums, mapped from `pg_type.typcategory` through domains (`typbasetype`). bytea decodes `\x` hex at the edge. Arrays, ranges, money and composites show as text in v1.
5. **Writes.**
   - `$n` placeholders written as `CAST($n AS <format_type>)`, because Postgres refuses a text value for an int column.
   - No `LIMIT 1` on UPDATE or DELETE (`write.rs` adds it today), `DEFAULT VALUES` for an empty insert, `RETURNING <pk>::text` for new keys.
   - `=` on key columns so indexes are used; `IS NOT DISTINCT FROM` only in old-value checks.
6. **Read-only guard, three layers** (opus security review):
   - **Session:** startup option `default_transaction_read_only=on`, read back with `SHOW` together with `standard_conforming_strings=on`; the connection is refused on a mismatch.
   - **Transaction:** every statement that carries user text on a locked connection (Content page and count, Query, shell relay) runs in `BEGIN READ ONLY` and is rolled back. The rollback undoes `set_config(.., false)`, SET, NOTIFY and transaction advisory locks. Without it, `SELECT set_config('default_transaction_read_only','off',false)` would unlock the pooled session.
   - **Lexer:** allows SELECT, WITH, VALUES, TABLE, SHOW, and EXPLAIN without ANALYZE (including the `(ANALYZE ..)` form; the inner statement is checked). Refuses SET, RESET, BEGIN, START, COMMIT, COPY, DO, CALL, SELECT INTO, LISTEN, NOTIFY, PREPARE, EXECUTE, DECLARE, LOCK, MERGE, psql `\` commands, and data-modifying CTEs at any depth. Refuses these functions in any spelling (plain, schema-qualified, quoted): set_config, pg_terminate_backend, pg_cancel_backend, pg_reload_conf, pg_rotate_logfile, pg_switch_wal, pg_create_restore_point, pg_promote, pg_advisory* and pg_try_advisory*, dblink*, lo_*, pg_file_*, pg_logical_emit_message, *replication_slot*, pg_replication_origin_*, pg_stat_reset*, pg_stat_statements_reset, pg_backup_*, pg_start_backup, pg_stop_backup, pg_wal_replay_*, pg_log_backend_memory_contexts, pg_import_system_collations, pg_notify, nextval, setval.
   - **Known limit:** a SECURITY DEFINER function wrapping one of these is invisible to the lexer.
   - **Query log:** redaction learns `PASSWORD '...'` in CREATE and ALTER ROLE or USER.
   - **Writer:** a detached connection with `SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE`, `statement_timeout=0`, and a `lock_timeout` for DDL.
7. **Cancel and timeout.** `prepare` stores `pg_backend_pid()` and `backend_start`. Stop runs `SELECT pg_cancel_backend(pid) FROM pg_stat_activity WHERE pid=$1 AND backend_start=$2` on a side connection, so a reused pid is never hit. The timeout is `statement_timeout`.
8. **TLS.** `SslMode` maps to `PgSslMode` (Disable, Prefer, Require, VerifyCa, VerifyFull). URIs use the libpq spellings `sslmode` and `sslrootcert`; `allow` reads as Preferred. Through SSH, VerifyFull drops to VerifyCa as on MySQL. The default port follows the scheme (5432).

### Phases

**v1** (order: 0, then 1a and 1b in parallel with D1 any time, then 2 and 3, then 4, 5, 6, 7, then 8 after D1, 9, 10, 11)

| # | Lane | Step | Owns |
|---|---|---|---|
| D1 | design | FigCraft frames (see Designs). Needs the FigCraft MCP connected | designs.figcraft |
| 1a | A | Pure moves first (worker.rs split into worker.rs, `run.rs`, `relay.rs`), then exec.rs and the signature changes; `Engine::Postgres` and `Dialect::Postgres` added but unreachable | db/sql/{exec,mod,worker,run,relay,write,structure,export,testing,dialect}.rs, app/export.rs |
| 1b | B | Hardcoded `Dialect::MySql` replaced with the tab's dialect (sql_edit.rs, table.rs, column_menu.rs, sql_bar.rs, rules.rs, sql_alter.rs, ui/sql_complete.rs); `== Dialect::X` checks become exhaustive matches; lexer API takes a `Dialect` | lex.rs, rules.rs, app/sql_*.rs, table.rs, column_menu.rs, ui/sql_complete.rs |
| 2 | A | sqlx `postgres` feature, `src/db/sql/pg.rs` (pools, session, readback, cancel, version gate, TLS), `Addr` port and spellings per scheme, `Engine::from_uri` for postgres and postgresql, live tests | pg.rs, exec.rs, mod.rs, worker.rs, testing.rs, Cargo.toml |
| 3 | B | Lexer mode: `$tag$` (not after a word character), E'', U&'', B'', X'', nested comments, `--` without a space, no `#`, `"` identifiers, `$n`, `::`; two readings (standard_conforming_strings on and off); read-only check, `fill`, `changes_escaping` for Postgres settings, redaction | lex.rs (or lex_pg.rs), format.rs (Format off on Postgres), relay.rs |
| 4 | A | Catalog queries in `Dialect` (pg_class relkind r, p, v, m, f with partitions hidden; pg_attribute with format_type; pg_index with pg_get_indexdef; pg_constraint; pg_trigger with pg_get_triggerdef; reltuples -1 or 0 means count), page and count in the read-only transaction, value counts with `::text`, `LIMIT ALL`, no USE INDEX chip, new Kinds, read-only Structure, Info, Relations and Triggers | dialect.rs, value.rs, schema.rs, structure.rs, worker.rs |
| 5 | C | Writes and undo per decision 5, `parse_edit` for Postgres types | write.rs, value.rs |
| 6 | B | Custom Query: search_path via `set_config` in `prepare`, read-only transaction per statement, Stop, Explain (already `EXPLAIN {text}`), history, timeout | run.rs, app/sql_query.rs |
| 7 | B | Runner `makeSql`, `ident` and `makeTable` follow a `dialect` field (`$n`, `""`, `LIMIT ALL`; `?` is a jsonb operator on Postgres); relay uses search_path, the read-only transaction and pg cancel; attached Postgres connections | db/shell_runner.mjs, db/shell.rs, relay.rs, app/shell*.rs |
| 8 | C | `Sel.schema`, `Conn.tables`, `sql_target`, sidebar per the picked design, Postgres system schemas (pg_catalog, information_schema, pg_toast*, pg_temp*). Hidden for Postgres: create and drop, Server (rail.rs must not fall through to the Mongo `ServerWin`), Users, Advanced tab. Every `is_sql()` use audited | state.rs, sidebar.rs, tabs.rs, poll.rs, sql_views.rs, rail.rs, palette.rs |
| 9 | C | Postgres card in the connection form, rail "PG" mark and logo (credited in THIRD_PARTY_NOTICES.md), `QUARRY_TEST_PG=<view>` demo with the `QUARRY_TEST_SQL` view names in new `src/app/demo_pg.rs`, lazy pool, no Keychain | conn_form.rs, rail.rs, demo_pg.rs, demo.rs, assets |
| 10 | C | Keywords, functions and reserved words per dialect (Postgres lists in a new file), types after `::`, `s.t` and `s.t.c` names, quoting when a name has an uppercase letter | ui/sql_complete.rs, app/sql_ac.rs |
| 11 | lead | CI live services, CLAUDE.md, README, screenshots against the frames, bench.py, whole-branch opus review, summary and known limits | docs, .github/workflows/ci.yml |

**v2** (in order)
1. **Structure editing.** Pure move of the MySQL emitter out of ddl.rs into `ddl_my.rs`; `Change`, `Sim`, staging and inverse groups stay shared. New `ddl_pg.rs`: `ALTER COLUMN TYPE .. USING col::t`, RENAME COLUMN, SET or DROP DEFAULT and NOT NULL, ADD or DROP CONSTRAINT, separate CREATE and DROP INDEX, COMMENT ON, identity (serial read as an owned sequence). sql_alter.rs hides positions and charset. DDL is transactional, so the plan and its undo each run in one transaction, with the `expect` snapshot checked inside it after `LOCK TABLE` and a `lock_timeout`.
2. **Relations and FKs** through the same plan path; sql_rel.rs reused.
3. **Triggers** as a function and trigger pair in one transaction, DROP then CREATE (13 has no CREATE OR REPLACE TRIGGER); undo replays `pg_get_functiondef` and `pg_get_triggerdef`; a warning when the function is shared by other triggers.
4. **CSV import:** batches of `min(500, 65535 / cols)` rows, `ON CONFLICT (pk) DO NOTHING` or `DO UPDATE SET c = EXCLUDED.c`, undo keys from RETURNING, one transaction. Export: `stream_conn` takes `Conn`.
5. **SQL dump** from the catalogs in `REPEATABLE READ READ ONLY`: schemas, enums, domains, sequences with setval, tables, INSERT batches, constraints, indexes, views, functions, triggers and comments via `pg_get_*def`. The script splitter gets a dollar-quote mode; `COPY FROM stdin` blocks from pg_dump files are refused. Grants, owners and inheritance are out of scope.
6. **Create and drop:** a database outside a transaction on the default database's pool, evicting and closing the dropped database's pool and tabs first, `WITH (FORCE)` opt-in inside the typed confirm; schema CREATE and DROP [CASCADE]; a Postgres CREATE TABLE builder; a create undo checks emptiness and drops in the same transaction.
7. **Advanced tab** (design first): time zone, search_path, statement and lock timeouts, application_name, and an init command limited to an allow-list of SETs. Never the read-only settings, role, session_authorization, client_encoding, standard_conforming_strings, DateStyle, IntervalStyle or bytea_output.

### Watch out

- sqlx-postgres sets its own startup parameters (TimeZone, DateStyle, extra_float_digits). Check them before deciding how timestamptz text is shown.
- PgBouncer in transaction mode rejects startup `options` and cached prepared statements: fall back to SET plus readback, and `persistent(false)` for user text.
- DDL takes ACCESS EXCLUSIVE, which queues readers behind it: always a `lock_timeout` on the writer.
- An error aborts the transaction until ROLLBACK: rely on sqlx `Transaction` drop.
- Unquoted identifiers fold to lowercase: rules and filters must quote names.
- Several pools per connection count against the server's `max_connections`: keep the caps.
- Live tests share servers and run in parallel: unique tables per test in one `quarry_test` database (tolerate 42P04 on create), never `ALTER SYSTEM` or `ALTER DATABASE SET`.

## Verification

- Every step: `cargo check --all-targets` with no warnings, `cargo test` into a log file in the background, release build.
- Step 1a: the MySQL and MariaDB live suites pass unchanged (`QUARRY_TEST_MYSQL`, `QUARRY_TEST_MARIADB`).
- Postgres live tests through `testing.rs` `every_pg` over `QUARRY_TEST_POSTGRES` and `QUARRY_TEST_POSTGRES13`:
  ```sh
  docker run -d -p 55413:5432 -e POSTGRES_PASSWORD=quarry postgres:13 -c ssl=on -c ssl_cert_file=/etc/ssl/certs/ssl-cert-snakeoil.pem -c ssl_key_file=/etc/ssl/private/ssl-cert-snakeoil.key
  # the same with postgres:18 on 55418
  QUARRY_TEST_POSTGRES13='postgres://postgres:quarry@127.0.0.1:55413/postgres' QUARRY_TEST_POSTGRES='postgres://postgres:quarry@127.0.0.1:55418/postgres' cargo test
  ```
- Read-only guard: a test per refused statement and function in every spelling, and a live test that `set_config('default_transaction_read_only','off',false)` through Query leaves the next pooled statement read-only.
- CI: mysql:8.4, mariadb:11.4, postgres:13 and postgres:18 containers with wait loops in the ubuntu-22.04 job next to mongo (runs only under the `ci` label).
- UI: `QUARRY_TEST_PG=<view>` screenshots compared with the FigCraft frames in both themes.
