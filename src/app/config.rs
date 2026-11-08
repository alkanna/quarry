//! What is saved between runs: preferences, query history and library, open tabs, and keychain secrets.

use super::*;

/// Per-collection column setup, saved between launches.
#[derive(Default, Serialize, Deserialize, Clone)]
#[serde(default)]
pub(super) struct ColPrefs {
    /// Highlighted field paths (array positions removed), like the original's highlightedFields.
    pub(super) highlight: Vec<String>,
    /// Chosen columns in order (dotted paths allowed). `None` means the first DEFAULT_COLS fields.
    pub(super) shown: Option<Vec<String>>,
    /// Pinned columns always come first.
    pub(super) pinned: Vec<String>,
    pub(super) widths: HashMap<String, f32>,
    /// 0 left (the default when missing), 1 center, 2 right.
    pub(super) align: HashMap<String, u8>,
    /// Header text shown instead of the field path.
    pub(super) labels: HashMap<String, String>,
}

impl ColPrefs {
    /// Header text for column `c`: its custom label or the path itself.
    pub(super) fn label<'a>(&'a self, c: &'a str) -> &'a str {
        self.labels.get(c).map_or(c, String::as_str)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub(super) struct UiPrefs {
    /// Open documents under their row (like the original) instead of in a side panel.
    pub(super) open_inline: bool,
    pub(super) table: bool,
    /// Show the list of changes before writing staged edits. Off by default, as asked.
    pub(super) confirm_save: bool,
    /// Write every edit right away and offer Undo, instead of collecting unsaved edits.
    pub(super) auto_save: bool,
    /// Every connection is writable and saving edits skips the change list.
    pub(super) always_writable: bool,
    /// Columns shown before the person picks their own.
    pub(super) default_cols: usize,
    pub(super) timeout_ms: u64,
    /// Path to Bun for the Shell; empty means find it automatically.
    pub(super) shell_runtime: String,
    /// Shell scripts run, newest first, per "connection/database" like the original's history refs.
    pub(super) shell_histories: HashMap<String, Vec<String>>,
    /// npm packages available to Shell scripts.
    pub(super) shell_packages: Vec<String>,
    /// Per collection ("conn/db/coll"): recent Query, Sort and Project inputs.
    pub(super) history: HashMap<String, QueryHist>,
    /// Per collection: named saved queries and pipelines.
    pub(super) library: HashMap<String, Vec<SavedQuery>>,
    /// Tabs open when the app was last used, restored at launch.
    pub(super) open_tabs: Vec<SavedTab>,
    pub(super) cur_tab: usize,
    pub(super) cols: HashMap<String, ColPrefs>,
    /// The connection picked when the app was last used, connected again at launch.
    pub(super) last_conn: Option<u64>,
    /// Per "conn/db": fields that point to documents of another collection.
    pub(super) relations: HashMap<String, Vec<Relation>>,
    /// Paths to mongodump and mongorestore; empty means find them automatically.
    pub(super) mongodump: String,
    pub(super) mongorestore: String,
    /// Install new releases from GitHub and restart into them.
    pub(super) auto_update: bool,
    /// Pipeline builder stages preview themselves after each change.
    pub(super) auto_preview: bool,
    /// Frame rate and the time each frame takes, in the status bar.
    pub(super) show_fps: bool,
    /// Quarry's memory and its CPU use, in the status bar.
    pub(super) show_mem: bool,
    pub(super) show_cpu: bool,
    /// Show the first characters of big objects and arrays in columns, as the original does,
    /// instead of "{12 fields}".
    pub(super) object_preview: bool,
    pub(super) appearance: Appearance,
    /// The version that last ran, to show the changelog once after an update.
    pub(super) last_version: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Default)]
pub(super) enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub(super) fn apply(self, ctx: &egui::Context) {
        ctx.set_theme(match self {
            Appearance::System => egui::ThemePreference::System,
            Appearance::Light => egui::ThemePreference::Light,
            Appearance::Dark => egui::ThemePreference::Dark,
        });
    }
}

/// The original keeps 30 recent inputs per collection and kind (HISTORY_LENGTH_BY_COLLECTION,
/// renderer line 195241), skipping empty ones and moving repeats to the top (addHistory, 195309).
pub(super) const HISTORY_LENGTH_BY_COLLECTION: usize = 30;

#[derive(Default, Serialize, Deserialize, Clone)]
#[serde(default)]
pub(super) struct QueryHist {
    pub(super) selector: Vec<String>,
    pub(super) sort: Vec<String>,
    pub(super) project: Vec<String>,
}

pub(super) fn push_history(list: &mut Vec<String>, value: &str) {
    let v = value.trim();
    if v.is_empty() || v == "{}" {
        return;
    }
    list.retain(|x| x != v);
    list.insert(0, v.to_string());
    list.truncate(HISTORY_LENGTH_BY_COLLECTION);
}

/// A named query (or pipeline) saved for a collection; saving under an existing name replaces it,
/// like the original's saved filters (CLS.saveFilter, renderer line 195332).
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub(super) struct SavedQuery {
    pub(super) name: String,
    pub(super) filter: String,
    pub(super) sort: String,
    pub(super) project: String,
    pub(super) pipeline: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(default)]
pub(super) struct SavedTab {
    /// Pipeline builder stages, kept so stages switched off survive a restart.
    pub(super) stages: Vec<SavedStage>,
    /// The Aggregate view shows the pipeline as text instead of stage cards.
    pub(super) agg_text_mode: bool,
    pub(super) conn: u64,
    pub(super) db: String,
    pub(super) coll: String,
    pub(super) view: String,
    pub(super) filter: String,
    pub(super) sort: String,
    pub(super) project: String,
    pub(super) agg: String,
    pub(super) shell: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(default)]
pub(super) struct SavedStage {
    pub(super) op: String,
    pub(super) text: String,
    pub(super) on: bool,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self { open_inline: true, table: true, confirm_save: false, auto_save: false, always_writable: false, default_cols: DEFAULT_COLS, timeout_ms: db::DEFAULT_TIMEOUT_MS, shell_runtime: String::new(), shell_histories: HashMap::new(), shell_packages: Vec::new(), history: HashMap::new(), library: HashMap::new(), open_tabs: Vec::new(), cur_tab: 0, last_conn: None, cols: HashMap::new(), relations: HashMap::new(), mongodump: String::new(), mongorestore: String::new(), auto_update: true, auto_preview: true, show_fps: false, show_mem: false, show_cpu: false, object_preview: false, appearance: Appearance::System, last_version: String::new() }
    }
}

/// Parses a config file. One that no longer parses is moved to `<name>.bak`, so the next save
/// cannot overwrite what the person had, and the reason comes back to be shown.
pub(super) fn parse_config<T: serde::de::DeserializeOwned + Default>(path: &std::path::Path) -> (T, Option<String>) {
    let Ok(b) = std::fs::read(path) else { return (T::default(), None) };
    match serde_json::from_slice(&b) {
        Ok(v) => (v, None),
        Err(err) => {
            let bak = path.with_extension("json.bak");
            let moved = match std::fs::rename(path, &bak) {
                Ok(()) => format!("The file was kept as {}.", bak.display()),
                Err(e) => format!("Moving it aside failed too ({e}); it will be overwritten on the next save."),
            };
            (T::default(), Some(format!("Could not read {}: {err}. {moved}", path.display())))
        }
    }
}

/// Like `parse_config` for a list, but an entry that no longer parses is skipped instead of
/// losing every other one. The whole file is copied to `<name>.bak` first, because the next save
/// writes back only the entries that parsed.
pub(super) fn parse_config_list<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> (Vec<T>, Option<String>) {
    let Ok(b) = std::fs::read(path) else { return (Vec::new(), None) };
    let Ok(items) = serde_json::from_slice::<Vec<serde_json::Value>>(&b) else { return parse_config(path) };
    let total = items.len();
    let mut errs = Vec::new();
    let good: Vec<T> = items.into_iter().filter_map(|v| serde_json::from_value(v).map_err(|e| errs.push(e.to_string())).ok()).collect();
    if errs.is_empty() {
        return (good, None);
    }
    let bak = path.with_extension("json.bak");
    let kept = match std::fs::copy(path, &bak) {
        Ok(_) => format!("The full file was kept as {}.", bak.display()),
        Err(e) => format!("Copying it aside failed ({e}); the skipped entries will be lost on the next save."),
    };
    (good, Some(format!("Skipped {} of {total} entries in {}: {}. {kept}", errs.len(), path.display(), errs.join("; "))))
}

/// Field values kept for suggestions, as BSON so number types come back unchanged.
// ponytail: an unreadable file is dropped, it is only a cache; no size cap since entries only exist for fields someone typed a value for.
pub(super) fn load_values() -> HashMap<String, HashMap<String, Vec<Bson>>> {
    std::fs::read(config_dir().join("values.bson")).ok().and_then(|b| mongodb::bson::from_slice(&b).ok()).unwrap_or_default()
}

pub(super) fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("quarry")
}

/// Where Quarry installs the MongoDB Database Tools itself when they are missing.
pub(super) fn tools_dir() -> PathBuf {
    config_dir().join("tools")
}

pub(super) fn keychain(id: u64) -> Option<keyring::Entry> {
    keyring::Entry::new("quarry", &format!("connection-{id}")).ok()
}

/// Other secrets of a connection: "ssh-password", "ssh-passphrase", "proxy-password".
pub(super) fn secret(id: u64, what: &str) -> Option<String> {
    keyring::Entry::new("quarry", &format!("connection-{id}-{what}")).ok()?.get_password().ok().filter(|s| !s.is_empty())
}

/// The connection's address from the Keychain.
pub(super) fn stored_uri(id: u64) -> Result<String, keyring::Error> {
    keychain(id).ok_or(keyring::Error::NoEntry)?.get_password()
}

pub(super) fn uri_err(e: keyring::Error) -> String {
    format!("Could not read the connection's address from the Keychain: {e}")
}

/// Stores a secret, or removes it when `value` is empty.
pub(super) fn set_secret(id: u64, what: &str, value: &str) -> Result<(), String> {
    let r = keyring::Entry::new("quarry", &format!("connection-{id}-{what}")).and_then(|k| if value.is_empty() { k.delete_credential() } else { k.set_password(value) });
    match r {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("Could not store the {} in the Keychain: {e}", what.replace('-', " "))),
    }
}

/// Written beside the file, then renamed over it: a crash mid-write leaves the old file whole
/// instead of a truncated one that no longer parses.
// ponytail: no fsync; ui.json is saved on every keystroke in the query boxes and an fsync on the UI
// thread stalls typing. Only a power loss right after a save can lose it; sync on a worker if that matters.
pub(super) fn write_atomic(dir: &std::path::Path, file: &str, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{file}.tmp"));
    let res = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, dir.join(file)));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

impl App {
    pub(super) fn write_config(&mut self, file: &str, bytes: Vec<u8>) {
        if self.demo {
            return;
        }
        if let Err(err) = write_atomic(&config_dir(), file, &bytes) {
            self.err(format!("Could not save {file}: {err}"));
        }
    }

    pub(super) fn save_conns(&mut self) {
        let metas: Vec<&ConnMeta> = self.conns.iter().map(|c| &c.meta).collect();
        let b = serde_json::to_vec_pretty(&metas).unwrap_or_default();
        self.write_config("connections.json", b);
    }

    pub(super) fn save_values(&mut self) {
        if let Ok(b) = mongodb::bson::to_vec(&self.values) {
            self.write_config("values.bson", b);
        }
    }

    /// Asks the server for a field's values once per session: a sample (the copy kept on disk shows
    /// meanwhile), or with `prefix` the values starting with the text being typed. `key` is the
    /// collection the request was made in, which a tab switch may have left.
    /// Returns whether a lookup was sent, so the caller redraws to show it running.
    pub(super) fn want_values(&mut self, key: String, path: String, prefix: Option<String>) -> bool {
        if key != self.col_key(false) || !self.values_asked.insert((key.clone(), path.clone(), prefix.clone())) {
            return false;
        }
        let Some(t) = self.target() else { return false };
        self.values_pending.insert((key.clone(), path.clone()));
        self.worker.field_values(key, t, path, prefix);
        true
    }

    pub(super) fn values_done(&mut self, key: String, path: String, prefix: Option<String>, res: db::Res<Option<Vec<Bson>>>) {
        self.values_pending.remove(&(key.clone(), path.clone()));
        // Suggestions are best effort: a refused or slow lookup keeps what the page offers. A typed
        // lookup that did not run says why under the list.
        let note = match &res {
            Ok(None) => Some("Field not indexed: only sampled values are suggested"),
            Err(_) if prefix.is_some() => Some("Lookup too slow: only sampled values are suggested"),
            _ => None,
        };
        if let Some(n) = note {
            self.values_note.insert((key.clone(), path.clone()), n);
        }
        let Ok(Some(mut vals)) = res else { return };
        vals.retain(suggestible);
        vals.truncate(db::FIELD_VALUES_MAX);
        let found = HashMap::from([(path, vals)]);
        for t in self.tabs.iter_mut().filter(|t| t.coll_key() == key) {
            merge_values(&mut t.field_info, &found);
        }
        // Values found by what was typed are kept for the session only, so the file holds samples.
        if prefix.is_none() {
            self.values.entry(key).or_default().extend(found);
            self.save_values();
        }
    }

    pub(super) fn save_prefs(&mut self) {
        let b = serde_json::to_vec_pretty(&self.prefs).unwrap_or_default();
        self.write_config("ui.json", b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_history() {
        let mut h = Vec::new();
        for v in ["{ a: 1 }", "{}", " ", "{ b: 2 }", "{ a: 1 }"] {
            push_history(&mut h, v);
        }
        assert_eq!(h, ["{ a: 1 }", "{ b: 2 }"]);
        for i in 0..40 {
            push_history(&mut h, &format!("{{ n: {i} }}"));
        }
        assert_eq!(h.len(), HISTORY_LENGTH_BY_COLLECTION);
    }

    #[test]
    fn atomic_write_replaces_or_leaves_the_old_file() {
        let dir = std::env::temp_dir().join(format!("quarry-atomic-{}", std::process::id()));
        write_atomic(&dir, "ui.json", b"old").unwrap();
        write_atomic(&dir, "ui.json", b"new").unwrap();
        assert_eq!(std::fs::read(dir.join("ui.json")).unwrap(), b"new");
        // A target the rename cannot replace: the write fails, the target is untouched, no temp file is left.
        std::fs::create_dir_all(dir.join("busy.json").join("keep")).unwrap();
        assert!(write_atomic(&dir, "busy.json", b"x").is_err());
        assert!(dir.join("busy.json").join("keep").is_dir());
        assert!(!dir.join(".busy.json.tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn broken_config_is_moved_aside() {
        let dir = std::env::temp_dir().join(format!("quarry-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("connections.json");
        std::fs::write(&p, "[{").unwrap();
        let (v, err): (Vec<ConnMeta>, _) = parse_config(&p);
        assert!(v.is_empty() && err.is_some());
        assert!(!p.exists() && dir.join("connections.json.bak").exists());
        let (_, err): (Vec<ConnMeta>, _) = parse_config(&p);
        assert!(err.is_none(), "a missing file is a fresh install, not an error");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bad_connection_is_skipped_not_all() {
        let dir = std::env::temp_dir().join(format!("quarry-cfg-list-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("connections.json");
        std::fs::write(&p, r#"[{"id": 1, "name": "good"}, {"id": "x", "name": "bad"}]"#).unwrap();
        let (v, err) = parse_config_list::<ConnMeta>(&p);
        assert_eq!(v.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["good"]);
        assert!(err.is_some());
        assert!(p.exists() && dir.join("connections.json.bak").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
