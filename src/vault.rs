//! Saved secrets, by name. On macOS they all live in one Keychain item, a JSON map: macOS asks for access per item, so
//! one item per secret meant one password prompt per saved connection at launch, and "Always Allow" covered only one.
//! Items saved by older versions under their own name move into it the first time they are read. Elsewhere each
//! secret keeps its own item: nothing prompts per item there, and Windows caps one secret at 2560 bytes.

use keyring::Error;
#[cfg(not(test))]
use keyring::Entry;
#[cfg(test)]
use fake::Entry;

const SERVICE: &str = "quarry";

#[cfg(any(target_os = "macos", test))]
pub use mac::{get, set};

#[cfg(not(any(target_os = "macos", test)))]
pub fn get(name: &str) -> keyring::Result<String> {
    Entry::new(SERVICE, name)?.get_password()
}

#[cfg(not(any(target_os = "macos", test)))]
pub fn set(name: &str, value: &str) -> keyring::Result<()> {
    let e = Entry::new(SERVICE, name)?;
    match if value.is_empty() { e.delete_credential() } else { e.set_password(value) } {
        Err(Error::NoEntry) => Ok(()),
        r => r,
    }
}

#[cfg(any(target_os = "macos", test))]
mod mac {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    const ITEM: &str = "secrets";

    /// The map as last read or written, so a launch reads the Keychain once.
    static CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

    fn read() -> keyring::Result<HashMap<String, String>> {
        match Entry::new(SERVICE, ITEM)?.get_password() {
            // A map that does not parse is an error, not an empty map: the next write would wipe every secret.
            Ok(s) => serde_json::from_str(&s).map_err(|e| Error::BadEncoding(e.to_string().into_bytes())),
            Err(Error::NoEntry) => Ok(HashMap::new()),
            Err(e) => Err(e),
        }
    }

    /// The secret saved as `name`, or `Error::NoEntry`.
    pub fn get(name: &str) -> keyring::Result<String> {
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if cache.is_none() {
            *cache = Some(read()?);
        }
        if let Some(v) = cache.as_ref().and_then(|m| m.get(name)) {
            return Ok(v.clone());
        }
        drop(cache);
        let old = Entry::new(SERVICE, name)?;
        let v = old.get_password()?;
        // The old item goes only once its value is safe in the new one; the value read is good either way.
        if set(name, &v).is_ok() {
            let _ = old.delete_credential();
        }
        Ok(v)
    }

    /// Saves `value` as `name`, or removes it when `value` is empty.
    pub fn set(name: &str, value: &str) -> keyring::Result<()> {
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        // Read again rather than trusting the cache, so a change saved by another running Quarry is kept.
        let mut map = read()?;
        if value.is_empty() {
            map.remove(name);
            // An item from an older version never read since would otherwise come back on the next get.
            if let Ok(old) = Entry::new(SERVICE, name) {
                let _ = old.delete_credential();
            }
        } else {
            map.insert(name.to_string(), value.to_string());
        }
        let json = serde_json::to_string(&map).map_err(|e| Error::BadEncoding(e.to_string().into_bytes()))?;
        Entry::new(SERVICE, ITEM)?.set_password(&json)?;
        *cache = Some(map);
        Ok(())
    }
}

/// The Keychain as a map, so the move from per-secret items runs in tests on every platform.
#[cfg(test)]
mod fake {
    use keyring::{Error, Result};
    use std::collections::HashMap;
    use std::sync::Mutex;

    pub static ITEMS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

    pub struct Entry(String);

    impl Entry {
        pub fn new(service: &str, name: &str) -> Result<Self> {
            Ok(Self(format!("{service}/{name}")))
        }
        fn with<R>(&self, f: impl FnOnce(&mut HashMap<String, String>, &str) -> R) -> R {
            f(ITEMS.lock().unwrap().get_or_insert_with(HashMap::new), &self.0)
        }
        pub fn get_password(&self) -> Result<String> {
            self.with(|m, k| m.get(k).cloned().ok_or(Error::NoEntry))
        }
        pub fn set_password(&self, v: &str) -> Result<()> {
            self.with(|m, k| m.insert(k.to_string(), v.to_string()));
            Ok(())
        }
        pub fn delete_credential(&self) -> Result<()> {
            self.with(|m, k| m.remove(k).map(|_| ()).ok_or(Error::NoEntry))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<String> {
        let mut k: Vec<String> = fake::ITEMS.lock().unwrap().as_ref().map(|m| m.keys().cloned().collect()).unwrap_or_default();
        k.sort();
        k
    }

    // One test: the fake Keychain and the cache are process-wide.
    #[test]
    fn older_items_move_into_one_and_removals_reach_both() {
        for (k, v) in [("connection-1", "mongodb://a"), ("connection-2", "mongodb://b"), ("connection-2-ssh-password", "pw")] {
            Entry::new(SERVICE, k).unwrap().set_password(v).unwrap();
        }
        assert_eq!(get("connection-1").unwrap(), "mongodb://a");
        assert_eq!(get("connection-1").unwrap(), "mongodb://a", "a second read comes from the moved value");
        assert!(matches!(get("connection-3"), Err(Error::NoEntry)));
        assert_eq!(items(), ["quarry/connection-2", "quarry/connection-2-ssh-password", "quarry/secrets"], "connection-1's own item is still there after the move");

        // An item never read since the upgrade must not come back after a removal.
        set("connection-2-ssh-password", "").unwrap();
        assert!(matches!(get("connection-2-ssh-password"), Err(Error::NoEntry)));
        set("connection-4", "mongodb://d").unwrap();
        assert_eq!(get("connection-2").unwrap(), "mongodb://b");
        assert_eq!(items(), ["quarry/secrets"]);
        let saved: std::collections::HashMap<String, String> = serde_json::from_str(&Entry::new(SERVICE, "secrets").unwrap().get_password().unwrap()).unwrap();
        assert_eq!(saved.len(), 3);
        assert_eq!(saved["connection-4"], "mongodb://d");
    }
}
