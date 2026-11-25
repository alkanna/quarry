use super::*;

fn privs(p: &[(&str, bool)]) -> Privs {
    p.iter().map(|(k, g)| (k.to_string(), *g)).collect()
}

fn table() -> Target {
    Target { kind: Kind::Tables, schema: "public".into(), name: "orders".into(), sig: String::new(), oid: "1".into() }
}

#[test]
fn acl_statements_and_their_inverse() {
    let from = privs(&[("SELECT", false), ("UPDATE", true), ("DELETE", false)]);
    let to = privs(&[("SELECT", true), ("UPDATE", false), ("INSERT", false)]);
    let c = Change::Acl { target: table(), grantee: "app writer".into(), from, to };
    assert_eq!(
        c.sql(18).unwrap(),
        [
            "REVOKE DELETE ON TABLE \"public\".\"orders\" FROM \"app writer\"",
            "REVOKE GRANT OPTION FOR UPDATE ON TABLE \"public\".\"orders\" FROM \"app writer\"",
            "GRANT INSERT ON TABLE \"public\".\"orders\" TO \"app writer\"",
            "GRANT SELECT ON TABLE \"public\".\"orders\" TO \"app writer\" WITH GRANT OPTION",
        ]
    );
    assert_eq!(
        c.inverse().sql(18).unwrap(),
        [
            "REVOKE INSERT ON TABLE \"public\".\"orders\" FROM \"app writer\"",
            "REVOKE GRANT OPTION FOR SELECT ON TABLE \"public\".\"orders\" FROM \"app writer\"",
            "GRANT DELETE ON TABLE \"public\".\"orders\" TO \"app writer\"",
            "GRANT UPDATE ON TABLE \"public\".\"orders\" TO \"app writer\" WITH GRANT OPTION",
        ]
    );
    assert_eq!(c.inverse().inverse(), c);
}

#[test]
fn privilege_names_come_from_the_lists_and_maintain_from_17() {
    let to = privs(&[("MAINTAIN", false)]);
    let c = Change::Acl { target: table(), grantee: "a".into(), from: Privs::new(), to };
    assert!(c.sql(16).is_err());
    assert_eq!(c.sql(17).unwrap(), ["GRANT MAINTAIN ON TABLE \"public\".\"orders\" TO \"a\""]);
    let bad = Change::Acl { target: table(), grantee: "a".into(), from: Privs::new(), to: privs(&[("SELECT; DROP TABLE x", false)]) };
    assert!(bad.sql(18).is_err());
    let public_go = Change::Acl { target: table(), grantee: String::new(), from: Privs::new(), to: privs(&[("SELECT", true)]) };
    assert!(public_go.sql(18).is_err());
    let public = Change::Acl { target: table(), grantee: String::new(), from: Privs::new(), to: privs(&[("SELECT", false)]) };
    assert_eq!(public.sql(18).unwrap(), ["GRANT SELECT ON TABLE \"public\".\"orders\" TO PUBLIC"]);
}

#[test]
fn every_kind_names_its_object() {
    let f = Target { kind: Kind::Functions, schema: "s\"x".into(), name: "f".into(), sig: "a integer, text".into(), oid: "1".into() };
    assert_eq!(f.sql(), "ROUTINE \"s\"\"x\".\"f\"(a integer, text)");
    assert_eq!(Target { kind: Kind::Schemas, name: "rep".into(), ..Default::default() }.sql(), "SCHEMA \"rep\"");
    assert_eq!(Target { kind: Kind::Databases, name: "shop".into(), ..Default::default() }.sql(), "DATABASE \"shop\"");
    assert_eq!(Target { kind: Kind::Sequences, schema: "public".into(), name: "s".into(), ..Default::default() }.sql(), "SEQUENCE \"public\".\"s\"");
}

#[test]
fn alter_role_writes_only_what_changed() {
    let from = Attrs { login: true, conn_limit: -1, ..Default::default() };
    let to = Attrs { login: true, createdb: true, inherit: false, conn_limit: 20, valid_until: "2026-12-31".into(), ..Default::default() };
    assert_eq!(attrs_sql("app", &from, &to).unwrap().unwrap(), "ALTER ROLE \"app\" WITH CREATEDB NOINHERIT CONNECTION LIMIT 20 VALID UNTIL '2026-12-31'");
    assert_eq!(attrs_sql("app", &to, &from).unwrap().unwrap(), "ALTER ROLE \"app\" WITH NOCREATEDB INHERIT CONNECTION LIMIT -1 VALID UNTIL 'infinity'");
    assert_eq!(attrs_sql("app", &from, &from).unwrap(), None);
    assert!(attrs_sql("app", &from, &Attrs { conn_limit: -2, ..from.clone() }).is_err());
    let quote = Attrs { valid_until: "2026-01-01' ; DROP ROLE x; --".into(), ..from.clone() };
    assert_eq!(attrs_sql("app", &from, &quote).unwrap().unwrap(), "ALTER ROLE \"app\" WITH VALID UNTIL '2026-01-01'' ; DROP ROLE x; --'");
}

#[test]
fn memberships_follow_the_version() {
    let opts16 = Opts { admin: false, inherit: Some(true), set: Some(false) };
    let o16 = Some(opts16);
    assert_eq!(member_sql(16, "rep", "app", &None, &o16), ["GRANT \"rep\" TO \"app\" WITH ADMIN FALSE, INHERIT TRUE, SET FALSE"]);
    assert_eq!(member_sql(16, "rep", "app", &o16, &None), ["REVOKE \"rep\" FROM \"app\""]);
    let admin16 = Some(Opts { admin: true, ..opts16 });
    assert_eq!(member_sql(16, "rep", "app", &o16, &admin16), ["GRANT \"rep\" TO \"app\" WITH ADMIN TRUE"]);
    assert_eq!(member_sql(16, "rep", "app", &admin16, &o16), ["GRANT \"rep\" TO \"app\" WITH ADMIN FALSE"]);
    let (plain, admin) = (Some(Opts::default()), Some(Opts { admin: true, ..Default::default() }));
    assert_eq!(member_sql(13, "rep", "app", &None, &plain), ["GRANT \"rep\" TO \"app\""]);
    assert_eq!(member_sql(13, "rep", "app", &None, &admin), ["GRANT \"rep\" TO \"app\" WITH ADMIN OPTION"]);
    assert_eq!(member_sql(13, "rep", "app", &admin, &plain), ["REVOKE ADMIN OPTION FOR \"rep\" FROM \"app\""]);
}

#[test]
fn default_privileges_name_the_creator_and_schema() {
    let c = Change::Default { owner: "etl".into(), schema: "staging".into(), kind: DefKind::Tables, grantee: "reporting".into(), from: Privs::new(), to: privs(&[("SELECT", false)]) };
    assert_eq!(c.sql(17).unwrap(), ["ALTER DEFAULT PRIVILEGES FOR ROLE \"etl\" IN SCHEMA \"staging\" GRANT SELECT ON TABLES TO \"reporting\""]);
    assert_eq!(c.inverse().sql(17).unwrap(), ["ALTER DEFAULT PRIVILEGES FOR ROLE \"etl\" IN SCHEMA \"staging\" REVOKE SELECT ON TABLES FROM \"reporting\""]);
    let all = Change::Default { owner: "etl".into(), schema: String::new(), kind: DefKind::Functions, grantee: String::new(), from: privs(&[("EXECUTE", false)]), to: Privs::new() };
    assert_eq!(all.sql(17).unwrap(), ["ALTER DEFAULT PRIVILEGES FOR ROLE \"etl\" REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC"]);
    let schemas = Change::Default { owner: "etl".into(), schema: "s".into(), kind: DefKind::Schemas, grantee: "r".into(), from: Privs::new(), to: privs(&[("USAGE", false)]) };
    assert!(schemas.sql(17).is_err());
}

#[test]
fn create_role_hides_the_password_in_the_preview() {
    let n = NewRole { name: "etl_loader".into(), attrs: Attrs { login: true, ..Default::default() }, groups: vec!["reporting".into()] };
    assert_eq!(create_sql(&n, Some(REDACTED)).unwrap(), "CREATE ROLE \"etl_loader\" WITH LOGIN INHERIT PASSWORD '***' IN ROLE \"reporting\"");
    let v = scram_verifier("s3cret'", &[1u8; 16], 4096);
    let sql = create_sql(&n, Some(&v)).unwrap();
    assert!(!sql.contains("s3cret"));
    // The Query log redacts the verifier like any password literal.
    let (logged, cut) = crate::db::log::redact_sql(&sql);
    assert!(!logged.contains("SCRAM"), "{logged}");
    assert_eq!(cut.len(), 1);
    assert!(crate::db::log::redact_sql(&password_sql("a", &v)).0.ends_with("PASSWORD '***'"));
    assert!(create_sql(&NewRole { name: "pg_x".into(), ..Default::default() }, None).is_err());
    assert!(create_sql(&NewRole { name: "public".into(), ..Default::default() }, None).is_err());
}

#[test]
fn scram_verifier_has_the_server_shape() {
    let v = scram_verifier("pencil", b"0123456789abcdef", 4096);
    let (head, keys) = v.split_once('$').unwrap().1.split_once('$').unwrap();
    assert_eq!(v.split('$').next().unwrap(), "SCRAM-SHA-256");
    assert_eq!(head, "4096:MDEyMzQ1Njc4OWFiY2RlZg==");
    let (stored, server) = keys.split_once(':').unwrap();
    assert_eq!((stored.len(), server.len()), (44, 44));
    assert_eq!(v, scram_verifier("pencil", b"0123456789abcdef", 4096));
    assert_ne!(v, scram_verifier("pencil", b"0123456789abcdeg", 4096));
    assert_eq!(b64(b"ab"), "YWI=");
    assert_eq!(b64(b"abc"), "YWJj");
    assert_eq!(b64(b"a"), "YQ==");
    // HMAC-SHA-256 test case 2 of RFC 4231.
    let mac = hmac(b"Jefe", &[b"what do ya want ", b"for nothing?"]);
    assert_eq!(mac.iter().map(|b| format!("{b:02x}")).collect::<String>(), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
}

#[test]
fn scram_verifier_matches_the_server() {
    // What PostgreSQL 18 stored for CREATE ROLE ... PASSWORD 'pencil' with this salt.
    let salt = [213, 162, 177, 185, 246, 83, 247, 67, 102, 63, 234, 9, 224, 106, 165, 225];
    assert_eq!(scram_verifier("pencil", &salt, 4096), "SCRAM-SHA-256$4096:1aKxufZT90NmP+oJ4Gql4Q==$YfDn2h9kok2vJsroTHF8qZB6imCrb6dZzOeUtflU/3g=:LWrs39JieGOjDtBFgWciRWgDO9e0ztNpQV1IXghvEPo=");
}

#[test]
fn non_ascii_passwords_are_normalized_like_libpq_does() {
    // SASLprep is NFKC, so the ligature reads as the two letters; psql and the server compute the same.
    assert_eq!(scram_verifier("Zq9 Marker \u{fb01}", &[7; 16], 16), scram_verifier("Zq9 Marker fi", &[7; 16], 16));
    // An ASCII password is used as it is, control characters included.
    assert_ne!(scram_verifier("a\u{7}", &[7; 16], 16), scram_verifier("a", &[7; 16], 16));
}

#[test]
fn errors_lose_the_password() {
    assert_eq!(scrub("near 'p''w' at x, also p'w".into(), &["p'w"]), "near '***' at x, also ***");
}

fn cluster(major: u32) -> Cluster {
    let role = |n: &str, inherit: bool| Role { name: n.into(), attrs: Attrs { inherit, ..Default::default() }, ..Default::default() };
    let m = |g: &str, mem: &str, inh: Option<bool>| Membership { group: g.into(), member: mem.into(), opts: Opts { admin: false, inherit: inh, set: inh }, grantors: vec![] };
    Cluster {
        major,
        me: "admin".into(),
        roles: vec![role("jane", true), role("noinh", false), role("reporting", true), role("readonly", true)],
        members: vec![m("reporting", "jane", Some(true)), m("readonly", "reporting", Some(true)), m("reporting", "noinh", Some(true)), m("readonly", "jane", Some(false))],
        databases: vec![],
        password_encryption: "scram-sha-256".into(),
    }
}

#[test]
fn inherited_privileges_follow_inherit() {
    let c16 = cluster(16);
    assert_eq!(c16.chain("jane"), ["jane", "reporting", "readonly", ""]);
    // On 16 the membership's option counts, not the member's attribute.
    assert_eq!(c16.chain("noinh"), ["noinh", "reporting", "readonly", ""]);
    let mut c13 = cluster(13);
    for m in c13.members.iter_mut() {
        m.opts.inherit = None;
        m.opts.set = None;
    }
    assert_eq!(c13.chain("noinh"), ["noinh", ""]);
    assert_eq!(c13.chain("jane"), ["jane", "reporting", "readonly", ""]);
    let obj = Obj {
        owner: "etl".into(),
        acl: vec![
            AclEntry { grantee: "readonly".into(), grantor: "etl".into(), privs: privs(&[("SELECT", false)]) },
            AclEntry { grantee: "jane".into(), grantor: "etl".into(), privs: privs(&[("INSERT", true)]) },
            AclEntry { grantee: String::new(), grantor: "etl".into(), privs: privs(&[("TRIGGER", false)]) },
        ],
        ..Default::default()
    };
    let cl = cell(&obj, &c16.chain("jane"));
    assert_eq!(cl.direct, privs(&[("INSERT", true)]));
    assert_eq!(cl.via, BTreeMap::from([("SELECT".to_string(), "readonly".to_string()), ("TRIGGER".to_string(), String::new())]));
    assert!(!cl.owner);
    assert!(cell(&obj, &["etl".to_string()]).owner);
}

#[test]
fn createrole_rules_follow_the_version() {
    let mut c = cluster(16);
    c.me = "mgr".into();
    c.roles.push(Role { name: "mgr".into(), attrs: Attrs { createrole: true, ..Default::default() }, ..Default::default() });
    let jane = c.role("jane").unwrap().clone();
    assert!(c.may_alter(&jane).is_err(), "16 needs ADMIN on the role");
    assert!(c.may_alter(&Role { admin: true, ..jane.clone() }).is_ok());
    assert!(c.may_set_flag("CREATEDB").is_err());
    assert!(c.may_set_flag("LOGIN").is_ok());
    c.major = 15;
    assert!(c.may_alter(&jane).is_ok(), "before 16 CREATEROLE manages any non-superuser role");
    assert!(c.may_set_flag("CREATEDB").is_ok());
    assert!(c.may_set_flag("REPLICATION").is_err());
    c.me = "jane".into();
    assert!(c.read_only_reason().unwrap().contains("neither CREATEROLE nor SUPERUSER"));
}

#[test]
fn acl_json_merges_per_grantor() {
    let acl = parse_acl(r#"[["","postgres","CONNECT",false],["","postgres","TEMPORARY",false],["app","postgres","CONNECT",true],["app","jane","CREATE",false]]"#);
    assert_eq!(acl.len(), 3);
    assert_eq!(merged(&acl, "app"), privs(&[("CONNECT", true), ("CREATE", false)]));
    assert_eq!(merged(&acl, ""), privs(&[("CONNECT", false), ("TEMPORARY", false)]));
}

#[test]
fn drop_plans_reassign_or_drop_and_refuse_what_cannot_work() {
    let info = DropInfo { role: "jane".into(), oid: "9".into(), dbs: vec![DbHold { database: "analytics".into(), owned: vec!["table t".into()], held: 2, deps: vec![] }, DbHold { database: "shop".into(), owned: vec![], held: 1, deps: vec![] }], ..Default::default() };
    let p = drop_plan(&info, &DropMode::Reassign("app_owner".into()), "postgres").unwrap();
    assert_eq!(drop_preview(&p), "-- in analytics\nREASSIGN OWNED BY \"jane\" TO \"app_owner\";\nDROP OWNED BY \"jane\";\n-- in shop\nREASSIGN OWNED BY \"jane\" TO \"app_owner\";\nDROP OWNED BY \"jane\";\n-- then, once\nDROP ROLE \"jane\";");
    assert_eq!(drop_plan(&info, &DropMode::DropOwned, "postgres").unwrap().len(), 3);
    assert!(drop_plan(&info, &DropMode::Reassign("jane".into()), "postgres").is_err());
    let owns_db = DropInfo { shared: vec!["database x".into()], ..info.clone() };
    assert!(drop_plan(&owns_db, &DropMode::DropOwned, "postgres").is_err());
    let only_db = DropInfo { dbs: vec![], ..owns_db.clone() };
    assert_eq!(drop_plan(&only_db, &DropMode::Reassign("a".into()), "postgres").unwrap()[0].0, "postgres");
    let unread = DropInfo { unread: vec![("shop".into(), "no access".into())], ..info };
    assert!(drop_plan(&unread, &DropMode::DropOwned, "postgres").is_err());
}

#[test]
fn the_signed_in_role_cannot_lock_itself_out() {
    let me = Attrs { login: true, createrole: true, superuser: true, ..Default::default() };
    let ch = |to: Attrs| Change::Attrs { role: "admin".into(), from: me.clone(), to };
    for f in ["LOGIN", "SUPERUSER", "CREATEROLE"] {
        let mut to = me.clone();
        *to.flag_mut(f).unwrap() = false;
        assert!(Cluster::self_lockout("admin", &ch(to.clone())).is_some_and(|w| w.contains(f)), "{f}");
        // Another role may lose it.
        assert!(Cluster::self_lockout("someone", &ch(to)).is_none(), "{f}");
    }
    assert!(Cluster::self_lockout("admin", &ch(Attrs { createdb: true, ..me.clone() })).is_none());
    assert!(Cluster::self_lockout("admin", &ch(Attrs { valid_until: "-infinity".into(), ..me.clone() })).is_some());
    // Its own membership in a group that carries CREATEROLE, or with ADMIN, asks for a typed confirmation.
    let mut c = cluster(16);
    c.me = "jane".into();
    c.roles.push(Role { name: "mgrs".into(), attrs: Attrs { createrole: true, ..Default::default() }, ..Default::default() });
    c.members.push(Membership { group: "mgrs".into(), member: "reporting".into(), opts: Opts { admin: false, inherit: Some(true), set: Some(true) }, grantors: vec![] });
    let o = Opts { admin: false, inherit: Some(true), set: Some(true) };
    let leave = |g: &str, from: Opts, to: Option<Opts>| Change::Member { group: g.into(), member: "jane".into(), from: Some(from), to };
    assert!(c.self_membership_risk(&leave("reporting", o, None)).is_some(), "reporting leads to mgrs");
    assert!(c.self_membership_risk(&leave("readonly", o, None)).is_none(), "readonly leads nowhere");
    assert!(c.self_membership_risk(&leave("readonly", Opts { admin: true, ..o }, Some(o))).is_some(), "its own ADMIN");
    assert!(c.self_membership_risk(&leave("reporting", o, Some(Opts { admin: true, ..o }))).is_none(), "more rights, no risk");
    assert!(c.self_membership_risk(&Change::Member { group: "reporting".into(), member: "noinh".into(), from: Some(o), to: None }).is_none(), "not the signed-in role");
    // The confirmed change gets no undo; the others keep theirs.
    let b = Batch { changes: vec![leave("reporting", o, None), Change::Member { group: "readonly".into(), member: "noinh".into(), from: None, to: Some(o) }], no_undo: BTreeSet::from([0]), ..Default::default() };
    assert_eq!(b.inverse().changes, [b.changes[1].inverse()]);
}

#[test]
fn minus_infinity_reads_as_expired_and_writes_back_as_itself() {
    assert_eq!(until_shown("-infinity"), "expired");
    assert_eq!(until_shown(""), "never");
    let from = Attrs { valid_until: "-infinity".into(), ..Default::default() };
    assert_eq!(attrs_sql("a", &Attrs::default(), &from).unwrap().unwrap(), "ALTER ROLE \"a\" WITH VALID UNTIL '-infinity'");
    assert!(from.text().contains("valid until expired"));
    assert!(UNTIL.contains("'-infinity' THEN '-infinity'"));
}

#[test]
fn an_undo_restores_each_grantors_grant_exactly() {
    let g = |v: &[(&str, &[(&str, bool)])]| -> Grantors<Privs> { v.iter().map(|(k, p)| (k.to_string(), privs(p))).collect() };
    // mgr gave SELECT with grant option where the owner had given it plain: the undo takes mgr's grant away whole.
    let before = g(&[("postgres", &[("SELECT", false)])]);
    let after = g(&[("postgres", &[("SELECT", false)]), ("mgr", &[("SELECT", true)])]);
    let c = Change::Acl { target: table(), grantee: "m".into(), from: privs(&[("SELECT", false)]), to: privs(&[("SELECT", true)]) };
    let ran = Batch { changes: vec![c.clone()], exact: vec![Some(Exact::Acl(before.clone(), after.clone()))], ..Default::default() };
    let back = ran.inverse();
    assert_eq!(back.exact, [Some(Exact::Acl(after.clone(), before.clone()))]);
    assert_eq!(stmts(&back.changes[0], back.exact[0].as_ref(), 18).unwrap(), ["REVOKE SELECT ON TABLE \"public\".\"orders\" FROM \"m\""]);
    // Merged, the two states differ from what the undo must leave, so the check compares grantor by grantor.
    assert!(!holds(&back.changes[0], back.exact[0].as_ref(), &State::Privs(g(&[("postgres", &[("SELECT", false)]), ("mgr", &[("SELECT", false)])])), false));
    assert!(holds(&back.changes[0], back.exact[0].as_ref(), &State::Privs(before.clone()), false));
    assert!(one_grantor(&after, &g(&[])).is_err(), "two grantors at once");
    // A membership row of another grantor is revoked by name from 16; before, there is one row and no name.
    let o = Opts { admin: false, inherit: Some(true), set: Some(true) };
    let rows = |v: &[(&str, Opts)]| -> Grantors<Opts> { v.iter().map(|(k, o)| (k.to_string(), *o)).collect() };
    let now = rows(&[("postgres", o), ("mgr", Opts { admin: true, ..o })]);
    assert_eq!(member_rows_sql(16, "g", "m", &now, &rows(&[("postgres", o)])), ["REVOKE \"g\" FROM \"m\" GRANTED BY \"mgr\""]);
    assert_eq!(member_rows_sql(16, "g", "m", &now, &rows(&[("postgres", o), ("mgr", o)])), ["GRANT \"g\" TO \"m\" WITH ADMIN FALSE GRANTED BY \"mgr\""]);
    let one = |admin| rows(&[("", Opts { admin, inherit: None, set: None })]);
    assert_eq!(member_rows_sql(13, "g", "m", &one(true), &one(false)), ["REVOKE ADMIN OPTION FOR \"g\" FROM \"m\""]);
    assert_eq!(merge_opts(&now), Some(Opts { admin: true, ..o }));
}

#[test]
fn a_role_with_only_shared_privileges_is_dropped_through_drop_owned() {
    let info = DropInfo { role: "jane".into(), oid: "9".into(), shared_held: vec!["database shop".into()], ..Default::default() };
    let p = drop_plan(&info, &DropMode::DropOwned, "postgres").unwrap();
    assert_eq!(drop_preview(&p), "-- in postgres\nDROP OWNED BY \"jane\";\n-- then, once\nDROP ROLE \"jane\";");
    assert_eq!(drop_plan(&DropInfo { shared_held: vec![], ..info }, &DropMode::DropOwned, "postgres").unwrap().len(), 1);
}

#[test]
fn password_settings_are_followed_and_bounded() {
    assert!(wants_md5("md5") && wants_md5(" on ") && !wants_md5("scram-sha-256"));
    assert_eq!(md5_verifier("pencil", "jane"), "md5ab570ec1cf09f85f83d43e87e1a7d3c7");
    assert_eq!(check_iterations("4096"), Ok(4096));
    assert_eq!(check_iterations("10000000"), Ok(10_000_000));
    for bad in ["1", "4095", "10000001", "", "x"] {
        assert!(check_iterations(bad).is_err(), "{bad}");
    }
}

#[test]
fn saslprep_follows_postgresql() {
    let v = |p: &str| scram_verifier(p, &[7; 16], 16);
    // Only characters mapped to nothing: PostgreSQL uses the password as typed rather than an empty one.
    assert!(pg_saslprep("\u{ad}").is_none());
    assert_ne!(v("\u{ad}"), v("\u{200b}"));
    // An emoji is unassigned in Unicode 3.2, so the whole password is used as typed, its no-break space included.
    assert!(pg_saslprep("Zq9\u{a0}x\u{1f600}").is_none());
    assert_ne!(v("Zq9\u{a0}x\u{1f600}"), v("Zq9 x\u{1f600}"));
    // Without one, the no-break space maps to a space and NFKC applies.
    assert_eq!(pg_saslprep("Zq9\u{a0}\u{fb01}").as_deref().map(String::as_str), Some("Zq9 fi"));
    // Right to left text must start and end right to left.
    assert!(pg_saslprep("\u{627}1").is_none());
    assert_eq!(pg_saslprep("\u{627}\u{628}").as_deref().map(String::as_str), Some("\u{627}\u{628}"));
}

#[test]
fn may_follows_the_server() {
    let mut c = cluster(15);
    c.me = "mgr".into();
    c.roles.push(Role { name: "mgr".into(), attrs: Attrs { createrole: true, ..Default::default() }, ..Default::default() });
    let repl = Role { name: "streamer".into(), attrs: Attrs { replication: true, login: true, ..Default::default() }, ..Default::default() };
    assert!(c.may_alter(&repl).is_err(), "before 16 a REPLICATION role is a superuser's");
    assert!(c.may_password(&repl).is_err());
    c.major = 16;
    assert!(c.may_alter(&Role { admin: true, ..repl.clone() }).is_ok());
    // Without CREATEROLE: its own password, and the members of a group it holds ADMIN on.
    for major in [13, 16] {
        let mut c = cluster(major);
        c.me = "jane".into();
        let jane = c.role("jane").unwrap().clone();
        let other = c.role("noinh").unwrap().clone();
        assert!(c.may_password(&jane).is_ok(), "{major}");
        assert!(c.may_password(&other).is_err(), "{major}");
        let group = c.role("reporting").unwrap().clone();
        assert!(c.may_grant_role(&group).is_err(), "{major}");
        assert!(c.may_grant_role(&Role { admin: true, ..group }).is_ok(), "{major}");
    }
}

#[test]
fn labels_say_what_changed() {
    let c = Change::Acl { target: table(), grantee: "app".into(), from: Privs::new(), to: privs(&[("DELETE", false)]) };
    assert_eq!(c.describe(), "granted DELETE on public.orders to app");
    assert_eq!(c.inverse().describe(), "revoked DELETE on public.orders from app");
    let go = Change::Acl { target: table(), grantee: "app".into(), from: privs(&[("SELECT", true)]), to: privs(&[("SELECT", false)]) };
    assert_eq!(go.describe(), "revoked grant option for SELECT on public.orders from app");
    let m = Change::Member { group: "rep".into(), member: "app".into(), from: None, to: Some(Opts::default()) };
    assert_eq!(m.describe(), "added app to rep");
}
