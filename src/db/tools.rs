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

/// Whether a URI option's value is a secret. authMechanismProperties is one only when it carries an AWS session token;
/// otherwise it holds plain names such as SERVICE_NAME.
fn secret_opt(k: &str, v: &str) -> bool {
    !v.is_empty() && (["proxyPassword", "tlsCertificateKeyFilePassword"].iter().any(|s| s.eq_ignore_ascii_case(k)) || (k.eq_ignore_ascii_case("authMechanismProperties") && v.to_ascii_uppercase().contains("AWS_SESSION_TOKEN")))
}

/// Where the options start. Hosts never hold "/", so the first "?" after the first "/" past the userinfo is it: a
/// password may hold "?" and an option value may hold "@". Without a path, the last "@" bounds the userinfo.
fn query_at(uri: &str) -> Option<usize> {
    let s = uri.find("://").map_or(0, |i| i + 3);
    let from = uri.rfind('@').filter(|&at| at > s && !uri[s..at].contains('/')).unwrap_or(s);
    match uri[from..].find('/') {
        Some(p) => uri[from + p..].find('?').map(|i| from + p + i),
        None => uri[from..].find('?').map(|i| from + i),
    }
}

/// The raw values of the secret options in a URI, for hiding them from text.
pub fn secret_opt_values(uri: &str) -> Vec<String> {
    let Some(i) = query_at(uri) else { return Vec::new() };
    let mut out = Vec::new();
    for (_, v) in uri[i + 1..].split('&').filter_map(|p| p.split_once('=')).filter(|(k, v)| secret_opt(k, v)) {
        // authMechanismProperties is name:value pairs; text may quote a secret value alone.
        for (name, value) in super::pct::decode(v).split(',').filter_map(|p| p.split_once(':').map(|(a, b)| (a.trim().to_string(), b.to_string()))) {
            if name.eq_ignore_ascii_case("AWS_SESSION_TOKEN") && !value.is_empty() {
                out.push(value);
            }
        }
        out.push(v.to_string());
    }
    out
}

pub fn mask_uri(uri: &str) -> String {
    let (head, query) = match query_at(uri) {
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
            Some((k, v)) if secret_opt(k, v) => format!("{k}=****"),
            _ => p.to_string(),
        }).collect();
        out.push('?');
        out.push_str(&parts.join("&"));
    }
    out
}
