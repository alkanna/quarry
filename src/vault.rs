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
pub use mac::{get, migrate, set};
#[cfg(not(any(target_os = "macos", test)))]
pub use plain::{get, migrate, set};

/// A random 32-byte key saved as `name`, created on first use. None when the Keychain refuses.
pub fn key(name: &str) -> Option<[u8; 32]> {
    use crate::agents::link;
    match get(name) {
        Ok(hex) => link::unhex(&hex).and_then(|b| b.try_into().ok()),
        Err(Error::NoEntry) => {
            let key = link::random();
            set(name, &link::hex(&key)).ok()?;
            Some(key)
        }
        Err(_) => None,
    }
}

#[cfg(not(any(target_os = "macos", test)))]
mod plain {
    use super::*;

    /// Nothing to move: only macOS keeps the secrets in one item.
    pub fn migrate(_names: &[String]) {}

    pub fn get(name: &str) -> keyring::Result<String> {
        Entry::new(SERVICE, name)?.get_password()
    }

    pub fn set(name: &str, value: &str) -> keyring::Result<()> {
        let e = Entry::new(SERVICE, name)?;
        match if value.is_empty() { e.delete_credential() } else { e.set_password(value) } {
            Err(Error::NoEntry) => Ok(()),
            r => r,
        }
    }
}

#[cfg(any(target_os = "macos", test))]
mod mac {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    const ITEM: &str = "secrets";
    /// Set in the map once the items of older versions have moved into it.
    const MOVED: &str = "#moved";
    // ponytail: a short cache covers the burst of reads at launch, one prompt for all of them if the user picks
    // Allow over Always Allow, while a change saved by another running Quarry is seen a few seconds later.
    const FRESH: Duration = Duration::from_secs(5);

    static CACHE: Mutex<Option<(Instant, HashMap<String, String>)>> = Mutex::new(None);
    /// A refused read is not asked again in this launch: each secret read at launch would prompt once more.
    static REFUSED: AtomicBool = AtomicBool::new(false);

    fn read() -> keyring::Result<HashMap<String, String>> {
        if REFUSED.load(Ordering::Relaxed) {
            return Err(Error::NoStorageAccess("access to the Keychain was refused earlier; restart Quarry to be asked again".into()));
        }
        let map = match Entry::new(SERVICE, ITEM)?.get_password() {
            // A map that does not parse is an error, not an empty map: the next write would wipe every secret.
            Ok(s) => serde_json::from_str(&s).map_err(|_| Error::BadStoreFormat(format!("the Keychain item \"{SERVICE}\" / \"{ITEM}\" is damaged; delete it in Keychain Access and enter the passwords again"))),
            Err(Error::NoEntry) => Ok(HashMap::new()),
            Err(e) => {
                REFUSED.store(true, Ordering::Relaxed);
                Err(e)
            }
        }?;
        remember(&map);
        Ok(map)
    }

    fn write(map: &HashMap<String, String>) -> keyring::Result<()> {
        let json = serde_json::to_string(map).map_err(|e| Error::BadStoreFormat(e.to_string()))?;
        Entry::new(SERVICE, ITEM)?.set_password(&json)?;
        remember(map);
        Ok(())
    }

    fn remember(map: &HashMap<String, String>) {
        *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), map.clone()));
    }

    /// The secret saved as `name`, or `Error::NoEntry`.
    pub fn get(name: &str) -> keyring::Result<String> {
        if let Some((at, map)) = &*CACHE.lock().unwrap_or_else(|e| e.into_inner())
            && at.elapsed() < FRESH
        {
            return lookup(map, name);
        }
        lookup(&read()?, name)
    }

    fn lookup(map: &HashMap<String, String>, name: &str) -> keyring::Result<String> {
        match map.get(name) {
            Some(v) => Ok(v.clone()),
            // Until the move has run, which it does not when an old item was refused, the old items still count.
            None if !map.contains_key(MOVED) => Entry::new(SERVICE, name)?.get_password(),
            None => Err(Error::NoEntry),
        }
    }

    /// Saves `value` as `name`, or removes it when `value` is empty.
    pub fn set(name: &str, value: &str) -> keyring::Result<()> {
        // Read again rather than trusting the cache, so a change saved by another running Quarry is kept.
        let mut map = read()?;
        if value.is_empty() {
            map.remove(name);
            if !map.contains_key(MOVED) {
                let _ = Entry::new(SERVICE, name).and_then(|e| e.delete_credential());
            }
        } else {
            map.insert(name.to_string(), value.to_string());
        }
        write(&map)
    }

    /// Moves the items older versions saved one per secret into the single one, once. `names` are every name that may
    /// have been saved. Stops without marking the move done when an old item is refused, so the next launch tries again.
    pub fn migrate(names: &[String]) {
        let Ok(mut map) = read() else { return };
        if map.contains_key(MOVED) {
            return;
        }
        let mut moved = Vec::new();
        let todo: Vec<&String> = names.iter().filter(|n| !map.contains_key(*n)).collect();
        for name in todo {
            match Entry::new(SERVICE, name).and_then(|e| e.get_password()) {
                Ok(v) => {
                    map.insert(name.clone(), v);
                    moved.push(name);
                }
                Err(Error::NoEntry) => {}
                Err(_) => return,
            }
        }
        map.insert(MOVED.to_string(), String::new());
        if write(&map).is_err() {
            return;
        }
        // An old item goes only once its value reads back from the new one, so two windows moving at once lose nothing.
        let Ok(saved) = read() else { return };
        for name in moved {
            if saved.get(name) == map.get(name) {
                let _ = Entry::new(SERVICE, name).and_then(|e| e.delete_credential());
            }
        }
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
    fn older_items_move_into_one_once() {
        let old = |k: &str, v: &str| Entry::new(SERVICE, k).unwrap().set_password(v).unwrap();
        old("connection-1", "mongodb://a");
        old("connection-2-ssh-password", "pw");
        assert_eq!(get("connection-1").unwrap(), "mongodb://a", "an old item still reads before the move");

        let names: Vec<String> = ["connection-1", "connection-1-ssh-password", "connection-2", "connection-2-ssh-password"].map(String::from).to_vec();
        migrate(&names);
        assert_eq!(items(), ["quarry/secrets"], "the old items are gone once moved");
        assert_eq!(get("connection-1").unwrap(), "mongodb://a");
        assert_eq!(get("connection-2-ssh-password").unwrap(), "pw");
        assert!(matches!(get("connection-2"), Err(Error::NoEntry)));

        // An item an older build saves after the move is not picked up again: the move runs once.
        old("connection-3", "mongodb://c");
        migrate(&["connection-3".to_string()]);
        assert!(matches!(get("connection-3"), Err(Error::NoEntry)));

        set("connection-2-ssh-password", "").unwrap();
        set("connection-4", "mongodb://d").unwrap();
        assert!(matches!(get("connection-2-ssh-password"), Err(Error::NoEntry)));
        let saved: std::collections::HashMap<String, String> = serde_json::from_str(&Entry::new(SERVICE, "secrets").unwrap().get_password().unwrap()).unwrap();
        assert_eq!(saved["connection-4"], "mongodb://d");
        assert_eq!(saved.len(), 3, "connection-1, connection-4 and the moved marker");
    }
}
