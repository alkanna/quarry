//! `agents/agents.json`, signed with a key kept in the keychain. An agent with a shell could otherwise edit the
//! file to give itself access, so a missing or wrong signature turns every connection back to Not shared.
//!
//! The file is `{"config": {...}, "sig": "<hex>"}` and the signature covers the compact JSON of `config` as read,
//! so fields added by a newer Quarry keep an older signature valid.

use super::config::{Access, AgentsConfig};
use super::link;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use std::path::Path;

pub const FILE: &str = "agents.json";

/// The signing key, read from the keychain on first use and created there if absent. None when the keychain
/// refuses. Never called by the demo or the tests.
pub fn key() -> Option<&'static [u8; 32]> {
    static KEY: std::sync::OnceLock<Option<[u8; 32]>> = std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        match crate::vault::get("agents-config-key") {
            Ok(hex) => link::unhex(&hex).and_then(|b| b.try_into().ok()),
            Err(keyring::Error::NoEntry) => {
                let key = link::random();
                crate::vault::set("agents-config-key", &link::hex(&key)).ok()?;
                Some(key)
            }
            Err(_) => None,
        }
    })
    .as_ref()
}

fn mac(key: &[u8; 32], config: &Value) -> Hmac<Sha256> {
    let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC takes any key length");
    m.update(&serde_json::to_vec(config).unwrap_or_default());
    m
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// Signed by this install, or no file yet (a first run is not tampering).
    Signed,
    /// Edited outside Quarry, signed by another key, unreadable or unsigned.
    Tampered,
    /// A file exists but the keychain would not hand over the key, so it cannot be checked.
    NoKeychain,
}

/// The saved settings and whether they can be trusted. Anything not signed by this install comes back with agents
/// off and every setting at its default except the custom rules: an agent could have raised its own access, turned
/// a preset or a limit off, or turned agents on, and only removing rules is something a reset cannot undo.
pub fn load(dir: &Path, key: Option<&[u8; 32]>) -> (AgentsConfig, Trust) {
    let Ok(bytes) = std::fs::read(dir.join(FILE)) else { return (AgentsConfig::default(), Trust::Signed) };
    let file: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let config = file.get("config").cloned().unwrap_or(Value::Null);
    let cfg: AgentsConfig = serde_json::from_value(config.clone()).unwrap_or_default();
    let trust = match (key, file["sig"].as_str().and_then(link::unhex)) {
        (None, _) => Trust::NoKeychain,
        (Some(k), Some(sig)) if mac(k, &config).verify_slice(&sig).is_ok() => Trust::Signed,
        _ => Trust::Tampered,
    };
    if trust == Trust::Signed {
        return (cfg, trust);
    }
    let mut safe = AgentsConfig { rules: cfg.rules, ..Default::default() };
    for p in safe.conns.values_mut() {
        p.access = Access::NotShared;
    }
    (safe, trust)
}

pub fn save(dir: &Path, cfg: &AgentsConfig, key: Option<&[u8; 32]>) -> Result<(), String> {
    let Some(key) = key else { return Err("Could not save the AI agents settings: the keychain did not give Quarry its signing key.".into()) };
    let config = serde_json::to_value(cfg).map_err(|e| e.to_string())?;
    let sig = link::hex(&mac(key, &config).finalize().into_bytes());
    let bytes = serde_json::to_vec_pretty(&json!({"config": config, "sig": sig})).map_err(|e| e.to_string())?;
    link::write_private(dir, FILE, &bytes).map_err(|e| format!("Could not save {FILE}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::config::ConnPolicy;

    fn dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("quarry-store-{}", link::hex(&link::random()[..6])))
    }

    fn shared() -> AgentsConfig {
        let mut c = AgentsConfig { enabled: true, ..Default::default() };
        c.conns.insert(7, ConnPolicy { access: Access::ReadWrite, ..Default::default() });
        c
    }

    #[test]
    fn a_signed_file_loads_as_saved() {
        let (d, k) = (dir(), [5u8; 32]);
        assert_eq!(load(&d, Some(&k)), (AgentsConfig::default(), Trust::Signed));
        save(&d, &shared(), Some(&k)).unwrap();
        assert_eq!(load(&d, Some(&k)), (shared(), Trust::Signed));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_edited_or_unsigned_file_shares_nothing() {
        let (d, k) = (dir(), [5u8; 32]);
        let mut c = shared();
        c.conns.get_mut(&7).unwrap().access = Access::Read;
        save(&d, &c, Some(&k)).unwrap();
        // An agent raises its own access in the file.
        let text = std::fs::read_to_string(d.join(FILE)).unwrap().replace("\"Read\"", "\"ReadWrite\"");
        std::fs::write(d.join(FILE), text).unwrap();
        let (got, trust) = load(&d, Some(&k));
        assert_eq!(trust, Trust::Tampered);
        assert_eq!(got.conn(7), None);
        // Signed with another key, or not signed at all: agents off, limits back to defaults, custom rules kept.
        let mut c = shared();
        c.limits.rows = 100_000;
        c.rules.push(crate::agents::config::RuleSpec { on: true, matcher: crate::agents::config::Matcher::FieldPath("a.b".into()), scope: Vec::new(), action: crate::agents::config::Action::Remove, mode: Default::default() });
        save(&d, &c, Some(&[6u8; 32])).unwrap();
        let (got, trust) = load(&d, Some(&k));
        assert_eq!(trust, Trust::Tampered);
        assert!(!got.enabled && got.conn(7).is_none() && got.limits == Default::default() && got.rules == c.rules);
        std::fs::write(d.join(FILE), serde_json::to_vec(&json!({"config": shared()})).unwrap()).unwrap();
        let (got, trust) = load(&d, Some(&k));
        assert!(trust == Trust::Tampered && got.conn(7).is_none() && !got.enabled);
        // No keychain: same safe settings, its own reason.
        assert_eq!(load(&d, None).1, Trust::NoKeychain);
        assert!(!load(&d, None).0.enabled);
        std::fs::write(d.join(FILE), "garbage").unwrap();
        assert_eq!(load(&d, Some(&k)).1, Trust::Tampered);
        assert!(save(&d, &shared(), None).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
