//! Vim keys outside the code editors, behind the same Settings > Editing > Vim motions switch.
//!
//! `begin` reads this pass's keys before anything draws. While the results grid has the keys, h j k l move a cell cursor
//! in it, or a line cursor in the record Tab opens, and a few more keys act on the cell under it. Anywhere else that no
//! text field has the keys, such as a focused sidebar row or the Library tree, h j k l, gg and G become the arrows, Home
//! and End that those already take, and `end` moves egui's focus for the arrows nothing took. Ctrl+h j k l move between
//! the sidebar, the query and the grid, from anywhere, editors included. Where Ctrl is the command key, Alt stands in for
//! it in those and in Ctrl+d u f b.

use super::*;
use egui::{Event, FocusDirection, Modifiers};

/// The record's tree for line `c` is folded by `sql_record`, which knows its id.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Act {
    Edit,
    View,
    Yank,
    YankRow,
    Fold(bool),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Cmd {
    /// Rows down (positive) or up, then columns right or left.
    Move(i64, i64),
    /// gg, or {n}gg and {n}G: the first row, or row n.
    Row(Option<usize>),
    /// G: the last row.
    Last,
    /// 0 and ^, and $.
    Col(bool),
    /// Half a screen of rows down or up.
    Half(bool),
    /// A screen of rows down or up.
    Screen(bool),
    /// gt and gT, with {n}gt going to tab n.
    Tab(Option<usize>, bool),
    Act(Act),
}

#[derive(Clone, Copy, PartialEq)]
enum Pane {
    Left,
    Down,
    Up,
    Right,
}

#[derive(Default)]
pub(super) struct VimNav {
    /// A count and a g or y typed so far, kept across passes until the command is whole.
    pending: String,
    /// What the grid's next draw does at the cursor.
    pub(super) act: Option<Act>,
    /// The grid scrolls its cursor's cell, or the record its line, into view on the next draw.
    pub(super) scroll: bool,
    /// What the grid drew last pass: its columns on screen, the lines of its record and the rows that fit.
    pub(super) cols: usize,
    pub(super) lines: usize,
    pub(super) page: usize,
    /// Arrows put in for keys typed with no grid to take them, which `end` turns into focus moves if nothing took them.
    injected: Vec<Key>,
}

/// The sidebar row that should take the keys, asked for by Ctrl+h and given by the rows as they draw.
fn side_id() -> egui::Id {
    egui::Id::unique("vim-side-focus")
}

fn first_id() -> egui::Id {
    egui::Id::unique("vim-side-first")
}

/// Called by each row of the sidebar's lists after it is drawn. The active one takes the focus Ctrl+h asked for; the first
/// one drawn is kept for `side_done` in case none is active.
pub(super) fn side_row(ctx: &egui::Context, r: &egui::Response, active: bool) {
    if !ctx.data(|d| d.get_temp::<bool>(side_id())).unwrap_or(false) {
        return;
    }
    ctx.data_mut(|d| {
        d.get_temp_mut_or_insert_with(first_id(), || r.id);
    });
    if active {
        r.request_focus();
        ctx.data_mut(|d| d.remove::<bool>(side_id()));
    }
}

/// After the sidebar drew: with no active row, the first row takes the focus.
pub(super) fn side_done(ctx: &egui::Context) {
    let want = ctx.data_mut(|d| d.remove_temp::<bool>(side_id())).unwrap_or(false);
    let first = ctx.data_mut(|d| {
        let id = d.get_temp::<egui::Id>(first_id());
        d.remove::<egui::Id>(first_id());
        id
    });
    if let (true, Some(id)) = (want, first) {
        ctx.memory_mut(|m| m.request_focus(id));
    }
}

/// Reads one key of a Normal mode command, with `pending` the count and prefix typed before it. Returns the command once
/// whole, and whether the key was Vim's: a key that is none, with nothing pending, is left to the app.
fn feed(pending: &mut String, c: char) -> (bool, Option<Cmd>) {
    let digits = pending.chars().take_while(char::is_ascii_digit).count();
    let count: Option<usize> = pending[..digits].parse().ok();
    let n = count.unwrap_or(1).min(100_000) as i64;
    let prefix = pending[digits..].to_string();
    if prefix.is_empty() && (c.is_ascii_digit() && (c != '0' || count.is_some())) {
        if pending.len() < 6 {
            pending.push(c);
        }
        return (true, None);
    }
    let cmd = match (prefix.as_str(), c) {
        ("", 'g' | 'y') => {
            pending.push(c);
            return (true, None);
        }
        ("g", 'g') => Some(Cmd::Row(count)),
        ("g", 't') => Some(Cmd::Tab(count, true)),
        ("g", 'T') => Some(Cmd::Tab(None, false)),
        ("y", 'y') => Some(Cmd::Act(Act::Yank)),
        ("", 'j') => Some(Cmd::Move(n, 0)),
        ("", 'k') => Some(Cmd::Move(-n, 0)),
        ("", 'l' | 'w') => Some(Cmd::Move(0, n)),
        ("", 'h' | 'b') => Some(Cmd::Move(0, -n)),
        ("", 'G') => Some(count.map_or(Cmd::Last, |r| Cmd::Row(Some(r)))),
        ("", '0' | '^') => Some(Cmd::Col(false)),
        ("", '$') => Some(Cmd::Col(true)),
        ("", 'i' | 'a') => Some(Cmd::Act(Act::Edit)),
        ("", 'v') => Some(Cmd::Act(Act::View)),
        ("", 'Y') => Some(Cmd::Act(Act::YankRow)),
        _ => None,
    };
    let ours = cmd.is_some() || !pending.is_empty();
    pending.clear();
    (ours, cmd)
}

/// Pane and screen keys: Ctrl where it is not the command key, as on macOS, else Alt.
fn pane_mods(m: &Modifiers) -> bool {
    !m.shift && !m.command && ((m.ctrl && !m.alt) || (cfg!(not(target_os = "macos")) && m.alt && !m.ctrl))
}

fn arrow(dir: FocusDirection) -> Key {
    match dir {
        FocusDirection::Up => Key::ArrowUp,
        FocusDirection::Down => Key::ArrowDown,
        FocusDirection::Left => Key::ArrowLeft,
        _ => Key::ArrowRight,
    }
}

impl App {
    /// The grid that has the keys, as `keys` gives them: the Documents or Aggregate grid of `selected`, or the Query view's.
    fn vim_grid(&self, ctx: &egui::Context) -> Option<View> {
        let t = self.t()?;
        let lib = self.lib_ui.panel.focus && !self.prefs.sidebar_hidden && self.prefs.side_mode() == SideMode::Library;
        let panel = t.sql.as_ref().is_some_and(|s| s.bar.panel.open && s.bar.panel.sel.is_some());
        if ctx.memory(|m| m.focused().is_some()) || ctx.any_popup_open() || self.sql_cell.is_some() || self.inline.is_some() || lib || panel {
            return None;
        }
        match (t.view, &t.sql) {
            (View::Documents | View::Aggregate, _) => Some(t.view),
            (View::Sql, Some(s)) if !s.text_mode && s.cur_set().is_some() => Some(View::Sql),
            _ => None,
        }
    }

    /// The selected row and the row count of that grid.
    fn vim_rows(&self, view: View) -> (Option<usize>, usize) {
        let Some(t) = self.t() else { return (None, 0) };
        match view {
            View::Sql => t.sql.as_ref().map_or((None, 0), |s| (s.qsel, s.cur_set().map_or(0, |x| x.rows.len()))),
            v => {
                let agg = v == View::Aggregate;
                (t.selected.filter(|s| s.0 == agg).map(|s| s.1), if agg { t.agg_docs.len() } else { t.docs.len() })
            }
        }
    }

    /// Selects row `i` of that grid as Up and Down do.
    fn vim_select(&mut self, view: View, i: usize) {
        self.scroll_sel = true;
        self.vnav.scroll = true;
        if view == View::Sql {
            if let Some(s) = self.tm().and_then(|t| t.sql.as_mut()) {
                s.qsel = Some(i);
            }
            return;
        }
        let agg = view == View::Aggregate;
        if self.t().and_then(|t| t.selected) == Some((agg, i)) {
            return;
        }
        // The open card moves with the cursor instead of leaving the previous one open.
        if let Some(prev) = self.t().and_then(|t| t.selected) {
            self.tm().unwrap().expanded.remove(&prev);
        }
        self.select(agg, i, false);
    }

    /// Reads this pass's Vim keys before anything draws, so no widget acts on them too.
    pub(super) fn vim_begin(&mut self, ctx: &egui::Context) {
        self.vnav.injected.clear();
        if !self.prefs.vim || self.window_open() {
            self.vnav.pending.clear();
            return;
        }
        let mut pane = None;
        ctx.input_mut(|i| {
            i.events.retain(|e| match e {
                Event::Key { key: k @ (Key::H | Key::J | Key::K | Key::L), pressed, modifiers, .. } if pane_mods(modifiers) => {
                    if *pressed {
                        pane = Some(match k {
                            Key::H => Pane::Left,
                            Key::J => Pane::Down,
                            Key::K => Pane::Up,
                            _ => Pane::Right,
                        });
                    }
                    false
                }
                _ => true,
            })
        });
        if let Some(p) = pane {
            self.vnav.pending.clear();
            return self.vim_pane(ctx, p);
        }
        // A text field, a cell editor that reads the keys itself, or an open date picker keeps every key.
        if ctx.text_edit_focused() || self.sql_cell.as_ref().is_some_and(|e| e.grabs_keys) || self.inline.as_ref().is_some_and(|i| i.pick.is_some()) {
            self.vnav.pending.clear();
            return;
        }
        let grid = self.vim_grid(ctx);
        let mut cmds = Vec::new();
        let mut pending = std::mem::take(&mut self.vnav.pending);
        ctx.input_mut(|i| {
            let mut kept = Vec::with_capacity(i.events.len());
            for e in std::mem::take(&mut i.events) {
                match &e {
                    Event::Text(t) => {
                        let mut rest = String::new();
                        for c in t.chars() {
                            let (ours, cmd) = feed(&mut pending, c);
                            cmds.extend(cmd);
                            if !ours {
                                rest.push(c);
                            }
                        }
                        if !rest.is_empty() {
                            kept.push(Event::Text(rest));
                        }
                    }
                    // Esc drops a half typed command before it closes anything.
                    Event::Key { key: Key::Escape, pressed: true, modifiers, .. } if modifiers.is_none() && !pending.is_empty() => pending.clear(),
                    Event::Key { key: k @ (Key::D | Key::U | Key::F | Key::B), pressed, modifiers, .. } if grid.is_some() && pane_mods(modifiers) => {
                        if *pressed {
                            cmds.push(match k {
                                Key::D => Cmd::Half(true),
                                Key::U => Cmd::Half(false),
                                Key::F => Cmd::Screen(true),
                                _ => Cmd::Screen(false),
                            });
                        }
                    }
                    _ => kept.push(e),
                }
            }
            i.events = kept;
        });
        self.vnav.pending = pending;
        for c in cmds {
            match grid {
                Some(v) => self.vim_grid_cmd(v, c),
                None => self.vim_elsewhere(ctx, c),
            }
        }
    }

    /// What a Vim key asked of the Query grid's cell under the cursor, or of its record's line, as its cell menu does.
    /// `names` are the columns the cursor walks. Copying the row is done here, since the menu has no such action.
    pub(super) fn vim_cell_act(&mut self, ctx: &egui::Context, sel: Option<usize>, at: usize, names: &[String], rows: Rows) -> Option<(usize, String, CellAct)> {
        let i = sel.filter(|_| self.prefs.vim)?;
        let a = self.vnav.act.filter(|a| !matches!(a, Act::Fold(_)))?;
        self.vnav.act = None;
        let c = names.get(at.min(names.len().saturating_sub(1)))?.clone();
        match a {
            Act::Yank => Some((i, c, CellAct::CopyValue)),
            Act::YankRow => {
                ctx.copy_text(db::to_edit_value(&Bson::Document(rows.owned(i)?)));
                self.ok("Copied the row as JSON".to_string());
                None
            }
            _ => Some((i, c, CellAct::View)),
        }
    }

    /// Turns the arrows put in by `vim_elsewhere` that nothing took into focus moves, which is what egui does with real
    /// ones before the pass. Called after everything drew.
    pub(super) fn vim_end(&mut self, ctx: &egui::Context) {
        // A grid action or scroll that no grid drew this pass is dropped, so it never fires on a later one.
        self.vnav.act = None;
        self.vnav.scroll = false;
        let injected = std::mem::take(&mut self.vnav.injected);
        if injected.is_empty() || ctx.memory(|m| m.focused().is_none()) {
            return;
        }
        let left: Vec<Key> = ctx.input(|i| i.events.iter().filter_map(|e| if let Event::Key { key, pressed: true, physical_key: None, .. } = e { injected.contains(key).then_some(*key) } else { None }).collect());
        let dir = left.last().map(|k| match k {
            Key::ArrowUp => FocusDirection::Up,
            Key::ArrowDown => FocusDirection::Down,
            Key::ArrowLeft => FocusDirection::Left,
            _ => FocusDirection::Right,
        });
        if let Some(d) = dir {
            ctx.memory_mut(|m| m.move_focus(d));
        }
    }

    /// A command typed where no grid has the keys: the moves become the keys the lists and egui's focus already take.
    fn vim_elsewhere(&mut self, ctx: &egui::Context, c: Cmd) {
        let mut push = |key: Key, n: i64| {
            let n = n.unsigned_abs().min(200);
            ctx.input_mut(|i| {
                for _ in 0..n {
                    i.events.push(Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
                }
            });
            self.vnav.injected.push(key);
        };
        match c {
            Cmd::Move(r, 0) => push(arrow(if r > 0 { FocusDirection::Down } else { FocusDirection::Up }), r),
            Cmd::Move(0, c) => push(arrow(if c > 0 { FocusDirection::Right } else { FocusDirection::Left }), c),
            Cmd::Row(None) => push(Key::Home, 1),
            Cmd::Last => push(Key::End, 1),
            Cmd::Tab(n, next) => self.vim_tab(n, next),
            _ => {}
        }
    }

    fn vim_tab(&mut self, n: Option<usize>, next: bool) {
        let len = self.tabs.len();
        if len == 0 {
            return;
        }
        let to = match n {
            Some(n) => n.clamp(1, len) - 1,
            None if next => (self.cur + 1) % len,
            None => (self.cur + len - 1) % len,
        };
        self.go_tab(to);
    }

    fn vim_grid_cmd(&mut self, view: View, c: Cmd) {
        let (sel, len) = self.vim_rows(view);
        let record = self.t().is_some_and(|t| t.record && t.record_row());
        let (cols, lines, page) = (self.vnav.cols.max(1), self.vnav.lines.max(1), self.vnav.page.max(2));
        let row = |r: i64| r.clamp(0, len.saturating_sub(1) as i64) as usize;
        let at = sel.map_or(0, |s| s as i64);
        // In the record the rows stay put and the cursor walks its lines; h and l fold and unfold a JSON value.
        if record {
            let line = self.t().map_or(0, |t| t.vline) as i64;
            let to = match c {
                Cmd::Move(r, 0) => line + r,
                Cmd::Move(0, c) => {
                    self.vnav.act = Some(Act::Fold(c > 0));
                    return;
                }
                Cmd::Row(None) => 0,
                Cmd::Row(Some(n)) => n as i64 - 1,
                Cmd::Last => lines as i64 - 1,
                Cmd::Half(down) => line + if down { 1 } else { -1 } * (page / 2) as i64,
                Cmd::Tab(n, next) => return self.vim_tab(n, next),
                Cmd::Act(a) => {
                    self.vnav.act = Some(a);
                    return;
                }
                _ => return,
            };
            if let Some(t) = self.tm() {
                t.vline = to.clamp(0, lines as i64 - 1) as usize;
            }
            self.vnav.scroll = true;
            return;
        }
        if len == 0 {
            return;
        }
        let to = match c {
            Cmd::Move(r, c) => {
                if let Some(t) = self.tm() {
                    t.vcol = (t.vcol.min(cols - 1) as i64 + c).clamp(0, cols as i64 - 1) as usize;
                }
                self.vnav.scroll = true;
                // The first move only picks the first row.
                match sel {
                    None => 0,
                    Some(_) if r == 0 => return,
                    Some(_) => row(at + r),
                }
            }
            Cmd::Row(n) => row(n.map_or(0, |n| n as i64 - 1)),
            Cmd::Last => len - 1,
            Cmd::Col(end) => {
                if let Some(t) = self.tm() {
                    t.vcol = if end { cols - 1 } else { 0 };
                }
                self.vnav.scroll = true;
                if sel.is_some() {
                    return;
                }
                0
            }
            Cmd::Half(down) => row(at + if down { 1 } else { -1 } * (page / 2) as i64),
            Cmd::Screen(down) => row(at + if down { 1 } else { -1 } * page as i64),
            Cmd::Tab(n, next) => return self.vim_tab(n, next),
            Cmd::Act(a) => {
                self.vnav.act = Some(a);
                if sel.is_some() {
                    return;
                }
                0
            }
        };
        self.vim_select(view, to);
    }

    /// Ctrl+h to the sidebar, Ctrl+k to the view's query or editor, Ctrl+j and Ctrl+l to the grid.
    fn vim_pane(&mut self, ctx: &egui::Context, p: Pane) {
        let surrender = || ctx.memory_mut(|m| m.focused().map(|f| m.surrender_focus(f)));
        match p {
            Pane::Left => {
                surrender();
                if self.prefs.sidebar_hidden {
                    self.prefs.sidebar_hidden = false;
                    self.save_prefs();
                }
                if self.prefs.side_mode() == SideMode::Library {
                    self.lib_ui.focus_tree = true;
                } else {
                    ctx.data_mut(|d| {
                        d.insert_temp(side_id(), true);
                        d.remove::<egui::Id>(first_id());
                    });
                }
            }
            Pane::Up => {
                self.lib_ui.panel.focus = false;
                if self.t().is_some_and(|t| t.view == View::Documents) {
                    self.focus_query(ctx);
                } else if let Some(id) = crate::ui::vim::page_editor(ctx) {
                    ctx.memory_mut(|m| m.request_focus(id));
                }
            }
            Pane::Down | Pane::Right => {
                surrender();
                self.lib_ui.panel.focus = false;
                self.sql_cell = None;
                self.inline = None;
                if let Some(v) = self.vim_grid(ctx) {
                    let (sel, len) = self.vim_rows(v);
                    if sel.is_none() && len > 0 {
                        self.vim_select(v, 0);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(keys: &str) -> (String, Vec<Cmd>, String) {
        let (mut p, mut cmds, mut rest) = (String::new(), Vec::new(), String::new());
        for c in keys.chars() {
            let (ours, cmd) = feed(&mut p, c);
            cmds.extend(cmd);
            if !ours {
                rest.push(c);
            }
        }
        (p, cmds, rest)
    }

    #[test]
    fn counts_and_prefixes_make_one_command() {
        assert_eq!(run("5j").1, [Cmd::Move(5, 0)]);
        assert_eq!(run("12G").1, [Cmd::Row(Some(12))]);
        assert_eq!(run("gg").1, [Cmd::Row(None)]);
        assert_eq!(run("G").1, [Cmd::Last]);
        assert_eq!(run("0$").1, [Cmd::Col(false), Cmd::Col(true)]);
        assert_eq!(run("10l").1, [Cmd::Move(0, 10)]);
        assert_eq!(run("3gt").1, [Cmd::Tab(Some(3), true)]);
        assert_eq!(run("yyY").1, [Cmd::Act(Act::Yank), Cmd::Act(Act::YankRow)]);
    }

    #[test]
    fn a_half_command_waits_and_unknown_keys_pass_through() {
        assert_eq!(run("g"), ("g".into(), vec![], String::new()));
        assert_eq!(run("2"), ("2".into(), vec![], String::new()));
        // A key that ends no command drops what was pending, and is the app's when nothing was.
        assert_eq!(run("gx"), (String::new(), vec![], String::new()));
        assert_eq!(run("x/"), (String::new(), vec![], "x/".into()));
    }
}
