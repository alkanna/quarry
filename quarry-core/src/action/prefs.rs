//! Settings (src/app/settings.rs draws it): the category shown and each setting it changes. Applying a look to egui
//! (theme, fonts, size, title bar) stays in the app, which compares the preferences before and after an action.

use super::{Dialog, Reply, Refusal, UiFx};
use crate::core::Core;
use crate::state::config::{Appearance, SavedStage, SavedTab, UiPrefs};
use crate::state::View;
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
    /// A database jumped to without opening a tab, which is what would otherwise record it as recent.
    RecentDb { conn: u64, db: String },
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
    SidebarHidden sidebar_hidden: bool,
    SidePanel side_panel: String,
    LibPreviewH lib_preview_h: f32,
    QueryEditorH query_editor_h: f32,
}

impl Pref {
    /// Settings that change how text and tables are drawn, so measured column widths no longer hold.
    fn changes_look(&self) -> bool {
        matches!(self, Pref::DarkTheme(_) | Pref::LightTheme(_) | Pref::Soft(_) | Pref::Accent(_) | Pref::UiScale(_) | Pref::UiFont(_) | Pref::DataFont(_) | Pref::ReadingLine(_) | Pref::GroupDigits(_))
    }
}

impl Core {
    /// Keeps the open tabs and their inputs in the settings file, so the next launch reopens them.
    pub fn remember_tabs(&mut self) {
        if self.ephemeral {
            return;
        }
        let tabs: Vec<SavedTab> = self
            .tabs
            .iter()
            .map(|t| SavedTab {
                conn: t.sel.conn,
                db: t.sel.db.clone(),
                schema: t.sel.schema.clone(),
                coll: t.sel.coll.clone(),
                view: match t.view {
                    View::Documents => "Documents",
                    View::Aggregate => "Aggregate",
                    View::Indexes => "Indexes",
                    View::Shell => "Shell",
                    View::Schema => "Schema",
                    View::Import => "Import",
                    View::Compare => "Compare",
                    View::Watch => "Watch",
                    View::Structure => "Structure",
                    View::Relations => "Relations",
                    View::Triggers => "Triggers",
                    View::Info => "Info",
                    View::Sql => "Sql",
                }
                .into(),
                filter: t.filter_txt.clone(),
                sort: t.sort_txt.clone(),
                project: t.proj_txt.clone(),
                agg: t.agg_txt.clone(),
                shell: t.shell.code.clone(),
                attached: t.shell.attached.clone(),
                clauses: t.sql.as_ref().and_then(|s| serde_json::to_string(&crate::state::sql::bar::Clauses { tree: s.bar.tree_json.clone(), ..s.bar.c.clone() }).ok()).unwrap_or_default(),
                query: t.sql.as_ref().map(|s| s.query.clone()).unwrap_or_default(),
                run_db: t.sql.as_ref().map(|s| s.run_db.clone()).unwrap_or_default(),
                run_schema: t.sql.as_ref().map(|s| s.run_schema.trim().to_string()).unwrap_or_default(),
                stages: if t.stages_src == t.agg_txt { t.stages.iter().map(|s| SavedStage { op: s.op.clone(), text: s.text.clone(), on: s.on }).collect() } else { Vec::new() },
                agg_text_mode: t.agg_text_mode,
                script: t.script.as_ref().map(|b| b.r.clone()),
            })
            .collect();
        let open: Vec<u64> = self.rail_order.iter().copied().filter(|id| self.conn(*id).is_some_and(|c| c.status.is_open())).collect();
        if tabs != self.prefs.open_tabs || self.cur != self.prefs.cur_tab || self.active != self.prefs.last_conn || open != self.prefs.open_conns {
            self.prefs.open_tabs = tabs;
            self.prefs.cur_tab = self.cur;
            self.prefs.last_conn = self.active;
            self.prefs.open_conns = open;
            self.save_prefs();
        }
    }

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
            PrefsA::RecentDb { conn, db } => {
                if self.prefs.note_recent_db(conn, &db) {
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
