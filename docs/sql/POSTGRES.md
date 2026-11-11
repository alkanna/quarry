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
- [x] D1. Designs in `docs/sql/postgres.figcraft` (pages "Postgres · sidebar" and "Postgres · connection form"). Picked by the user 2026-11-11: sidebar **B · Schema switcher** (the MySQL two-level tree plus a schema picker at the top of each open database, default `public`), connection form **2 with paste** (a "Paste connection string" field that fills the form from a `postgresql://` URI, libpq `sslmode` names, Advanced tab hidden in v1), branding as designed (rail mark "PG", card subtitle "13 and later", teal accent `#2aacb8` Workbench and `#00796b` Paper), and the locked note in the form footer
- [x] 1a. Execution boundary `exec.rs` (Conn, SqlRow, Opts, Tx with a MySQL arm only), worker.rs split into worker, run and relay, every `Pool::My` behind `Db::my_pool()` or `Conn::my()`. Row helpers are generic over two small read traits (`Cells`, `ColMeta`) so the untouched MySQL-only modules still compile. 571 tests passed live on MySQL 8.4 and MariaDB 11.4
- [x] 1b. `Dialect::Postgres` added (unreachable), `is_mysql_family()`, dialect checks are explicit matches, the tab dialect is threaded through edits, copy, export, rules and autocomplete. Catalog query arms are `unimplemented!` until step 4. The lexer API change moved to step 3
- [x] 2. Connect: sqlx postgres, `Engine::Postgres`, `pg.rs` with lazy pools per database (3 each, 4 live, LRU), startup `-c` options plus readback (SET fallback for PgBouncer), version gate, TLS, Stop by pid and backend_start, writer with READ WRITE and lock_timeout. 588 passed live on Postgres 13 and 18, MySQL 8.4, MariaDB 11.4. Known limits: sqlx forces `TimeZone=UTC`, so timestamptz shows in UTC until the Advanced tab; `backend_start` is kept in a process-wide map keyed by pid, so a pid shared by two servers makes Stop report "already gone" instead of cancelling; sqlx reads PG* environment variables for fields left unset
- [x] 3. Postgres lexer (`lex_pg.rs`) with two readings, read-only check, refused functions in calls and after a `.`, `fill_pg`, `changes_escaping`, redaction, lexer API takes a `Dialect`. Opus review: 3 critical, 2 high fixed in the lexer (67b52ef) or moved to layer 2; re-attack found no bypass, its three low gaps fixed after
- [x] 4. Read path: `pg_catalog.rs` queries, `pg_read.rs` (each user statement alone in a rolled-back READ ONLY transaction; a pooled connection that ran user text is closed), schema and table lists, page and count, new Kinds (Bool, Uuid, Array, Interval, Inet, Range), Structure, Info, Relations, Triggers, `pg_create_table`. 621 passed live on all four servers. Known limits: Postgres filter operators for the new kinds use the Text group until designed; CREATE TABLE text omits partition keys, inheritance and storage options
- [x] 5. Writes and undo (opus review). Merged 0c9b646: every value bound as text with `CAST($n AS format_type)`, no LIMIT 1 (more than one matched row is refused and rolled back), RETURNING keys, OVERRIDING SYSTEM VALUE for undo on identity columns, `parse_edit_for` lets the server validate Postgres types; pooler fallback is SET LOCAL in every transaction and statement pooling is refused (review M2), wired into Query and shell in 1100424. Opus review: no injection through `format_type` and no write from a locked connection; fixed in cf83a74 and dafa7e8: Export runs each statement alone in a rolled-back READ ONLY transaction, column types are read under `search_path = pg_catalog` so casts name their schema, identity and generated flags come from `attidentity`/`attgenerated`, a key matching two rows is refused before any row is touched, undo of an insert deletes only a row that still holds the inserted values, jsonb/json/float/bit cells take part in the stale-value check. 687 passed live on Postgres 13 and 18, MySQL, MariaDB and Mongo
- [x] 6. Custom Query: a locked run sends, per user statement, `BEGIN READ ONLY`, `SET LOCAL statement_timeout`, the statement (paged or as typed) and `ROLLBACK`, after a once-per-run `set_config('search_path', $1, false)`; an unlocked run sends the statements as typed (never in the page or count wrapper, so a side effect runs as often as written and no count reruns it) on a READ WRITE session and closes it; counts run in the same read-only transaction; behind a pooler nothing is set on the session, each statement is `SET LOCAL search_path` plus its own `BEGIN READ ONLY` or `BEGIN READ WRITE`, a user BEGIN or COMMIT is refused, and Stop closes the connection instead of cancelling by pid; server errors carry the SQLSTATE and position through `db::explain`. Known limits: the tab's schema is a free text field until step 8; export of a Query result still names the database as the schema
- [x] 7. Shell and attached connections (opus review). An unlocked relay reads back `client_encoding` (UTF8), `standard_conforming_strings` (on) and `backslash_quote` before each `fill_pg`, because a view or rule over pg_settings can change them unseen. Merged 1aacb9b; review found no lock escape, its 3 medium and 3 low findings fixed in 2aedbfe (plain INSERT with opt-in `returning`, readback only before a fill, a cut cancels its backend and drops the relay, describe inside the transaction, `$n` never joins a following digit, pooler mode wraps every statement in its own transaction and refuses script transaction control). Known limit: locking a connection takes effect on the next run
- [x] 8. App wiring (e1d0f02): schema switcher B under each open database (default `public`, counts, "Show system schemas"), `Sel.schema`, `Conn.tables`, `sql_target` on `Db::at`, caches keyed by database and schema, tabs and every view (Content, Structure, Info, Relations, Triggers, edits, undo, Query and its export, Shell) use the tab's schema, a foreign key to another schema opens that table. Hidden for Postgres: create and drop, dump and import, Users, Server, structure editing (a note says it is not built yet). Undo runs on the pool of the database it wrote to. 706 passed live on five servers. To check on screen in step 11: picker spacing and toggle size, proportional font in the picker
- [x] 9. Connection form, rail mark and logo done in 3fb5695: Postgres card ("13 and later", teal `ENGINE_PG()`), port 5432, Database field, libpq sslmode names, Advanced tab hidden, "Paste connection string" (`Addr::from_paste`: postgresql:// and postgres:// URIs and the libpq key=value form; a pasted password is cleared from the box), locked-connection footer note, a drawn "PG" monogram as the logo (no third-party asset). Demo `QUARRY_TEST_PG=<view>` in `src/app/demo_pg.rs` (a256338): content, rules, sqlmode, grouped, edit, select, structure, info, relations, triggers, query, shell, ac, acquery, schemas, connform, paste, tls; `bench.py --pg-only`. Known limit: no client certificate or key fields
- [x] 10. Autocomplete per dialect (44b2a8a): `sql_words_pg.rs` with 281 keywords, 101 reserved words, 112 functions with signatures and 25 types after `::`; Postgres quoting rules; `schema.table` and `schema.table.column`; dialect-aware statement splitting so dollar quotes and E'' strings do not break completion; refused functions are never offered. `Scope.schemas` is empty until step 8
- [x] 11. CI live services (33a6e3f: MySQL, MariaDB, Postgres 13 and 18 and Bun in the Linux job), README (3b13522), CLAUDE.md `QUARRY_TEST_PG` line (local, untracked), screenshots of every `QUARRY_TEST_PG` view in both themes compared with the designs, bench (`--quick --pg-only`: about 103 MB RSS, 31 MB footprint, frame p50 0.6 ms; the MySQL footprint swings by 40 MB between identical runs on master too, so no regression), whole-branch opus review (1 high, 4 medium, 7 low, all fixed in ed3568a and 251769e), /simplify (c829d52, 408cd60, 041195c to 7f8d669), and the cell editor fix found by the user (a664bda: grid clicks ran while the page was out of the tab)

**v1 summary:** a PostgreSQL 13+ connection opens with TLS, SSH or a pasted `postgresql://` string, lists databases and, per database, a schema picker; Content reads and edits rows with typed casts and undo; Structure, Info, Relations and Triggers are read-only; Custom Query and the Shell run statements with the three-layer read-only guard (session default, one rolled-back READ ONLY transaction per user statement, the lexer); PgBouncer-style poolers get transaction-local settings only; autocomplete knows PostgreSQL words, types and schema names. Checked on a664bda against postgres:13 and 18, mysql:8.4, mariadb:11.4 and mongo:8.

**v1 known limits:** a locked connection is only as safe as the role, because a server function that wraps a side effect is invisible to the lexer and non-transactional effects are not undone; structure editing, import, SQL dump, create and drop, users, the server window and the Advanced tab are v2 or v3 and hidden; timestamptz shows in UTC; behind a pooler Stop closes the connection and the statement runs until statement_timeout; no client certificate fields; CREATE TABLE text leaves out partitioning, inheritance and storage options; the logo is a drawn monogram.

**v2** (after v1 ships)
- [ ] 1. UI tests of the critical paths with `egui_kittest` (requested 2026-11-11, first in v2 so every v2 step lands with UI tests): a test-only `App` constructor that needs no window, then headless flows against the Docker servers (MySQL, MariaDB, PostgreSQL, Mongo) that click and type like a user and check the rows on the server afterwards: inline cell edit and Save, Esc then edit again, multi-row edit and Save, row delete and multi-select delete, insert, undo of each, switching view or tab while an edit is open, and the locked connection refusing all of them. Why: the live tests cover the database layer, but the path from a click to the write batch had no automated test, and the cell editor bug of a664bda lived exactly there. First check that the kittest release matches egui 0.37
- [ ] 2. Line coverage in CI with `cargo llvm-cov` on the Linux job that runs the live servers, published with each run, so gaps are measured rather than guessed
- [ ] 3. Structure editing (`ddl_pg.rs`)
- [ ] 4. Relations and FKs
- [ ] 5. Triggers
- [ ] 6. CSV import and export
- [ ] 7. SQL dump
- [ ] 8. Create and drop database, schema and table
- [ ] 9. Connection form Advanced tab (design first)

**v3** (after v2, requested by the user 2026-11-11)
- [ ] 1. Cloud SQL connections through the Cloud SQL Auth Proxy, with instance discovery from the user's gcloud credentials (see "Cloud SQL (v3)" below; design first)

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

### Cloud SQL (v3)

The user opens Cloud SQL databases today with `cloud-sql-proxy --port 5432 --auto-iam-authn <project>:<region>:<instance>` and then connects to localhost. Quarry should do both: list the instances the current gcloud credentials can reach, and start and stop the proxy with the connection, the way it already opens an SSH tunnel.

- **Connection kind.** A Postgres connection gets a third way in next to Direct and SSH: "Cloud SQL", saved as the instance connection name (`project:region:instance`), IAM sign-in on or off, and public or private IP. No password is saved when IAM sign-in is on.
- **Discovery.** Through the gcloud CLI only, run as a child process and found like Bun with `find_exe`: `gcloud projects list --format=json`, then `gcloud sql instances list --project <p> --format=json` for PostgreSQL instances with region, state, IP kinds (public, private, PSC) and whether IAM sign-in is enabled (`cloudsql.iam_authentication`). The IAM user name is filled from `gcloud config get-value account` (a service account drops its `.gserviceaccount.com`). Decided 2026-11-11: no sign-in without the CLI; when gcloud is missing the form says so and links to its install page.
- **The proxy.** Run the official `cloud-sql-proxy` binary as a child process on a free localhost port Quarry picks (not 5432, which may be taken), with `--auto-iam-authn` when IAM is on, and `--private-ip` or `--psc` per the saved choice (both in the first version, decided 2026-11-11). Wait for it to accept connections, connect to `127.0.0.1:<port>`, and stop it when the connection closes or Quarry quits. Its output goes to the status bar list on failure. Nothing secret is on its command line: it reads the same gcloud credentials itself, which keeps the rule against passwords on command lines.
- **Finding or getting the proxy.** Look on PATH, then in the gcloud SDK's own `bin` (`gcloud info --format="value(installation.sdk_root)"`), where `gcloud components install cloud-sql-proxy` puts it. When it is missing, offer two buttons: install the gcloud component (only when gcloud manages its own components; package-manager installs refuse it), or download Google's release for this platform into Quarry's data folder, verified against the published SHA-256 before it is ever run, the same rule as the self-updater. Decided 2026-11-11: the download is offered.
- **Later, if the binary is a burden:** connect natively. Google publishes connector libraries for Go, Java and Python but not Rust; the protocol (Admin API `connectSettings` and `generateEphemeralCert`, then TLS to the instance with the ephemeral certificate, and the IAM token as the password) could be done in Rust, but only once the proxy version has shown what users need.
- **Read-only.** Unchanged: the proxy is only the way in; the session guard, the per-statement read-only transaction and the lexer apply as for any Postgres connection. A role reached through IAM is still only as safe as its grants (the locked note in the form says so).
- **Tests.** Discovery parsing from recorded `gcloud ... --format=json` output; proxy start, ready, stop and failure with a stub binary on PATH; one manual check against a real instance before ticking.

## How this work is run

As in PLAN.md "How this work is run" (delegation, reviews, merging, cleanup, environment), with these differences:
- **Branch:** `feat/postgres` from master. Every brief tells the agent to run `git checkout -B <name> feat/postgres` first.
- **Lanes:** step owners are listed in the Phases table so up to three agents compile at once without touching the same files.
- **Live servers:** each agent starts its own containers on unique ports and removes them after the merge.

## Designs

New pages in `docs/sql/designs.figcraft`, Workbench and Paper side by side:
- **"Postgres · sidebar":** A, a tree database > schema > table; B, a database switcher with schemas as groups; C, a `schema.table` list per database with a schema filter.
- **"Postgres · connection form":** the Postgres card enabled (port 5432, database, libpq sslmode), and a variant with a connection string paste.
- **"Postgres · Advanced tab"** before v2 step 9.

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
1. **UI tests** of the critical paths with `egui_kittest` against the Docker servers (see Progress).
2. **Line coverage** in CI with `cargo llvm-cov`.
3. **Structure editing.** Pure move of the MySQL emitter out of ddl.rs into `ddl_my.rs`; `Change`, `Sim`, staging and inverse groups stay shared. New `ddl_pg.rs`: `ALTER COLUMN TYPE .. USING col::t`, RENAME COLUMN, SET or DROP DEFAULT and NOT NULL, ADD or DROP CONSTRAINT, separate CREATE and DROP INDEX, COMMENT ON, identity (serial read as an owned sequence). sql_alter.rs hides positions and charset. DDL is transactional, so the plan and its undo each run in one transaction, with the `expect` snapshot checked inside it after `LOCK TABLE` and a `lock_timeout`.
4. **Relations and FKs** through the same plan path; sql_rel.rs reused.
5. **Triggers** as a function and trigger pair in one transaction, DROP then CREATE (13 has no CREATE OR REPLACE TRIGGER); undo replays `pg_get_functiondef` and `pg_get_triggerdef`; a warning when the function is shared by other triggers.
6. **CSV import:** batches of `min(500, 65535 / cols)` rows, `ON CONFLICT (pk) DO NOTHING` or `DO UPDATE SET c = EXCLUDED.c`, undo keys from RETURNING, one transaction. Export: `stream_conn` takes `Conn`.
7. **SQL dump** from the catalogs in `REPEATABLE READ READ ONLY`: schemas, enums, domains, sequences with setval, tables, INSERT batches, constraints, indexes, views, functions, triggers and comments via `pg_get_*def`. The script splitter gets a dollar-quote mode; `COPY FROM stdin` blocks from pg_dump files are refused. Grants, owners and inheritance are out of scope.
8. **Create and drop:** a database outside a transaction on the default database's pool, evicting and closing the dropped database's pool and tabs first, `WITH (FORCE)` opt-in inside the typed confirm; schema CREATE and DROP [CASCADE]; a Postgres CREATE TABLE builder; a create undo checks emptiness and drops in the same transaction.
9. **Advanced tab** (design first): time zone, search_path, statement and lock timeouts, application_name, and an init command limited to an allow-list of SETs. Never the read-only settings, role, session_authorization, client_encoding, standard_conforming_strings, DateStyle, IntervalStyle or bytea_output.

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
