//! Saving the settings, values and connections the app holds; the types and file helpers live in quarry_core::state::config.

use super::*;
pub(crate) use quarry_core::state::config::*;

/// Applying a theme or a size goes through egui, so these stay with the drawing.
pub(crate) trait AppearanceUi {
    fn apply(self, ctx: &egui::Context);
}

impl AppearanceUi for Appearance {
    fn apply(self, ctx: &egui::Context) {
        ctx.set_theme(match self {
            Appearance::System => egui::ThemePreference::System,
            Appearance::Light => egui::ThemePreference::Light,
            Appearance::Dark => egui::ThemePreference::Dark,
        });
    }
}

pub(crate) trait PrefsLook {
    fn scale(&self) -> f32;
    fn look(&self) -> Look;
}

impl PrefsLook for UiPrefs {
    /// A hand-edited ui.json may hold any number; only the offered sizes are applied.
    fn scale(&self) -> f32 {
        if SCALES.iter().any(|s| s.0 == self.ui_scale) { self.ui_scale } else { 1.0 }
    }

    fn look(&self) -> Look {
        Look {
            scale: self.scale(),
            dark_theme: theme_index(&self.dark_theme, false),
            light_theme: theme_index(&self.light_theme, true),
            ui_font: font_index(&UI_FONTS, &self.ui_font),
            data_font: font_index(&DATA_FONTS, &self.data_font),
            soft: self.soft,
            accent: accent_index(&self.accent),
            reading_line: self.reading_line,
            group_digits: self.group_digits,
        }
    }
}

impl App {
    pub(super) fn write_config(&mut self, file: &str, bytes: Vec<u8>) {
        if self.ephemeral {
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
        // The demo must not touch the keychain, and without a key nothing is written in plain text.
        if self.ephemeral {
            return;
        }
        if let Some(b) = cache_key().and_then(|k| seal(k, &mongodb::bson::to_vec(&self.values).ok()?)) {
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
            merge_values(Arc::make_mut(&mut t.field_info), &found);
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
    fn theme_and_font_names_resolve_with_fallbacks() {
        // An older ui.json: no theme or font names, OpenDyslexic as a flag.
        let mut old: UiPrefs = serde_json::from_str(r#"{"dyslexic_font":true}"#).unwrap();
        old.migrate();
        let saved: UiPrefs = serde_json::from_str(&serde_json::to_string(&old).unwrap()).unwrap();
        let l = saved.look();
        assert_eq!((DARK_THEMES[l.dark_theme].name, LIGHT_THEMES[l.light_theme].name, UI_FONTS[l.ui_font].0, DATA_FONTS[l.data_font].0), ("Workbench", "Paper", "OpenDyslexic", "JetBrains Mono"));
        // A light theme named in the dark slot, or an unknown name, falls back to the default of that kind.
        let p = UiPrefs { dark_theme: "Glacier".into(), light_theme: "Sage".into(), data_font: "Comic Mono".into(), ..Default::default() };
        let l = p.look();
        assert_eq!((DARK_THEMES[l.dark_theme].name, LIGHT_THEMES[l.light_theme].name, DATA_FONTS[l.data_font].0), ("Workbench", "Sage", "JetBrains Mono"));
    }
}
