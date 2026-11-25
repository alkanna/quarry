//! Settings (src/app/settings.rs draws it): the category shown and each setting it changes. Applying a look to egui
//! (theme, fonts, size, title bar) stays in the app, which compares the preferences before and after an action.

use super::{Dialog, Reply, Refusal, UiFx};
use crate::core::Core;
use crate::state::config::{Appearance, UiPrefs};
use crate::state::settings::SettingsCat;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum PrefsA {
    /// Opens Settings on this category.
    Open(SettingsCat),
    /// A category picked in the Settings list.
    Cat(SettingsCat),
    Set(Pref),
    /// Cmd+=, Cmd+- or Cmd+0 picked this interface size.
    Zoom(f32),
}

/// Declares the settings of the Settings window once: `Pref` holds one with its new value, `PrefVals` all of them for a
/// frame of drawing, and `diff` turns what the drawing changed into `Pref`s.
macro_rules! prefs {
    ($($v:ident $f:ident: $t:ty,)*) => {
        /// One setting of the Settings window with its new value.
        #[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
        pub enum Pref {
            $($v($t),)*
        }

        /// The settings the Settings window edits, copied out of `UiPrefs` for a frame of drawing.
        #[derive(Clone, PartialEq)]
        pub struct PrefVals {
            $(pub $f: $t,)*
        }

        impl PrefVals {
            pub fn of(p: &UiPrefs) -> Self {
                Self { $($f: p.$f.clone(),)* }
            }

            /// The settings that differ in `new`, with their new values.
            pub fn diff(&self, new: &PrefVals) -> Vec<Pref> {
                let mut out = Vec::new();
                $(if self.$f != new.$f {
                    out.push(Pref::$v(new.$f.clone()));
                })*
                out
            }
        }

        impl Pref {
            /// Writes the value; false when it was already set.
            fn apply(self, p: &mut UiPrefs) -> bool {
                match self {
                    $(Pref::$v(x) => {
                        let changed = p.$f != x;
                        p.$f = x;
                        changed
                    })*
                }
            }

            /// One of each, for the serde round trip test.
            #[cfg(test)]
            pub fn samples() -> Vec<Pref> {
                let p = PrefVals::of(&UiPrefs::default());
                vec![$(Pref::$v(p.$f),)*]
            }
        }
    };
}

prefs! {
    Appearance appearance: Appearance,
    DarkTheme dark_theme: String,
    LightTheme light_theme: String,
    Soft soft: bool,
    Accent accent: String,
    OwnTitlebar own_titlebar: bool,
    UiScale ui_scale: f32,
    UiFont ui_font: String,
    DataFont data_font: String,
    ReadingLine reading_line: bool,
    GroupDigits group_digits: bool,
    LockScrollAxis lock_scroll_axis: bool,
    KeepToasts keep_toasts: bool,
    OpenInline open_inline: bool,
    DefaultCols default_cols: usize,
    ObjectPreview object_preview: bool,
    AutoSave auto_save: bool,
    AlwaysWritable always_writable: bool,
    Vim vim: bool,
    ValuePanel value_panel: bool,
    ConfirmSave confirm_save: bool,
    TimeoutMs timeout_ms: u64,
    ShellRuntime shell_runtime: String,
    Gcloud gcloud: String,
    CloudSqlProxy cloud_sql_proxy: String,
    Mongodump mongodump: String,
    Mongorestore mongorestore: String,
    ShowServer show_server: bool,
    ShowFps show_fps: bool,
    ShowMem show_mem: bool,
    ShowCpu show_cpu: bool,
    AutoUpdate auto_update: bool,
}

impl Pref {
    /// Settings that change how text and tables are drawn, so measured column widths no longer hold.
    fn changes_look(&self) -> bool {
        matches!(self, Pref::DarkTheme(_) | Pref::LightTheme(_) | Pref::Soft(_) | Pref::Accent(_) | Pref::UiScale(_) | Pref::UiFont(_) | Pref::DataFont(_) | Pref::ReadingLine(_) | Pref::GroupDigits(_))
    }
}

impl Core {
    pub(super) fn prefs_act(&mut self, a: PrefsA) -> Result<Reply, Refusal> {
        match a {
            PrefsA::Open(cat) => {
                self.settings_cat = cat;
                self.ui_fx.push(UiFx::Open(Dialog::Settings));
            }
            PrefsA::Cat(cat) => self.settings_cat = cat,
            PrefsA::Set(p) => {
                let (look, unlocks) = (p.changes_look(), p == Pref::AlwaysWritable(false));
                if p.apply(&mut self.prefs) {
                    // Column widths were measured in the old font and number display.
                    if look {
                        self.width_cache.clear();
                    }
                    // The pref unlocks every connection each frame, so turning it off has to lock them back.
                    if unlocks {
                        self.conns.iter_mut().for_each(|c| c.writable = false);
                    }
                    self.save_prefs();
                }
            }
            PrefsA::Zoom(s) => {
                if self.prefs.ui_scale != s {
                    self.prefs.ui_scale = s;
                    self.save_prefs();
                }
            }
        }
        Ok(Reply::Done)
    }
}
