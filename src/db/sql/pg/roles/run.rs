//! Reading roles, objects and default privileges, running a checked batch, and creating, dropping and
//! changing the password of a role.

use super::*;

// Reading.

/// The moment a role expires, read back the same way whatever the session's time zone. to_char gives NULL for -infinity, which
/// would read as never, so both infinities are named.
pub(super) const UNTIL: &str = "CASE WHEN r.rolvaliduntil IS NULL OR r.rolvaliduntil = 'infinity' THEN '' WHEN r.rolvaliduntil = '-infinity' THEN '-infinity' ELSE to_char(r.rolvaliduntil AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS') || '+00' END";

pub(super) fn roles_sql(filter: &str) -> String {
    format!(
        "SELECT r.oid::text AS oid, r.rolname::text AS name, r.rolcanlogin::text AS login, r.rolsuper::text AS su, r.rolcreatedb::text AS cdb, r.rolcreaterole::text AS cr, r.rolinherit::text AS inh, \
         r.rolreplication::text AS repl, r.rolbypassrls::text AS bypass, r.rolconnlimit::text AS lim, {UNTIL} AS until, pg_has_role(current_user, r.oid, 'MEMBER WITH ADMIN OPTION')::text AS adm \
         FROM pg_roles r {filter} ORDER BY r.rolname"
    )
}

pub(super) fn members_sql(major: u32) -> String {
    let (inh, set) = if major >= 16 { ("a.inherit_option::text", "a.set_option::text") } else { ("NULL::text", "NULL::text") };
    format!(
        "SELECT g.rolname::text AS grp, m.rolname::text AS member, COALESCE(gr.rolname::text, '') AS grantor, a.admin_option::text AS adm, {inh} AS inh, {set} AS setopt \
         FROM pg_auth_members a JOIN pg_roles g ON g.oid = a.roleid JOIN pg_roles m ON m.oid = a.member LEFT JOIN pg_roles gr ON gr.oid = a.grantor ORDER BY 1, 2, 3"
    )
}

pub(super) fn t(r: &SqlRow, c: &str) -> String {
    text(r, c).unwrap_or_default()
}

pub(super) fn b(r: &SqlRow, c: &str) -> bool {
    matches!(text(r, c).as_deref(), Some("true" | "t"))
}

pub(super) fn ob(r: &SqlRow, c: &str) -> Option<bool> {
    text(r, c).map(|v| v == "true" || v == "t")
}

pub(super) fn attrs_of(r: &SqlRow) -> Attrs {
    Attrs {
        login: b(r, "login"),
        superuser: b(r, "su"),
        createdb: b(r, "cdb"),
        createrole: b(r, "cr"),
        inherit: b(r, "inh"),
        replication: b(r, "repl"),
        bypassrls: b(r, "bypass"),
        conn_limit: t(r, "lim").parse().unwrap_or(-1),
        valid_until: t(r, "until"),
    }
}

pub(super) fn memberships(rows: &[SqlRow]) -> Vec<Membership> {
    let mut out: Vec<Membership> = Vec::new();
    for r in rows {
        let (g, m) = (t(r, "grp"), t(r, "member"));
        let o = Opts { admin: b(r, "adm"), inherit: ob(r, "inh"), set: ob(r, "setopt") };
        match out.iter_mut().find(|x| x.group == g && x.member == m) {
            Some(x) => {
                x.opts.admin |= o.admin;
                x.opts.inherit = x.opts.inherit.zip(o.inherit).map(|(a, b)| a || b).or(o.inherit);
                x.opts.set = x.opts.set.zip(o.set).map(|(a, b)| a || b).or(o.set);
                x.grantors.push(t(r, "grantor"));
            }
            None => out.push(Membership { group: g, member: m, opts: o, grantors: vec![t(r, "grantor")] }),
        }
    }
    out
}

/// 17 from "17.2" or "18beta1", the way `Db::version` holds it.
pub(super) fn major_of_version(v: &str) -> u32 {
    v.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0)
}

pub(super) fn major_of(version_num: &str) -> u32 {
    version_num.trim().parse::<u32>().map(|n| n / 10000).unwrap_or(0)
}

pub(super) async fn load_cluster(db: &Db) -> Res<Cluster> {
    let head = read(db, "", &[("SELECT current_user::text AS me, current_setting('server_version_num') AS v, current_setting('password_encryption') AS pe", Vec::new())]).await?.remove(0);
    let r = head.first().ok_or("The server gave no answer")?;
    let (me, major, password_encryption) = (t(r, "me"), major_of(&t(r, "v")), t(r, "pe"));
    let (roles, members) = (roles_sql(""), members_sql(major));
    let mut all = read(db, "", &[(roles.as_str(), Vec::new()), (members.as_str(), Vec::new()), (crate::db::sql::pg::DB_LIST, Vec::new())]).await?;
    let dbs = all.pop().unwrap_or_default();
    let members = memberships(&all.pop().unwrap_or_default());
    let roles = all.pop().unwrap_or_default().iter().map(|r| Role { oid: t(r, "oid"), name: t(r, "name"), attrs: attrs_of(r), admin: b(r, "adm") }).collect();
    let databases = dbs.iter().filter_map(|r| crate::db::sql::worker::text_at(r, 0)).collect();
    Ok(Cluster { major, me, roles, members, databases, password_encryption })
}

/// The ACL of a row as JSON triples, NULL read as the defaults the owner gets.
pub(super) fn acl_json(col: &str, ch: char, owner: &str) -> String {
    format!(
        "COALESCE((SELECT json_agg(json_build_array(CASE WHEN a.grantee = 0 THEN '' ELSE pg_get_userbyid(a.grantee)::text END, pg_get_userbyid(a.grantor)::text, a.privilege_type, a.is_grantable))::text \
         FROM aclexplode(COALESCE({col}, acldefault('{ch}', {owner}))) a), '[]')"
    )
}

pub(super) fn parse_acl(json: &str) -> Vec<AclEntry> {
    let rows: Vec<(String, String, String, bool)> = serde_json::from_str(json).unwrap_or_default();
    let mut out: Vec<AclEntry> = Vec::new();
    for (grantee, grantor, p, g) in rows {
        match out.iter_mut().find(|e| e.grantee == grantee && e.grantor == grantor) {
            Some(e) => {
                e.privs.insert(p, g);
            }
            None => out.push(AclEntry { grantee, grantor, privs: BTreeMap::from([(p, g)]) }),
        }
    }
    out
}

/// How many objects the matrix lists at most; a schema with more says so.
pub const OBJ_LIMIT: usize = 2000;

pub(super) const SCHEMAS: &str = "SELECT n.nspname::text AS name FROM pg_namespace n WHERE n.nspname !~ '^pg_' AND n.nspname <> 'information_schema' ORDER BY 1";
pub(super) const DB_OID: &str = "SELECT oid::text AS o FROM pg_database WHERE datname = current_database()";

pub(super) fn objects_sql(kind: Kind) -> String {
    let (_, col, owner, ch) = kind.catalog();
    let lim = OBJ_LIMIT + 1;
    match kind {
        Kind::Tables | Kind::Sequences => {
            let (kinds, detail) = if kind == Kind::Tables {
                ("'r','v','m','f','p'", "CASE c.relkind WHEN 'v' THEN 'view' WHEN 'm' THEN 'materialized view' WHEN 'f' THEN 'foreign table' WHEN 'p' THEN 'partitioned' ELSE '' END")
            } else {
                ("'S'", "''")
            };
            format!(
                "SELECT c.oid::text AS oid, c.relname::text AS name, '' AS sig, {detail} AS detail, pg_get_userbyid(c.{owner})::text AS owner, {} AS acl \
                 FROM pg_class c WHERE c.relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1) AND c.relkind IN ({kinds}) ORDER BY 2 LIMIT {lim}",
                acl_json(&format!("c.{col}"), ch, &format!("c.{owner}"))
            )
        }
        Kind::Functions => format!(
            "SELECT p.oid::text AS oid, p.proname::text AS name, replace(pg_get_function_identity_arguments(p.oid), ' ORDER BY ', ', ') AS sig, CASE p.prokind WHEN 'p' THEN 'procedure' WHEN 'a' THEN 'aggregate' WHEN 'w' THEN 'window' ELSE '' END AS detail, \
             pg_get_userbyid(p.proowner)::text AS owner, {} AS acl FROM pg_proc p WHERE p.pronamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1) ORDER BY 2, 3 LIMIT {lim}",
            acl_json("p.proacl", ch, "p.proowner")
        ),
        Kind::Schemas => format!(
            "SELECT n.oid::text AS oid, n.nspname::text AS name, '' AS sig, '' AS detail, pg_get_userbyid(n.nspowner)::text AS owner, {} AS acl \
             FROM pg_namespace n WHERE n.nspname !~ '^pg_' AND n.nspname <> 'information_schema' AND $1::text IS NOT NULL ORDER BY 2 LIMIT {lim}",
            acl_json("n.nspacl", ch, "n.nspowner")
        ),
        Kind::Databases => format!(
            "SELECT d.oid::text AS oid, d.datname::text AS name, '' AS sig, CASE WHEN d.datistemplate THEN 'template' ELSE '' END AS detail, pg_get_userbyid(d.datdba)::text AS owner, {} AS acl \
             FROM pg_database d WHERE d.datallowconn AND $1::text IS NOT NULL ORDER BY 2 LIMIT {lim}",
            acl_json("d.datacl", ch, "d.datdba")
        ),
    }
}

pub(super) async fn load_objects(db: &Db, schema: &str, kind: Kind) -> Res<Objects> {
    let sql = objects_sql(kind);
    let mut all = read(db, schema, &[(DB_OID, Vec::new()), (SCHEMAS, Vec::new()), (sql.as_str(), vec![Param::Text(schema.into())])]).await?;
    let rows = all.pop().unwrap_or_default();
    let schemas = all.pop().unwrap_or_default().iter().map(|r| t(r, "name")).collect();
    let db_oid = all.pop().unwrap_or_default().first().map(|r| t(r, "o")).unwrap_or_default();
    let cut = rows.len() > OBJ_LIMIT;
    let objs = rows
        .iter()
        .take(OBJ_LIMIT)
        .map(|r| Obj { oid: t(r, "oid"), schema: if kind.in_schema() { schema.to_string() } else { String::new() }, name: t(r, "name"), sig: t(r, "sig"), detail: t(r, "detail"), owner: t(r, "owner"), acl: parse_acl(&t(r, "acl")) })
        .collect();
    Ok(Objects { database: db.pg_database(), schema: schema.into(), kind, objs, schemas, db_oid, cut })
}

pub(super) const DEFAULTS: &str = "SELECT pg_get_userbyid(d.defaclrole)::text AS owner, COALESCE(n.nspname::text, '') AS schema, d.defaclobjtype::text AS kind, \
    COALESCE((SELECT json_agg(json_build_array(CASE WHEN a.grantee = 0 THEN '' ELSE pg_get_userbyid(a.grantee)::text END, pg_get_userbyid(a.grantor)::text, a.privilege_type, a.is_grantable))::text FROM aclexplode(d.defaclacl) a), '[]') AS acl \
    FROM pg_default_acl d LEFT JOIN pg_namespace n ON n.oid = d.defaclnamespace ORDER BY 1, 2, 3";

pub(super) async fn load_defaults(db: &Db) -> Res<Defaults> {
    let mut all = read(db, "", &[(DB_OID, Vec::new()), (SCHEMAS, Vec::new()), (DEFAULTS, Vec::new())]).await?;
    let rows = all.pop().unwrap_or_default().iter().filter_map(|r| Some(DefaultAcl { owner: t(r, "owner"), schema: t(r, "schema"), kind: DefKind::of(&t(r, "kind"))?, acl: parse_acl(&t(r, "acl")) })).collect();
    let schemas = all.pop().unwrap_or_default().iter().map(|r| t(r, "name")).collect();
    let db_oid = all.pop().unwrap_or_default().first().map(|r| t(r, "o")).unwrap_or_default();
    Ok(Defaults { database: db.pg_database(), db_oid, rows, schemas })
}

// Running a batch.

pub(super) const ROLE_OID: &str = "SELECT oid::text AS o FROM pg_roles WHERE rolname = $1";
/// The grantee as an oid, 0 for PUBLIC.
pub(super) const GRANTEE: &str = "CASE WHEN $2 = '' THEN 0::oid ELSE (SELECT oid FROM pg_roles WHERE rolname = $2) END";

pub(super) async fn rows(tx: &mut Tx<'_>, db: &Db, sql: &str, args: Vec<Param>) -> Res<Vec<SqlRow>> {
    let t0 = Instant::now();
    let r = tx.fetch_bound(sql, &args).await;
    db.note("", sql, t0, &r);
    r.map_err(se)
}

/// Privileges by grantor, from rows of `gr`, `p` and `g`.
pub(super) fn grants_of(rows: &[SqlRow]) -> Grantors<Privs> {
    let mut out = Grantors::<Privs>::new();
    for r in rows {
        *out.entry(t(r, "gr")).or_default().entry(t(r, "p")).or_insert(false) |= b(r, "g");
    }
    out
}

/// Whether the object is still the one the window read: same oid, name and schema, and for a function the same arguments.
pub(super) async fn target_ok(tx: &mut Tx<'_>, db: &Db, tg: &Target) -> Res<bool> {
    let (sql, args): (&str, Vec<Param>) = match tg.kind {
        Kind::Tables | Kind::Sequences => ("SELECT count(*)::text AS n FROM pg_class WHERE oid = $1::oid AND relname = $2 AND relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $3)", vec![Param::Text(tg.oid.clone()), Param::Text(tg.name.clone()), Param::Text(tg.schema.clone())]),
        Kind::Functions => (
            "SELECT count(*)::text AS n FROM pg_proc WHERE oid = $1::oid AND proname = $2 AND pronamespace = (SELECT oid FROM pg_namespace WHERE nspname = $3) AND replace(pg_get_function_identity_arguments(oid), ' ORDER BY ', ', ') = $4",
            vec![Param::Text(tg.oid.clone()), Param::Text(tg.name.clone()), Param::Text(tg.schema.clone()), Param::Text(tg.sig.clone())],
        ),
        Kind::Schemas => ("SELECT count(*)::text AS n FROM pg_namespace WHERE oid = $1::oid AND nspname = $2", vec![Param::Text(tg.oid.clone()), Param::Text(tg.name.clone())]),
        Kind::Databases => ("SELECT count(*)::text AS n FROM pg_database WHERE oid = $1::oid AND datname = $2", vec![Param::Text(tg.oid.clone()), Param::Text(tg.name.clone())]),
    };
    Ok(rows(tx, db, sql, args).await?.first().is_some_and(|r| t(r, "n") == "1"))
}

/// What a change looks at, read inside the transaction, grantor by grantor.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum State {
    Privs(Grantors<Privs>),
    Attrs(Attrs),
    Member(Grantors<Opts>),
}

pub(super) async fn state(tx: &mut Tx<'_>, db: &Db, major: u32, c: &Change) -> Res<State> {
    match c {
        Change::Acl { target, grantee, .. } => {
            let (cat, col, owner, ch) = target.kind.catalog();
            let sql = format!("SELECT pg_get_userbyid(a.grantor)::text AS gr, a.privilege_type AS p, a.is_grantable::text AS g FROM {cat} c, aclexplode(COALESCE(c.{col}, acldefault('{ch}', c.{owner}))) a WHERE c.oid = $1::oid AND a.grantee = {GRANTEE}");
            Ok(State::Privs(grants_of(&rows(tx, db, &sql, vec![Param::Text(target.oid.clone()), Param::Text(grantee.clone())]).await?)))
        }
        Change::Attrs { role, .. } => {
            let sql = roles_sql("WHERE r.rolname = $1");
            let rs = rows(tx, db, &sql, vec![Param::Text(role.clone())]).await?;
            let r = rs.first().ok_or_else(|| format!("Role {role} is gone."))?;
            Ok(State::Attrs(attrs_of(r)))
        }
        Change::Member { group, member, .. } => {
            let (inh, set) = if major >= 16 { ("a.inherit_option::text", "a.set_option::text") } else { ("NULL::text", "NULL::text") };
            let sql = format!(
                "SELECT COALESCE(gr.rolname::text, '') AS gr, a.admin_option::text AS adm, {inh} AS inh, {set} AS setopt FROM pg_auth_members a LEFT JOIN pg_roles gr ON gr.oid = a.grantor \
                 WHERE a.roleid = (SELECT oid FROM pg_roles WHERE rolname = $1) AND a.member = (SELECT oid FROM pg_roles WHERE rolname = $2)"
            );
            let rs = rows(tx, db, &sql, vec![Param::Text(group.clone()), Param::Text(member.clone())]).await?;
            // Before 16 there is one row, and a GRANT WITH ADMIN OPTION may rewrite its grantor, which means nothing there.
            Ok(State::Member(rs.iter().map(|r| (if major >= 16 { t(r, "gr") } else { String::new() }, Opts { admin: b(r, "adm"), inherit: ob(r, "inh"), set: ob(r, "setopt") })).collect()))
        }
        Change::Default { owner, schema, kind, grantee, .. } => {
            // Without a row for the whole database, new objects get the built-in defaults; a schema's row only adds to them.
            let sql = format!(
                "WITH d AS (SELECT defaclacl FROM pg_default_acl WHERE defaclrole = (SELECT oid FROM pg_roles WHERE rolname = $1) AND defaclobjtype = $3 \
                 AND defaclnamespace = CASE WHEN $4 = '' THEN 0::oid ELSE (SELECT oid FROM pg_namespace WHERE nspname = $4) END) \
                 SELECT pg_get_userbyid(a.grantor)::text AS gr, a.privilege_type AS p, a.is_grantable::text AS g FROM aclexplode(COALESCE((SELECT defaclacl FROM d), CASE WHEN $4 = '' THEN acldefault('{}', (SELECT oid FROM pg_roles WHERE rolname = $1)) END)) a \
                 WHERE a.grantee = {GRANTEE}",
                kind.acl_char()
            );
            let args = vec![Param::Text(owner.clone()), Param::Text(grantee.clone()), Param::Text(kind.code().into()), Param::Text(schema.clone())];
            Ok(State::Privs(grants_of(&rows(tx, db, &sql, args).await?)))
        }
    }
}

impl State {
    pub(super) fn text(&self) -> String {
        match self {
            State::Privs(p) => privs_text(&merge_privs(p)),
            State::Attrs(a) => a.text(),
            State::Member(o) => Opts::text(&merge_opts(o)),
        }
    }
}

/// Whether `now` is what `c` expects before (`before`) or after it runs: grantor by grantor when the run recorded its exact
/// state, else merged, as the window shows it.
pub(super) fn holds(c: &Change, exact: Option<&Exact>, now: &State, before: bool) -> bool {
    match (c, exact, now) {
        (_, Some(Exact::Acl(a, b)), State::Privs(p)) => p == if before { a } else { b },
        (_, Some(Exact::Member(a, b)), State::Member(m)) => m == if before { a } else { b },
        (Change::Acl { from, to, .. } | Change::Default { from, to, .. }, None, State::Privs(p)) => merge_privs(p) == *if before { from } else { to },
        (Change::Attrs { from, to, .. }, None, State::Attrs(a)) => a.same_touched(if before { from } else { to }, from, to),
        (Change::Member { from, to, .. }, None, State::Member(m)) => merge_opts(m) == *if before { from } else { to },
        _ => false,
    }
}

pub(super) fn exact_of(before: &State, after: &State) -> Option<Exact> {
    match (before, after) {
        (State::Privs(a), State::Privs(b)) => Some(Exact::Acl(a.clone(), b.clone())),
        (State::Member(a), State::Member(b)) => Some(Exact::Member(a.clone(), b.clone())),
        _ => None,
    }
}

/// What a change is about, for messages.
pub(super) fn subject(c: &Change) -> String {
    match c {
        Change::Acl { target, grantee, .. } => format!("what {} holds on {}", grantee_shown(grantee), target.shown()),
        Change::Attrs { role, .. } => format!("role {role}"),
        Change::Member { group, member, .. } => format!("the membership of {member} in {group}"),
        Change::Default { owner, schema, kind, grantee, .. } => format!("the default privileges {owner} gives {} on new {}{}", grantee_shown(grantee), kind.label().to_lowercase(), if schema.is_empty() { String::new() } else { format!(" in {schema}") }),
    }
}

/// VALID UNTIL as typed, read by the server in this session and written back as `UNTIL` reads it, so the check after the run and
/// the undo compare like with like, with whether that moment has passed. A value the written form would not give back (a
/// fraction of a second, a year BC) is refused rather than changed.
pub(super) async fn canonical_until(tx: &mut Tx<'_>, db: &Db, typed: &str) -> Res<(String, bool)> {
    let v = typed.trim();
    if v.is_empty() || v.eq_ignore_ascii_case("never") || v.eq_ignore_ascii_case("infinity") {
        return Ok((String::new(), false));
    }
    // The window shows UTC, and the session may run in the computer's zone (a value without an offset reads in it), so the cast reads in UTC.
    // set_config with is_local is SET LOCAL, which also holds behind a pooler because it ends with the transaction.
    rows(tx, db, "SELECT set_config('TimeZone', 'UTC', true)", Vec::new()).await?;
    let sql = "WITH c AS (SELECT ts, CASE WHEN ts = 'infinity' THEN '' WHEN ts = '-infinity' THEN '-infinity' ELSE to_char(ts AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS') || '+00' END AS v FROM (SELECT $1::timestamptz AS ts) t) \
               SELECT v, (CASE WHEN v = '' THEN ts = 'infinity' ELSE v::timestamptz = ts END)::text AS same, (ts <= now())::text AS past FROM c";
    let rs = rows(tx, db, sql, vec![Param::Text(v.into())]).await.map_err(|e| format!("Valid until: {e}"))?;
    let r = rs.first().ok_or("The server gave no answer")?;
    if !b(r, "same") {
        return Err(format!("Valid until {v} reads as {}, which is not the same moment: Quarry keeps whole seconds, in UTC, of years AD. Nothing was changed.", until_shown(&t(r, "v"))));
    }
    Ok((t(r, "v"), b(r, "past")))
}

/// Why a table REVOKE in `c` is refused: PostgreSQL revokes a privilege, or its grant option, on every column of the table along
/// with the table's, and nothing here would give those column grants back.
pub(super) async fn column_grants(tx: &mut Tx<'_>, db: &Db, c: &Change, exact: Option<&Exact>) -> Res<Option<String>> {
    let (Change::Acl { target, grantee, .. }, Some((from, to))) = (c, acl_pair(c, exact)?) else { return Ok(None) };
    if target.kind != Kind::Tables {
        return Ok(None);
    }
    // A privilege revoked, or only its grant option (true when `to` still holds it).
    let lost: Vec<(&String, bool)> = from.iter().filter(|(p, go)| to.get(*p).is_none_or(|g| **go && !*g)).map(|(p, _)| (p, to.contains_key(p))).collect();
    if lost.is_empty() {
        return Ok(None);
    }
    let sql = format!(
        "SELECT a.privilege_type AS p, bool_or(a.is_grantable)::text AS g, string_agg(DISTINCT att.attname::text, ', ') AS cols FROM pg_attribute att, aclexplode(att.attacl) a \
         WHERE att.attrelid = $1::oid AND att.attnum > 0 AND NOT att.attisdropped AND att.attacl IS NOT NULL AND a.grantee = {GRANTEE} GROUP BY 1 ORDER BY 1"
    );
    for r in rows(tx, db, &sql, vec![Param::Text(target.oid.clone()), Param::Text(grantee.clone())]).await? {
        let p = t(&r, "p");
        if lost.iter().any(|(l, only_go)| **l == p && (!only_go || b(&r, "g"))) {
            return Ok(Some(format!(
                "{} also holds {p} on columns of {} ({}). A REVOKE on the table takes those column grants too, and Quarry could not give them back. Revoke the column grants first, or keep {p}.",
                grantee_shown(grantee),
                target.shown(),
                t(&r, "cols")
            )));
        }
    }
    Ok(None)
}

pub(super) async fn check_roles(tx: &mut Tx<'_>, db: &Db, roles: &[(String, String)], c: &[Change]) -> Res<()> {
    let named: BTreeSet<&str> = c.iter().flat_map(Change::roles).collect();
    for (name, oid) in roles.iter().filter(|(n, _)| named.contains(n.as_str())) {
        let now = rows(tx, db, ROLE_OID, vec![Param::Text(name.clone())]).await?.first().map(|r| t(r, "o"));
        match now {
            None => return Err(format!("Role {name} is gone.")),
            Some(o) if !oid.is_empty() && o != *oid => return Err(format!("Role {name} was dropped and made again since the window read it.")),
            _ => {}
        }
    }
    Ok(())
}

/// Runs a batch in one transaction: every check, then every statement, then every check of the result. `undoing` words the
/// refusals for an undo. Returns the batch as it ran, with VALID UNTIL written the way the server reads it back and the exact
/// state of each change, which its undo restores.
pub(super) async fn run_batch(db: &Db, mut b: Batch, read_only: bool, undoing: bool) -> Res<Batch> {
    let major = major_of_version(&db.version);
    let mut conn = writer_conn(db, "", read_only).await?;
    let out = async {
        let mut tx = conn.begin_write(db).await.map_err(se)?;
        let res = async {
            set_lock_wait(&mut tx, db, "").await.map_err(se)?;
            let refuse = |why: String| if undoing { format!("Undo refused: {why} Nothing was changed.") } else { format!("{why} Nothing was changed: refresh and stage it again.") };
            if !b.db_oid.is_empty() {
                let now = rows(&mut tx, db, DB_OID, Vec::new()).await?.first().map(|r| t(r, "o")).unwrap_or_default();
                if now != b.db_oid {
                    return Err(refuse(format!("database {} was dropped and made again since.", b.database)));
                }
            }
            check_roles(&mut tx, db, &b.roles, &b.changes).await.map_err(refuse)?;
            let me = rows(&mut tx, db, "SELECT current_user::text AS me", Vec::new()).await?.first().map(|r| t(r, "me")).unwrap_or_default();
            for c in b.changes.iter_mut() {
                if let Some(why) = Cluster::self_lockout(&me, c) {
                    return Err(refuse(why));
                }
                if let Change::Attrs { role, from, to } = c && from.valid_until != to.valid_until {
                    let (until, past) = canonical_until(&mut tx, db, &to.valid_until).await?;
                    if past && *role == me {
                        return Err(refuse(format!("{me} is the role you are signed in as. A VALID UNTIL that has passed would stop it signing in, and then nothing could undo it.")));
                    }
                    to.valid_until = until;
                }
            }
            let given = std::mem::take(&mut b.exact);
            let ex = |i: usize| given.get(i).and_then(Option::as_ref);
            let mut before = Vec::new();
            for (i, c) in b.changes.iter().enumerate() {
                if let Change::Acl { target, .. } = c && !target_ok(&mut tx, db, target).await? {
                    return Err(refuse(format!("{} was dropped, renamed or made again since.", target.shown())));
                }
                let now = state(&mut tx, db, major, c).await?;
                if !holds(c, ex(i), &now, true) {
                    let when = if undoing { "after this change ran" } else { "after the window read it" };
                    return Err(refuse(format!("{} changed {when} (it is {} now).", subject(c), now.text())));
                }
                if let Some(why) = column_grants(&mut tx, db, c, ex(i)).await? {
                    return Err(refuse(why));
                }
                before.push(now);
            }
            for (i, c) in b.changes.iter().enumerate() {
                for s in stmts(c, ex(i), major).map_err(refuse)? {
                    exec(&mut tx, db, "", &s).await.map_err(se)?;
                }
            }
            let mut exact = Vec::new();
            for (i, c) in b.changes.iter().enumerate() {
                let now = state(&mut tx, db, major, c).await?;
                if !holds(c, ex(i), &now, false) {
                    return Err(format!(
                        "After the statements ran, {} was {} rather than what they were meant to leave. A grant made by another role is revoked only by that role, so it may still be there. Nothing was changed.",
                        subject(c),
                        now.text()
                    ));
                }
                exact.push(exact_of(&before[i], &now));
            }
            b.exact = exact;
            Ok(())
        }
        .await;
        match res {
            Ok(()) => tx.commit().await.map_err(|e| commit_unknown(e, "Refresh the window and check before running it again.")),
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }
    .await;
    let _ = conn.close().await;
    out.map(|_| b)
}

// Creating, dropping and passwords.

/// What goes out for `role`'s password, as the server's password_encryption asks: the SCRAM verifier, with the settings read
/// first and the hashing on a blocking thread, all before the write transaction opens, so neither holds a worker or the
/// transaction for the length of PBKDF2; or the MD5 hash.
pub(super) async fn verifier(db: &Db, role: &str, password: &str) -> Res<zeroize::Zeroizing<String>> {
    const SETTINGS: &str = "SELECT current_setting('password_encryption') AS pe, COALESCE(current_setting('scram_iterations', true), '4096') AS n";
    let rs = read(db, "", &[(SETTINGS, Vec::new())]).await?.remove(0);
    let r = rs.first().ok_or("The server gave no answer")?;
    if wants_md5(&t(r, "pe")) {
        return Ok(zeroize::Zeroizing::new(md5_verifier(password, role)));
    }
    let n = check_iterations(&t(r, "n"))?;
    let (pw, salt) = (zeroize::Zeroizing::new(password.to_string()), new_salt());
    tokio::task::spawn_blocking(move || zeroize::Zeroizing::new(scram_verifier(&pw, &salt, n))).await.map_err(|e| format!("Computing the password's verifier failed: {e}"))
}

/// The server's error on a statement with a password, with the password and verifier cut out. A policy extension can refuse a
/// verifier where it wants the password in clear, so that is said.
pub(super) fn password_err(e: String, secrets: &[&str]) -> String {
    let e = scrub(e, secrets);
    if e.to_lowercase().contains("password") || e.to_lowercase().contains("encrypt") {
        return format!("{e}. Quarry sends a password only as its hash, and a password policy extension such as credcheck may refuse one sent that way.");
    }
    e
}

/// How a role stood right after this app made it, which its undo must still find.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Made {
    pub name: String,
    pub oid: String,
    /// The roles it is in, and the roles in it (a CREATEROLE creator gets ADMIN on it from 16).
    pub groups: Vec<String>,
    pub members: Vec<String>,
}

pub(super) const MADE_GROUPS: &str = "SELECT g.rolname::text AS n FROM pg_auth_members a JOIN pg_roles g ON g.oid = a.roleid WHERE a.member = $1::oid ORDER BY 1";
pub(super) const MADE_MEMBERS: &str = "SELECT m.rolname::text AS n FROM pg_auth_members a JOIN pg_roles m ON m.oid = a.member WHERE a.roleid = $1::oid ORDER BY 1";
/// Whatever the role owns or holds in any database or on the server, and its per-role settings.
pub(super) const HOLDS: &str = "SELECT ((SELECT count(*) FROM pg_shdepend WHERE refclassid = 'pg_authid'::regclass AND refobjid = $1::oid) + (SELECT count(*) FROM pg_db_role_setting WHERE setrole = $1::oid))::text AS n";

pub(super) async fn names(tx: &mut Tx<'_>, db: &Db, sql: &str, oid: &str) -> Res<Vec<String>> {
    Ok(rows(tx, db, sql, vec![Param::Text(oid.into())]).await?.iter().map(|r| t(r, "n")).collect())
}

pub(super) async fn in_write<T>(db: &Db, read_only: bool, body: impl AsyncFnOnce(&mut Tx<'_>) -> Res<T>) -> Res<T> {
    let mut conn = writer_conn(db, "", read_only).await?;
    let out = async {
        let mut tx = conn.begin_write(db).await.map_err(se)?;
        let res = async {
            set_lock_wait(&mut tx, db, "").await.map_err(se)?;
            body(&mut tx).await
        }
        .await;
        match res {
            Ok(v) => tx.commit().await.map(|_| v).map_err(|e| commit_unknown(e, "Refresh the window and check before running it again.")),
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }
    .await;
    let _ = conn.close().await;
    out
}

pub(super) async fn create_role(db: &Db, n: &NewRole, password: &str, read_only: bool) -> Res<Made> {
    create_sql(n, None)?;
    if read_only {
        return Err(LOCKED.into());
    }
    let secret = if password.is_empty() { None } else { Some(verifier(db, &n.name, password).await?) };
    in_write(db, read_only, async |tx| {
        let sql = zeroize::Zeroizing::new(create_sql(n, secret.as_deref().map(String::as_str))?);
        exec(tx, db, "", &sql).await.map_err(|e| password_err(se(e), &[password, secret.as_deref().map(String::as_str).unwrap_or_default()]))?;
        let oid = rows(tx, db, ROLE_OID, vec![Param::Text(n.name.clone())]).await?.first().map(|r| t(r, "o")).ok_or("The new role cannot be read back")?;
        Ok(Made { name: n.name.clone(), groups: names(tx, db, MADE_GROUPS, &oid).await?, members: names(tx, db, MADE_MEMBERS, &oid).await?, oid })
    })
    .await
}

pub(super) async fn undo_created(db: &Db, m: &Made, read_only: bool) -> Res<()> {
    in_write(db, read_only, async |tx| {
        let now = rows(tx, db, ROLE_OID, vec![Param::Text(m.name.clone())]).await?.first().map(|r| t(r, "o"));
        match now {
            None => return Err(format!("Undo refused: role {} is gone already.", m.name)),
            Some(o) if o != m.oid => return Err(format!("Undo refused: role {} was dropped and made again since, so it is not the role this created.", m.name)),
            _ => {}
        }
        let holds = rows(tx, db, HOLDS, vec![Param::Text(m.oid.clone())]).await?.first().map(|r| t(r, "n")).unwrap_or_default();
        if holds != "0" {
            return Err(format!("Not undone: {} owns objects, holds privileges or has settings now, so it is not dropped.", m.name));
        }
        if names(tx, db, MADE_GROUPS, &m.oid).await? != m.groups || names(tx, db, MADE_MEMBERS, &m.oid).await? != m.members {
            return Err(format!("Not undone: the memberships of {} changed since it was created, so it is not dropped.", m.name));
        }
        exec(tx, db, "", &format!("DROP ROLE {}", q(&m.name))).await.map_err(se)
    })
    .await
}

pub(super) async fn set_password(db: &Db, role: &str, oid: &str, password: &str, read_only: bool) -> Res<()> {
    check_password(password)?;
    if read_only {
        return Err(LOCKED.into());
    }
    let secret = verifier(db, role, password).await?;
    in_write(db, read_only, async |tx| {
        let now = rows(tx, db, ROLE_OID, vec![Param::Text(role.into())]).await?.first().map(|r| t(r, "o"));
        if now.as_deref() != Some(oid) {
            return Err(format!("Role {role} is gone or was made again since the window read it. Nothing was changed."));
        }
        let sql = zeroize::Zeroizing::new(password_sql(role, &secret));
        exec(tx, db, "", &sql).await.map_err(|e| password_err(se(e), &[password, &secret]))
    })
    .await
}

/// What a role owns and holds in one database.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DbHold {
    pub database: String,
    /// What `pg_describe_object` calls each owned object.
    pub owned: Vec<String>,
    /// Privileges and default privileges held there.
    pub held: usize,
    /// Every pg_shdepend row of the role in this database as `classid:objid:objsubid:deptype`, sorted. The drop finds the same set
    /// again before it runs, so an object swapped for another between the dialog and the run stops it.
    pub deps: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DropInfo {
    pub role: String,
    pub oid: String,
    pub dbs: Vec<DbHold>,
    /// Databases (and tablespaces) the role owns: shared objects that DROP OWNED does not remove.
    pub shared: Vec<String>,
    /// Shared objects the role holds privileges on (a database's CONNECT, a tablespace, a parameter). DROP OWNED in any one
    /// database revokes them.
    pub shared_held: Vec<String>,
    /// The pg_shdepend rows of the shared objects, as in `DbHold::deps`.
    pub shared_deps: Vec<String>,
    /// Databases with something of the role that could not be read, with the reason.
    pub unread: Vec<(String, String)>,
}

impl DropInfo {
    pub fn owned_count(&self) -> usize {
        self.dbs.iter().map(|d| d.owned.len()).sum::<usize>() + self.shared.len()
    }
}

/// What to do with what the role owns before DROP ROLE.
#[derive(Clone, Debug, PartialEq)]
pub enum DropMode {
    Reassign(String),
    DropOwned,
}

/// The statements of a drop, per database ("" for the connection's own), DROP ROLE last.
pub fn drop_plan(info: &DropInfo, mode: &DropMode, home: &str) -> Res<Vec<(String, Vec<String>)>> {
    if let Some((d, why)) = info.unread.first() {
        return Err(format!("{} has objects or privileges in {d}, which could not be read ({why}), so Quarry cannot tell what dropping it would do.", info.role));
    }
    let r = q(&info.role);
    let steps = |to: &DropMode| match to {
        DropMode::Reassign(t) => vec![format!("REASSIGN OWNED BY {r} TO {}", q(t)), format!("DROP OWNED BY {r}")],
        DropMode::DropOwned => vec![format!("DROP OWNED BY {r}")],
    };
    if let DropMode::Reassign(t) = mode && (t.is_empty() || *t == info.role) {
        return Err("Pick the role that gets the objects.".into());
    }
    if *mode == DropMode::DropOwned && !info.shared.is_empty() {
        return Err(format!("{} owns {}, which DROP OWNED does not drop. Give the objects to another role, or drop that first.", info.role, info.shared.join(", ")));
    }
    let mut plan: Vec<(String, Vec<String>)> = info.dbs.iter().map(|d| (d.database.clone(), steps(mode))).collect();
    // A shared object is reached from any database: REASSIGN OWNED moves a database owned, DROP OWNED revokes a CONNECT held.
    if plan.is_empty() && (!info.shared.is_empty() || !info.shared_held.is_empty()) {
        plan.push((home.to_string(), steps(mode)));
    }
    plan.push((String::new(), vec![format!("DROP ROLE {r}")]));
    Ok(plan)
}

pub fn drop_preview(plan: &[(String, Vec<String>)]) -> String {
    let mut out = Vec::new();
    for (d, stmts) in plan {
        out.push(if d.is_empty() { "-- then, once".to_string() } else { format!("-- in {d}") });
        out.extend(stmts.iter().map(|s| format!("{s};")));
    }
    out.join("\n")
}

/// The role's pg_shdepend rows in this database (`{}` is `current_database()`) or on the server (`0`), with what each is.
pub(super) fn hold_sql(dbid: &str) -> String {
    format!(
        "SELECT pg_describe_object(classid, objid, objsubid) AS what, deptype::text AS dep, classid::text || ':' || objid::text || ':' || objsubid::text || ':' || deptype::text AS k \
         FROM pg_shdepend WHERE dbid = {dbid} AND refclassid = 'pg_authid'::regclass AND refobjid = $1::oid"
    )
}

pub(super) const IN_DB: &str = "(SELECT oid FROM pg_database WHERE datname = current_database())";
pub(super) const HOLD_DBS: &str = "SELECT d.datname::text AS name FROM pg_shdepend s JOIN pg_database d ON d.oid = s.dbid WHERE s.refclassid = 'pg_authid'::regclass AND s.refobjid = $1::oid GROUP BY 1 ORDER BY 1";

pub(super) fn deps_of(rs: &[SqlRow]) -> Vec<String> {
    let mut v: Vec<String> = rs.iter().map(|r| t(r, "k")).collect();
    v.sort();
    v
}

/// (owned, held) descriptions, each sorted.
pub(super) fn split_hold(rs: &[SqlRow]) -> (Vec<String>, Vec<String>) {
    let (mut owned, mut held) = (Vec::new(), Vec::new());
    for r in rs {
        if t(r, "dep") == "o" { owned.push(t(r, "what")) } else { held.push(t(r, "what")) }
    }
    owned.sort();
    held.sort();
    (owned, held)
}

pub(super) fn hold_of(database: &str, rs: &[SqlRow]) -> DbHold {
    let (owned, held) = split_hold(rs);
    DbHold { database: database.into(), owned, held: held.len(), deps: deps_of(rs) }
}

pub(super) async fn drop_info(db: &Db, role: &str) -> Res<DropInfo> {
    let oid = read(db, "", &[(ROLE_OID, vec![Param::Text(role.into())])]).await?.remove(0).first().map(|r| t(r, "o")).ok_or_else(|| format!("Role {role} is gone."))?;
    let shared_sql = hold_sql("0");
    let mut all = read(db, "", &[(shared_sql.as_str(), vec![Param::Text(oid.clone())]), (HOLD_DBS, vec![Param::Text(oid.clone())])]).await?;
    let dbs = all.pop().unwrap_or_default();
    let shared_rows = all.pop().unwrap_or_default();
    let (shared, shared_held) = split_hold(&shared_rows);
    let mut info = DropInfo { role: role.into(), oid: oid.clone(), shared, shared_held, shared_deps: deps_of(&shared_rows), ..Default::default() };
    // Only the databases where the role has something are opened, each through its own pool.
    let in_db = hold_sql(IN_DB);
    for name in dbs.iter().map(|r| t(r, "name")) {
        match read(&db.at(&name), "", &[(in_db.as_str(), vec![Param::Text(oid.clone())])]).await {
            Ok(mut v) => info.dbs.push(hold_of(&name, &v.remove(0))),
            Err(e) => info.unread.push((name, e)),
        }
    }
    Ok(info)
}

/// Runs a drop: each database's statements in a transaction of their own on that database, after checking it holds what the
/// window showed, then DROP ROLE. The first step also checks the shared objects, which its DROP OWNED or REASSIGN OWNED reaches.
/// No undo: what DROP OWNED removed and the password are gone.
pub(super) async fn drop_role(db: &Db, info: &DropInfo, mode: &DropMode, read_only: bool) -> Res<String> {
    let home = db.pg_database();
    let plan = drop_plan(info, mode, &home)?;
    let mut done: Vec<String> = Vec::new();
    let (in_db, shared_sql) = (hold_sql(IN_DB), hold_sql("0"));
    for (i, (database, stmts)) in plan.iter().enumerate() {
        let at = if database.is_empty() { db.clone() } else { db.at(database) };
        let want = info.dbs.iter().find(|d| d.database == *database).map(|d| &d.deps);
        let r = in_write(&at, read_only, async |tx| {
            let now = rows(tx, &at, ROLE_OID, vec![Param::Text(info.role.clone())]).await?.first().map(|r| t(r, "o"));
            if now.as_deref() != Some(info.oid.as_str()) {
                return Err(format!("Role {} is gone or was made again since the window read it.", info.role));
            }
            if let Some(want) = want && deps_of(&rows(tx, &at, &in_db, vec![Param::Text(info.oid.clone())]).await?) != *want {
                return Err(format!("What {} owns or holds in {database} changed since the window read it. Look again before dropping it.", info.role));
            }
            if i == 0 && deps_of(&rows(tx, &at, &shared_sql, vec![Param::Text(info.oid.clone())]).await?) != info.shared_deps {
                return Err(format!("The databases, tablespaces or parameters {} owns or holds privileges on changed since the window read it. Look again before dropping it.", info.role));
            }
            for s in stmts {
                exec(tx, &at, "", s).await.map_err(se)?;
            }
            Ok(())
        })
        .await;
        if let Err(e) = r {
            let place = if database.is_empty() { String::new() } else { format!(" in {database}") };
            let before = if done.is_empty() { "Nothing was changed.".to_string() } else { format!("Already done in {}, which stays done. The role was not dropped.", done.join(", ")) };
            return Err(format!("{e} (stopped{place}). {before}"));
        }
        if !database.is_empty() {
            done.push(database.clone());
        }
    }
    Ok(format!("Dropped role {}", info.role))
}
