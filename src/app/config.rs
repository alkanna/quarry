//! Saving the settings, values and connections the app holds; the types and file helpers live in quarry_core::state::config.

use super::*;
pub(crate) use quarry_core::state::config::*;
use quarry_core::action::{Action, DocsA};

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
    /// Asks for a field's values; true when the core will look them up, so the caller redraws to show it running.
    pub(super) fn want_values(&mut self, key: String, path: String, prefix: Option<String>) -> bool {
        if key != self.col_key(false) || self.core.values_asked.contains(&(key.clone(), path.clone(), prefix.clone())) {
            return false;
        }
        self.exec(Action::Docs(DocsA::WantValues { key, path, prefix }));
        self.target().is_some()
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
