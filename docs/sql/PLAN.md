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

**v2 progress** (branch, state):
- [x] 1. Attached connections in the shell: runtime 371fb4d, review fixes d848044, UI 5e1fcbd. Opus review of the relay done (findings fixed, see "Review (c5761a5)").
- [x] 2. Structure editing: built e993e30, opus review found 14 issues, all fixed in c8d5a09 (see "Review (e993e30)").
- [x] 3. Relations and 4. triggers: built 49a4432 and c5c77f1 on `relations-triggers`, rebased onto feat/mysql on `reltrig-fix` (FK changes take the same planned, checked ALTER path as column and index changes); opus review found 14 issues, all fixed on `reltrig-fix` (see "Review (49a4432, c5c77f1)").
- [x] 5. Import and export: built 16bb646, opus review found 13 issues, all fixed in 90ff5fc (see "Review (16bb646)").
- [x] 6. Create and drop database and table: built ee27114 on `create-drop`, rebased onto feat/mysql as 339c804 on `create-fix`; opus review found 6 issues (9 points), all fixed in 305b250 (see "Review (ee27114)"). New database and New table dialogs with the statement preview, typed confirmation for drop database, drop table, drop view and empty table (`Danger::SqlDrop`), the "+" in the sidebar, undo of a create while the object is still empty (`SqlUndo::CreatedDb`, `CreatedTable`), everything disabled while locked. Statement builders and live tests in src/db/sql/create.rs, dialogs in src/app/sql_create.rs.
- [x] 7. Connection form Advanced tab: design approved 2026-11-09 (frames 12544 and 12617), built 26b3c22 on `advanced-tab`, rebased onto feat/mysql as c79997f on `advanced-fix`; opus review found 10 issues, all fixed in b4c8c63 (see "Review (26b3c22)"). Settings live in `ConnMeta.advanced` (`src/db/sql/advanced.rs`, not secret, not written when default) and are applied by `session()` in the worker after the read-only guard: time zone, SET NAMES, sql_mode, statement timeout, then the init command. The init command only sets user variables and allow-listed session variables to literals (`init_statements`), each sent as a prepared statement; the guard and autocommit are set again and read back after it, and a failing setup fails the connect at once with its statement.
- [x] Bench baseline measured on 4abe8c6 (last commit before v2), `python3 bench.py --quick`, release build, 2026-11-09. Compared 2026-11-10 by running 4abe8c6 and 1afb9cf back to back on the same machine: memory equal within 1 MB, frame p50 equal, frame p95 equal or lower on head (for example sql query 1,000 rows 5.0 vs 1.8 ms); a lone head run had shown a higher p95 that the back-to-back run put down to machine load:

| scenario | rss_mb | footprint_mb | cpu_avg | cpu_max |
|---|---|---|---|---|
| demo | 102.1 | 32.0 | 0.0 | 0.0 |
| 1,000 docs | 138.6 | 45.0 | 0.1 | 0.9 |
| 10,000 docs | 436.2 | 234.0 | 0.3 | 2.7 |
| 50 docs x 1 MB | 224.4 | 122.0 | 0.0 | 0.2 |
| sql content 1,000 rows | 120.9 | 49.0 | 0.0 | 0.0 |
| sql content 10,000 rows | 237.1 | 50.0 | 0.0 | 0.0 |
| sql content 200 rows x 100 KB | 169.5 | 97.0 | 0.0 | 0.0 |
| sql query 1,000 rows | 104.5 | 32.0 | 0.0 | 0.0 |
| sql rules | 102.9 | 30.0 | 0.0 | 0.0 |
| sql grouped | 102.4 | 30.0 | 0.0 | 0.0 |
| sql structure | 102.5 | 30.0 | 0.0 | 0.0 |
| sql shell | 103.0 | 30.0 | 0.0 | 0.0 |
| scenario | wall_s | cpu_s | peak_rss_mb | frame_p50 | frame_p95 | frame_max |
|---|---|---|---|---|---|---|
| demo | 0.6 | 0.4 | 166.2 | 0.8 | 2.8 | 160.4 |
| 1,000 docs | 0.5 | 0.5 | 196.9 | 1.1 | 3.0 | 23.4 |
| 10,000 docs | 1.8 | 1.8 | 494.5 | 3.8 | 4.7 | 22.6 |
| 50 docs x 1 MB | 1.2 | 1.2 | 278.8 | 0.8 | 2.2 | 19.5 |
| sql content 1,000 rows | 0.4 | 0.4 | 178.4 | 1.0 | 2.7 | 18.5 |
| sql content 10,000 rows | 1.0 | 1.0 | 294.9 | 3.5 | 4.3 | 23.3 |
| sql content 200 rows x 100 KB | 0.4 | 0.4 | 207.9 | 1.0 | 2.5 | 20.1 |
| sql query 1,000 rows | 0.3 | 0.3 | 163.7 | 0.6 | 2.1 | 24.6 |
| sql rules | 0.3 | 0.3 | 160.8 | 0.7 | 2.5 | 20.4 |
| sql grouped | 0.4 | 0.3 | 158.3 | 0.7 | 2.8 | 19.2 |
| sql structure | 0.3 | 0.3 | 157.8 | 0.5 | 2.4 | 17.1 |
| sql shell | 0.3 | 0.3 | 159.0 | 0.5 | 2.2 | 17.9 |

**v2 summary:** a Shell tab can attach other saved connections, Mongo or SQL, each read-only unless allowed and unlocked, all through one Bun runner and the existing SQL relay. The Structure page edits columns, indexes, FKs and triggers through a previewed, checked ALTER with undo; FK cells open the row they point to. CSV and SQL dumps import and export streamed. Databases and tables are created and dropped with a statement preview and a typed confirm, and an empty new one can be undone. The connection form has an Advanced tab (time zone, charset, sql_mode, statement timeout, init command). Checked on 1b5824e: `cargo check --all-targets` clean, 454 tests passed live on mysql:8.4, mariadb:11.4 and mongo with Bun, the two language server tests passed, release build done.

**v2 known limits:** the Content bar's filter literals use the sql_mode read at connect; MariaDB before 10.5.10 does not report column CHECKs, so they are not seen there; the init command only sets user variables and allow-listed session variables (not autocommit, completion_type, sql_select_limit or the charset variables, which the form owns); a row undo checks its key just before its transaction, not inside it; a create undo does not record the table id, so it relies on the emptiness check; the Advanced tab's System time zone on Windows was only type-checked. Still v3: users, the process list, variables, Save to library and Format in Query.


**v2 screenshots** (2026-11-10, every new view in both themes against its frame): fixed after the comparison: gzip in the dump dialog (a `.gz` name writes it compressed), the Rows column of "Tables that point here" (the view now asks for the schema's row estimates), whole rows in the Attach list. Left as they are, for the user to decide: the dump dialog has no Format row (CSV and JSON per table go through Content's Export; Excel does not exist); checkboxes and radios in dialogs use egui's grey look, not the design's accent fill, app-wide (Mongo views too), only the grids' `check_box` is filled; a focused field has no accent ring; disabled primary buttons are pale in Paper. A dialog can look see-through in a `--screenshot`: frames come so fast that its fade-in is not over at frame 120; on screen it is opaque.

**v2 definition of done** (the user asked that v2 is not considered finished before all of this holds):
1. All seven v2 items are on feat/mysql and ticked here; the Advanced tab design was shown to and approved by the user before it was built.
2. Every item that writes, and the attached-connections relay, had an independent opus review; every finding is fixed with its failing input as a test, or marked "won't fix" here with the reason.
3. On the final head: `cargo check --all-targets` with no warnings; full `cargo test`; live tests against both mysql:8.4 and mariadb:11 (QUARRY_TEST_MYSQL, QUARRY_TEST_MARIADB), shell live tests with Bun included; release build; `python3 bench.py --quick` with no regression against the numbers recorded here; screenshots of every new view in both themes compared with the designs.
4. One final whole-branch opus review of all v2 commits together, its findings fixed.
5. No worktrees, agent branches or test containers left; this file has a short v2 summary and its known limits.

**Review (e993e30)**, structure editing, fixed on branch `structure-fix` (each finding has its failing input as a test in `src/db/sql/ddl.rs`; live ones in `review_live`, on MySQL 8.4 and MariaDB 11.4):
1. An undo that drops a column or converts a type back asks for the table name like a drop and names what it loses (`AlterUndo::loss`, `Danger::UndoAlter`); every structure undo first re-reads the table and refuses unless the columns and indexes it touches are as the change left them (`snapshot`).
2. ENUM and SET values are read with MySQL's escapes in every sql_mode (the server writes COLUMN_TYPE that way) and written with the session's.
3. Every ALTER and undo step runs as one prepared statement, so a second statement cannot ride along; `check_expr` reads `"` as an identifier under ANSI_QUOTES.
4. MySQL's hex default of a binary column and the `b'..'` default of a BIT column are written back as those literals.
5. MySQL SRID and invisible indexes, and MariaDB ignored indexes and JSON (LONGTEXT with its json_valid CHECK) are read and kept; other MariaDB column CHECKs and COMPRESSED columns are refused like generated ones.
6. The inverse is one ALTER per inverse step, run in order, so each names columns as the steps before it left them.
7. Dropping an index with an expression part is refused, since the expression cannot be added back.
8. MariaDB's index added back without a dropped column uses the names the whole ALTER leaves.
9. The staged bar's preview comes from the same `plan` as Apply, with MariaDB's index edits.
10. Undo is worked out while previewing: "This change cannot be undone: why" shows in the staged bar, the column form and the typed confirmation, which only promises undo when there is one.
11. Apply now and Apply are refused while an ALTER runs.
12. Apply passes the connection's read-only state to the worker.
13. Statements are written for the sql_mode the app last saw, and the worker reads `@@SESSION.sql_mode` on the connection that runs them: when it differs nothing runs, and the app takes the new mode so the preview follows it. Row edits in `write.rs` bind every value as a parameter, so they do not have this gap; only their preview text, the Content bar's filter literals and the shell relay use the mode read at connect.
14. Duplicate column and index names compare without case, the index check against the indexes the staged changes leave; a collation that is only the table default is no longer pinned by edits or undo.
Won't fix: MariaDB column CHECKs are read from `CHECK_CONSTRAINTS.LEVEL`, which servers before 10.5.10 lack; there they are not seen, and an edit could drop one.

**Review (16bb646)**, import and export (each fix has its failing input as a test; live tests ran on mysql:8.4 and mariadb:11.4):
- Names written in `--` comment lines of a dump have every control character replaced by a space, so a table named with a line break cannot become a statement on restore (`a_name_in_a_comment_line_cannot_start_a_statement`, `live_a_table_name_with_a_line_break_cannot_drop_another_database`).
- A 0 in a mapped auto-increment key is generated by the server like NULL unless the session has NO_AUTO_VALUE_ON_ZERO, so it records no undo and says why (`live_a_zero_auto_increment_key_records_no_undo`).
- Script files are split as bytes and a statement that is not UTF-8 runs as `PREPARE ... FROM @var` with the bytes bound, so raw BLOB bytes from mysqldump arrive unchanged (`raw_bytes_reach_the_statement_unchanged`, `live_binary_values_written_raw_arrive_byte_for_byte`).
- Doubles are written with an exponent (shortest round trip), whole numbers under 1e15 stay plain, and the dump reads FLOAT columns as `col * 1e0` because the server prints FLOAT with six digits (`doubles_are_written_so_they_read_back_exactly`, `live_doubles_and_control_characters_survive_a_dump` checks CHECKSUM TABLE). Copy as SQL INSERT tests unchanged (their values are text).
- The engine of the table is read from information_schema.TABLES: MyISAM, Aria, MEMORY and the like get a warning before the import and an error or Cancel reports how many rows stayed (`live_an_engine_without_transactions_reports_the_rows_that_stay`).
- The dump connection runs with `sql_mode = ''` (kept for a filtered dump, whose WHERE is the user's text) and `sql_quote_show_create = 1`, so ANSI_QUOTES on the server no longer writes `"t"` (`live_a_server_in_ansi_quotes_mode_writes_a_dump_that_runs_back`).
- The dump reads at `time_zone = '+00:00'` and the file sets it and puts the old zone back at the end (`live_timestamps_keep_their_instant_in_another_zone`).
- Views are written after the views they select from, found from the backtick names after `AS` in each CREATE VIEW; a loop falls back to list order (`views_come_after_the_views_they_select_from`, `live_views_on_views_restore_in_dependency_order`).
- DELIMITER is recognized after leading `--`, `#` and `/* */` comments (`a_delimiter_line_after_comments_is_still_a_delimiter_line`).
- The splitter keeps its scan state between feeds (a 32 MB statement in 256 KB chunks is read once), caps a statement at the server's max_allowed_packet, and Cancel is checked while reading (`a_long_statement_in_many_chunks_is_read_once`, `a_statement_past_max_allowed_packet_is_an_error_not_more_memory`, `live_cancel_stops_a_file_whose_quote_never_closes`).
- Run SQL file passes the connection's lock to the worker instead of `false` (the worker's `writer` guard applies; covered by `live_a_script_stops_at_the_first_error_and_names_it`).
- String literals write NUL and Ctrl-Z as `\0` and `\Z` when backslash escapes are on, raw under NO_BACKSLASH_ESCAPES (`literals_escape_nul_and_ctrl_z_like_mysqldump`).
- The script Cancel tooltip says it interrupts the running statement; a bare DELIMITER line is an error and never sent (`a_bare_delimiter_line_is_an_error_and_never_sent`).
- Won't fix: a column or alias that shares a view's name can add an unneeded view order (harmless; a loop it makes falls back to list order). A non-UTF-8 statement that the server cannot prepare (rare: data statements are preparable) fails with a message that says why.

**Review (ee27114)**, create and drop, fixed on branch `create-fix` (each finding has its failing input as a test in `src/db/sql/create.rs` or `src/app/sql_create.rs`; live ones ran on mysql:8.4 and mariadb:11.4, the name folding also on mariadb:11.4 with --lower-case-table-names=1):
1. CREATE TABLE is built on the writer connection for the sql_mode its session reads (`@@SESSION.sql_mode` through ddl.rs `session` and `Mode`); a mode changed since connecting is handed back in `Msg::SqlDdl::sql_mode` so the preview follows it. Every create, drop, truncate and undo statement runs as one prepared statement (`ddl::exec`); LOCK TABLES and UNLOCK TABLES stay text because MySQL cannot prepare them (1295) and they hold only quoted names (`live_a_default_value_stays_text_when_the_sql_mode_changed_since_connecting`: `\'); DROP DATABASE victim; -- ` is stored as text and victim survives, `C:\temp` is stored exactly).
2. The drop dialog never calls an estimate empty: an estimate shows "about N rows", an estimate of 0 or none "row count unknown"; MySQL reads TABLE_ROWS with `information_schema_stats_expiry = 0` on a detached connection (`an_estimate_is_never_called_empty`, `live_a_locked_table_gets_a_fresh_estimate_in_about_3s_and_no_count_is_left`: cached 0, then 1000 rows inserted under another session's LOCK TABLES ... WRITE).
3. Views are not counted; a table's COUNT runs on a connection taken out of the pool and after 3 s that connection is KILLed and the answer waits until PROCESSLIST no longer lists it (`live_a_slow_view_is_not_counted` with a `SELECT SLEEP(25)` view, and the locked-table test above).
4. Drop, drop view and empty take any existing name that is not empty and has no control character (`a.b`, ` lead`, `x.y`); the path and space rules stay for new names (`existing_names_with_dots_slashes_or_spaces_can_be_dropped`, `live_existing_names_with_a_dot_or_a_leading_space_can_be_emptied_and_dropped`).
5. Undo of a database create opens the writer first and runs the emptiness check and the DROP on it (`live_undo_of_a_database_create_opens_the_writer_before_checking`); undo of a table create records CREATE_TIME at create time (`SqlUndo::CreatedTable { made }`) and compares it under the lock (`live_undo_of_a_table_create_refuses_a_table_made_again_since`).
6. The drop dialog warns when the database is the one in the connection's address; created names are mapped to what SHOW DATABASES and SHOW TABLES list before the sidebar, tabs and undo use them (`a_created_name_is_mapped_to_how_the_server_lists_it`, `live_a_created_name_comes_back_as_the_server_lists_it`); the system schema check folds case only when lower_case_table_names is not 0 (`a_system_name_in_other_case_is_a_user_database_where_case_counts`); "+ Column" is the painted `add_chip`.
Won't fix: TABLE_ID is not recorded (MySQL 8 keys INNODB_TABLES by the filename-encoded name), so a table dropped and made again by someone else within the same second as the create is not told apart; it is then just as empty. The database drop dialog still sums cached TABLE_ROWS (fresh statistics for every table of a big schema would hold the dialog), shown only as "about N rows" and never as empty.

**Review (49a4432, c5c77f1)**, relations and triggers, fixed on branch `reltrig-fix` (unit tests in `src/db/sql/ddl.rs`, `src/app/sql_trig.rs`, `src/app/sql_rel.rs` and `src/app/sql_alter.rs`; live ones in `ddl::live_rel::review`, `triggers` and `foreign_keys`, on MySQL 8.4 and MariaDB 11.4):
1. A trigger body is checked by `check_body`, which follows the server's grammar with the lexer under the session's quote rules: one plain statement, or one compound statement whose BEGIN, IF, CASE, LOOP, WHILE, REPEAT and FOR blocks (labels included) are tracked to the END that closes it, with only a `;` after it. Executable comments are refused, a keyword after a dot is a name, and where it cannot tell it refuses. The worker checks the whole CREATE again (`one_trigger_statement`) right before sending it (`nothing_runs_after_the_end_of_a_compound_body`, live: the reviewer's bodies, `BEGIN SET NEW.tag='new'; END; DELIMITER ;` changes nothing).
2. Undo and the MySQL recovery replay SHOW CREATE TRIGGER in the session the trigger was made in: its sql_mode, character set (kept only when the text can be sent as is) and collation are set on the writer and put back after (`replay`). The editor warns when the trigger's mode differs from the session's (live: a PIPES_AS_CONCAT trigger edited then undone still gives 'ab').
3. MariaDB edits and undoes with CREATE OR REPLACE (`restate`). MySQL first creates the new trigger under a scratch name, then drops and creates under `LOCK TABLES t WRITE`, puts the old one back if the CREATE still fails, and its undo does the same with the edited trigger as the fallback. A trigger answer reloads the structure even when it failed, and undo runs when the trigger is absent after a half-run one (live: an ANSI_QUOTES trigger survives a refused edit and an edit and undo; an undo whose old body the server refuses leaves the edited trigger).
4. Foreign key changes are ordinary `Change`s of the same `plan`: one prepared ALTER per undo step, the `snapshot` check (which now holds the touched foreign keys) and the typed confirmation of a losing undo; `sql_ddl` and its separate undo are gone (`a_batch_with_a_foreign_key_has_one_checked_undo`, live: staged [Drop column wrong, AddFk fk_mix] is refused when the key changed and undone per step when not).
5. Trigger order is read from ACTION_ORDER; an edit, an undo and an undone drop put the trigger back after the trigger before it or before the one after it (`the_order_is_found_from_the_neighbours`, live: rt_1 then rt_2 appending stays "12" through edit, undo, drop and undo).
6. An edited trigger keeps its DEFINER, shown in the preview; when the login lacks SET_USER_ID (or SUPER), the error says so and nothing changed (live with a definer qrf_owner@% and a login without the privilege).
7. MySQL's DROP and CREATE run under a write lock with UNLOCK TABLES on every way out (in 3); a login without LOCK TABLES is told why and nothing changes.
8. `SqlAltered` carries `alter`: only an ALTER answer clears the staged structure changes (`only_an_alter_clears_the_staged_changes`).
9. The orphan count is keyed on the columns and the referenced table only, so typing a name or picking an action asks nothing (`typing_a_name_does_not_count_orphans_again`).
10. The Add relation dialog reads `@@restrict_fk_on_non_standard_key` (on by default from MySQL 8.4 until it answers) and then wants a primary or unique key on exactly the referenced columns (`a_strict_server_needs_a_whole_unique_key`, live: the 6125 case is caught by the pre-check).
11. IF ... END IF without BEGIN and labelled blocks are taken (`every_kind_of_compound_body_is_taken`, live).
12. The worker reads `@@SESSION.sql_mode` on the writer for every trigger job, runs nothing when it differs from the mode the text was written for and hands the new mode back; ANSI_QUOTES is read from the exact mode names (`ansi_quotes_is_read_exactly`, live with a session in ANSI_QUOTES).
13. Opening the referenced row loads its tab once, never replaces a tab holding the person's filter, sort or clauses (it opens another), first lets the cell editor of the source tab finish, and refuses FLOAT and DOUBLE keys with a note, since the grid's rounded value cannot find the row (`a_float_key_gets_no_filter`, `a_tab_with_clauses_is_not_reused`).
14. The foreign key check is part of the exact `snapshot`: both sides are read from the same server, which spells table names the same way whatever lower_case_table_names is, so it needs no case folding. The trigger check compares definer, sql_mode and order too (`a_trigger_is_the_same_only_when_every_part_is`), and the draft of a dropped trigger is dropped (`a_dropped_trigger_takes_its_draft_along`).
Won't fix: a stored trigger is replayed as the server returns it without running `check_body` on it (the server parsed it as one statement in that same mode, and the check could refuse valid bodies it was never meant for). A client character set other than UTF-8 is not restored for a trigger whose text is not ASCII, since the text goes out as UTF-8; its collation, which decides what its strings are, is. On MariaDB a body naming a missing NEW column is only refused when the trigger runs, as the server does.

**Review (26b3c22)**, connection form Advanced tab, rebased onto feat/mysql as c79997f and fixed on branch `advanced-fix` (unit tests in `src/db/sql/advanced.rs` and `src/app/state.rs`; live ones in `advanced::live`, on MySQL 8.4 and MariaDB 11.4):
Decision on the init command: it is no longer "session SQL minus a deny-list". It accepts only SET statements whose every assignment is a user variable (`@name`) or a session variable on the allow-list `INIT_VARS`, with a value made of literals (numbers, single-quoted strings without backslashes, ON, OFF, DEFAULT, TRUE, FALSE, NULL) and arithmetic on them; no subqueries, functions, variable reads, executable comments, `SET STATEMENT`, GLOBAL, PERSIST or `@@global.`. The allow-list: group_concat_max_len, innodb_lock_wait_timeout, lock_wait_timeout, sql_safe_updates, max_execution_time, max_statement_time, wait_timeout, interactive_timeout, net_read_timeout, net_write_timeout, max_heap_table_size, tmp_table_size, sort_buffer_size, join_buffer_size, read_buffer_size, read_rnd_buffer_size, cte_max_recursion_depth, max_recursive_iterations, div_precision_increment, default_week_format, lc_time_names, max_sort_length, optimizer_search_depth, optimizer_prune_level, eq_range_index_dive_limit, long_query_time, profiling, transaction_isolation and tx_isolation. sql_select_limit, max_join_size and sql_big_selects were left out after the final review (a limit of 1 cut dumps and information_schema reads to one row). time_zone, sql_mode and the charset variables are refused with a pointer to their field; autocommit and completion_type are refused because a write would report success and be rolled back.
1. Bypass through readings: the text must lex the same under all eight readings (backslash escapes on and off, ANSI_QUOTES on and off, executable comments as code and as comment, `lex::every_reading`), and each statement is sent alone as a prepared statement (`sqlx::query(..).persistent(false)`), never as multi-statement text (`init_command_refuses_the_proven_bypasses`: the `/*!99999 ' */`, `/*M! ' */`, `/*!999999 ' */`, NO_BACKSLASH_ESCAPES and ANSI_QUOTES inputs; live `live_init_command_cannot_lift_the_guard_or_write`, and no quarry_probe_af2 database exists after).
2. Quoted variable names: names compare lower case after removing backticks and double quotes and the `@@session.`, `@@local.`, `SESSION` and `LOCAL` prefixes, against the allow-list (the four quoted read_only spellings in the same test). After the init command the setup sets READ ONLY again, forces `autocommit = 1` and reads `@@SESSION.transaction_read_only` (`tx_read_only` on old MariaDB) and autocommit back, failing the connect unless both are 1 (`live_session_setup_holds_without_the_init_check` runs the hostile texts through `session` directly: each fails or leaves the guard on).
3. Windows build: `TIME_ZONE_ID_DAYLIGHT` is not in `Win32::System::Time` of windows-sys 0.61.2, so it is a local const 2 with a comment; the arithmetic moved to `bias_offset_secs`, tested on every platform (`offsets`). The function was type-checked for x86_64-pc-windows-msvc in a scratch crate against windows-sys 0.61.2 (the whole crate cannot be, ring's C build fails on macOS).
4. Data loss with autocommit=0: refused by the allow-list, and forced on after the init command (`live_unlocked_insert_persists_with_autocommit_forced` installs `SET SESSION autocommit = 0` past the check; the unlocked INSERT is still there from another connection).
5. Charset variables are refused with "set the character set with the Charset field above" (`init_command_refuses_session_breaking_and_owned_variables`, live with character_set_results = latin1).
6. Fail fast: the setup runs once on a plain connection before the pool is built and its error, with the statement, is the connect error; a later `PoolTimedOut` is answered with the stored setup failure (`Db::acquire`, used by Query and the shell relay and count) (`live_failing_setup_fails_fast_with_its_text`: `group_concat_max_len = 'x'` and zone Nowhere/Atlantis fail in under 3 s with a 20 s timeout).
7. The query timeout follows Settings when Query is empty, not the Connect field (`Advanced::timeouts`, `timeouts`).
8. Value suggestions run the pool's session setup on their own connection, then put their 300 ms limit back (`live_field_values_follow_the_custom_zone`: a TIMESTAMP stored at 00:00 UTC is suggested as 05:00 with zone +05:00). `Db` carries the setup (`Db::setup`).
9. A Test answer carries its request id (`Msg::Tested::req`) and only the form whose last Test it answers takes it; both answers are cleared when a form opens or closes. Custom SQL mode starts from the server's mode once, tracked by `mode_seeded` (set by a saved Custom mode, a chip click or an edit of the free field), so a deliberate all-off mode survives the next Test; switching to Custom after a Test still starts from the server's mode (`a_test_answer_seeds_custom_mode_once_and_only_for_its_own_form`).
10. Offsets are limited to -13:59 .. +14:00 with minutes 00 to 59 (`zones`: +14:59 and -14:00 refused). The utf8mb3 option notes that characters outside the BMP come back as '?' and such rows cannot be edited in the grid.
Won't fix: the init command no longer takes SELECT, SHOW or DO (nothing it could do there matters to the session; kept out so the parser stays a SET parser). A PoolTimedOut from a statement that uses the pool directly (paging, schema reads) still says "pool timed out"; only acquire sites map it, and the connect itself fails fast. The fixes to the writer, dump and export connections (autocommit, timeout, SET NAMES, sql_select_limit) belong to the final review's fix and are not in this commit.

**Review (final, v2 whole branch)**, cross-item findings, fixed on branch `final-fix` (sessions that get odd settings from the server's globals are simulated with per-session pools, `testing::with_session`; live tests in `src/db/sql/review_final.rs` unless named otherwise, on mysql:8.4 and mariadb:11.4; each one fails without its fix):
1. Writers commit: `write::writer` and `worker::prepare` (Query, shell relay, run file through the writer, dumps and exports) run `SET SESSION autocommit = 1`, so a server whose global autocommit is 0 no longer reports a write done and rolls it back on close (`live_writers_commit_on_a_session_without_autocommit`: run_file, Query and relay INSERTs persist).
2. Dumps and exports pin charset and limits: the dump, the all-rows export and the script export run `SET NAMES utf8mb4` and `sql_select_limit = DEFAULT` (`worker::pin_reads`); the writer resets sql_select_limit too, and the information_schema reads of the structure, the page's key and types, the schema and the table list carry their own `LIMIT` (`structure::ALL_ROWS`), since they run on the pool (`live_dumps_exports_and_structure_ignore_the_session_charset_and_row_limit`: with utf8mb3, sql_select_limit = 1 and latin1 results, 'a\u{1F600}b', 'dé' and a 3-row table round-trip exactly, export has 3 rows, structure both columns).
3. Drops keep the right tab: `remove_tabs` keeps the current tab by position among the kept ones (as `close_tab`), used by drop database, drop table, a script's lists and the Mongo drops; Delete rows (both dialogs), Apply ALTER and Drop trigger carry the `Sel` they were raised for and run on the tab showing it through `on_tab`, or refuse when none does (`removing_a_tab_before_the_current_one_keeps_the_current_tab`, `a_confirmation_finds_its_table_after_the_tabs_moved` in `src/app/tabs.rs`).
4. The writer runs with the statement timeout at 0 (`max_statement_time` on MariaDB, `max_execution_time` on MySQL for consistency), so ALTER, trigger changes, create, drop and undo are not cut short (`live_an_alter_runs_past_the_statement_timeout`: under a 1 s limit an ALTER waits 2.5 s on a metadata lock and succeeds).
5. Row undo (edit, delete, insert, import) reads the table's primary key first and refuses, keeping the entry, when it is not the key it recorded (`import::tests::live_row_undo_is_refused_once_the_primary_key_changed`: ids 1-3 imported, key changed to (id, region), (2,'eu') inserted, undo refused and both id 2 rows remain; with the key put back the same undo runs).
6. A structure undo keeps its inverse changes (`AlterUndo::groups`) and writes its steps again for the session's sql_mode when it changed (`steps_for`); one that cannot be written for it is refused once with the reason and dropped (`an_undo_is_written_again_for_a_changed_sql_mode`, live `review_live::final_6_undo_after_the_sql_mode_changed`: a drop made in the default mode undone in a NO_BACKSLASH_ESCAPES session restores `C:\temp` and `it's \ here`). Trigger undo was not affected: it replays SHOW CREATE TRIGGER in the session the trigger was made in.
7. Drop database clears the cached schema, `sql_schema_asked` and looked up values (`forget_schema`), so autocomplete and shell types come back after a re-create. Checked by reading: App cannot be built in a unit test.
8. After a script, finished, failed or stopped, the app reads the database list and the tables of its database (`Worker::sql_lists`, `Msg::SqlLists`): the list updates, tabs of a database or table that is gone close, the others of its database reload, and its tables refresh (`tabs_of_what_a_script_dropped_close` in `src/app/sql_dump.rs`).
9. Copy as SQL, the filter to a value and key IN lists write a double plain between 1e-6 and 1e15 (Rust's shortest digits, at most 17 significant), with an exponent outside it and for -0 (`doubles_are_written_so_they_read_back_exactly`, live `live_plain_doubles_read_back_as_the_same_double`).
10. The shell relay reads `@@SESSION.sql_mode` on its own connection and fills values for it (`live_the_relay_fills_values_for_its_own_session_mode`: `C:\temp\'x` stored exactly in a NO_BACKSLASH_ESCAPES session).
Known limits: the Content bar's filter text and "filter to this value" splice literals with the sql_mode read at connect (as noted in Review (e993e30) 13); row writes bind their values and are not affected. Every pooled session now starts with autocommit on (`session()`), so a server with global autocommit off cannot hold reads in one snapshot. The key check of a row undo runs just before its transaction, not inside it.

**v3** (started 2026-11-10, decided with the user): users and privileges, the process list with Kill, server overview, status and variables, SQL statements in the Query log, Save to library and Format in Query.
- Kill (KILL QUERY, KILL connection in the row menu) needs only the two-step confirmation, also on a read-only connection, like Mongo's Operations tab (user's choice).
- Variables are view only.
- The Users tabs Account, Global and Limits and the Server tabs Overview and Status have no design: they are drawn first (both themes) and shown to the user before they are built; the designed parts (Users Databases tab, Processes, Variables, Query toolbar Format and Save to library) start meanwhile.
- Passwords (create user, password change) are never shown in a preview, never logged (the Query log redacts the string after IDENTIFIED BY, PASSWORD( and SET PASSWORD) and not kept in state after Apply.
- The rail Server button opened the Mongo Server window on SQL connections, where it spun forever; v3 opens the SQL one.

**v3 progress** (builders: A = log, Format, library on `v3-query`; B = server window on `v3-server`; C = users on `v3-users`):
- [ ] 1. SQL statements in the Query log, passwords redacted.
- [ ] 2. Format in Query (sqlformat, kept only when the lexer reads the same tokens), undoable.
- [ ] 3. Save to library in Query (`SavedQuery.sql`, per connection).
- [ ] 4. Server window for SQL: Processes with Kill, Variables; then Overview and Status after their design.
- [ ] 5. Users and privileges for SQL: Databases tab, grants preview, create and drop user; then Account, Global and Limits after their design.
- [x] 6. Designs for the missing tabs shown to the user and approved (2026-11-10, page "Server"): Users · Account 12879/12962, Users · Global 13047/13156, Users · Limits 13267/13324, Server · Overview 13383/13450, Server · Status 13519/13575.

**v3 definition of done:** as v2's: every item merged and ticked here; every item that writes, kills or redacts had an independent opus review with its findings fixed (failing input as a test) or marked won't fix with the reason; on the final head `cargo check --all-targets` clean, full live `cargo test` on mysql:8.4, mariadb:11.4 and Mongo with Bun, release build, `bench.py --quick` back to back with v2's head, screenshots of every new view in both themes against the designs (at a moment the user agrees to); one final whole-branch opus review; no worktrees, agent branches or test containers left; a v3 summary and known limits here.

## How this work is run

The plan and its progress list live in this file; update it whenever something lands or is decided. CLAUDE.md is not tracked by git here, so switches added for the SQL work are documented there locally and summarized in the Designs and Verification sections below.

- **Delegation:** each step goes to a subagent with `isolation: worktree`, model sonnet for building and opus for reviews of risky code (writes, the read-only guard, anything that builds SQL text). A brief is self-contained: the goal, what exists, file ownership so parallel agents do not touch the same files, the strict comment rules (no em dashes, emojis or arrows; explain why), no rustfmt, and the verify list (cargo check with no warnings, cargo test into a log file, live tests in Docker on a unique port, release build, screenshots read and compared with the designs).
- **Worktrees start at master,** so every brief tells the agent to run `git checkout -B <name> feat/mysql` first.
- **Merging:** one Conventional Commit per reviewed step on `feat/mysql`, local only, never pushed without the user. Cherry-pick or fast-forward the agent commit, then check `git ls-files | grep -E '\.(bak|orig|rej)$'` (agents committed .bak files three times; *.bak is now in .gitignore), run cargo check and cargo test, tick this file, commit.
- **Cleanup right after each merge:** `git worktree remove -f -f`, delete the agent branches, `docker rm -f` its test container. The user asked for this explicitly.
- **Reviews:** security-sensitive steps get an independent opus review before they are ticked; findings go to a fix agent with each failing input as a test.
- **Environment:** if cargo says sccache is missing, prefix with `RUSTC_WRAPPER=`. Many parallel worktrees saturate the machine (each builds its own target dir); keep at most three agents compiling at once.
- **Designs:** `docs/sql/designs.figcraft`; read a frame with `figcraft-cli export docs/sql/designs.figcraft -o /tmp/x.png --node "<frame name>"`. New screens are designed in FigCraft (Workbench and Paper frames side by side, real theme tokens) and shown to the user before they are built. The generator scripts that drew them were in a session scratchpad and are gone; draw new frames with the FigCraft MCP tools directly or rebuild a small generator.
- **Demo switches** for screenshots: `QUARRY_TEST_SQL=<view>` (content, rules, sqlmode, grouped, edit, select, colpick, structure, info, relations, triggers, addrel, trigedit, fkcell, query, shell, shellac, shellattach, attach, ac, acquery, coledit, stagebar, import, dump, newdb, newtable, droptable, processes, variables, overview, status), `QUARRY_TEST_SQL_JSON=<file>`, `QUARRY_TEST_RAIL=1`, `QUARRY_TEST_CHECKED=1`, `QUARRY_TEST_SHELL_SIG=1` (or `details`; the Mongo shell, or the SQL one with `QUARRY_TEST_SQL=shell`, with parameter hints for `map` and a long elided type above the caret and the suggestion list under it, `details` with the hints expanded and no list), `QUARRY_TEST_CONNFORM=mysql` with `QUARRY_TEST_CONNFORM_TAB=advanced` (the connection form on its Advanced tab with the design's example values; add `QUARRY_TEST_TALL=1` for a 1180 px tall window that shows the whole tab down to TIMEOUTS), `QUARRY_TEST_EXPORT=1` (or `script`, `script-ok` for the All statements scope); `shellattach` is the SQL shell tab with a read-only Mongo connection "travels" (db app) attached and a run that used both, `attach` is the Attach dialog with travels picked; `bench.py --sql-only` for memory and frame time.

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
- Review (c5761a5), fixed in one commit:
  1. Repeated connection names (including Mongo vs SQL and an attachment equal to main) fail the run in the runner, and `run_shell` refuses a job whose `sql` repeats a name before Bun starts.
  2. An invalid Date or one outside years 0 to 9999 becomes null in `walk`, so the result still decodes.
  3. `__proto__` keys survive `walk` (defineProperty instead of assignment).
  4. Command monitoring is on only when the run has more than one connection, so single-connection Mongo scripts are as fast as before.
  5. Queries panel text: `count` shows rows, multi-batch finds omit rows, failed sensitive commands show a message or "failed", only collection commands use a string as the collection (others show `db.<command>(...)`), and the 2000 char cut keeps the text well formed.
  6. The panel keeps the last 1000 queries but trims in batches (at 1500) instead of draining on every message.
  7. An attachment name that collides with a built-in, helper, package or main's collection or table fails the run naming it, whatever the input order.
  - Won't fix: script BigInts always come back as Int64, intended so values beyond 2^53 stay exact.

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
