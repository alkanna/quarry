//! Vim modal editing for the code editors, behind Settings > Editing > Vim motions.
//!
//! An editor calls `begin` before its wrappers (autocomplete, autopair) so they never see the keys
//! Vim handled, and `end` after its TextEdit drew, to report Vim's edits as changes, scroll to the
//! caret and paint the block caret and the mode badge. With the setting off both return at once.

mod engine;

use crate::ui::theme::*;
use eframe::egui::{
    self, Event, Key, Modifiers,
    mutex::Mutex,
    text::{CCursor, CCursorRange, LayoutJob},
    text_edit::TextEditOutput,
};
use engine::{History, K, Mode, Vim};
use std::sync::Arc;

/// Passes to wait for the clipboard after asking for it. Nothing comes when it holds no text, or
/// when there is no window, as in tests; the paste then uses Vim's own register.
const PASTE_WAIT: u64 = 3;

/// Mirrors the setting for the editors, which have no `App`. Kept in the context rather than a static
/// because UI tests run several apps at once in one process.
pub(crate) fn set_on(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("vim-on"), on));
}

fn on(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(egui::Id::unique("vim-on"))).unwrap_or(false)
}

#[derive(Default)]
struct State {
    vim: Vim,
    cur: usize,
    /// The selection last written to the editor as (secondary, primary). Anything else there came
    /// from a click, a drag or a Cmd shortcut.
    wrote: Option<(usize, usize)>,
    /// The pass the editor last had focus in. One pass old means Esc just took it away.
    last_focus: u64,
    /// Vim changed the text this pass; `end` reports it as a change of the editor.
    edited: bool,
    /// Vim moved the caret this pass; `end` scrolls to it.
    moved: bool,
    /// A paste waiting for the clipboard: its keys and the pass it was asked for.
    paste_wait: Option<(Vec<char>, u64)>,
    /// Keys typed while a paste waits, run after it in order.
    queued: Vec<K>,
}

/// Shared rather than copied in and out of egui's temp data each pass: the register can hold a
/// large yank.
type Shared = Arc<Mutex<State>>;

fn sid(id: egui::Id) -> egui::Id {
    id.with("vim")
}

fn shared(ctx: &egui::Context, id: egui::Id) -> Shared {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Shared>(sid(id)).clone())
}

/// True when the editor is in Normal or Visual mode, where typing does not insert.
pub(crate) fn normal(ctx: &egui::Context, id: egui::Id) -> bool {
    on(ctx) && ctx.data(|d| d.get_temp::<Shared>(sid(id))).is_some_and(|s| s.lock().vim.mode != Mode::Insert)
}

/// Removes the keys Vim handles from this pass's input and returns them, with the clipboard text
/// when `wait` says a paste asked for it. Keys held with Cmd, Ctrl or Alt stay, so running, undo,
/// the clipboard and shortcuts like Ctrl+Tab and Ctrl+Space work in every mode. Esc stays too:
/// `begin` has already taken it when it meant something here.
fn take_keys(ctx: &egui::Context, wait: bool) -> (Vec<K>, Option<String>) {
    let mut keys = Vec::new();
    let mut pasted = None;
    ctx.input_mut(|i| {
        i.events.retain(|e| match e {
            Event::Text(t) => {
                keys.extend(t.chars().map(K::Ch));
                false
            }
            Event::Paste(t) if wait && pasted.is_none() => {
                pasted = Some(t.clone());
                false
            }
            Event::Key { key, pressed, modifiers, .. } => {
                // Redo is Ctrl+R only on macOS, where Ctrl is not the command key. Elsewhere Ctrl+R is
                // Reload and redo stays on Ctrl+Y and Ctrl+Shift+Z.
                if *key == Key::R && modifiers.ctrl && !modifiers.command && !modifiers.shift && !modifiers.alt {
                    if *pressed {
                        keys.push(K::CtrlR);
                    }
                    return false;
                }
                if modifiers.command || modifiers.ctrl || modifiers.alt || *key == Key::Escape {
                    return true;
                }
                // Printable keys also arrive as Text, which is what Vim reads; dropping their Key events
                // keeps the editor from acting on Tab, Delete, Home and the like behind Vim's back.
                if *pressed {
                    match key {
                        Key::Enter => keys.push(K::Enter),
                        Key::Backspace => keys.push(K::Backspace),
                        Key::ArrowLeft => keys.push(K::Left),
                        Key::ArrowRight => keys.push(K::Right),
                        Key::ArrowUp => keys.push(K::Up),
                        Key::ArrowDown => keys.push(K::Down),
                        _ => {}
                    }
                }
                false
            }
            Event::Ime(_) => false,
            _ => true,
        })
    });
    (keys, pasted)
}

/// Puts keys back as input for the editor, for those typed after a key that started Insert mode in
/// the same frame.
fn give_back(ctx: &egui::Context, keys: impl Iterator<Item = K>) {
    let key = |key| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
    ctx.input_mut(|i| {
        for k in keys {
            i.events.push(match k {
                K::Ch(c) => Event::Text(c.to_string()),
                K::Enter => key(Key::Enter),
                K::Backspace => key(Key::Backspace),
                K::Left => key(Key::ArrowLeft),
                K::Right => key(Key::ArrowRight),
                K::Up => key(Key::ArrowUp),
                K::Down => key(Key::ArrowDown),
                K::Esc | K::CtrlR => continue,
            });
        }
    });
}

fn range(sec: usize, prim: usize) -> CCursorRange {
    CCursorRange { primary: CCursor::new(prim), secondary: CCursor::new(sec), h_pos: None }
}

/// Test switch QUARRY_TEST_VIM=<normal|visual|insert>: focuses this editor in that mode once, for screenshots.
pub(crate) fn demo(ui: &egui::Ui, id: egui::Id, text: &str) {
    let ctx = ui.ctx();
    let Ok(m) = std::env::var("QUARRY_TEST_VIM") else { return };
    let sh = shared(ctx, id);
    let mut s = sh.lock();
    if !on(ctx) || s.last_focus > 0 {
        return;
    }
    ctx.memory_mut(|mm| mm.request_focus(id));
    let at = text.find("orders.find").map_or(0, |b| text[..b].chars().count());
    s.vim.mode = match m.as_str() {
        "visual" => Mode::Visual,
        "insert" => Mode::Insert,
        _ => Mode::Normal,
    };
    s.vim.anchor = at;
    s.cur = if s.vim.mode == Mode::Visual { at + 10 } else { at };
    // Counts as focused this pass, so `end` keeps the mode instead of starting Normal.
    s.last_focus = ctx.cumulative_pass_nr();
    let (a, b) = s.vim.selection(text, s.cur);
    let mut st = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    st.cursor.set_char_range(Some(range(a, b)));
    st.store(ctx, id);
    s.wrote = Some((a, b));
}

/// `begin`, the editor, then `end`, for editors whose wrappers hand back the TextEdit's output.
pub(crate) fn wrap(ui: &mut egui::Ui, id: egui::Id, text: &mut String, show: impl FnOnce(&mut egui::Ui, &mut String) -> TextEditOutput) -> TextEditOutput {
    begin(ui, id, text);
    let mut out = show(ui, text);
    end(ui, id, &mut out);
    out
}

/// Takes this pass's Esc when it means something to Vim. egui drops focus on a bare Esc before any
/// widget runs, so when this editor had it last pass and Esc leaves Insert or Visual mode or cancels
/// a pending command, focus is taken back and the Esc with it, so the page or dialog behind does not
/// act on it too. Text typed before the Esc in the same frame is returned, since it still belongs to
/// the Insert session the Esc ends.
fn take_esc(ctx: &egui::Context, id: egui::Id, s: &State, focused: bool, pass: u64) -> Option<String> {
    if !(focused || s.last_focus + 1 == pass) || !s.vim.wants_esc() {
        return None;
    }
    let is_esc = |e: &Event| matches!(e, Event::Key { key: Key::Escape, pressed: true, modifiers, .. } if modifiers.is_none());
    let at = ctx.input(|i| i.events.iter().position(is_esc))?;
    let insert = s.vim.mode == Mode::Insert;
    let typed = ctx.input_mut(|i| {
        let mut typed = String::new();
        let mut n = 0;
        i.events.retain(|e| {
            let before = n < at;
            n += 1;
            match e {
                Event::Text(t) if before && insert => {
                    typed.push_str(t);
                    false
                }
                e if n - 1 == at && is_esc(e) => false,
                _ => true,
            }
        });
        typed
    });
    ctx.memory_mut(|m| m.request_focus(id));
    Some(typed)
}

/// Runs before the editor's wrappers. In Normal and Visual mode it takes the keys, edits `text` and
/// moves the caret; in Insert mode it only watches for Esc.
pub(crate) fn begin(ui: &mut egui::Ui, id: egui::Id, text: &mut String) {
    let ctx = ui.ctx().clone();
    if !on(&ctx) {
        return;
    }
    let sh = shared(&ctx, id);
    let mut s = sh.lock();
    let pass = ctx.cumulative_pass_nr();
    let focused = ctx.memory(|m| m.has_focus(id));
    let esc = take_esc(&ctx, id, &s, focused, pass);
    if !focused && esc.is_none() {
        // Leaving the editor drops a paste still waiting for the clipboard.
        s.paste_wait = None;
        s.queued.clear();
        return;
    }
    let mut st = egui::TextEdit::load_state(&ctx, id).unwrap_or_default();
    let now = st.cursor.char_range().map(|r| (r.secondary.index.0, r.primary.index.0));
    let mut cur = s.cur;
    let mut touched = false;
    if s.vim.mode == Mode::Insert {
        cur = now.map_or(cur, |r| r.1);
    } else if let Some((a, b)) = now.filter(|&r| Some(r) != s.wrote) {
        // A drag selects as Visual mode does; a click or a Cmd shortcut just moves the caret.
        touched = true;
        if a != b {
            s.vim.mode = Mode::Visual;
            s.vim.anchor = a;
            cur = if b > a { b - 1 } else { b };
        } else {
            s.vim.reset();
            cur = b;
        }
    }
    let mut keys = Vec::new();
    if let Some(typed) = &esc {
        if !typed.is_empty() {
            let at = crate::ui::editor::byte_at(text, cur);
            text.insert_str(at, typed);
            cur += typed.chars().count();
            s.edited = true;
        }
        keys.push(K::Esc);
    }
    let mut fetched: Option<Option<String>> = None;
    if esc.is_some() || s.vim.mode != Mode::Insert {
        let (more, pasted) = take_keys(&ctx, s.paste_wait.is_some());
        keys.extend(more);
        if let Some((wait, since)) = s.paste_wait.take() {
            if pasted.is_some() || pass > since + PASTE_WAIT {
                fetched = Some(pasted);
                let queued = std::mem::take(&mut s.queued);
                keys.splice(0..0, wait.into_iter().map(K::Ch).chain(queued));
            } else {
                s.paste_wait = Some((wait, since));
                s.queued.append(&mut keys);
                ctx.request_repaint();
            }
        }
    }
    if !keys.is_empty() {
        touched = true;
        // A yank reaches the system clipboard only after this frame, so a paste after it in the same
        // frame uses Vim's own register instead of asking for the stale clipboard.
        let yanked = std::cell::Cell::new(false);
        let clip_text = fetched.clone().flatten();
        let mut clip = || if yanked.get() { None } else { clip_text.clone() };
        let mut undoer = st.undoer();
        let mut rest = keys.into_iter();
        while let Some(k) = rest.next() {
            if s.paste_wait.is_some() {
                s.queued.push(k);
                continue;
            }
            if s.vim.mode == Mode::Insert && k != K::Esc {
                give_back(&ctx, std::iter::once(k).chain(rest.by_ref()));
                break;
            }
            s.vim.defer_paste = fetched.is_none() && !yanked.get();
            let from = cur;
            let o = s.vim.key(text, &mut cur, k, &mut clip);
            // The text before each change becomes an undo point, so `u` and Cmd+Z undo one Vim change.
            if let Some(before) = o.before {
                undoer.add_undo(&(CCursorRange::one(CCursor::new(from)), before));
                s.edited = true;
            }
            if let Some(y) = o.yank {
                yanked.set(true);
                ctx.copy_text(y);
            }
            if let Some(wait) = o.paste {
                s.paste_wait = Some((wait, pass));
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                ctx.request_repaint();
            }
            if let Some(h) = o.history {
                let now = (CCursorRange::one(CCursor::new(cur)), text.clone());
                let back = match h {
                    History::Undo => undoer.undo(&now),
                    History::Redo => undoer.redo(&now),
                };
                if let Some((r, t)) = back.cloned() {
                    *text = t;
                    cur = r.primary.index.0.min(r.secondary.index.0);
                    s.vim.reset();
                    s.edited = true;
                }
            }
            s.moved |= cur != from;
        }
        st.set_undoer(undoer);
        if s.vim.mode == Mode::Insert {
            st.cursor.set_char_range(Some(range(cur, cur)));
            s.wrote = None;
        }
    }
    if s.vim.mode != Mode::Insert {
        // Hidden while the block caret is painted in `end`, where it is restored.
        ui.visuals_mut().text_cursor.stroke.color = egui::Color32::TRANSPARENT;
        // Only after a key or an outside move: both read the whole text, which can be megabytes.
        if touched || s.wrote.is_none() {
            if s.vim.mode == Mode::Normal {
                cur = engine::clamp_chars(text, cur);
            }
            let (a, b) = s.vim.selection(text, cur);
            st.cursor.set_char_range(Some(range(a, b)));
            s.wrote = Some((a, b));
        }
    }
    st.store(&ctx, id);
    s.cur = cur;
}

/// Runs after the editor drew: reports Vim's edits as a change of the editor, starts Normal mode when
/// the editor gains focus, scrolls to a caret Vim moved, and paints the block caret and the mode badge.
pub(crate) fn end(ui: &mut egui::Ui, id: egui::Id, out: &mut TextEditOutput) {
    let ctx = ui.ctx().clone();
    if !on(&ctx) {
        return;
    }
    ui.visuals_mut().text_cursor.stroke.color = ctx.global_style().visuals.text_cursor.stroke.color;
    let sh = shared(&ctx, id);
    let mut s = sh.lock();
    // Vim edits the text before the TextEdit runs, so the TextEdit itself never reports them.
    if std::mem::take(&mut s.edited) {
        out.response.mark_changed();
    }
    if !out.response.has_focus() {
        return;
    }
    let pass = ctx.cumulative_pass_nr();
    // Focus that comes back after a gap starts in Normal mode. A one-pass blip, like a click on a
    // suggestion, keeps the mode.
    if s.last_focus + 1 < pass {
        s.vim.reset();
        let text = out.galley.text();
        let c = out.cursor_range.map_or(0, |r| r.primary.index.0);
        s.cur = engine::clamp_chars(text, c);
        let mut st = out.state.clone();
        st.cursor.set_char_range(Some(range(s.cur, s.cur)));
        st.store(&ctx, id);
        s.wrote = Some((s.cur, s.cur));
    }
    s.last_focus = pass;
    let mode = s.vim.mode;
    let g = &out.galley;
    let r0 = g.pos_from_cursor(CCursor::new(s.cur));
    let r1 = g.pos_from_cursor(CCursor::new(s.cur + 1));
    // The block is one char wide, or half a line high past the end of a line.
    let w = if (r1.min.y - r0.min.y).abs() < 0.5 && r1.min.x > r0.min.x { r1.min.x - r0.min.x } else { r0.height() * 0.5 };
    let caret = egui::Rect::from_min_size(out.galley_pos + r0.min.to_vec2(), egui::vec2(w, r0.height()));
    // The TextEdit scrolls only to carets it moved itself.
    if std::mem::take(&mut s.moved) {
        ui.scroll_to_rect(caret, None);
    }
    let clip = out.response.rect.intersect(out.text_clip_rect);
    let painter = ui.painter().with_clip_rect(out.text_clip_rect);
    if mode != Mode::Insert {
        painter.rect_filled(caret, R_CARET, tint(PRIMARY(), 110));
    }
    let name = match mode {
        Mode::Normal => "NORMAL",
        Mode::Insert => "INSERT",
        Mode::Visual => "VISUAL",
        Mode::VisualLine => "V-LINE",
    };
    let pending = s.vim.pending();
    let label = if pending.is_empty() { name.to_string() } else { format!("{name}  {pending}") };
    let fg = if mode == Mode::Insert { MUTED() } else { PRIMARY() };
    let g = painter.layout_job(LayoutJob::single_section(label, mono_fmt(TXT_CAPTION, fg, W_SEMIBOLD)));
    let size = egui::vec2(g.size().x + 2.0 * VIM_BADGE_PAD, STATUS_CHIP_H);
    let inset = egui::Vec2::splat(VIM_BADGE_PAD);
    // A one-line box like the Mongo query has no room for it without covering the text; the block
    // caret still tells Normal from Insert there.
    if clip.height() >= 2.0 * (size.y + inset.y) {
        let rect = egui::Rect::from_min_size(clip.right_bottom() - size - inset, size);
        // An opaque base keeps the badge readable over the text it covers.
        painter.rect_filled(rect, R_SM, BG());
        painter.rect_filled(rect, R_SM, tint(fg, 31));
        let dy = optical_dy(&g);
        painter.galley(rect.center() - g.size() / 2.0 + egui::vec2(0.0, dy), g, fg);
    }
}
