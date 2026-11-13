//! Vim modal editing for the code editors, behind Settings > Editing > Vim motions.
//!
//! An editor calls `begin` before its wrappers (autocomplete, autopair) so they never see the keys
//! Vim handled, and `end` after its TextEdit drew, to report Vim's edits as changes, scroll to the
//! caret and paint the block caret and the mode badge. `wrap` and `wrap_with` do both around an
//! editor. With the setting off they return at once.

mod engine;

use crate::ui::theme::*;
use eframe::egui::{
    self, Event, Key, Modifiers,
    mutex::Mutex,
    text::{CCursor, CCursorRange, LayoutJob},
    text_edit::TextEditOutput,
};
use engine::{History, K, Mode, Vim};
use std::{cell::Cell, sync::Arc};

/// Passes to wait for the clipboard after asking for it. Nothing comes when it holds no text, or
/// when there is no window, as in tests; the paste then uses Vim's own register.
const PASTE_WAIT: u64 = 3;

/// The keys that are not text, as egui names them and as Vim reads them.
const KEYS: [(Key, K); 6] = [(Key::Enter, K::Enter), (Key::Backspace, K::Backspace), (Key::ArrowLeft, K::Left), (Key::ArrowRight, K::Right), (Key::ArrowUp, K::Up), (Key::ArrowDown, K::Down)];

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
    ctx.data(|d| d.get_temp::<Shared>(sid(id))).unwrap_or_else(|| ctx.data_mut(|d| d.get_temp_mut_or_default::<Shared>(sid(id)).clone()))
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
                if let (true, Some(&(_, k))) = (*pressed, KEYS.iter().find(|(name, _)| name == key)) {
                    keys.push(k);
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
    ctx.input_mut(|i| {
        for k in keys {
            i.events.push(match k {
                K::Ch(c) => Event::Text(c.to_string()),
                k => match KEYS.iter().find(|(_, v)| *v == k) {
                    Some(&(key, _)) => Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE },
                    None => continue,
                },
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
    let b: Vec<char> = text.chars().collect();
    let (a, z) = s.vim.selection(&b, s.cur);
    let mut st = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    st.cursor.set_char_range(Some(range(a, z)));
    st.store(ctx, id);
    s.wrote = Some((a, z));
}

/// `begin`, the editor, then `end`, for editors whose wrappers hand back more than the TextEdit's
/// output, like the suggestion lists.
pub(crate) fn wrap_with<R>(ui: &mut egui::Ui, id: egui::Id, text: &mut String, show: impl FnOnce(&mut egui::Ui, &mut String) -> (TextEditOutput, R)) -> (TextEditOutput, R) {
    begin(ui, id, text);
    let (mut out, r) = show(ui, text);
    end(ui, id, &mut out);
    (out, r)
}

/// `wrap_with` for editors that hand back only the TextEdit's output.
pub(crate) fn wrap(ui: &mut egui::Ui, id: egui::Id, text: &mut String, show: impl FnOnce(&mut egui::Ui, &mut String) -> TextEditOutput) -> TextEditOutput {
    wrap_with(ui, id, text, |ui, text| (show(ui, text), ())).0
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
    let at = ctx.input(|i| i.events.iter().position(|e| matches!(e, Event::Key { key: Key::Escape, pressed: true, modifiers, .. } if modifiers.is_none())))?;
    let insert = s.vim.mode == Mode::Insert;
    let typed = ctx.input_mut(|i| {
        let after = i.events.split_off(at + 1);
        i.events.pop();
        let mut typed = String::new();
        if insert {
            i.events.retain(|e| match e {
                Event::Text(t) => {
                    typed.push_str(t);
                    false
                }
                _ => true,
            });
        }
        i.events.extend(after);
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
    // Whether the caret or selection must be written back to the editor this pass.
    let mut write = esc.is_some() || s.wrote.is_none();
    if s.vim.mode == Mode::Insert {
        cur = now.map_or(cur, |r| r.1);
    } else if let Some((a, b)) = now.filter(|&r| Some(r) != s.wrote) {
        // A drag selects as Visual mode does; a click or a Cmd shortcut just moves the caret.
        write = true;
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
    // Some(answer) once a paste's request for the clipboard is answered, or given up on.
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
    // The text as chars, built once for this pass's keys and the caret below.
    let mut chars: Option<Vec<char>> = None;
    if !keys.is_empty() {
        write = true;
        let answered = fetched.is_some();
        // A yank reaches the system clipboard only after this frame, so a paste after it in the same
        // frame uses Vim's own register instead of the clipboard's older text.
        let yanked = Cell::new(false);
        let clip_text = Cell::new(fetched.flatten());
        let mut clip = || if yanked.get() { None } else { clip_text.take() };
        let mut b: Vec<char> = text.chars().collect();
        let mut dirty = false;
        let mut undo_at = cur;
        let mut undoer = st.undoer();
        // Writes `b` back to `text`; the text before becomes an undo point, so `u` and Cmd+Z undo Vim's changes.
        let flush = |text: &mut String, b: &[char], at: usize, undoer: &mut egui::util::undoer::Undoer<(CCursorRange, String)>| {
            let old = std::mem::replace(text, b.iter().collect());
            undoer.add_undo(&(CCursorRange::one(CCursor::new(at)), old));
        };
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
            s.vim.defer_paste = !answered && !yanked.get();
            let from = cur;
            let o = s.vim.key(&mut b, &mut cur, k, &mut clip);
            dirty |= o.changed;
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
                if std::mem::take(&mut dirty) {
                    flush(text, &b, undo_at, &mut undoer);
                }
                let now = (CCursorRange::one(CCursor::new(cur)), std::mem::take(text));
                let back = match h {
                    History::Undo => undoer.undo(&now),
                    History::Redo => undoer.redo(&now),
                }
                .cloned();
                match back {
                    Some((r, t)) => {
                        *text = t;
                        cur = r.primary.index.0.min(r.secondary.index.0);
                        s.vim.reset();
                        s.edited = true;
                    }
                    None => *text = now.1,
                }
                b = text.chars().collect();
                undo_at = cur;
            }
            s.moved |= cur != from;
        }
        if dirty {
            flush(text, &b, undo_at, &mut undoer);
            s.edited = true;
        }
        st.set_undoer(undoer);
        if s.vim.mode == Mode::Insert {
            st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(cur))));
            s.wrote = None;
        }
        chars = Some(b);
    }
    if s.vim.mode != Mode::Insert {
        // Hidden while the block caret is painted in `end`, where it is restored.
        ui.visuals_mut().text_cursor.stroke.color = egui::Color32::TRANSPARENT;
        // Only after a key or an outside move: it reads the whole text, which can be megabytes.
        if write {
            let b = chars.unwrap_or_else(|| text.chars().collect());
            if s.vim.mode == Mode::Normal {
                cur = engine::clamp(&b, cur);
            }
            let (a, z) = s.vim.selection(&b, cur);
            st.cursor.set_char_range(Some(range(a, z)));
            s.wrote = Some((a, z));
        }
    }
    if write {
        st.store(&ctx, id);
    }
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
        let b: Vec<char> = out.galley.text().chars().collect();
        s.cur = engine::clamp(&b, out.cursor_range.map_or(0, |r| r.primary.index.0));
        let mut st = out.state.clone();
        st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(s.cur))));
        st.store(&ctx, id);
        s.wrote = Some((s.cur, s.cur));
    }
    s.last_focus = pass;
    let mode = s.vim.mode;
    let moved = std::mem::take(&mut s.moved);
    let painter = ui.painter().with_clip_rect(out.text_clip_rect);
    if mode != Mode::Insert || moved {
        let g = &out.galley;
        let r0 = g.pos_from_cursor(CCursor::new(s.cur));
        let r1 = g.pos_from_cursor(CCursor::new(s.cur + 1));
        // The block is one char wide, or half a line high past the end of a line.
        let w = if (r1.min.y - r0.min.y).abs() < 0.5 && r1.min.x > r0.min.x { r1.min.x - r0.min.x } else { r0.height() * 0.5 };
        let caret = egui::Rect::from_min_size(out.galley_pos + r0.min.to_vec2(), egui::vec2(w, r0.height()));
        // The TextEdit scrolls only to carets it moved itself.
        if moved {
            ui.scroll_to_rect(caret, None);
        }
        if mode != Mode::Insert {
            painter.rect_filled(caret, R_CARET, tint(PRIMARY(), 110));
        }
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
    let clip = out.response.rect.intersect(out.text_clip_rect);
    // A one-line box like the Mongo query has no room for it without covering the text; the block
    // caret still tells Normal from Insert there.
    if clip.height() >= 2.0 * (size.y + inset.y) {
        let rect = egui::Rect::from_min_size(clip.right_bottom() - size - inset, size);
        // An opaque base keeps the badge readable over the text it covers.
        painter.rect_filled(rect, R_SM, BG());
        crate::ui::widgets::paint_badge(&painter, rect, g, fg, 31);
    }
}
