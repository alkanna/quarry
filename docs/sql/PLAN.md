# MySQL support for Quarry (designs first, then phased implementation)

## Context

Quarry is a MongoDB-only client. The goal is to open it to SQL engines, starting with MySQL/MariaDB, and reach Sequel Ace level features over three phases. The data layer is coupled to the mongodb driver everywhere (`Conn.client: Option<mongodb::Client>`, `Tab` holds BSON rows, `ConnMeta` has no engine field), so MySQL needs its own SQL path that sits beside Mongo rather than behind a shared trait. The user chose: phased delivery, a generic SQL layer (MySQL now, Postgres/SQLite later), a new FigCraft file, and 2-3 variants on key screens.

## Progress

Update this list when a step lands. Each step is reviewed before it is ticked.

**v1**
- [x] 1. sqlx dependency, `db::sql::Engine` and `Addr` `ConnMeta.engine`, connection form B. Advanced tab skipped until needed. Temporary spots for step 4: disabled Test for SQL in conn_form.rs, early return in `App::connect`
- [x] 2. SSH plain port-forward mode in src/ssh.rs (`ssh::open(.., Some((host, port)))`, connect to 127.0.0.1:tunnel.port)
- [x] 3. `db/sql/{lex,value}.rs` and `Dialect` with unit tests. Reviewed: read-only check passes under every quoting mode and both readings of versioned comments
- [x] 4. SQL worker, new `Msg` variants, live tests (`QUARRY_TEST_MYSQL`). Reviewed; live tests pass on MySQL 8.4 and MariaDB 11
- [x] 5. state, tabs, poll and sidebar wiring (with a basic Content grid and the `QUARRY_TEST_SQL=content` demo; table lists load when a database opens)
- [x] 6. Content view with design F: Rules / SQL bar, clause chips, edits, undo, colors, column actions, multi-select, Copy as (incl. Markdown and HTML). Reviewed; writes refuse partial keys, undo is safe with generated and renamed columns
- [x] 7. Structure (design A) and Info, read-only, plus read-only Relations and Triggers views. Structure loads when one of those views opens
- [x] 8. Custom Query: highlighter, several result sets, Stop (`KILL QUERY`), Explain, history, Query tabs from the sidebar. Save to library and Format left out
- [x] 9. SQL autocomplete (tables, aliases, columns, values with frequencies, functions and signature help) in the Content bar and the Query editor
- [x] 10. SQL shell with typed tables, and the shared shell layout for Mongo and SQL. Reviewed; values are filled with the server's quoting rules, escaping changes are refused, BIGINT is exact, a bad `where` never means every row
- [x] 11. Demo data, `QUARRY_TEST_SQL` switches, CLAUDE.md, and the final polish (searchable column pickers, all columns by default, prepared statements refused in the shell, README)

**v1 known limits:** the Advanced tab of the connection form is not built; structure editing, relations and trigger editing, import and export, create and drop, attached shell connections, users, the process list and variables are v2 or v3; Save to library and Format in Query are not built.

**After v1 (requested by the user, done):** rail shortcuts for open connections and tabs that switch the active connection; SQL autocomplete inside sql`...` in the shell; resizable editor/results split in Query and Shell; Query results paged at 1,000 with the count in the background; Export for every Copy as format (selected, page, all rows streamed); multi-select in Query and Shell results; Query pager order fix; slow-drag divider fix.

**Open before v2 (do these first, in order):**
- [x] Cleanup from /simplify merged (c50f17d): shared escaping, hex, quote_ident and highlighter helpers, stale allows and dead code removed, per-frame work cached, width cache keyed by a rows generation, `refused` flag on ResultSet, popover in widgets. Kept as is on purpose: `size` vs `human_bytes` (different text), the Include column names row, `sets_req` vs `run_req`.
- [x] Export "All statements in the editor" (user request): run every statement of the Query editor on one connection (USE and @vars carry over), refuse the export if any statement fails `lex::read_only_check` even when unlocked, stream every result set; output "one file per result" (name-1.csv, name-2.csv, any format) or "one file" for SQL INSERT (appended), JSON lines (rows get a `statement` field), JSON (array of {statement, rows}), Markdown and HTML (a heading per statement); progress "Statement 2 of 5, 340k rows" with Cancel; .part files renamed at the end.
- [x] Polish: the shell popup footer is cut with an ellipsis to the popup width, and views get their own `w` badge (the demo schema now holds a view).
- [x] Polish: the LIMIT clause chip text is centered on its ink like the other chips.
- [ ] Ask the user to verify by hand: slow drag of the editor divider (if it still jerks, check the `hi` limit that depends on the results space), rail right-click menu, Export from Content, Shell and Mongo, the column combobox.
- [ ] User decisions: smoke test on real MySQL/MariaDB servers (SSH tunnel, big tables), open the PR feat/mysql into master with the `ci` label, and push/PR `fix/linux-wayland-fallback` (its Linux-only code was never compiled; CI on ubuntu-22.04 will).

**v2**, driven end to end by session "attach-shell-connections" (it owns feat/mysql merges for v2; item 1 on branch `shell-attach`) (designs already exist for all of these except the connection form Advanced tab; see Designs): attached connections in the shell (first: the user asked for it; design page "Shell · attached connections", plan section "Attached connections in the shell"), structure editing with the ALTER preview (column editor dialog on the Structure page), relations and FKs (create, drop, click a FK cell to open the row), triggers editing, import and export (CSV and SQL dump, streamed), create and drop database and table (typed confirm), connection form Advanced tab (time zone, SQL mode, init command, timeouts: needs a quick design first).

**v2 definition of done** (the user asked that v2 is not considered finished before all of this holds):
1. All seven v2 items are on feat/mysql and ticked here; the Advanced tab design was shown to and approved by the user before it was built.
2. Every item that writes, and the attached-connections relay, had an independent opus review; every finding is fixed with its failing input as a test, or marked "won't fix" here with the reason.
3. On the final head: `cargo check --all-targets` with no warnings; full `cargo test`; live tests against both mysql:8.4 and mariadb:11 (QUARRY_TEST_MYSQL, QUARRY_TEST_MARIADB), shell live tests with Bun included; release build; `python3 bench.py --quick` with no regression against the numbers recorded here; screenshots of every new view in both themes compared with the designs.
4. One final whole-branch opus review of all v2 commits together, its findings fixed.
5. No worktrees, agent branches or test containers left; this file has a short v2 summary and its known limits.

**v3**: users and privileges, process list, server variables, console log.

## How this work is run

The plan and its progress list live in this file; update it whenever something lands or is decided. CLAUDE.md is not tracked by git here, so switches added for the SQL work are documented there locally and summarized in the Designs and Verification sections below.

- **Delegation:** each step goes to a subagent with `isolation: worktree`, model sonnet for building and opus for reviews of risky code (writes, the read-only guard, anything that builds SQL text). A brief is self-contained: the goal, what exists, file ownership so parallel agents do not touch the same files, the strict comment rules (no em dashes, emojis or arrows; explain why), no rustfmt, and the verify list (cargo check with no warnings, cargo test into a log file, live tests in Docker on a unique port, release build, screenshots read and compared with the designs).
- **Worktrees start at master,** so every brief tells the agent to run `git checkout -B <name> feat/mysql` first.
- **Merging:** one Conventional Commit per reviewed step on `feat/mysql`, local only, never pushed without the user. Cherry-pick or fast-forward the agent commit, then check `git ls-files | grep -E '\.(bak|orig|rej)$'` (agents committed .bak files three times; *.bak is now in .gitignore), run cargo check and cargo test, tick this file, commit.
- **Cleanup right after each merge:** `git worktree remove -f -f`, delete the agent branches, `docker rm -f` its test container. The user asked for this explicitly.
- **Reviews:** security-sensitive steps get an independent opus review before they are ticked; findings go to a fix agent with each failing input as a test.
- **Environment:** if cargo says sccache is missing, prefix with `RUSTC_WRAPPER=`. Many parallel worktrees saturate the machine (each builds its own target dir); keep at most three agents compiling at once.
- **Designs:** `docs/sql/designs.figcraft`; read a frame with `figcraft-cli export docs/sql/designs.figcraft -o /tmp/x.png --node "<frame name>"`. New screens are designed in FigCraft (Workbench and Paper frames side by side, real theme tokens) and shown to the user before they are built. The generator scripts that drew them were in a session scratchpad and are gone; draw new frames with the FigCraft MCP tools directly or rebuild a small generator.
- **Demo switches** for screenshots: `QUARRY_TEST_SQL=<view>` (content, rules, sqlmode, grouped, edit, select, colpick, structure, info, relations, triggers, query, shell, shellac, ac, acquery), `QUARRY_TEST_SQL_JSON=<file>`, `QUARRY_TEST_RAIL=1`, `QUARRY_TEST_CHECKED=1`, `QUARRY_TEST_EXPORT=1` (or `script`, `script-ok` for the All statements scope); `bench.py --sql-only` for memory and frame time.

## Designs

`docs/sql/designs.figcraft` (FigCraft). Every screen is drawn in both themes, Workbench (dark) and Paper (light), with the real theme tokens. To look at a page without the app:

```sh
figcraft-cli render docs/sql/designs.figcraft -o /tmp/page.png --page "Table content"
figcraft-cli export docs/sql/designs.figcraft -o /tmp/frame.png --node "Content · in the window · Workbench"
```

Pages: Connections (pick: **B**), Table content, Filters and selection, F · Query builder (pick: **F**), Structure (pick: **A**), Query, Shell, Shell · attached connections, Relations, triggers, info, Import and export, Server, Create and drop.

## Implementation

### Core decisions
- **Driver:** `sqlx 0.8` with `runtime-tokio`, `tls-rustls-ring-webpki` and `mysql` (no default features). Postgres and SQLite later are one feature flag each, with the same API. Rejected: mysql_async, which is MySQL only, and `sqlx::Any`, which can't read DATETIME or DECIMAL columns. Turn off sqlx's session defaults (`pipes_as_concat(false)`, `timezone(None)`) so user SQL behaves like it does in the mysql CLI.
- **Generic layer without traits:** `db::Engine { Mongo, MySql }` on `ConnMeta` (`#[serde(default)]`, so old files load as Mongo). `sql::Pool { My(MySqlPool) }` and `sql::Dialect { MySql, MariaDb }` (detected from `VERSION()`) hold the SQL that differs per engine: identifier quoting, placeholders, paging, information_schema queries, read-only session, timeouts and cancel. Postgres and SQLite become new enum arms. No trait spans Mongo and SQL, which keeps to the repo learning against abstraction layers.
- **Rows stored as raw BSON:** each SQL row is a `db::Row`, with a hidden `_id` document holding its primary key. Grid drawing (`doc_table`, table.rs:319), autocomplete values (complete.rs:187, 275), the values cache, drafts and undo (drafts.rs, db.rs:171), the pager and `Msg::Docs/Collections/Saved/Written/Tested/FieldValues` are reused unchanged. A list of `Col { name, ty, kind: Kind, nullable }` drives cell colors and edit parsing. Exact values (DECIMAL, dates, JSON, ENUM) are stored as server text. Tables without a primary key are read-only, as in Sequel Ace.
- **Tabs:** `Tab.sql: Option<Box<SqlTab>>` (state.rs:145). Content reuses `docs`, `fields`, `filter_txt` (the WHERE clause), `sort_txt` (the ORDER BY clause), `req`, `page` and the rest. `View` gets `Structure`, `Sql` and `Info`. `Documents` acts as Content.
- **Read-only guard**, one place in Rust that the Custom Query and the shell both go through: the session runs with `transaction_read_only=1`, and every statement must pass an allow-list (SELECT, WITH, SHOW, DESCRIBE, EXPLAIN, `SET @var` and similar). INTO OUTFILE is refused, and `/*! */` comments are treated as code because MySQL runs them.
- **SSH:** add a plain port-forward mode to `ssh::open` (ssh.rs:185) that pipes `direct-tcpip` straight through, sharing a `pipe()` helper with the SOCKS path. Mongo keeps SOCKS. TLS settings come from the URI query (`ssl-mode`, `ssl-ca`, ...).

### New files
- `src/db/sql/mod.rs`: `Db`, `Pool`, `Session`, `Col`, `Kind`, `ResultSet`, `Structure`, `Schema`, `Dialect`.
- `src/db/sql/lex.rs`: tokenizer, `split_statements`, read-only check. Shared by the worker, the highlighter and autocomplete.
- `src/db/sql/value.rs`: server text to BSON by `Kind`, BSON to bind value, editor text to BSON.
- `src/db/sql/worker.rs`: an `impl Worker` block with `sql_connect/test/tables/page/structure/run/kill/save/delete_rows/insert/schema/field_values`. Make `Worker::spawn` (worker.rs:117) `pub(super)`. New `Msg` variants: `SqlConnected`, `SqlStructure`, `SqlRan`, `SqlSchema`.
- `src/app/sql_views.rs` and `src/app/sql_ops.rs`: drawing the Content bar, Structure, Query and Info views, and sending and handling requests.
- `src/ui/sql_complete.rs`: `suggest_sql`, plugged into the existing `ac_field` (complete.rs:511). Suggestions depend on the cursor position:
  - After FROM or JOIN: tables.
  - After `alias.`: that table's columns.
  - In a value position: frequent values, then a capped `GROUP BY` lookup on the server.
  - Anywhere else: keywords and functions with signatures.
  - The schema comes from `SqlSchema`, cached per database.
- `src/ui/editor.rs`: a `sql_job` highlighter, modeled on `js_job` (editor.rs:207).
- `src/db/shell_runner.mjs`: the one shell runtime for Mongo and SQL (it replaced sql_runner.mjs), described below.

### Changes to existing files
- `state.rs`: `connect` (:902) branches on the engine, `writable_sql()` sits beside `writable_target` (:874), `ServerInfo::status_line` shows "MySQL 8.4", and the `agg_source`/`agg_store` arms cover Query results.
- `poll.rs`: handles the 4 new messages, takes SQL `fields` from the column list, and branches `reload_of` (:536).
- `tabs.rs:36`, `documents.rs:113` (SQL header and view tabs), `sidebar.rs:321` (table and view glyphs, Mongo-only menu items hidden).
- `table.rs:511` (cell color by `Kind`), `table.rs:224` (column filter and sort actions write SQL).
- `drafts.rs:73` and `undo.rs:11` branch to the SQL save, delete and insert.
- `tree.rs:312`: inline edits parse values with `sql::value`.
- `config.rs:352`: `want_values` has a SQL branch.
- `conn_form.rs:152`: engine choice, `mysql://` and `mariadb://` accepted, SOCKS proxy hidden for SQL.
- `demo.rs`: a "MySQL sample" connection with no Keychain access, and a `QUARRY_TEST_SQL=content|structure|query|info|ac|shell` switch documented in CLAUDE.md.

### Content query builder (design F, "F · Query builder" page)
- One bar with a Rules / SQL switch. Rules mode: Sequel Ace typed rules (operator list per column type: number, text, date, ENUM/SET, BLOB/JSON; date rules add the relative-date shortcuts from `src/db/dates.rs`), earlier rules collapse into tokens. SQL mode: the WHERE text with `suggest_sql` autocomplete. Rules turn into SQL always; SQL turns back into rules while it is a flat AND/OR list.
- Clause chips under the bar, mirroring the Mongo find options (sort, project, limit, skip, hint): ORDER BY (several keys, shift-click a header adds one), LIMIT, OFFSET, Columns (the SELECT list, with expressions), GROUP BY (dates by hour/day/week/month/year, aggregate values, HAVING with typed rules), DISTINCT, index hint (`USE INDEX`). Each chip has a typed editor in Rules mode and SQL text in SQL mode. The full SELECT folds open under the bar.
- Grouped results are read-only; double-clicking a group adds its values as rules and ungroups.

### Multi-select and copy (same as the Mongo table)
- Checkbox column with select-all and shift-click ranges (`toggle_check`, `CHECK_W`), the floating `selection_bar` and the row menu acting on all checked rows (`docs_menu`, documents.rs:26), with SQL actions: Copy as, Duplicate, Export, Filter to these (`WHERE pk IN`), Set value (stages one draft per row), Edit in Query, Delete.
- **Copy as** submenu, also on Query and Shell result tables: SQL INSERT, SQL UPDATE, CSV, TSV, JSON, **Markdown table**, **HTML table**, values only, with an "Include column names" switch. Markdown right-aligns numbers and escapes `|`; NULL is an empty cell. HTML is also put on the clipboard as rich text (`text/html`) so it pastes as a table. The formatters are plain functions over `db::Row` + column list, with unit tests; add them to the Mongo copy menu too since they work on rows.

### Picked designs
- Connection form: **B** (engine cards on the left, tabbed form Standard / SSH / TLS / Advanced).
- Structure: **A** (dense column grid, index grid below, column editor dialog with the ALTER preview).
- Content: **F** (Rules / SQL bar with clause chips), multi-select and Copy as from "Filters and selection".

### One shell layout for both engines ("Shell" and "Shell · attached connections" pages)
- Mongo and SQL shells share one layout: toolbar (Run, Stop, connection chips + Attach, Packages / History / Help on the right), editor with line numbers, result header with Table/JSON, Copy as and Export, then a Console / Queries panel. The read-only note becomes a chip state per connection. Scripts keep the existing `async function run() { … }` form on both.
- Restyle `src/app/shell.rs` to this layout as part of v1 so the two never drift.

### Attached connections in the shell (cross-database scripts)
- A shell tab can attach other saved connections of either engine, each under a JS name (default: the database name), saved with the tab. Attaching connects the connection (and its SSH tunnel) if needed.
- SQL handles (`shop.sql\`…\``, `shop.orders.find()`) always go through the Rust relay, so they reuse the SQL read-only check per connection.
- Mongo handles (`travels.users.find()`) are extra `MongoClient`s in the runner, built from that connection's URI and proxy like `shell_proxy` (src/app/shell.rs:183). The runner input becomes `{ …, attached: [{ name, engine, db, uri?, proxy?, readOnly, collections }] }`.
- Read-only: attached connections are read-only unless "Allow writes" is set and that connection is unlocked. ponytail: the Mongo guard patches prototypes (shell_runner.mjs:18), so it is global: if any Mongo client in the run is read-only, Mongo writes are blocked for all of them; make it per client when someone needs to write to one Mongo while reading another.
- Language server: declare each handle (`declare const travels: { users: Collection<Users>, … }`, `declare const shop: SqlConn<ShopTables>`) from the same field info and information_schema types.
- The Queries panel and the query log name the connection of every query.
- Phase: v2, right after the SQL shell ships, since it needs the SQL relay and the shared shell layout first.

### SQL shell (same power as the Mongo Shell)
Scripts get no SQL driver. `shell_runner.mjs` gives them:
- a `sql` tagged template, which turns `${}` values into `?` placeholders;
- `sql.unsafe(text, params)`;
- one global per table with `find/count/insert/update/delete`.

Each query goes back to Rust as a `{type:"sql", id, text, params}` line on stdout. `run_shell` (shell.rs:107) keeps stdin open, runs the query on the tab's session through the same read-only checkpoint, and answers with relaxed-EJSON rows. The child process never gets the password or a tunnel, and every write path is blocked because the only path is Rust. Bun.SQL was rejected: the only way to guard it would be patching its methods, which the standing learning rules out.

For the language server, `sql_globals` (next to `js_globals`, js_ls.rs:96) declares `interface Tables` from information_schema: ENUM columns become literal unions and nullable columns get `| null`. `shell_ls.rs:131` picks between the Mongo and SQL declarations. `dangerous_words` (shell.rs:8) gets the SQL write keywords.

### Phases
**v1** (in order):
1. Cargo and `Engine`, then the connection form.
2. SSH forward mode.
3. `sql/{mod,lex,value}` with unit tests.
4. SQL worker, new messages and live tests.
5. state, tabs, poll and sidebar wiring.
6. Content view with edits, undo, colors and column actions.
7. Structure and Info, read-only.
8. Custom Query with highlighter, multiple result sets and Stop (`KILL QUERY`).
9. Autocomplete.
10. SQL shell with typed tables (moved into v1 because the user wants querying parity with Mongo).
11. Demo switches and CLAUDE.md.

**v2:** structure editing with a SQL preview, relations and FKs (create, drop, click through), triggers, CSV and SQL-dump import and export (streamed), create and drop of databases and tables through `danger.rs`.

**v3:** users and privileges (the users.rs window), process list with Kill, server status and variables (server.rs), console log of each statement. New windows get listed in `window_open` (chrome.rs:49).

After approval, save a trail learning with these decisions: the phases, the enum-based generic SQL layer, rows stored as BSON, and the shell routed through Rust.

## Verification
- **Designs:** `screenshot` each frame in both themes. `inspect_document` shows no clipped or overflowing text. Then save the file.
- **Unit tests:** `cargo test`, covering quoting, statement splitting (quotes, comments, `/*!`), the read-only classifier, value conversion per `Kind`, the UPDATE builder with a composite primary key, `Engine::from_uri` and loading old `ConnMeta` files, positions for `suggest_sql`, and `sql_globals`.
- **Live tests:** run `docker run -d -p 3306:3306 -e MYSQL_ROOT_PASSWORD=quarry mysql:8.4`, then `QUARRY_TEST_MYSQL='mysql://root:quarry@127.0.0.1:3306' cargo test`, and the same with `mariadb:11` under `QUARRY_TEST_MARIADB`. Cases:
  - paging, filters and sorts;
  - every column type;
  - multiple statements, including an empty result set;
  - a save conflict;
  - read-only refusals (DELETE, `SET SESSION transaction_read_only=0`, `/*!DELETE*/`, a stored function that writes);
  - `KILL QUERY` on `SLEEP(10)`;
  - the SSH forward.
  
  Add a mysql service to the CI job that runs under the label.
- **UI:** `HOME=/tmp/qhome QUARRY_NO_FOCUS=1 QUARRY_TEST_DARK=1 QUARRY_TEST_SQL=content ./target/release/quarry --screenshot out.png` for each view in both themes, compared against the FigCraft frames.
- `cargo check --all-targets`, then `trail check` before committing.
