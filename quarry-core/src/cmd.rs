//! The command registry: every command of the macOS menu bar, the Cmd+K palette, the page shortcuts and the Cmd+/ sheet
//! is declared once in `CMDS`. The app turns a `Chord` into egui's shortcut, runs a command through `Cmd::action` when it
//! is an `Action`, and through its own small match when only the UI can do it (a dialog, focus, the clipboard).
//!
//! Three things stay apart per command, because they differ today: `available` is whether the palette lists it,
//! `keys_on` whether its key is taken (an unmet guard leaves the key for whatever else reads it), and `action` what it
//! does on the current tab or connection.

use crate::action::{Action, ConnA, DdlA, MongoServerA, MongoUsersA, PgServerA, ServerA, SideA, SqlA, SqlServerA, SqlUsersA, TabA, UpdateA, UsersA};
use crate::core::Core;
use crate::db::sql::Engine;
use crate::state::config::SideMode;
use crate::state::{Status, View};
use std::sync::LazyLock;

#[cfg(test)]
mod tests;

/// The views of a Mongo tab and of a SQL tab, in the order Cmd+Alt+digit and the View menu number them.
pub const MONGO_VIEWS: [(View, &str); 8] = [(View::Documents, "Documents"), (View::Aggregate, "Aggregate"), (View::Indexes, "Indexes"), (View::Schema, "Schema"), (View::Import, "Import"), (View::Compare, "Compare"), (View::Shell, "Shell"), (View::Watch, "Watch")];
pub const SQL_VIEWS: [(View, &str); 7] = [(View::Documents, "Content"), (View::Structure, "Structure"), (View::Relations, "Relations"), (View::Triggers, "Triggers"), (View::Info, "Info"), (View::Sql, "Query"), (View::Shell, "Shell")];

pub fn views(sql: bool) -> &'static [(View, &'static str)] {
    if sql { &SQL_VIEWS } else { &MONGO_VIEWS }
}

/// A command. Positions (`ViewAt`, `GoTab`) are fixed per entry of `CMDS`, never runtime ids, so each one is one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cmd {
    Reload,
    Run,
    RunAll,
    Explain,
    /// The view at this place of `views`, for the tab's engine.
    ViewAt(u8),
    NewQuery,
    CloseTab,
    /// Tab 1 to 8 by place; 8 is the last tab.
    GoTab(u8),
    NextTab,
    PrevTab,
    Back,
    Forward,
    FindQuery,
    FindColl,
    Connect,
    LockWrites,
    NewDb,
    Server,
    Users,
    Disconnect,
    EditConn,
    AllowWrites,
    SaveToLibrary,
    SaveAs,
    OpenPicker,
    ShowLibrary,
    LinkFolder,
    NewGroup,
    NewConn,
    NewTab,
    Palette,
    CloudSql,
    Reopen,
    Undo,
    Log,
    Settings,
    CheckUpdates,
    Shortcuts,
    Insert,
    Export,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Quit,
    Github,
    ReleaseNotes,
    ReportIssue,
    TextUndo,
    TextRedo,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

/// A key with its modifiers. `key` is egui's spelling of it (`Key::from_name`), which is also how menus and the sheet
/// print it. `cmd` is Cmd on macOS and Ctrl elsewhere; `ctrl` is the Control key itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub cmd: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: &'static str,
}

impl Chord {
    pub fn mods(&self) -> usize {
        [self.cmd, self.ctrl, self.shift, self.alt].iter().filter(|m| **m).count()
    }

    /// "Cmd+Shift+R", as the sheet lists it and muda parses it.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for (on, name) in [(self.cmd, "Cmd+"), (self.ctrl, "Ctrl+"), (self.shift, "Shift+"), (self.alt, "Alt+")] {
            if on {
                s += name;
            }
        }
        s + self.key
    }
}

const fn chord(cmd: bool, ctrl: bool, shift: bool, alt: bool, key: &'static str) -> Chord {
    Chord { cmd, ctrl, shift, alt, key }
}
const fn cm(key: &'static str) -> Chord {
    chord(true, false, false, false, key)
}
const fn cs(key: &'static str) -> Chord {
    chord(true, false, true, false, key)
}
const fn ca(key: &'static str) -> Chord {
    chord(true, false, false, true, key)
}

/// Where a command's key is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `App::keys` takes it on `Layer::Page` only; a menu pick calls the command.
    Page,
    /// A page command whose key a view, an editor or a dialog also reads itself (Cmd+S on staged edits, Cmd+F in a code
    /// editor, which Cmd+Shift+F also matches, Cmd+Enter in Aggregate and the Shell, Cmd+E in a snippet form).
    /// `App::keys` takes what is left on the page; a menu pick replays the key, because macOS hands a key equivalent only
    /// to the menu.
    View,
    /// On every layer, dialogs and windows included.
    App,
    /// egui's text fields or the system handle it; `App::keys` never reads it, the menu replays it.
    Native,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Menu {
    App,
    File,
    Edit,
    View,
    Query,
    Window,
    Help,
}

/// A command's place in the macOS menu bar: its menu, its group there (separators go between groups) and its title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuPath {
    pub menu: Menu,
    pub group: u8,
    pub title: &'static str,
}

/// The palette's sections, in the order it shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pal {
    Tab,
    Conn,
    Library,
    Groups,
    App,
}

pub const PAL_ORDER: [Pal; 5] = [Pal::Tab, Pal::Conn, Pal::Library, Pal::Groups, Pal::App];

pub struct CmdSpec {
    pub cmd: Cmd,
    /// The palette's name for it, when `title` has no other.
    pub label: &'static str,
    /// Its line in the Cmd+/ sheet, a second line under it after a newline; empty keeps it off the sheet.
    pub help: &'static str,
    /// The first is the one the menu and the palette show.
    pub keys: &'static [Chord],
    pub scope: Scope,
    pub menu: Option<MenuPath>,
    pub pal: Option<Pal>,
}

const fn s(cmd: Cmd, label: &'static str, help: &'static str, keys: &'static [Chord], scope: Scope, menu: Option<MenuPath>, pal: Option<Pal>) -> CmdSpec {
    CmdSpec { cmd, label, help, keys, scope, menu, pal }
}
const fn m(menu: Menu, group: u8, title: &'static str) -> Option<MenuPath> {
    Some(MenuPath { menu, group, title })
}

use Scope::{App as A, Native as N, Page as P, View as V};
const TAB: Option<Pal> = Some(Pal::Tab);
const CONN: Option<Pal> = Some(Pal::Conn);
const LIB: Option<Pal> = Some(Pal::Library);
const APP: Option<Pal> = Some(Pal::App);
const VIEWS_HELP: &str = "Documents, Aggregate, Indexes, Schema, Import, Compare, Shell, Watch\nOn a SQL tab: Content, Structure, Relations, Triggers, Info, Query, Shell";

/// Every command once. The palette lists a section's commands in this order and the menus a group's, and the sheet
/// follows it; `App::keys` tries them longest chord first (`KEY_ORDER`).
pub const CMDS: &[CmdSpec] = &[
    s(Cmd::Reload, "Reload tab", "Reload the current tab", &[cm("R")], P, m(Menu::Query, 1, "Reload Tab"), TAB),
    s(Cmd::Run, "Run", "Run the pipeline, script or SQL statement under the caret, or the selected script of the Library panel", &[cm("Enter")], V, m(Menu::Query, 0, "Run Pipeline or Script"), None),
    s(Cmd::RunAll, "Run all", "Run every statement of a SQL Query view", &[cs("Enter")], P, None, None),
    s(Cmd::Explain, "Explain", "Explain the query, pipeline or SQL statement", &[cm("E")], V, m(Menu::Query, 0, "Explain"), TAB),
    s(Cmd::ViewAt(0), "Documents or Content", VIEWS_HELP, &[ca("1")], P, m(Menu::View, 0, "Documents or Content"), TAB),
    s(Cmd::ViewAt(1), "Aggregate or Structure", VIEWS_HELP, &[ca("2")], P, m(Menu::View, 0, "Aggregate or Structure"), TAB),
    s(Cmd::ViewAt(2), "Indexes or Relations", VIEWS_HELP, &[ca("3")], P, m(Menu::View, 0, "Indexes or Relations"), TAB),
    s(Cmd::ViewAt(3), "Schema or Triggers", VIEWS_HELP, &[ca("4")], P, m(Menu::View, 0, "Schema or Triggers"), TAB),
    s(Cmd::ViewAt(4), "Import or Info", VIEWS_HELP, &[ca("5")], P, m(Menu::View, 0, "Import or Info"), TAB),
    s(Cmd::ViewAt(5), "Compare or Query", VIEWS_HELP, &[ca("6")], P, m(Menu::View, 0, "Compare or Query"), TAB),
    s(Cmd::ViewAt(6), "Shell", VIEWS_HELP, &[ca("7")], P, m(Menu::View, 0, "Shell"), TAB),
    s(Cmd::ViewAt(7), "Watch", VIEWS_HELP, &[ca("8")], P, m(Menu::View, 0, "Watch"), TAB),
    s(Cmd::NewQuery, "New query", "", &[], P, None, TAB),
    s(Cmd::CloseTab, "Close tab", "Close the tab", &[cm("W")], P, m(Menu::File, 3, "Close Tab"), TAB),
    s(Cmd::GoTab(0), "Go to tab 1", "Go to that tab", &[cm("1")], P, None, None),
    s(Cmd::GoTab(1), "Go to tab 2", "Go to that tab", &[cm("2")], P, None, None),
    s(Cmd::GoTab(2), "Go to tab 3", "Go to that tab", &[cm("3")], P, None, None),
    s(Cmd::GoTab(3), "Go to tab 4", "Go to that tab", &[cm("4")], P, None, None),
    s(Cmd::GoTab(4), "Go to tab 5", "Go to that tab", &[cm("5")], P, None, None),
    s(Cmd::GoTab(5), "Go to tab 6", "Go to that tab", &[cm("6")], P, None, None),
    s(Cmd::GoTab(6), "Go to tab 7", "Go to that tab", &[cm("7")], P, None, None),
    s(Cmd::GoTab(7), "Go to tab 8", "Go to that tab", &[cm("8")], P, None, None),
    s(Cmd::GoTab(8), "Go to the last tab", "Go to the last tab", &[cm("9")], P, None, None),
    s(Cmd::NextTab, "Next tab", "Next tab", &[cs("]"), cs("}"), chord(false, true, false, false, "Tab")], P, m(Menu::Window, 0, "Show Next Tab"), None),
    s(Cmd::PrevTab, "Previous tab", "Previous tab", &[cs("["), cs("{"), chord(false, true, true, false, "Tab")], P, m(Menu::Window, 0, "Show Previous Tab"), None),
    s(Cmd::Back, "Back", "Back and forward through the tabs you visited", &[cm("[")], P, m(Menu::Window, 0, "Back"), None),
    s(Cmd::Forward, "Forward", "Back and forward through the tabs you visited", &[cm("]")], P, m(Menu::Window, 0, "Forward"), None),
    s(Cmd::FindQuery, "Find in query", "Focus the query", &[cm("F")], V, m(Menu::Edit, 2, "Find in Query"), None),
    // "/" is a plain key, taken only while nothing is being typed into.
    s(Cmd::FindColl, "Find collection", "Find a collection in the sidebar", &[cs("F"), chord(false, false, false, false, "/")], V, m(Menu::Edit, 2, "Find Collection"), None),
    s(Cmd::Connect, "Refresh connection", "Refresh the connection's databases", &[cs("R")], P, m(Menu::Query, 1, "Refresh Connection"), CONN),
    s(Cmd::LockWrites, "Lock writes", "", &[], P, None, CONN),
    s(Cmd::NewDb, "New database\u{2026}", "", &[], P, None, CONN),
    s(Cmd::Server, "Server status", "", &[], P, None, CONN),
    s(Cmd::Users, "Users", "", &[], P, None, CONN),
    s(Cmd::Disconnect, "Disconnect", "", &[], P, None, CONN),
    s(Cmd::EditConn, "Edit connection\u{2026}", "", &[], P, None, CONN),
    s(Cmd::AllowWrites, "Read-only: allow writes\u{2026}", "", &[], P, None, CONN),
    s(Cmd::SaveToLibrary, "Save to library\u{2026}", "Save the script to the library, or write the library script or file it came from", &[cm("S")], V, m(Menu::File, 1, "Save to library"), LIB),
    s(Cmd::SaveAs, "Save to library as new", "Save the script to the library as a new one", &[cs("S")], P, None, None),
    s(Cmd::OpenPicker, "Pick a script", "Pick a script of the library for this view", &[cm("O")], P, None, None),
    s(Cmd::ShowLibrary, "Show the library", "Show the Library panel and move the keys to it", &[cs("L")], P, None, LIB),
    s(Cmd::LinkFolder, "Link a folder\u{2026}", "", &[], P, None, LIB),
    s(Cmd::NewGroup, "New group", "", &[], P, None, Some(Pal::Groups)),
    s(Cmd::NewConn, "New connection\u{2026}", "New connection", &[cm("N")], P, m(Menu::File, 0, "New Connection\u{2026}"), APP),
    s(Cmd::NewTab, "New tab", "Open a collection in a new tab", &[cm("T")], P, m(Menu::File, 0, "New Tab\u{2026}"), None),
    s(Cmd::Palette, "Jump to", "Jump to a connection, database or collection", &[cm("K")], P, m(Menu::File, 0, "Open Collection\u{2026}"), None),
    s(Cmd::CloudSql, "Add from Cloud SQL\u{2026}", "", &[], P, None, APP),
    s(Cmd::Reopen, "Reopen closed tab", "Reopen the last closed tab", &[cs("T")], P, m(Menu::File, 0, "Reopen Closed Tab"), APP),
    s(Cmd::Undo, "Recent changes", "", &[], P, None, APP),
    s(Cmd::Log, "Query log", "Show or hide the query log", &[cm("L")], P, m(Menu::View, 1, "Query Log"), APP),
    s(Cmd::Settings, "Settings", "Settings", &[cm(",")], P, m(Menu::App, 1, "Settings\u{2026}"), APP),
    s(Cmd::CheckUpdates, "Check for updates", "", &[], A, m(Menu::App, 0, "Check for Updates\u{2026}"), APP),
    s(Cmd::Shortcuts, "Keyboard shortcuts", "This list", &[cm("/")], P, m(Menu::Help, 0, "Keyboard Shortcuts"), APP),
    s(Cmd::Insert, "Insert document", "", &[], A, m(Menu::File, 2, "Insert Document\u{2026}"), None),
    s(Cmd::Export, "Export", "", &[], A, m(Menu::File, 2, "Export\u{2026}"), None),
    s(Cmd::ZoomIn, "Bigger interface", "", &[cm("="), cm("+")], A, m(Menu::View, 2, "Bigger Interface"), None),
    s(Cmd::ZoomOut, "Smaller interface", "", &[cm("-")], A, m(Menu::View, 2, "Smaller Interface"), None),
    s(Cmd::ZoomReset, "Actual size", "", &[cm("0")], A, m(Menu::View, 2, "Actual Size"), None),
    s(Cmd::Quit, "Quit", "", &[cm("Q")], N, m(Menu::App, 2, "Quit Quarry"), None),
    s(Cmd::Github, "Quarry on GitHub", "", &[], A, m(Menu::Help, 1, "Quarry on GitHub"), None),
    s(Cmd::ReleaseNotes, "Release notes", "", &[], A, m(Menu::Help, 1, "Release Notes"), None),
    s(Cmd::ReportIssue, "Report an issue", "", &[], A, m(Menu::Help, 1, "Report an Issue"), None),
    s(Cmd::TextUndo, "Undo", "", &[cm("Z")], N, m(Menu::Edit, 0, "Undo"), None),
    s(Cmd::TextRedo, "Redo", "", &[cs("Z")], N, m(Menu::Edit, 0, "Redo"), None),
    s(Cmd::Cut, "Cut", "", &[cm("X")], N, m(Menu::Edit, 1, "Cut"), None),
    s(Cmd::Copy, "Copy", "", &[cm("C")], N, m(Menu::Edit, 1, "Copy"), None),
    s(Cmd::Paste, "Paste", "", &[cm("V")], N, m(Menu::Edit, 1, "Paste"), None),
    s(Cmd::SelectAll, "Select all", "", &[cm("A")], N, m(Menu::Edit, 1, "Select All"), None),
];

/// Keys that widgets read themselves, listed on the Cmd+/ sheet after the commands. Cmd is Ctrl on Linux and Windows.
pub const NOTES: &[(&str, &str)] = &[
    ("Cmd+click, middle-click", "Open a collection of the sidebar or Cmd+K in a new tab; Cmd+Enter in Cmd+K"),
    ("Up, Down", "Select the previous or next document"),
    ("PageUp, PageDown", "Previous or next page"),
    ("Enter", "Run the query"),
    ("Esc", "Close or deselect"),
    ("Alt+Up/Down (Left/Right)", "Move the focused row, column, tab or stage"),
    ("Cmd+Backspace, Delete", "Delete the selected script or folder of the Library panel; the toast offers Undo"),
    ("Ctrl+Space", "Suggestions in a query or SQL field, also where none show by themselves"),
    ("Tab", "Show the selected SQL row as a record, one line per column; Esc closes it"),
    ("Vim motions", "Modal editing in the code editors, turned on in Settings > Editing"),
    ("", "With Vim motions on, where no text field has the keys:"),
    ("h j k l, w b", "Move the grid's cell cursor; in a record, j and k walk its lines and h and l fold its JSON"),
    ("gg, G, {n}G, 0, $", "First, last or nth row; first or last column"),
    ("Ctrl+d/u, Ctrl+f/b", "Half a screen or a screen of rows down or up (Alt where Ctrl is the command key)"),
    ("i, v", "Edit the cell under the cursor, or view its value in full"),
    ("yy, Y", "Copy the cell's value, or the row as JSON"),
    ("gt, gT, {n}gt", "Next, previous or nth tab"),
    ("Ctrl+h, Ctrl+k, Ctrl+j/l", "Move the keys to the sidebar, the query or editor, the grid (Alt where Ctrl is the command key)"),
    ("Enter", "Open the selected script of the Library panel"),
    ("Alt+Enter", "Open the selected script of the Library panel in a new tab"),
    ("F2", "Rename the selected script or folder of the Library panel"),
];

/// Every chord with its command, longest first: extra Shift and Alt still match a shorter shortcut (egui ignores
/// modifiers a shortcut does not name), so Cmd+Shift+T must be tried before Cmd+T. Stable, so equal lengths keep `CMDS`
/// order.
pub static KEY_ORDER: LazyLock<Vec<(&'static CmdSpec, Chord)>> = LazyLock::new(|| {
    let mut v: Vec<(&'static CmdSpec, Chord)> = CMDS.iter().flat_map(|s| s.keys.iter().map(move |c| (s, *c))).collect();
    v.sort_by_key(|(_, c)| std::cmp::Reverse(c.mods()));
    v
});

/// The items of `menu` in `groups`, in order, with None for the separator between two groups.
pub fn menu_items(menu: Menu, groups: std::ops::RangeInclusive<u8>) -> Vec<Option<&'static CmdSpec>> {
    let mut items: Vec<(u8, &'static CmdSpec)> = CMDS.iter().filter_map(|s| s.menu.filter(|p| p.menu == menu && groups.contains(&p.group)).map(|p| (p.group, s))).collect();
    items.sort_by_key(|i| i.0);
    let mut out = Vec::new();
    for (i, (g, s)) in items.iter().enumerate() {
        if i > 0 && items[i - 1].0 != *g {
            out.push(None);
        }
        out.push(Some(*s));
    }
    out
}

/// The Cmd+/ sheet: the commands with a key, neighbours with the same line folded into one ("Cmd+1 to Cmd+8"), then
/// the keys widgets read.
pub fn sheet() -> String {
    let rows: Vec<&CmdSpec> = CMDS.iter().filter(|s| !s.help.is_empty() && !s.keys.is_empty()).collect();
    let mut lines = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let run = rows[i..].iter().take_while(|r| r.help == rows[i].help).count();
        let keys = match run {
            1 => rows[i].keys.iter().map(Chord::text).collect::<Vec<_>>().join(", "),
            2 => format!("{}, {}", rows[i].keys[0].text(), rows[i + 1].keys[0].text()),
            _ => format!("{} to {}", rows[i].keys[0].text(), rows[i + run - 1].keys[0].text()),
        };
        // A help line after the first goes on its own row, under the first; long keys keep a gap before their help.
        for (j, what) in rows[i].help.lines().enumerate() {
            let k = if j == 0 { keys.as_str() } else { "" };
            lines.push(format!("{k:<w$}{what}", w = 30.max(k.len() + 2)));
        }
        i += run;
    }
    lines.extend(NOTES.iter().map(|(k, what)| format!("{k:<30}{what}")));
    lines.join("\n")
}

/// The palette's command rows that apply now, by section in `PAL_ORDER`.
pub fn palette(core: &Core) -> impl Iterator<Item = &'static CmdSpec> + '_ {
    PAL_ORDER.into_iter().flat_map(move |p| CMDS.iter().filter(move |s| s.pal == Some(p) && s.cmd.available(core)))
}

impl Cmd {
    pub fn spec(self) -> &'static CmdSpec {
        CMDS.iter().find(|s| s.cmd == self).expect("every command is in CMDS")
    }

    pub fn url(self) -> Option<&'static str> {
        match self {
            Cmd::Github => Some("https://github.com/alkanna/quarry"),
            Cmd::ReleaseNotes => Some("https://github.com/alkanna/quarry/releases"),
            Cmd::ReportIssue => Some("https://github.com/alkanna/quarry/issues"),
            _ => None,
        }
    }

    /// Its palette name, which for a few names what it acts on now.
    pub fn title(self, core: &Core) -> String {
        let c = core.active.and_then(|id| core.conn(id));
        match self {
            Cmd::ViewAt(i) => format!("Show {}", views(tab_sql(core)).get(i as usize).map_or("", |v| v.1)),
            Cmd::NewQuery => format!("New query in {}", core.t().map_or("", |t| t.sel.db.as_str())),
            Cmd::Connect if !c.is_some_and(|c| c.status == Status::Ready) => "Connect".into(),
            Cmd::Users if c.is_some_and(|c| c.meta.engine == Engine::Postgres) => "Roles".into(),
            _ => self.spec().label.into(),
        }
    }

    /// Whether the palette lists it: the current tab's commands with a tab, the connection's with an active one.
    pub fn available(self, core: &Core) -> bool {
        let t = core.t();
        let c = core.active.and_then(|id| core.conn(id));
        let ready = c.is_some_and(|c| c.status == Status::Ready);
        let lockable = ready && !core.prefs.always_writable;
        let writable = c.is_some_and(|c| c.writable);
        match self {
            Cmd::Reload | Cmd::Explain | Cmd::CloseTab => t.is_some(),
            Cmd::ViewAt(i) => t.is_some_and(|t| !t.sel.coll.is_empty() && views(tab_sql(core)).get(i as usize).is_some_and(|v| v.0 != t.view)),
            Cmd::NewQuery => t.is_some() && tab_sql(core),
            Cmd::Connect => c.is_some_and(|c| c.status != Status::Connecting),
            Cmd::LockWrites => lockable && writable,
            Cmd::NewDb => ready && c.is_some_and(|c| !c.meta.engine.is_sql()),
            Cmd::Server | Cmd::Users | Cmd::Disconnect => ready,
            Cmd::EditConn => c.is_some(),
            Cmd::AllowWrites => lockable && !writable,
            Cmd::SaveToLibrary => core.tab_has_text(),
            Cmd::CloudSql => !c.is_some_and(|c| !c.meta.engine.is_sql()),
            Cmd::Reopen => !core.closed.is_empty(),
            Cmd::Undo => !core.undo.is_empty(),
            Cmd::CheckUpdates => core.can_update(),
            _ => true,
        }
    }

    /// Whether `App::keys` takes the key now; when false the key is left alone for whatever else reads it. `text` is
    /// whether a text field has the keys.
    pub fn keys_on(self, core: &Core, text: bool) -> bool {
        let n = core.tabs.len();
        let query_view = core.t().is_some_and(|t| t.view == View::Sql && t.sql.is_some());
        // With the Library tree focused, Cmd+Enter runs the selected script, which the panel takes itself. The tree has the keys
        // only while it is on screen and no text field has taken them, so a flag left from a click never holds the editor's.
        let tree_keys = core.lib_ui.panel.focus && !core.prefs.sidebar_hidden && core.prefs.side_mode() == SideMode::Library && !text;
        match self {
            // The cell editor of a SQL grid owns Cmd+S while it is open; a view that holds no script leaves it alone.
            Cmd::SaveToLibrary | Cmd::SaveAs => core.sql_cell.is_none() && core.tab_has_text(),
            Cmd::OpenPicker => core.tab_has_text(),
            Cmd::Run | Cmd::RunAll => query_view && !tree_keys,
            Cmd::NextTab | Cmd::PrevTab | Cmd::Back | Cmd::Forward | Cmd::GoTab(_) => n > 0,
            // A SQL tab has seven views, so Cmd+Alt+8 is left for Cmd+8.
            Cmd::ViewAt(i) => n > 0 && (i as usize) < views(core.t().is_some_and(|t| t.sql.is_some())).len(),
            _ => true,
        }
    }

    /// What it does on the current tab or the active connection, when that is an `Action`; None leaves it to the app.
    pub fn action(self, core: &Core) -> Option<Action> {
        let t = core.t();
        let n = core.tabs.len();
        let pick = |i: usize| Some(Action::Tab(TabA::Pick { tab: core.tabs.get(i)?.uid }));
        let conn = core.active.and_then(|id| core.conn(id)).map(|c| (c.meta.id, c.meta.engine));
        let query_view = t.is_some_and(|t| t.view == View::Sql && t.sql.is_some());
        match self {
            Cmd::Run | Cmd::RunAll | Cmd::Explain if query_view => Some(Action::Sql(SqlA::Run { tab: t?.uid, all: self == Cmd::RunAll, explain: self == Cmd::Explain })),
            Cmd::ViewAt(i) => Some(Action::Tab(TabA::View { tab: t?.uid, view: views(t?.sql.is_some()).get(i as usize)?.0 })),
            Cmd::GoTab(i) if n > 0 => pick(if i == 8 { n - 1 } else { (i as usize).min(n - 1) }),
            Cmd::NextTab if n > 0 => pick((core.cur + 1) % n),
            Cmd::PrevTab if n > 0 => pick((core.cur + n - 1) % n),
            Cmd::Back | Cmd::Forward => Some(Action::Tab(TabA::Nav { back: self == Cmd::Back })),
            Cmd::Reopen => Some(Action::Tab(TabA::Reopen)),
            Cmd::Connect if !core.demo => Some(Action::Conn(ConnA::Connect { conn: conn?.0 })),
            Cmd::Disconnect => Some(Action::Conn(ConnA::Disconnect { conn: conn?.0 })),
            Cmd::LockWrites => Some(Action::Conn(ConnA::Lock { conn: conn?.0 })),
            Cmd::Server => Some(Action::Server(match conn? {
                (conn, Engine::MySql) => ServerA::Sql(SqlServerA::Open { conn }),
                (conn, Engine::Postgres) => ServerA::Pg(PgServerA::Open { conn }),
                (conn, Engine::Mongo) => ServerA::Mongo(MongoServerA::Open { conn }),
            })),
            Cmd::Users => Some(Action::Users(match conn? {
                (conn, e) if e.is_sql() => UsersA::Sql(SqlUsersA::Open { conn }),
                (conn, _) => UsersA::Mongo(MongoUsersA::Open { conn }),
            })),
            Cmd::NewDb => Some(match conn? {
                (conn, e) if e.is_sql() => Action::Ddl(DdlA::NewDb { conn }),
                (conn, _) => Action::Side(SideA::MongoNewDb { conn }),
            }),
            Cmd::CheckUpdates => Some(Action::Update(UpdateA::CheckNow)),
            _ => None,
        }
    }
}

/// Whether the current tab is on a SQL connection, as the palette has always judged it.
fn tab_sql(core: &Core) -> bool {
    core.t().is_some_and(|t| core.conn(t.sel.conn).is_some_and(|c| c.meta.engine.is_sql()))
}

/// What is over the page. Page commands run only on `Page`; the app adds `Modal`, which it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Page,
    Window,
    Modal,
}

/// Every floating window Core holds, once. Each takes its own keys, Esc included, so page commands stay off while one is
/// open; a new kind of window belongs in this list.
pub const WINDOWS: &[(&str, fn(&Core) -> bool)] = &[
    ("cloud_browser", |c| c.cloud_browser.is_some()),
    ("insert", |c| c.insert.is_some()),
    ("value_dialog", |c| c.value_dialog_open()),
    ("users", |c| c.users.is_some()),
    ("server", |c| c.server.is_some()),
    ("sql_users", |c| c.sql_users.is_some()),
    ("pg_roles", |c| c.pg_roles.is_some()),
    ("sql_server", |c| c.sql_server.is_some()),
    ("pg_server", |c| c.pg_server.is_some()),
    ("routines", |c| c.routines.window_open()),
    ("tool", |c| c.tool.is_some()),
    ("agent_asks", |c| !c.agents.asks.is_empty()),
    ("agent_activity", |c| c.agents.activity_open),
    ("agent_rules", |c| c.agents.rules.is_some()),
];

impl Core {
    pub fn layer(&self) -> Layer {
        if WINDOWS.iter().any(|(_, open)| open(self)) { Layer::Window } else { Layer::Page }
    }

    /// Whether the current tab's view holds a script (Query, Shell, Documents, Aggregate, a SQL table's Content), without
    /// copying its text: the keys ask every frame.
    pub fn tab_has_text(&self) -> bool {
        self.t().is_some_and(|t| matches!((t.view, t.sql.is_some()), (View::Sql, true) | (View::Shell, _) | (View::Documents | View::Aggregate, false)) || (t.view == View::Documents && t.sql.is_some() && !t.sel.coll.is_empty()))
    }
}
