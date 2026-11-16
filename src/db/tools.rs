//! mongodump and mongorestore arguments, and URIs made safe to show or pass to them.

/// The address without a database path: mongorestore treats one as an implicit --db and then
/// ignores --nsInclude (getMongorestoreCommand, renderer line 135880).
pub fn uri_without_db(uri: &str) -> String {
    let Some(s) = uri.find("://").map(|i| i + 3) else { return uri.into() };
    let Some(slash) = uri[s..].find('/').map(|i| s + i) else { return uri.into() };
    let q = uri[slash..].find('?').map_or(uri.len(), |i| slash + i);
    let db = &uri[slash + 1..q];
    let opts = uri[q..].trim_start_matches('?');
    let key = |k: &str| opts.split('&').any(|o| o.split('=').next().is_some_and(|n| n.eq_ignore_ascii_case(k)));
    // The path database is also the default auth database, so keep it as authSource. SRV
    // addresses are left alone because their TXT record usually sets authSource, which wins over
    // the path, and the external mechanisms never authenticate against the path database.
    let external = opts.split('&').any(|o| {
        o.split_once('=').is_some_and(|(k, v)| k.eq_ignore_ascii_case("authMechanism") && ["MONGODB-X509", "GSSAPI", "MONGODB-AWS", "MONGODB-OIDC"].contains(&v))
    });
    if !db.is_empty() && uri.starts_with("mongodb://") && uri[s..slash].contains('@') && !key("authSource") && !external {
        let sep = if opts.is_empty() { "" } else { "&" };
        return format!("{}/?{opts}{sep}authSource={db}", &uri[..slash]);
    }
    format!("{}/{}", &uri[..slash], &uri[q..])
}

/// mongodump arguments as the original builds them (getMongodumpCommand, renderer line 135838).
/// They are passed straight to the program, so no shell quoting is needed.
pub fn dump_args(uri: &str, db: &str, coll: Option<&str>, archive: &str, gzip: bool) -> Vec<String> {
    // A path database that differs from --db makes mongodump refuse the pair, so it goes as with mongorestore.
    let mut a = vec![format!("--uri={}", uri_without_db(uri)), format!("--archive={archive}"), format!("--db={db}")];
    if let Some(c) = coll {
        a.push(format!("--collection={c}"));
    }
    if gzip {
        a.push("--gzip".into());
    }
    a.push("--forceTableScan".into());
    a
}

/// The YAML that mongodump and mongorestore read with --config. A JSON string is also a valid
/// YAML scalar, so any character in the address is quoted safely.
pub fn tool_config(uri: &str) -> String {
    format!("uri: {}\n", serde_json::Value::from(uri))
}

/// Writes the tool config to a fresh temp file that only the user can read.
pub(crate) fn write_tool_config(uri: &str) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let path = std::env::temp_dir().join(format!("quarry-tool-{}-{nanos}.yaml", std::process::id()));
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    // Windows temp folders are already private to the user.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(&path)?.write_all(tool_config(uri).as_bytes())?;
    Ok(path)
}

/// mongorestore arguments (getMongorestoreCommand, renderer line 135878): only `from_db` (or one
/// collection of it) is restored, renamed into `to_db` when the names differ.
pub fn restore_args(uri: &str, archive: &str, from_db: &str, coll: Option<&str>, to_db: &str, drop: bool, gzip: bool) -> Vec<String> {
    let ns = coll.map_or("*".to_string(), str::to_string);
    let mut a = vec![format!("--uri={}", uri_without_db(uri)), format!("--archive={archive}"), format!("--nsInclude={from_db}.{ns}")];
    if from_db != to_db {
        a.push(format!("--nsFrom={from_db}.{ns}"));
        a.push(format!("--nsTo={to_db}.{ns}"));
    }
    if drop {
        a.push("--drop".into());
    }
    if gzip {
        a.push("--gzip".into());
    }
    a
}

/// URI options whose value is a secret or can hold one (authMechanismProperties carries AWS session tokens).
const SECRET_OPTS: [&str; 3] = ["proxyPassword", "tlsCertificateKeyFilePassword", "authMechanismProperties"];

/// The raw values of the secret options in a URI, for hiding them from text.
pub fn secret_opt_values(uri: &str) -> Vec<String> {
    let Some((_, q)) = uri.split_once('?') else { return Vec::new() };
    q.split('&').filter_map(|p| p.split_once('=')).filter(|(k, v)| !v.is_empty() && SECRET_OPTS.iter().any(|s| s.eq_ignore_ascii_case(k))).map(|(_, v)| v.to_string()).collect()
}

pub fn mask_uri(uri: &str) -> String {
    // Options come after the last "?" that follows the authority, so a password holding "?" stays in the userinfo.
    let (head, query) = match uri.rfind('@').map_or(uri.find('?'), |at| uri[at..].find('?').map(|i| at + i)) {
        Some(i) => (&uri[..i], Some(&uri[i + 1..])),
        None => (uri, None),
    };
    // Hide the password between "user:" and "@".
    let mut out = head.to_string();
    if let (Some(s), Some(at)) = (head.find("://"), head.rfind('@')) && let Some(colon) = head[s + 3..at].find(':') {
        out = format!("{}****{}", &head[..s + 3 + colon + 1], &head[at..]);
    }
    if let Some(q) = query {
        let parts: Vec<String> = q.split('&').map(|p| match p.split_once('=') {
            Some((k, v)) if !v.is_empty() && SECRET_OPTS.iter().any(|s| s.eq_ignore_ascii_case(k)) => format!("{k}=****"),
            _ => p.to_string(),
        }).collect();
        out.push('?');
        out.push_str(&parts.join("&"));
    }
    out
}
