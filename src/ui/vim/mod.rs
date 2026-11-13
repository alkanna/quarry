//! Vim modal editing for the code editors, behind Settings > Editing > Vim motions.
//!
//! An editor calls `begin` before its wrappers (autocomplete, autopair) so they never see the keys
//! Vim handled, and `end` after its TextEdit drew, to paint the block caret and the mode badge.
//! With the setting off both return at once.

mod engine;

use crate::ui::theme::*;
use eframe::egui::{
    self, Event, Key, Modifiers,
    text::{CCursor, CCursorRange, LayoutJob},
    text_edit::TextEditOutput,
};
use engine::{History, K, Mode, Vim};

/// Mirrors the setting for the editors, which have no `App`. Kept in the context rather than a static
/// because UI tests run several apps at once in one process.
pub(crate) fn set_on(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("vim-on"), on));
}

fn on(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(egui::Id::unique("vim-on"))).unwrap_or(false)
}

#[derive(Clone, Default)]
struct State {
    vim: Vim,
    cur: usize,
    /// The selection last written to the editor as (secondary, primary). Anything else there came
    /// from a click, a drag or a Cmd shortcut.
    wrote: Option<(usize, usize)>,
    /// The pass the editor last had focus in. One pass old means Esc just took it away.
    last_focus: u64,
}

fn sid(id: egui::Id) -> egui::Id {
    id.with("vim")
}

fn load(ctx: &egui::Context, id: egui::Id) -> State {
    ctx.data(|d| d.get_temp::<State>(sid(id))).unwrap_or_default()
}

fn store(ctx: &egui::Context, id: egui::Id, s: State) {
    ctx.data_mut(|d| d.insert_temp(sid(id), s));
}

/// True when the editor is in Normal or Visual mode, where typing does not insert.
pub(crate) fn normal(ctx: &egui::Context, id: egui::Id) -> bool {
    on(ctx) && ctx.data(|d| d.get_temp::<State>(sid(id))).is_some_and(|s| s.vim.mode != Mode::Insert)
}

/// Removes the keys Vim handles from this pass's input and returns them. Command shortcuts (Cmd on
/// macOS, Ctrl elsewhere) stay, so running, undo and the clipboard work in every mode. Esc stays
/// too: `begin` has already taken it when it meant something here.
fn take_keys(ctx: &egui::Context) -> Vec<K> {
    let mut keys = Vec::new();
    ctx.input_mut(|i| {
        i.events.retain(|e| match e {
            Event::Text(t) => {
                keys.extend(t.chars().map(K::Ch));
                false
            }
            Event::Key { key, pressed, modifiers, .. } => {
                if modifiers.ctrl && *key == Key::R {
                    if *pressed {
                        keys.push(K::CtrlR);
                    }
                    return false;
                }
                if modifiers.command || *key == Key::Escape {
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
    keys
}

fn range(sec: usize, prim: usize) -> CCursorRange {
    CCursorRange { primary: CCursor::new(prim), secondary: CCursor::new(sec), h_pos: None }
}

/// Test switch QUARRY_TEST_VIM=<normal|visual|insert>: focuses this editor in that mode once, for screenshots.
pub(crate) fn demo(ui: &egui::Ui, id: egui::Id, text: &str) {
    let ctx = ui.ctx();
    let Ok(m) = std::env::var("QUARRY_TEST_VIM") else { return };
    let mut s = load(ctx, id);
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
    store(ctx, id, s);
}

/// `begin`, the editor, then `end`, for editors whose wrappers hand back the TextEdit's output.
pub(crate) fn wrap(ui: &mut egui::Ui, id: egui::Id, text: &mut String, show: impl FnOnce(&mut egui::Ui, &mut String) -> TextEditOutput) -> TextEditOutput {
    begin(ui, id, text);
    let out = show(ui, text);
    end(ui, id, &out);
    out
}

/// Runs before the editor's wrappers. In Normal and Visual mode it takes the keys, edits `text` and
/// moves the caret; in Insert mode it only watches for Esc.
pub(crate) fn begin(ui: &mut egui::Ui, id: egui::Id, text: &mut String) {
    let ctx = ui.ctx().clone();
    if !on(&ctx) {
        return;
    }
    let mut s = load(&ctx, id);
    let pass = ctx.cumulative_pass_nr();
    let mut focused = ctx.memory(|m| m.has_focus(id));
    let mut keys = Vec::new();
    // egui drops focus on a bare Esc before any widget runs. When this editor had it last pass and
    // Esc means something here (leaving Insert or Visual, or a pending command), take focus back
    // and the Esc with it, so the page or dialog behind does not act on it too.
    if (focused || s.last_focus + 1 == pass) && s.vim.wants_esc() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        ctx.memory_mut(|m| m.request_focus(id));
        focused = true;
        keys.push(K::Esc);
    }
    if !focused {
        return;
    }
    let mut st = egui::TextEdit::load_state(&ctx, id).unwrap_or_default();
    let now = st.cursor.char_range().map(|r| (r.secondary.index.0, r.primary.index.0));
    let mut cur = s.cur;
    if s.vim.mode == Mode::Insert {
        cur = now.map_or(cur, |r| r.1);
    } else {
        if let Some((a, b)) = now.filter(|&r| Some(r) != s.wrote) {
            // A drag selects as Visual mode does; a click or a Cmd shortcut just moves the caret.
            if a != b {
                s.vim.mode = Mode::Visual;
                s.vim.anchor = a;
                cur = if b > a { b - 1 } else { b };
            } else {
                s.vim.reset();
                cur = b;
            }
        }
        keys.extend(take_keys(&ctx));
        // Hidden while the block caret is painted in `end`, where it is restored.
        ui.visuals_mut().text_cursor.stroke.color = egui::Color32::TRANSPARENT;
    }
    if !keys.is_empty() {
        let mut clip = || arboard::Clipboard::new().ok()?.get_text().ok();
        let mut undoer = st.undoer();
        for k in keys {
            let snap = (CCursorRange::one(CCursor::new(cur)), text.clone());
            let o = s.vim.key(text, &mut cur, k, &mut clip);
            // The text before each change becomes an undo point, so `u` and Cmd+Z undo one Vim change.
            if o.changed {
                undoer.add_undo(&snap);
            }
            if let Some(y) = o.yank {
                ctx.copy_text(y);
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
                }
            }
        }
        st.set_undoer(undoer);
        if s.vim.mode == Mode::Insert {
            st.cursor.set_char_range(Some(range(cur, cur)));
            s.wrote = None;
        }
    }
    if s.vim.mode != Mode::Insert {
        if s.vim.mode == Mode::Normal {
            cur = engine::clamp_chars(text, cur);
        }
        let (a, b) = s.vim.selection(text, cur);
        st.cursor.set_char_range(Some(range(a, b)));
        s.wrote = Some((a, b));
    }
    st.store(&ctx, id);
    s.cur = cur;
    store(&ctx, id, s);
}

/// Runs after the editor drew: notes focus, starts Normal mode when the editor gains focus, and
/// paints the block caret and the mode badge.
pub(crate) fn end(ui: &mut egui::Ui, id: egui::Id, out: &TextEditOutput) {
    let ctx = ui.ctx().clone();
    if !on(&ctx) {
        return;
    }
    ui.visuals_mut().text_cursor.stroke.color = ctx.global_style().visuals.text_cursor.stroke.color;
    if !out.response.has_focus() {
        return;
    }
    let mut s = load(&ctx, id);
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
    let clip = out.response.rect.intersect(out.text_clip_rect);
    let painter = ui.painter().with_clip_rect(out.text_clip_rect);
    if mode != Mode::Insert {
        let g = &out.galley;
        let r0 = g.pos_from_cursor(CCursor::new(s.cur));
        let r1 = g.pos_from_cursor(CCursor::new(s.cur + 1));
        // The block is one char wide, or half a line high past the end of a line.
        let w = if (r1.min.y - r0.min.y).abs() < 0.5 && r1.min.x > r0.min.x { r1.min.x - r0.min.x } else { r0.height() * 0.5 };
        let rect = egui::Rect::from_min_size(out.galley_pos + r0.min.to_vec2(), egui::vec2(w, r0.height()));
        painter.rect_filled(rect, 1.0, tint(PRIMARY(), 110));
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
    let g = painter.layout_job(LayoutJob::single_section(label, mono_fmt(TXT_CAPTION - 1.0, fg, W_SEMIBOLD)));
    let size = egui::vec2(g.size().x + 12.0, 16.0);
    let rect = egui::Rect::from_min_size(clip.right_bottom() - size - egui::vec2(6.0, 6.0), size);
    // An opaque base keeps the badge readable over the text it covers.
    painter.rect_filled(rect, R_SM, BG());
    painter.rect_filled(rect, R_SM, tint(fg, 31));
    let dy = optical_dy(&g);
    painter.galley(rect.center() - g.size() / 2.0 + egui::vec2(0.0, dy), g, fg);
    store(&ctx, id, s);
}
