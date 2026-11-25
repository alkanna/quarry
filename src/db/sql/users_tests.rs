use super::*;
use crate::db::sql::testing::every_server;

fn set(p: &[&str]) -> BTreeSet<String> {
    p.iter().map(|s| s.to_string()).collect()
}

#[test]
fn mysql_8_4_grant_lines() {
    let g = parse_grant("GRANT USAGE ON *.* TO `report`@`10.0.%`");
    assert_eq!(g.scope, Scope::Global);
    assert!(g.privs.is_empty());
    let g = parse_grant("GRANT SELECT, INSERT, UPDATE ON `shop`.* TO `report`@`10.0.%`");
    assert_eq!(g.scope, Scope::Db("shop".into()));
    assert_eq!(g.privs, ["SELECT", "INSERT", "UPDATE"]);
    let g = parse_grant("GRANT SELECT ON `my\\_db`.* TO `u`@`%` WITH GRANT OPTION");
    assert_eq!(g.scope, Scope::Db("my\\_db".into()));
    assert_eq!(g.privs, ["SELECT", "GRANT OPTION"]);
    let g = parse_grant("GRANT ALL PRIVILEGES ON `shop`.* TO `u`@`%`");
    assert_eq!(g.privs.len(), DB_PRIVS.len() - 1);
    assert!(!g.privs.contains(&"GRANT OPTION".to_string()));
    let g = parse_grant("GRANT CREATE TEMPORARY TABLES, LOCK TABLES ON `a``b`.* TO `u`@`%`");
    assert_eq!(g.scope, Scope::Db("a`b".into()));
    assert_eq!(g.privs, ["CREATE TEMPORARY TABLES", "LOCK TABLES"]);
}

#[test]
fn global_and_object_grants_are_not_database_rows() {
    let g = parse_grant("GRANT SELECT, PROCESS, REPLICATION CLIENT ON *.* TO `u`@`%`");
    assert_eq!(g.scope, Scope::Global);
    assert_eq!(g.privs, ["SELECT", "PROCESS", "REPLICATION CLIENT"]);
    assert!(!only_usage(&[g]));
    let g = parse_grant("GRANT SELECT, INSERT ON `shop`.`orders` TO `u`@`%`");
    assert_eq!(g.scope, Scope::Object("shop.orders".into()));
    let g = parse_grant("GRANT SELECT (`a`, `b`), UPDATE (`a`) ON `shop`.`orders` TO `u`@`%`");
    assert_eq!(g.scope, Scope::Object("shop.orders".into()));
    let g = parse_grant("GRANT EXECUTE ON PROCEDURE `shop`.`p` TO `u`@`%`");
    assert_eq!(g.scope, Scope::Object("shop.p".into()));
    let g = parse_grant("GRANT EXECUTE ON `shop`.* TO `u`@`%`");
    assert_eq!(g.scope, Scope::Db("shop".into()));
    assert_eq!(db_grants(&[g]).len(), 1);
}

#[test]
fn role_and_unknown_lines_are_kept_not_guessed() {
    let g = parse_grant("GRANT `reader`@`%`, `w`@`h` TO `u`@`%`");
    assert_eq!((g.scope, g.privs), (Scope::Role, vec!["reader@%".to_string(), "w@h".to_string()]));
    let g = parse_grant("GRANT `reader`, `writer` TO `u`@`%`");
    assert_eq!((g.scope, g.privs), (Scope::Role, vec!["reader".to_string(), "writer".to_string()]));
    for l in ["GRANT PROXY ON ''@'' TO `u`@`%`", "REVOKE SELECT ON `a`.`t` FROM `u`@`%`", "something else", "GRANT SELECT ON TO"] {
        let g = parse_grant(l);
        assert_eq!(g.scope, Scope::Other, "{l}");
        assert_eq!(g.raw, l);
    }
}

#[test]
fn mariadb_11_4_grant_lines() {
    let g = parse_grant("GRANT USAGE ON *.* TO `u`@`%` IDENTIFIED VIA mysql_native_password USING '*2470C0C06DEE42FD1618BB99005ADCA2EC9D1E19'");
    assert_eq!(g.scope, Scope::Global);
    assert!(g.privs.is_empty());
    let g = parse_grant("GRANT SELECT, DELETE HISTORY ON `shop`.* TO `u`@`%` IDENTIFIED BY PASSWORD '*ABC' WITH GRANT OPTION");
    assert_eq!(g.privs, ["SELECT", "GRANT OPTION", "DELETE HISTORY"]);
    let g = parse_grant("GRANT ALL PRIVILEGES ON `shop`.* TO `u`@`localhost`");
    assert_eq!(g.privs.len(), DB_PRIVS.len() - 1);
    assert_eq!(parse_grant("GRANT PROCESS, BINLOG MONITOR ON *.* TO `u`@`%`").privs, ["PROCESS", "REPLICATION CLIENT"]);
    let g = parse_grant("GRANT `r1` TO `u`@`%`");
    assert_eq!(g.scope, Scope::Role);
    // A string that says WITH GRANT OPTION is a hash, not the clause.
    let g = parse_grant("GRANT SELECT ON `shop`.* TO `u`@`%` IDENTIFIED BY PASSWORD 'GRANT OPTION'");
    assert_eq!(g.privs, ["SELECT"]);
}

#[test]
fn odd_names_parse_back_exactly() {
    let g = parse_grant("GRANT SELECT ON `we``ird\\_%db`.* TO `o'brien`@`h'ost`");
    assert_eq!(g.scope, Scope::Db("we`ird\\_%db".into()));
    let g = parse_grant("GRANT SELECT ON `100\\%_sure`.* TO `a`@`b`");
    assert_eq!(g.scope, Scope::Db("100\\%_sure".into()));
    assert_eq!(shown_pattern("my\\_db"), "my_db");
    assert_eq!(shown_pattern("50\\%"), "50%");
    assert_eq!(shown_pattern("shop_%"), "shop_%");
    assert_eq!(pattern_of("my_db%"), "my\\_db\\%");
    assert_eq!(shown_pattern(&pattern_of("my_db%")), "my_db%");
    // The statement writes the pattern back as it was read.
    let sql = grant_sql(Mode::DEFAULT, &Account::named("u", "%"), "we`ird\\_%db", &["SELECT".into()], false).unwrap();
    assert_eq!(sql, "GRANT SELECT ON `we``ird\\_%db`.* TO 'u'@'%'");
}

#[test]
fn db_rows_merge_and_keep_order() {
    let g = [parse_grant("GRANT SELECT ON `a`.* TO `u`@`%`"), parse_grant("GRANT INSERT ON `b`.* TO `u`@`%`"), parse_grant("GRANT DELETE ON `a`.* TO `u`@`%`"), parse_grant("GRANT USAGE ON *.* TO `u`@`%`")];
    assert_eq!(db_grants(&g), vec![("a".to_string(), set(&["SELECT", "DELETE"])), ("b".to_string(), set(&["INSERT"]))]);
}

#[test]
fn statements_and_inverses() {
    let u = Account::named("report", "10.0.%");
    let ed = diff("shop", &set(&["SELECT", "INSERT"]), &set(&["SELECT", "EXECUTE", "DELETE"]));
    assert_eq!(ed.grant, ["DELETE", "EXECUTE"]);
    assert_eq!(ed.revoke, ["INSERT"]);
    assert_eq!(edit_sql(Mode::DEFAULT, &u, &ed).unwrap(), ["GRANT DELETE, EXECUTE ON `shop`.* TO 'report'@'10.0.%'", "REVOKE INSERT ON `shop`.* FROM 'report'@'10.0.%'"]);
    let back = ed.inverse();
    assert_eq!(back.grant, ["INSERT"]);
    assert_eq!(back.revoke, ["DELETE", "EXECUTE"]);
    assert_eq!(back.inverse(), ed);
    assert_eq!(preview(Mode::DEFAULT, &u, &[Edit { db: "analytics".into(), grant: vec!["SELECT".into(), "EXECUTE".into()], revoke: vec![] }]).unwrap(), "GRANT SELECT, EXECUTE ON `analytics`.* TO 'report'@'10.0.%';");
    assert!(diff("x", &set(&["SELECT"]), &set(&["SELECT"])).is_empty());
    // A privilege this window does not know is left alone.
    assert!(diff("x", &set(&["SELECT", "DELETE HISTORY"]), &set(&["SELECT"])).is_empty());
}

#[test]
fn privilege_names_never_come_from_text() {
    let u = Account::named("u", "%");
    assert!(grant_sql(Mode::DEFAULT, &u, "db", &["SELECT ON *.* TO 'x'; --".into()], false).is_err());
    assert!(grant_sql(Mode::DEFAULT, &u, "db", &[], false).is_err());
    assert!(grant_sql(Mode::DEFAULT, &u, "d\0b", &["SELECT".into()], false).is_err());
}

#[test]
fn account_strings_follow_the_sql_mode() {
    let u = Account::named("o'b\\x", "h'");
    assert_eq!(u.literal(true), "'o''b\\\\x'@'h'''");
    assert_eq!(u.literal(false), "'o''b\\x'@'h'''");
    let sql = grant_sql(Mode { bs: false, ansi: true }, &u, "d", &["SELECT".into()], true).unwrap();
    assert_eq!(sql, "REVOKE SELECT ON `d`.* FROM 'o''b\\x'@'h'''");
    assert_eq!(Account { user: "r".into(), is_role: true, ..Default::default() }.literal(true), "'r'");
    assert_eq!(drop_user_sql(Mode::DEFAULT, &Account { user: "r".into(), is_role: true, ..Default::default() }, Dialect::MariaDb), "DROP ROLE 'r'");
    assert_eq!(drop_user_sql(Mode::DEFAULT, &Account::named("u", "h"), Dialect::MariaDb), "DROP USER 'u'@'h'");
}

#[test]
fn the_password_is_hidden_in_every_preview() {
    let u = Account::named("app", "%");
    let shown = create_user_sql(Mode::DEFAULT, &u, "s3cr'et", true);
    assert_eq!(shown, "CREATE USER 'app'@'%' IDENTIFIED BY '***'");
    assert!(!shown.contains("s3cr"));
    assert_eq!(create_user_sql(Mode::DEFAULT, &u, "s3cr'et", false), "CREATE USER 'app'@'%' IDENTIFIED BY 's3cr''et'");
    assert!(!scrub("syntax near 's3cr''et'".into(), "s3cr'et").contains("s3cr"));
    assert!(check_new_user("app", "%", "").is_err());
    assert!(check_new_user("", "%", "x").is_err());
    assert!(check_new_user("app", "", "x").is_err());
    assert!(check_new_user("a\nb", "%", "x").is_err());
    assert!(check_new_user("app", "%", "x").is_ok());
}

#[test]
fn labels_for_recent_changes() {
    let ed = [Edit { db: "my\\_db".into(), grant: vec!["SELECT".into()], revoke: vec![] }];
    assert_eq!(label_of(&ed), "granted select on my_db");
    assert_eq!(label_of(&[ed[0].inverse()]), "revoked select on my_db");
    assert_eq!(split_account("o@brien@%"), ("o@brien".to_string(), "%".to_string()));
}

#[test]
fn global_privileges_use_everything_as_the_scope() {
    let u = Account::named("report", "10.0.%");
    let ed = diff("", &set(&["SELECT", "SHOW VIEW", "REPLICATION CLIENT", "CONNECTION_ADMIN"]), &set(&["SELECT", "SHOW VIEW", "REPLICATION CLIENT", "CONNECTION_ADMIN", "PROCESS", "SYSTEM_VARIABLES_ADMIN"]));
    assert_eq!(ed.grant, ["PROCESS", "SYSTEM_VARIABLES_ADMIN"]);
    assert!(ed.revoke.is_empty());
    assert_eq!(edit_sql(Mode::DEFAULT, &u, &ed).unwrap(), ["GRANT PROCESS, SYSTEM_VARIABLES_ADMIN ON *.* TO 'report'@'10.0.%'"]);
    assert_eq!(edit_sql(Mode::DEFAULT, &u, &ed.inverse()).unwrap(), ["REVOKE PROCESS, SYSTEM_VARIABLES_ADMIN ON *.* FROM 'report'@'10.0.%'"]);
    // A privilege that is not on the design's list or a dynamic name is never written.
    assert!(grant_sql(Mode::DEFAULT, &u, "", &["FILE".into()], false).is_err());
    assert!(grant_sql(Mode::DEFAULT, &u, "", &["X_ADMIN ON *.* TO 'a'; --".into()], false).is_err());
    assert!(grant_sql(Mode::DEFAULT, &u, "", &["lower_case".into()], false).is_err());
    assert!(grant_sql(Mode::DEFAULT, &u, "", &["ADMIN".into()], false).is_err());
    assert!(is_dynamic("CONNECTION_ADMIN") && is_dynamic("BINLOG_ADMIN") && !is_dynamic("SELECT") && !is_dynamic("A_B;"));
    // A database privilege name is not valid as a global one by being dynamic-looking.
    assert!(grant_sql(Mode::DEFAULT, &u, "db", &["CONNECTION_ADMIN".into()], false).is_err());
    let g = parse_grant("GRANT SELECT, PROCESS ON *.* TO `u`@`%`");
    let h = parse_grant("GRANT CONNECTION_ADMIN, SYSTEM_VARIABLES_ADMIN ON *.* TO `u`@`%` WITH GRANT OPTION");
    // A grant option on a line of dynamic privileges belongs to those, not to everything.
    assert_eq!(global_privs(&[g.clone(), h.clone()]), set(&["SELECT", "PROCESS", "CONNECTION_ADMIN", "SYSTEM_VARIABLES_ADMIN"]));
    assert_eq!(held(&[g, h.clone()], ""), ["SELECT", "PROCESS", "CONNECTION_ADMIN", "SYSTEM_VARIABLES_ADMIN"]);
    assert_eq!(dynamic_with_grant(&[h]), set(&["CONNECTION_ADMIN", "SYSTEM_VARIABLES_ADMIN"]));
    let all = parse_grant("GRANT ALL PRIVILEGES ON *.* TO `u`@`%` WITH GRANT OPTION");
    assert!(all.privs.contains(&"SUPER".to_string()) && all.privs.contains(&"GRANT OPTION".to_string()));
}

#[test]
fn alter_user_statements_and_inverses() {
    let u = Account::named("report", "10.0.%");
    let base = Settings::default();
    let locked = Settings { locked: true, ..base.clone() };
    assert_eq!(alter_sql(Mode::DEFAULT, &u, &base, &locked).unwrap().unwrap(), "ALTER USER 'report'@'10.0.%' ACCOUNT LOCK");
    assert_eq!(alter_sql(Mode::DEFAULT, &u, &locked, &base).unwrap().unwrap(), "ALTER USER 'report'@'10.0.%' ACCOUNT UNLOCK");
    assert_eq!(alter_sql(Mode::DEFAULT, &u, &base, &base).unwrap(), None);
    let lim = Settings { limits: [5000, 0, 0, 3], ..base.clone() };
    assert_eq!(alter_sql(Mode::DEFAULT, &u, &base, &lim).unwrap().unwrap(), "ALTER USER 'report'@'10.0.%' WITH MAX_QUERIES_PER_HOUR 5000 MAX_USER_CONNECTIONS 3");
    assert_eq!(alter_sql(Mode::DEFAULT, &u, &lim, &base).unwrap().unwrap(), "ALTER USER 'report'@'10.0.%' WITH MAX_QUERIES_PER_HOUR 0 MAX_USER_CONNECTIONS 0");
    let all = Settings { locked: true, expire: Expire::Days(90), tls: Tls::X509, limits: [1, 2, 3, 4], ..Default::default() };
    assert_eq!(
        alter_sql(Mode::DEFAULT, &u, &base, &all).unwrap().unwrap(),
        "ALTER USER 'report'@'10.0.%' REQUIRE X509 WITH MAX_QUERIES_PER_HOUR 1 MAX_UPDATES_PER_HOUR 2 MAX_CONNECTIONS_PER_HOUR 3 MAX_USER_CONNECTIONS 4 PASSWORD EXPIRE INTERVAL 90 DAY ACCOUNT LOCK"
    );
    let never = Settings { expire: Expire::Never, ..base.clone() };
    assert_eq!(alter_sql(Mode::DEFAULT, &u, &base, &never).unwrap().unwrap(), "ALTER USER 'report'@'10.0.%' PASSWORD EXPIRE NEVER");
    assert_eq!(alter_sql(Mode::DEFAULT, &u, &never, &base).unwrap().unwrap(), "ALTER USER 'report'@'10.0.%' PASSWORD EXPIRE DEFAULT");
    assert!(alter_sql(Mode::DEFAULT, &u, &base, &Settings { expire: Expire::Days(0), ..base.clone() }).is_err());
    assert!(alter_sql(Mode::DEFAULT, &u, &base, &Settings { expire: Expire::Days(70000), ..base.clone() }).is_err());
    assert!(alter_sql(Mode::DEFAULT, &u, &base, &Settings { limits: [LIMIT_MAX + 1, 0, 0, 0], ..base.clone() }).is_err());
    assert!(alter_sql(Mode::DEFAULT, &u, &base, &Settings { tls: Tls::Specified, ..base.clone() }).is_err());
    // A requirement this window cannot write is never touched when nothing else about TLS changed.
    let spec = Settings { tls: Tls::Specified, ..base.clone() };
    assert_eq!(alter_sql(Mode::DEFAULT, &u, &spec, &Settings { locked: true, ..spec.clone() }).unwrap().unwrap(), "ALTER USER 'report'@'10.0.%' ACCOUNT LOCK");
}

#[test]
fn password_statements_hide_the_password_and_take_plugins_from_a_list() {
    let u = Account::named("a", "%");
    assert_eq!(password_sql(Mode::DEFAULT, Dialect::MySql, &u, None, "p'w", true).unwrap(), "ALTER USER 'a'@'%' IDENTIFIED BY '***'");
    assert_eq!(password_sql(Mode::DEFAULT, Dialect::MySql, &u, Some("mysql_native_password"), "p'w", false).unwrap(), "ALTER USER 'a'@'%' IDENTIFIED WITH mysql_native_password BY 'p''w'");
    assert!(password_sql(Mode::DEFAULT, Dialect::MySql, &u, Some("x' BY 'y"), "p", true).is_err());
    // MariaDB has no plugin clause here.
    // MariaDB's ALTER USER ... IDENTIFIED BY would replace every way of logging in, SET PASSWORD keeps the others.
    assert_eq!(password_sql(Mode::DEFAULT, Dialect::MariaDb, &u, Some("mysql_native_password"), "pw", true).unwrap(), "SET PASSWORD FOR 'a'@'%' = PASSWORD('***')");
    assert_eq!(password_sql(Mode::DEFAULT, Dialect::MariaDb, &u, None, "p'w", false).unwrap(), "SET PASSWORD FOR 'a'@'%' = PASSWORD('p''w')");
    assert!(check_password("").is_err() && check_password("a\0b").is_err() && check_password("x").is_ok());
    let shown = password_sql(Mode::DEFAULT, Dialect::MySql, &u, Some("caching_sha2_password"), "hunter2", true).unwrap();
    assert!(!shown.contains("hunter2"));
}

#[test]
fn a_plan_runs_in_order_on_the_new_name() {
    let u = Account::named("report", "10.0.%");
    let plan = AccountPlan {
        rename: Some(Account::named("reporting", "10.0.%")),
        settings: Some((Settings::default(), Settings { locked: true, ..Default::default() })),
        roles_grant: vec!["app_read@%".into()],
        roles_revoke: vec!["old".into()],
    };
    assert_eq!(
        plan_sql(Mode::DEFAULT, Dialect::MySql, &u, &plan).unwrap(),
        ["RENAME USER 'report'@'10.0.%' TO 'reporting'@'10.0.%'", "ALTER USER 'reporting'@'10.0.%' ACCOUNT LOCK", "REVOKE 'old' FROM 'reporting'@'10.0.%'", "GRANT 'app_read'@'%' TO 'reporting'@'10.0.%'"]
    );
    assert_eq!(plan_sql(Mode::DEFAULT, Dialect::MariaDb, &Account::named("u", ""), &AccountPlan { roles_grant: vec!["r".into()], ..Default::default() }).unwrap(), ["GRANT 'r' TO 'u'@''"]);
    let role = Account { user: "u".into(), is_role: true, ..Default::default() };
    assert_eq!(plan_sql(Mode::DEFAULT, Dialect::MariaDb, &role, &AccountPlan { roles_grant: vec!["r".into()], ..Default::default() }).unwrap(), ["GRANT 'r' TO 'u'"]);
    assert!(AccountPlan::default().is_empty());
    assert!(plan_sql(Mode::DEFAULT, Dialect::MySql, &u, &AccountPlan { rename: Some(Account::named("", "%")), ..Default::default() }).is_err());
    assert!(role_sql(Mode::DEFAULT, Dialect::MySql, &u, "", false).is_err());
    let hostile = role_sql(Mode::DEFAULT, Dialect::MySql, &u, "r'; DROP USER x; --@%", false).unwrap();
    assert_eq!(hostile, "GRANT 'r''; DROP USER x; --'@'%' TO 'report'@'10.0.%'");
    assert_eq!(role_shown("reader@%"), "reader");
    assert_eq!(role_shown("w@h"), "w@h");
}

#[test]
fn what_each_version_supports() {
    let c = Caps::of(Dialect::MySql, "8.4.3");
    assert!(c.alter_user && c.lock && c.lifetime && c.roles && c.dynamic && c.plugin_choice);
    let c = Caps::of(Dialect::MySql, "5.7.5");
    assert!(!c.alter_user && !c.lock && c.lifetime && !c.roles && !c.dynamic);
    let c = Caps::of(Dialect::MySql, "5.7.3");
    assert!(!c.lifetime);
    let c = Caps::of(Dialect::MySql, "5.7.8-log");
    assert!(c.alter_user && c.lock && c.lifetime && !c.roles);
    let c = Caps::of(Dialect::MariaDb, "11.4.5-MariaDB");
    assert!(c.alter_user && c.lock && c.lifetime && c.roles && !c.dynamic && !c.plugin_choice);
    let c = Caps::of(Dialect::MariaDb, "5.5.5-10.3.39-MariaDB");
    assert!(c.alter_user && !c.lock && !c.lifetime && c.roles);
    let c = Caps::of(Dialect::MariaDb, "10.4.2-MariaDB");
    assert!(c.lock && !c.lifetime);
}

#[test]
fn password_lifetimes_read_in_both_servers_forms() {
    assert_eq!(lifetime_of(None), Expire::Default);
    assert_eq!(lifetime_of(Some("0")), Expire::Never);
    assert_eq!(lifetime_of(Some("90")), Expire::Days(90));
    assert_eq!(lifetime_of(Some("-1")), Expire::Default);
    assert_eq!(lifetime_of(Some("junk")), Expire::Default);
}

#[test]
fn partial_revokes_are_read_and_a_global_revoke_over_them_is_refused() {
    let g = [parse_grant("GRANT SELECT, INSERT ON *.* TO `u`@`%`"), parse_grant("REVOKE SELECT ON `mysql`.* FROM `u`@`%`"), parse_grant("REVOKE INSERT, UPDATE ON `sys`.* FROM `u`@`%`")];
    assert_eq!(g[1].scope, Scope::Revoked("mysql".into()));
    assert_eq!(g[1].privs, ["SELECT"]);
    assert_eq!(held(&g, ""), ["SELECT", "INSERT", "-INSERT on sys", "-SELECT on mysql", "-UPDATE on sys"]);
    let ed = |p: &str| Edit { db: String::new(), grant: vec![], revoke: vec![p.into()] };
    let err = check_partial(&g, &[ed("SELECT")]).unwrap_err();
    assert!(err.contains("mysql") && err.contains("partial revokes"), "{err}");
    assert!(check_partial(&g, &[ed("INSERT")]).unwrap_err().contains("sys"));
    assert!(check_partial(&g, &[ed("DELETE")]).is_ok());
    // A database-level edit is not a global revoke.
    assert!(check_partial(&g, &[Edit { db: "mysql".into(), grant: vec![], revoke: vec!["SELECT".into()] }]).is_ok());
    assert!(preview_checked(Mode::DEFAULT, &Account::named("u", "%"), &[ed("SELECT")], &g).is_err());
    // Roles and unreadable lines are not partial revokes.
    assert_eq!(parse_grant("REVOKE SELECT ON `a`.`t` FROM `u`@`%`").scope, Scope::Other);
}

#[test]
fn ansi_quoted_grant_lines_read_like_backtick_ones() {
    let g = parse_grant("GRANT SELECT, INSERT ON \"shop\".* TO \"report\"@\"10.0.%\"");
    assert_eq!(g.scope, Scope::Db("shop".into()));
    assert_eq!(g.privs, ["SELECT", "INSERT"]);
    let g = parse_grant("GRANT SELECT ON \"my\\_db\".* TO \"u\"@\"%\" WITH GRANT OPTION");
    assert_eq!(g.scope, Scope::Db("my\\_db".into()));
    assert_eq!(g.privs, ["SELECT", "GRANT OPTION"]);
    assert_eq!(parse_grant("GRANT SELECT ON \"a\"\"b\".* TO \"u\"@\"%\"").scope, Scope::Db("a\"b".into()));
    assert_eq!(parse_grant("GRANT USAGE ON *.* TO \"u\"@\"%\" IDENTIFIED BY PASSWORD '*AB'").scope, Scope::Global);
    assert_eq!(parse_grant("GRANT \"r\"@\"%\" TO \"u\"@\"%\"").privs, ["r@%"]);
}

#[test]
fn a_server_that_folds_case_is_followed_or_no_undo_is_recorded() {
    let g = [parse_grant("GRANT SELECT ON `shop`.* TO `u`@`%`")];
    let mut eds = [Edit { db: "Shop".into(), grant: vec!["SELECT".into()], revoke: vec![] }];
    let snap = fold_edits(&g, &mut eds).unwrap();
    assert_eq!(eds[0].db, "shop");
    assert_eq!(snap, vec![("shop".to_string(), vec!["SELECT".to_string()])]);
    let mut nope = [Edit { db: "Other".into(), grant: vec!["SELECT".into()], revoke: vec![] }];
    assert!(fold_edits(&g, &mut nope).is_err());
    // A revoke of everything leaves no row, which is not a miss.
    let mut gone = [Edit { db: "Other".into(), grant: vec![], revoke: vec!["SELECT".into()] }];
    assert_eq!(fold_edits(&[], &mut gone).unwrap(), vec![("Other".to_string(), Vec::<String>::new())]);
    // Two stored names that differ only by case cannot be told apart.
    let two = [parse_grant("GRANT SELECT ON `ab`.* TO `u`@`%`"), parse_grant("GRANT SELECT ON `aB`.* TO `u`@`%`")];
    let mut amb = [Edit { db: "AB".into(), grant: vec!["INSERT".into()], revoke: vec![] }];
    assert!(fold_edits(&two, &mut amb).is_err());
}

/// Review finding: an undo took away what the account held before the change. Under lower_case_table_names 1
/// a row typed `Shop` granted SELECT, INSERT on the stored `shop`, which held SELECT already, and the undo, the
/// plain inverse, revoked that SELECT too. A stale window and another admin's grants did the same.
#[test]
fn edits_are_cut_to_what_changes_so_undo_never_takes_what_was_held() {
    let g = [parse_grant("GRANT SELECT ON `shop`.* TO `u`@`%`"), parse_grant("GRANT RELOAD ON *.* TO `u`@`%`")];
    let ask = [Edit { db: "Shop".into(), grant: vec!["SELECT".into(), "INSERT".into()], revoke: vec![] }];
    let (kept, left) = trim_edits(&g, &ask, true).unwrap();
    assert_eq!(kept, vec![Edit { db: "shop".into(), grant: vec!["INSERT".into()], revoke: vec![] }]);
    assert_eq!(left, vec![Edit { db: "shop".into(), grant: vec!["SELECT".into()], revoke: vec![] }]);
    assert_eq!(kept[0].inverse().revoke, vec!["INSERT".to_string()]);
    // Without folding `Shop` is another database, so nothing it asks for is held.
    let (kept, left) = trim_edits(&g, &ask, false).unwrap();
    assert_eq!(kept, ask.to_vec());
    assert!(left.is_empty());
    // A stale window: a revoke of what is gone and a grant of what another admin gave meanwhile.
    let stale = [Edit { db: "shop".into(), grant: vec![], revoke: vec!["SELECT".into(), "DELETE".into()] }, Edit { db: String::new(), grant: vec!["RELOAD".into(), "PROCESS".into()], revoke: vec!["SUPER".into()] }];
    let (kept, left) = trim_edits(&g, &stale, false).unwrap();
    assert_eq!(kept, vec![Edit { db: "shop".into(), grant: vec![], revoke: vec!["SELECT".into()] }, Edit { db: String::new(), grant: vec!["PROCESS".into()], revoke: vec![] }]);
    assert_eq!(left_note(&left), " Already held, so not granted again: reload on *.*. Not held, so not revoked: delete on shop; super on *.*.");
    let (kept, _) = trim_edits(&g, &[Edit { db: "shop".into(), grant: vec!["SELECT".into()], revoke: vec![] }], false).unwrap();
    assert!(kept.is_empty());
    // Two edits of one database: the second sees what the first will have done.
    let twice = [Edit { db: "Shop".into(), grant: vec!["INSERT".into()], revoke: vec![] }, Edit { db: "shop".into(), grant: vec!["INSERT".into()], revoke: vec!["SELECT".into()] }];
    let (kept, _) = trim_edits(&g, &twice, true).unwrap();
    assert_eq!(kept, vec![Edit { db: "shop".into(), grant: vec!["INSERT".into()], revoke: vec![] }, Edit { db: "shop".into(), grant: vec![], revoke: vec!["SELECT".into()] }]);
    // Two stored spellings the server does not tell apart are refused before anything runs.
    let two = [parse_grant("GRANT SELECT ON `ab`.* TO `u`@`%`"), parse_grant("GRANT SELECT ON `aB`.* TO `u`@`%`")];
    assert!(trim_edits(&two, &[Edit { db: "AB".into(), grant: vec!["INSERT".into()], revoke: vec![] }], true).is_err());
}

/// The same finding for roles: giving a role the account had already recorded an undo that took it away.
#[test]
fn roles_already_given_are_not_given_again_nor_missing_ones_taken() {
    let held = ["reader@%".to_string(), "ops@localhost".to_string()];
    let (g, r, left) = trim_roles(&held, &["reader@%".into(), "writer@%".into()], &["ops@LOCALHOST".into(), "gone@%".into()]);
    assert_eq!(g, vec!["writer@%".to_string()]);
    assert_eq!(r, vec!["ops@localhost".to_string()]);
    assert_eq!(left, vec!["reader@%".to_string(), "gone@%".to_string()]);
    // A bare MySQL name is the role at host %.
    let (_, r, _) = trim_roles(&held, &[], &["reader".into()]);
    assert_eq!(r, vec!["reader@%".to_string()]);
    // MariaDB names a role without a host, compared as written.
    let (g, r, _) = trim_roles(&["Reader".into()], &["reader".into()], &["Reader".into()]);
    assert_eq!((g, r), (vec!["reader".to_string()], vec!["Reader".to_string()]));
}

#[test]
fn grant_option_stays_with_its_line() {
    let st = parse_grant("GRANT SELECT ON *.* TO `u`@`%` WITH GRANT OPTION");
    assert_eq!(st.privs, ["SELECT", "GRANT OPTION"]);
    let dy = parse_grant("GRANT CONNECTION_ADMIN ON *.* TO `u`@`%` WITH GRANT OPTION");
    assert_eq!(dy.privs, ["CONNECTION_ADMIN"]);
    assert!(dy.with_grant);
    let only = parse_grant("GRANT USAGE ON *.* TO `u`@`%` WITH GRANT OPTION");
    assert_eq!(only.privs, ["GRANT OPTION"]);
    assert_eq!(dynamic_with_grant(&[st.clone(), dy]), set(&["CONNECTION_ADMIN"]));
    assert!(dynamic_with_grant(&[st]).is_empty());
}

#[test]
fn role_grants_carry_the_admin_option() {
    let g = [parse_grant("GRANT `a`@`%` TO `u`@`%` WITH ADMIN OPTION"), parse_grant("GRANT `b`@`%` TO `u`@`%`")];
    assert!(g[0].with_admin && !g[1].with_admin);
    assert_eq!(roles_with_admin(&g), set(&["a@%"]));
    assert_eq!(roles_state(&g, &["b@%".to_string()]), ["a@% +admin", "b@% +default"]);
    let u = Account::named("u", "%");
    assert_eq!(role_grant_sql(Mode::DEFAULT, Dialect::MySql, &u, "a@%", false, true).unwrap(), "GRANT 'a'@'%' TO 'u'@'%' WITH ADMIN OPTION");
    assert_eq!(role_grant_sql(Mode::DEFAULT, Dialect::MariaDb, &u, "a", false, true).unwrap(), "GRANT 'a' TO 'u'@'%' WITH ADMIN OPTION");
    assert_eq!(default_roles_sql(Mode::DEFAULT, Dialect::MySql, &u, &["a@%".into(), "b@%".into()]).unwrap().unwrap(), "SET DEFAULT ROLE 'a'@'%', 'b'@'%' TO 'u'@'%'");
    assert_eq!(default_roles_sql(Mode::DEFAULT, Dialect::MariaDb, &u, &["a".into()]).unwrap().unwrap(), "SET DEFAULT ROLE 'a' FOR 'u'@'%'");
    assert_eq!(default_roles_sql(Mode::DEFAULT, Dialect::MySql, &u, &[]).unwrap(), None);
}

#[test]
fn tls_requirements_are_written_back_exactly() {
    let u = Account::named("u", "%");
    let spec = TlsSpec { cipher: "AES256-SHA".into(), issuer: "/CN=ca'x".into(), subject: "/CN=me".into() };
    let held = Settings { tls: Tls::Specified, spec: spec.clone(), ..Default::default() };
    let none = Settings::default();
    assert_eq!(alter_sql(Mode::DEFAULT, &u, &held, &none).unwrap().unwrap(), "ALTER USER 'u'@'%' REQUIRE NONE");
    assert_eq!(alter_sql(Mode::DEFAULT, &u, &none, &held).unwrap().unwrap(), "ALTER USER 'u'@'%' REQUIRE SUBJECT '/CN=me' AND ISSUER '/CN=ca''x' AND CIPHER 'AES256-SHA'");
    // Only the cipher: the others are not required.
    let c = Settings { tls: Tls::Specified, spec: TlsSpec { cipher: "X".into(), ..Default::default() }, ..Default::default() };
    assert_eq!(alter_sql(Mode::DEFAULT, &u, &none, &c).unwrap().unwrap(), "ALTER USER 'u'@'%' REQUIRE CIPHER 'X'");
    // A changed subject under the same kind of requirement is a change.
    let d = Settings { spec: TlsSpec { subject: "/CN=you".into(), ..spec.clone() }, ..held.clone() };
    assert!(alter_sql(Mode::DEFAULT, &u, &held, &d).unwrap().unwrap().contains("SUBJECT '/CN=you'"));
    assert_eq!(alter_sql(Mode::DEFAULT, &u, &held, &held).unwrap(), None);
}

#[test]
fn errors_lose_the_password_in_every_quoted_form() {
    for pw in ["o'k", "a\\b", "plain-secret", "x'y\\z"] {
        for echoed in [pw.to_string(), lit(pw, true), lit(pw, false), pw.replace('\'', "''"), pw.replace('\\', "\\\\")] {
            let e = scrub(format!("Access denied for {echoed} in a statement"), pw);
            assert!(!e.contains(pw) && !e.contains(&echoed), "{pw}: {e}");
        }
    }
    let e = scrub("You have an error in your SQL syntax; check the manual near ''secret'' at line 1".into(), "secret");
    assert!(!e.contains("secret") && e.contains("hidden"), "{e}");
}

#[test]
fn a_user_without_a_host_is_not_a_role() {
    let u = Account::named("u", "");
    assert_eq!(u.literal(true), "'u'@''");
    assert_eq!(u.label(), "u@");
    let r = Account { user: "r".into(), is_role: true, ..Default::default() };
    assert_eq!((r.literal(true).as_str(), r.label().as_str()), ("'r'", "r"));
    assert_eq!(drop_user_sql(Mode::DEFAULT, &u, Dialect::MariaDb), "DROP USER 'u'@''");
    assert_eq!(drop_user_sql(Mode::DEFAULT, &r, Dialect::MariaDb), "DROP ROLE 'r'");
}

#[test]
fn plugin_choices_follow_the_version() {
    let c = Caps::of(Dialect::MySql, "8.4.3");
    assert_eq!(c.plugins("caching_sha2_password"), ["caching_sha2_password", "sha256_password"]);
    assert_eq!(c.plugins("mysql_native_password"), ["caching_sha2_password", "mysql_native_password", "sha256_password"]);
    assert_eq!(Caps::of(Dialect::MySql, "8.0.36").plugins(""), ["caching_sha2_password", "mysql_native_password", "sha256_password"]);
    assert_eq!(Caps::of(Dialect::MySql, "5.7.44").plugins(""), ["mysql_native_password", "sha256_password"]);
    assert_eq!(Caps::of(Dialect::MySql, "9.1.0").plugins("caching_sha2_password"), ["caching_sha2_password"]);
}

// Live tests. Each makes a user of its own with a unique name and drops it at the end.

fn unique(tag: &str) -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("qu_{tag}_{:x}", n & 0xffff_ffff_ffff)
}

fn done(l: &crate::db::sql::testing::Live) -> (Account, Res<String>, Option<UndoAction>) {
    match l.rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() {
        Msg::SqlUserDone { account, res, undo, .. } => (account, res, undo.into_iter().next()),
        _ => panic!("expected SqlUserDone"),
    }
}

fn grants(l: &crate::db::sql::testing::Live, a: &Account) -> Vec<String> {
    l.w.rt.block_on(async {
        let sql = format!("SHOW GRANTS FOR {}", a.literal(true));
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql.as_str())).fetch_all(&l.pool).await.unwrap().iter().map(|r| super::super::worker::text_at(r, 0).unwrap_or_default()).collect()
    })
}

fn create(l: &crate::db::sql::testing::Live, a: &Account) -> Option<UndoAction> {
    l.w.sql_user_create(1, l.db.clone(), a.clone(), "pw-for-tests".into(), false);
    let (_, res, undo) = done(l);
    res.unwrap();
    undo
}

fn drop_it(l: &crate::db::sql::testing::Live, a: &Account) {
    l.w.rt.block_on(async { sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP USER IF EXISTS {}", a.literal(true)))).execute(&l.pool).await.unwrap() });
}

#[test]
fn live_grant_then_revoke_leaves_show_grants_as_before() {
    every_server(|l, var| {
        let a = Account::named(&unique("rt"), "%");
        create(l, &a);
        let before = grants(l, &a);
        let ed = Edit { db: pattern_of("shop_x"), grant: vec!["SELECT".into(), "INSERT".into(), "EXECUTE".into(), "GRANT OPTION".into()], revoke: vec![] };
        l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![ed.clone()], false);
        let (_, res, undo) = done(l);
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        let mid = grants(l, &a);
        let parsed: Vec<Grant> = mid.iter().map(|g| parse_grant(g)).collect();
        let rows = db_grants(&parsed);
        assert_eq!(rows.len(), 1, "{var}: {mid:?}");
        assert_eq!(shown_pattern(&rows[0].0), "shop_x", "{var}: {mid:?}");
        assert_eq!(rows[0].1, set(&["SELECT", "INSERT", "EXECUTE", "GRANT OPTION"]), "{var}: {mid:?}");
        // Undo is the exact inverse and passes its check.
        let undo = undo.expect("an undo entry");
        l.w.sql_undo(l.db.clone(), undo, Vec::new(), false);
        let (_, res, _) = done(l);
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(grants(l, &a), before, "{var}");
        // The inverse written by hand does the same.
        l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![ed.clone()], false);
        let (_, res, _) = done(l);
        res.unwrap();
        l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![ed.inverse()], false);
        let (_, res, _) = done(l);
        res.unwrap();
        assert_eq!(grants(l, &a), before, "{var}");
        drop_it(l, &a);
    });
}

#[test]
fn live_undo_refuses_when_the_grants_changed_since() {
    every_server(|l, var| {
        let a = Account::named(&unique("ch"), "%");
        create(l, &a);
        let ed = Edit { db: pattern_of("shop_y"), grant: vec!["SELECT".into()], revoke: vec![] };
        l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![ed], false);
        let (_, res, undo) = done(l);
        res.unwrap();
        l.sql(&format!("GRANT DELETE ON `shop\\_y`.* TO {}", a.literal(true)));
        l.w.sql_undo(l.db.clone(), undo.unwrap(), Vec::new(), false);
        let (_, res, back) = done(l);
        let err = res.unwrap_err();
        assert!(err.starts_with("Undo refused"), "{var}: {err}");
        assert!(back.is_some(), "{var}: the entry stays");
        assert!(grants(l, &a).iter().any(|g| g.contains("DELETE")), "{var}");
        drop_it(l, &a);
    });
}

/// Review finding: a window that read the grants before another admin changed them granted what the account
/// held already, and the undo then took that away. Only what changed runs, so the undo leaves it.
#[test]
fn live_undo_keeps_what_the_account_held_before() {
    every_server(|l, var| {
        let a = Account::named(&unique("hd"), "%");
        create(l, &a);
        l.sql(&format!("GRANT SELECT ON `shop\\_h`.* TO {}", a.literal(true)));
        let before = grants(l, &a);
        l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![Edit { db: pattern_of("shop_h"), grant: vec!["SELECT".into(), "INSERT".into()], revoke: vec!["DELETE".into()] }], false);
        let (_, res, undo) = done(l);
        let msg = res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert!(msg.contains("not granted again") && msg.contains("not revoked"), "{var}: {msg}");
        l.w.sql_undo(l.db.clone(), undo.expect("an undo entry"), Vec::new(), false);
        done(l).1.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(grants(l, &a), before, "{var}: the SELECT held before is still there");
        drop_it(l, &a);
    });
}

#[test]
fn live_create_user_then_undo_drops_it_only_while_unused() {
    every_server(|l, var| {
        let a = Account::named(&unique("cr"), "%");
        let undo = create(l, &a).expect("undo");
        assert_eq!(l.one(&format!("SELECT COUNT(*) FROM mysql.user WHERE User = '{}'", a.user)), "1", "{var}");
        l.w.sql_undo(l.db.clone(), undo, Vec::new(), false);
        let (_, res, _) = done(l);
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(l.one(&format!("SELECT COUNT(*) FROM mysql.user WHERE User = '{}'", a.user)), "0", "{var}");
        // With a grant given since, the undo leaves the user in place.
        let b = Account::named(&unique("cr2"), "%");
        let undo = create(l, &b).unwrap();
        l.sql(&format!("GRANT SELECT ON `shop\\_z`.* TO {}", b.literal(true)));
        l.w.sql_undo(l.db.clone(), undo, Vec::new(), false);
        let (_, res, back) = done(l);
        assert!(res.unwrap_err().starts_with("Not undone"), "{var}");
        assert!(back.is_some());
        assert_eq!(l.one(&format!("SELECT COUNT(*) FROM mysql.user WHERE User = '{}'", b.user)), "1", "{var}");
        drop_it(l, &b);
    });
}

#[test]
fn live_every_write_refuses_on_a_read_only_connection() {
    every_server(|l, var| {
        let a = Account::named(&unique("ro"), "%");
        l.w.sql_user_create(1, l.db.clone(), a.clone(), "pw".into(), true);
        assert!(done(l).1.unwrap_err().contains("locked"), "{var}");
        l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![Edit { db: "d".into(), grant: vec!["SELECT".into()], revoke: vec![] }], true);
        assert!(done(l).1.unwrap_err().contains("locked"), "{var}");
        l.w.sql_user_drop(1, l.db.clone(), a.clone(), true);
        assert!(done(l).1.unwrap_err().contains("locked"), "{var}");
        assert_eq!(l.one(&format!("SELECT COUNT(*) FROM mysql.user WHERE User = '{}'", a.user)), "0", "{var}");
    });
}

#[test]
fn live_an_apostrophe_in_the_name_works() {
    every_server(|l, var| {
        let a = Account::named(&format!("{}'b;--", unique("ap")), "h'ost");
        create(l, &a);
        let ed = Edit { db: "we`ird_db".into(), grant: vec!["SELECT".into()], revoke: vec![] };
        l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![Edit { db: pattern_of("we`ird_db"), ..ed }], false);
        done(l).1.unwrap_or_else(|x| panic!("{var}: {x}"));
        l.w.sql_user_grants(1, 9, l.db.clone(), a.clone());
        let Msg::SqlUserGrants { res, .. } = l.rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() else { panic!() };
        let rows = db_grants(&res.unwrap().grants);
        assert_eq!(rows.len(), 1, "{var}");
        assert_eq!(shown_pattern(&rows[0].0), "we`ird_db", "{var}");
        l.w.sql_users(1, 1, l.db.clone());
        let Msg::SqlUsers { res, .. } = l.rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() else { panic!() };
        let list = res.unwrap();
        assert!(list.whole, "{var}: {:?}", list.why);
        assert!(list.accounts.iter().any(|x| x.user == a.user && x.host == a.host), "{var}");
        l.w.sql_user_drop(1, l.db.clone(), a.clone(), false);
        done(l).1.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(l.one(&format!("SELECT COUNT(*) FROM mysql.user WHERE User = {}", lit(&a.user, true))), "0", "{var}");
    });
}

#[test]
fn live_a_weak_user_sees_only_itself() {
    every_server(|l, var| {
        let a = Account::named(&unique("me"), "%");
        create(l, &a);
        l.sql(&format!("GRANT SELECT ON `shop\\_w`.* TO {}", a.literal(true)));
        let uri = std::env::var(crate::db::sql::testing::ENVS[if var.ends_with("MARIADB") { 1 } else { 0 }]).unwrap();
        let host = uri.split('@').nth(1).unwrap().to_string();
        let weak = format!("mysql://{}:pw-for-tests@{host}", a.user);
        let (tx, rx) = std::sync::mpsc::channel();
        let w = Worker::for_test(tx);
        w.sql_connect(2, weak, std::time::Duration::from_secs(30), crate::db::Net::default());
        let Msg::SqlConnected { res: Ok((db, _, _)), .. } = rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() else { panic!("connect") };
        w.sql_users(2, 1, db);
        let Msg::SqlUsers { res, .. } = rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() else { panic!() };
        let list = res.unwrap();
        assert!(!list.whole, "{var}");
        assert!(list.why.is_some());
        assert_eq!(list.accounts.len(), 1, "{var}");
        assert_eq!(list.accounts[0].user, a.user, "{var}");
        drop_it(l, &a);
    });
}

fn info(l: &crate::db::sql::testing::Live, a: &Account) -> AccountInfo {
    l.w.sql_user_grants(1, 5, l.db.clone(), a.clone());
    let Msg::SqlUserGrants { res, .. } = l.rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() else { panic!() };
    res.unwrap().info.expect("account info")
}

fn plan_run(l: &crate::db::sql::testing::Live, a: &Account, plan: AccountPlan) -> (Account, Res<String>, Vec<UndoAction>) {
    l.w.sql_user_account(1, l.db.clone(), a.clone(), plan, false);
    match l.rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() {
        Msg::SqlUserDone { account, res, undo, .. } => (account, res, undo),
        _ => panic!("expected SqlUserDone"),
    }
}

#[test]
fn live_partial_revokes_are_never_widened() {
    // Needs a MySQL 8 server started with --partial-revokes=ON.
    let Some(l) = crate::db::sql::testing::live("QUARRY_TEST_MYSQL_PR") else { return };
    let a = Account::named(&unique("pr"), "%");
    create(&l, &a);
    l.sql(&format!("GRANT SELECT, INSERT ON *.* TO {}", a.literal(true)));
    l.sql(&format!("REVOKE SELECT ON `mysql`.* FROM {}", a.literal(true)));
    let before = grants(&l, &a);
    assert!(before.iter().any(|g| g.starts_with("REVOKE SELECT ON `mysql`")), "{before:?}");
    // Revoking SELECT everywhere would drop the partial revoke: refused, and nothing changes.
    l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![Edit { db: String::new(), grant: vec![], revoke: vec!["SELECT".into()] }], false);
    let (_, res, undo) = done(&l);
    let err = res.unwrap_err();
    assert!(err.contains("partial revokes") && undo.is_none(), "{err}");
    assert_eq!(grants(&l, &a), before);
    // INSERT has none: it goes and comes back, with the partial revoke intact.
    l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![Edit { db: String::new(), grant: vec![], revoke: vec!["INSERT".into()] }], false);
    let (_, res, undo) = done(&l);
    res.unwrap();
    l.w.sql_undo(l.db.clone(), undo.unwrap(), Vec::new(), false);
    let (_, res, _) = done(&l);
    res.unwrap();
    assert_eq!(grants(&l, &a), before);
    // An escaped underscore still names the database my_db under partial revokes.
    let ed = Edit { db: pattern_of("my_db"), grant: vec!["SELECT".into()], revoke: vec![] };
    l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![ed], false);
    let (_, res, undo) = done(&l);
    res.unwrap_or_else(|x| panic!("{x}"));
    let rows = db_grants(&grants(&l, &a).iter().map(|g| parse_grant(g)).collect::<Vec<_>>());
    assert_eq!(rows.len(), 1);
    assert_eq!(shown_pattern(&rows[0].0), "my_db");
    l.w.sql_undo(l.db.clone(), undo.unwrap(), Vec::new(), false);
    done(&l).1.unwrap();
    assert_eq!(grants(&l, &a), before);
    drop_it(&l, &a);
}

#[test]
fn live_tls_requirements_come_back_with_undo() {
    every_server(|l, var| {
        let a = Account::named(&unique("tl"), "%");
        create(l, &a);
        l.sql(&format!("ALTER USER {} REQUIRE SUBJECT '/CN=me' AND ISSUER '/CN=ca' AND CIPHER 'AES256-SHA'", a.literal(true)));
        let before = info(l, &a).settings;
        assert_eq!(before.tls, Tls::Specified, "{var}");
        assert_eq!((before.spec.subject.as_str(), before.spec.issuer.as_str(), before.spec.cipher.as_str()), ("/CN=me", "/CN=ca", "AES256-SHA"), "{var}");
        // Replace the requirement and lock the account in one change.
        let caps = Caps::of(l.db.dialect, &l.db.version);
        let to = Settings { locked: caps.lock, tls: Tls::Ssl, spec: TlsSpec::default(), limits: [3, 0, 0, 0], ..before.clone() };
        let (_, res, undo) = plan_run(l, &a, AccountPlan { settings: Some((before.clone(), to.clone())), ..Default::default() });
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(info(l, &a).settings.tls, Tls::Ssl, "{var}");
        l.w.sql_undo(l.db.clone(), undo.into_iter().next().unwrap(), Vec::new(), false);
        let (_, res, back) = done(l);
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert!(back.is_none(), "{var}");
        assert_eq!(info(l, &a).settings, before, "{var}: lock, limits and the requirement all come back");
        drop_it(l, &a);
    });
}

#[test]
fn live_a_failed_undo_keeps_its_entry() {
    every_server(|l, var| {
        let a = Account::named(&unique("ke"), "%");
        create(l, &a);
        let s0 = Settings::default();
        let s1 = Settings { limits: [4, 0, 0, 0], ..s0.clone() };
        let (_, res, undo) = plan_run(l, &a, AccountPlan { settings: Some((s0, s1)), ..Default::default() });
        res.unwrap();
        // The account is gone, so the undo fails before it runs anything and stays in the list.
        drop_it(l, &a);
        l.w.sql_undo(l.db.clone(), undo.into_iter().next().unwrap(), Vec::new(), false);
        let (_, res, back) = done(l);
        assert!(res.is_err(), "{var}");
        assert!(back.is_some(), "{var}");
    });
}

#[test]
fn live_database_grants_are_read_and_checked_under_ansi_quotes() {
    every_server(|l, var| {
        let a = Account::named(&unique("an"), "%");
        create(l, &a);
        let ansi = l.with_session("SET SESSION sql_mode = CONCAT(@@sql_mode, ',ANSI_QUOTES')");
        let ed = Edit { db: pattern_of("ansi_db"), grant: vec!["SELECT".into()], revoke: vec![] };
        l.w.sql_user_edit(1, ansi.clone(), a.clone(), vec![ed], false);
        let (_, res, undo) = done(l);
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        l.w.sql_user_grants(1, 7, ansi.clone(), a.clone());
        let Msg::SqlUserGrants { res, .. } = l.rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() else { panic!() };
        let rows = db_grants(&res.unwrap().grants);
        assert_eq!(rows.len(), 1, "{var}: the database row is found");
        assert_eq!(shown_pattern(&rows[0].0), "ansi_db", "{var}");
        // The undo check is real: a change made behind its back refuses it.
        l.sql(&format!("GRANT INSERT ON `ansi\\_db`.* TO {}", a.literal(true)));
        l.w.sql_undo(ansi.clone(), undo.unwrap(), Vec::new(), false);
        let (_, res, back) = done(l);
        assert!(res.unwrap_err().starts_with("Undo refused"), "{var}");
        assert!(back.is_some(), "{var}");
        drop_it(l, &a);
    });
}

#[test]
fn live_undo_refuses_a_namesake_made_after_a_drop() {
    every_server(|l, var| {
        let a = Account::named(&unique("ns"), "%");
        create(l, &a);
        let s0 = Settings::default();
        let s1 = Settings { limits: [9, 0, 0, 0], ..s0.clone() };
        let (_, res, undo) = plan_run(l, &a, AccountPlan { settings: Some((s0, s1)), ..Default::default() });
        res.unwrap();
        drop_it(l, &a);
        l.sql(&format!("CREATE USER {} IDENTIFIED BY 'a-different-password'", a.literal(true)));
        l.sql(&format!("ALTER USER {} WITH MAX_QUERIES_PER_HOUR 9", a.literal(true)));
        l.w.sql_undo(l.db.clone(), undo.into_iter().next().unwrap(), Vec::new(), false);
        let (_, res, back) = done(l);
        let err = res.unwrap_err();
        assert!(err.starts_with("Undo refused") && err.contains("not the account"), "{var}: {err}");
        assert!(back.is_some(), "{var}");
        assert_eq!(info(l, &a).settings.limits[0], 9, "{var}: the namesake was left alone");
        drop_it(l, &a);
    });
}

#[test]
fn live_roles_give_back_their_admin_option_and_default_status() {
    every_server(|l, var| {
        let caps = Caps::of(l.db.dialect, &l.db.version);
        if !caps.roles {
            return;
        }
        let a = Account::named(&unique("rd"), "%");
        let role = unique("rr");
        create(l, &a);
        l.sql(&format!("CREATE ROLE '{role}'"));
        l.sql(&format!("GRANT '{role}' TO {} WITH ADMIN OPTION", a.literal(true)));
        let maria = l.db.dialect == Dialect::MariaDb;
        l.sql(&if maria { format!("SET DEFAULT ROLE '{role}' FOR {}", a.literal(true)) } else { format!("SET DEFAULT ROLE '{role}' TO {}", a.literal(true)) });
        let given = if maria { role.clone() } else { format!("{role}@%") };
        let (_, res, undo) = plan_run(l, &a, AccountPlan { roles_revoke: vec![given.clone()], ..Default::default() });
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        // MariaDB keeps the SET DEFAULT ROLE line after a revoke; only the role grant must be gone.
        assert!(!grants(l, &a).iter().any(|g| g.starts_with("GRANT") && g.contains(&role)), "{var}");
        l.w.sql_undo(l.db.clone(), undo.into_iter().next().unwrap(), Vec::new(), false);
        let (_, res, _) = done(l);
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        let g = grants(l, &a);
        assert!(g.iter().any(|x| x.contains(&role) && x.contains("WITH ADMIN OPTION")), "{var}: {g:?}");
        let defaults = if maria { l.one(&format!("SELECT default_role FROM mysql.user WHERE User = {}", lit(&a.user, true))) } else { l.one(&format!("SELECT DEFAULT_ROLE_USER FROM mysql.default_roles WHERE USER = {}", lit(&a.user, true))) };
        assert_eq!(defaults, role, "{var}");
        drop_it(l, &a);
        l.sql(&format!("DROP ROLE '{role}'"));
    });
}

#[test]
fn live_a_grant_option_on_a_dynamic_privilege_is_not_everyones() {
    every_server(|l, var| {
        if !Caps::of(l.db.dialect, &l.db.version).dynamic {
            return;
        }
        let a = Account::named(&unique("dg"), "%");
        create(l, &a);
        l.sql(&format!("GRANT CONNECTION_ADMIN ON *.* TO {} WITH GRANT OPTION", a.literal(true)));
        l.sql(&format!("GRANT SELECT ON *.* TO {}", a.literal(true)));
        let g: Vec<Grant> = grants(l, &a).iter().map(|g| parse_grant(g)).collect();
        assert_eq!(global_privs(&g), set(&["SELECT", "CONNECTION_ADMIN"]), "{var}: {:?}", grants(l, &a));
        assert_eq!(dynamic_with_grant(&g), set(&["CONNECTION_ADMIN"]), "{var}");
        drop_it(l, &a);
    });
}

#[test]
fn live_mariadb_set_password_keeps_the_login_methods_and_refuses_a_chain() {
    every_server(|l, var| {
        if l.db.dialect != Dialect::MariaDb {
            return;
        }
        // A chain needs a second plugin; use unix_socket when the server has it.
        let a = Account::named(&unique("ch"), "localhost");
        let made = l.w.rt.block_on(async { sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE USER {} IDENTIFIED VIA unix_socket OR mysql_native_password USING PASSWORD('x1')", a.literal(true)))).execute(&l.pool).await.is_ok() });
        if made {
            l.w.sql_user_password(1, l.db.clone(), a.clone(), None, "new-secret".into(), false);
            let err = done(l).1.unwrap_err();
            assert!(err.contains("more than one method") && !err.contains("new-secret"), "{var}: {err}");
            // The chain is untouched.
            assert_eq!(l.one(&format!("SELECT COUNT(*) FROM mysql.global_priv WHERE User = {} AND JSON_QUERY(Priv, '$.auth_or') IS NOT NULL", lit(&a.user, true))), "1", "{var}");
            drop_it(l, &a);
        }
        // A plain account takes the new password and no other plugin appears.
        let b = Account::named(&unique("pl"), "%");
        create(l, &b);
        l.w.sql_user_password(1, l.db.clone(), b.clone(), None, "second-secret".into(), false);
        done(l).1.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(info(l, &b).plugin, "mysql_native_password", "{var}");
        drop_it(l, &b);
    });
}

#[test]
fn live_a_mariadb_role_is_dropped_as_a_role() {
    every_server(|l, var| {
        if l.db.dialect != Dialect::MariaDb {
            return;
        }
        let role = unique("dr");
        l.sql(&format!("CREATE ROLE '{role}'"));
        let acct = Account { user: role.clone(), host: String::new(), is_role: true, ..Default::default() };
        l.w.sql_user_drop(1, l.db.clone(), acct, false);
        done(l).1.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(l.one(&format!("SELECT COUNT(*) FROM mysql.user WHERE User = '{role}'")), "0", "{var}");
    });
}

#[test]
fn live_settings_read_change_and_undo_exactly() {
    every_server(|l, var| {
        let a = Account::named(&unique("st"), "%");
        create(l, &a);
        let caps = Caps::of(l.db.dialect, &l.db.version);
        let before = info(l, &a);
        assert_eq!(before.settings, Settings::default(), "{var}");
        let to = Settings { locked: caps.lock, expire: if caps.lifetime { Expire::Days(90) } else { Expire::Default }, tls: Tls::Ssl, limits: [5000, 7, 9, 3], ..Default::default() };
        let (_, res, undo) = plan_run(l, &a, AccountPlan { settings: Some((before.settings.clone(), to.clone())), ..Default::default() });
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(info(l, &a).settings, to, "{var}");
        assert_eq!(undo.len(), 1, "{var}");
        // Never and the default are different things on both servers.
        let never = Settings { expire: Expire::Never, ..to.clone() };
        if caps.lifetime {
            let (_, res, undo2) = plan_run(l, &a, AccountPlan { settings: Some((to.clone(), never.clone())), ..Default::default() });
            res.unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!(info(l, &a).settings, never, "{var}");
            l.w.sql_undo(l.db.clone(), undo2.into_iter().next().unwrap(), Vec::new(), false);
            let (_, res, _) = done(l);
            res.unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!(info(l, &a).settings, to, "{var}");
        }
        l.w.sql_undo(l.db.clone(), undo.into_iter().next().unwrap(), Vec::new(), false);
        let (_, res, _) = done(l);
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(info(l, &a).settings, before.settings, "{var}");
        drop_it(l, &a);
    });
}

#[test]
fn live_a_settings_undo_refuses_when_the_field_changed_since() {
    every_server(|l, var| {
        let a = Account::named(&unique("sr"), "%");
        create(l, &a);
        let s0 = Settings::default();
        let s1 = Settings { limits: [10, 0, 0, 0], ..s0.clone() };
        let (_, res, undo) = plan_run(l, &a, AccountPlan { settings: Some((s0, s1)), ..Default::default() });
        res.unwrap();
        l.sql(&format!("ALTER USER {} WITH MAX_QUERIES_PER_HOUR 11", a.literal(true)));
        l.w.sql_undo(l.db.clone(), undo.into_iter().next().unwrap(), Vec::new(), false);
        let (_, res, back) = done(l);
        assert!(res.unwrap_err().starts_with("Undo refused"), "{var}");
        assert!(back.is_some());
        assert_eq!(info(l, &a).settings.limits[0], 11, "{var}");
        drop_it(l, &a);
    });
}

#[test]
fn live_rename_and_undo() {
    every_server(|l, var| {
        let a = Account::named(&unique("rn"), "%");
        let b = Account::named(&format!("{}'x", a.user), "h.example");
        create(l, &a);
        l.sql(&format!("GRANT SELECT ON `shop\\_rn`.* TO {}", a.literal(true)));
        let (cur, res, undo) = plan_run(l, &a, AccountPlan { rename: Some(b.clone()), ..Default::default() });
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!((cur.user.as_str(), cur.host.as_str()), (b.user.as_str(), b.host.as_str()), "{var}");
        assert!(grants(l, &b).iter().any(|g| g.contains("SELECT")), "{var}: the grants follow the name");
        l.w.sql_undo(l.db.clone(), undo.into_iter().next().unwrap(), Vec::new(), false);
        let Msg::SqlUserDone { account, res, op, .. } = l.rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() else { panic!() };
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(op, Op::UndoneRename, "{var}");
        assert_eq!(account.user, a.user, "{var}");
        assert!(grants(l, &a).iter().any(|g| g.contains("SELECT")), "{var}");
        drop_it(l, &a);
    });
}

#[test]
fn live_roles_are_given_and_taken_back_with_undo() {
    every_server(|l, var| {
        let caps = Caps::of(l.db.dialect, &l.db.version);
        if !caps.roles {
            return;
        }
        let a = Account::named(&unique("ro"), "%");
        let role = unique("rl");
        l.sql(&format!("CREATE ROLE '{role}'"));
        create(l, &a);
        let given = if l.db.dialect == Dialect::MySql { format!("{role}@%") } else { role.clone() };
        let (_, res, undo) = plan_run(l, &a, AccountPlan { roles_grant: vec![given.clone()], ..Default::default() });
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        l.w.sql_user_grants(1, 6, l.db.clone(), a.clone());
        let Msg::SqlUserGrants { res, .. } = l.rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() else { panic!() };
        assert_eq!(roles_of(&res.unwrap().grants), vec![given.clone()], "{var}");
        l.w.sql_undo(l.db.clone(), undo.into_iter().next().unwrap(), Vec::new(), false);
        let (_, res, _) = done(l);
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert!(!grants(l, &a).iter().any(|g| g.contains(&role)), "{var}");
        drop_it(l, &a);
        l.sql(&format!("DROP ROLE '{role}'"));
    });
}

#[test]
fn live_global_privileges_round_trip_and_dynamic_ones_on_mysql() {
    every_server(|l, var| {
        let a = Account::named(&unique("gl"), "%");
        create(l, &a);
        let before = grants(l, &a);
        let mut grant = vec!["PROCESS".to_string(), "SHOW VIEW".to_string(), "REPLICATION CLIENT".to_string()];
        if Caps::of(l.db.dialect, &l.db.version).dynamic {
            grant.push("CONNECTION_ADMIN".into());
        }
        let ed = Edit { db: String::new(), grant, revoke: vec![] };
        l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![ed.clone()], false);
        let (_, res, undo) = done(l);
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        let parsed: Vec<Grant> = grants(l, &a).iter().map(|g| parse_grant(g)).collect();
        assert_eq!(global_privs(&parsed), ed.grant.iter().cloned().collect::<BTreeSet<_>>(), "{var}");
        l.w.sql_undo(l.db.clone(), undo.unwrap(), Vec::new(), false);
        let (_, res, _) = done(l);
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(grants(l, &a), before, "{var}");
        drop_it(l, &a);
    });
}

#[test]
fn live_a_changed_password_logs_in_and_leaves_no_trace() {
    every_server(|l, var| {
        let a = Account::named(&unique("pw"), "%");
        create(l, &a);
        let secret = "n3w'pa\\ss-for-tests";
        l.w.sql_user_password(1, l.db.clone(), a.clone(), None, secret.into(), false);
        let (_, res, undo) = done(l);
        let msg = res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert!(!msg.contains(secret) && undo.is_none(), "{var}");
        // The new password logs in; the old one does not.
        let uri = std::env::var(crate::db::sql::testing::ENVS[if var.ends_with("MARIADB") { 1 } else { 0 }]).unwrap();
        let host = uri.split('@').nth(1).unwrap().to_string();
        let try_login = |pw: &str| {
            let url = format!("mysql://{}:{}@{host}", a.user, pw.replace('\\', "%5C").replace('\'', "%27"));
            l.w.rt.block_on(async { sqlx::MySqlPool::connect(&url).await.is_ok() })
        };
        assert!(try_login(secret), "{var}");
        assert!(!try_login("pw-for-tests"), "{var}");
        // A refused change reports no password either.
        l.w.sql_user_password(1, l.db.clone(), a.clone(), Some("no_such_plugin_x".into()), secret.into(), false);
        let (_, res, _) = done(l);
        if l.db.dialect == Dialect::MySql {
            assert!(!res.unwrap_err().contains(secret), "{var}");
        }
        drop_it(l, &a);
    });
}

#[test]
fn the_log_never_holds_the_password_of_create_or_change() {
    // Quotes and backslashes in the password end a literal early under one reading or the other; none may leak.
    let secrets = ["Zq9Marker", "Zq9'Marker", "Zq9\\'Marker", "Zq9\\Marker\\", "Zq9\"Marker`x", "Zq9''Marker\\\\"];
    let a = Account::named("log_pw_user", "%");
    let mut n = 0u64;
    for bs in [true, false] {
        let m = Mode { bs, ansi: false };
        for d in [Dialect::MySql, Dialect::MariaDb] {
            for secret in secrets {
                let mut stmts = vec![create_user_sql(m, &a, secret, false)];
                for plugin in [None, Some("caching_sha2_password")] {
                    if let Ok(sql) = password_sql(m, d, &a, plugin, secret, false) {
                        stmts.push(sql);
                    }
                }
                for sql in stmts {
                    n += 1;
                    let conn = 991_000 + n;
                    crate::db::log_sql(conn, "", &sql, 1, None);
                    // And once more as a failure whose message quotes the statement from where it broke.
                    crate::db::log_sql(conn, "", &sql, 1, Some(&format!("You have an error in your SQL syntax; check the manual for the right syntax to use near '{}' at line 1", &sql[sql.find("BY ").or_else(|| sql.find("PASSWORD(")).unwrap_or(0)..])));
                    let log = crate::db::log();
                    for l in log.iter().filter(|l| l.conn == conn) {
                        let all = format!("{} {}", l.cmd, l.err.clone().unwrap_or_default());
                        assert!(!all.contains("Marker") && !all.contains("Zq9"), "bs={bs} {d:?} {secret:?}: {all}");
                        assert!(l.cmd.contains("'***'"), "{}", l.cmd);
                    }
                }
            }
        }
    }
}

#[test]
fn live_users_writes_reach_the_query_log_with_passwords_redacted() {
    every_server(|l, var| {
        let a = Account::named(&unique("lg"), "%");
        let secret = "Zq9'Marker\\x";
        l.w.sql_user_create(1, l.db.clone(), a.clone(), secret.into(), false);
        done(l).1.unwrap_or_else(|x| panic!("{var}: {x}"));
        let ed = Edit { db: pattern_of("shop_lg"), grant: vec!["SELECT".into()], revoke: vec![] };
        l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![ed], false);
        let (_, res, undo) = done(l);
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        l.w.sql_undo(l.db.clone(), undo.expect("undo"), Vec::new(), false);
        done(l).1.unwrap_or_else(|x| panic!("{var}: {x}"));
        l.w.sql_user_password(1, l.db.clone(), a.clone(), None, secret.into(), false);
        done(l).1.unwrap_or_else(|x| panic!("{var}: {x}"));
        l.w.sql_user_drop(1, l.db.clone(), a.clone(), false);
        done(l).1.unwrap_or_else(|x| panic!("{var}: {x}"));
        let log = crate::db::log();
        let mine: Vec<&str> = log.iter().filter(|e| e.conn == l.db.conn && e.cmd.contains(&a.user)).map(|e| e.cmd.as_str()).collect();
        let has = |w: &str| mine.iter().any(|c| c.starts_with(w));
        assert!(has("CREATE USER") && has("GRANT SELECT") && has("REVOKE SELECT") && has("DROP USER"), "{var}: {mine:?}");
        assert!(mine.iter().any(|c| c.contains("PASSWORD(") || c.starts_with("ALTER USER") || c.starts_with("SET PASSWORD")), "{var}: {mine:?}");
        assert!(mine.iter().all(|c| !c.contains("Marker") && !c.contains("Zq9")), "{var}: {mine:?}");
    });
}
