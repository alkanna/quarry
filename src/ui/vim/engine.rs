//! Vim keys over a text buffer. No egui here, so every command is tested on plain strings.
//!
//! Positions are char indices, as in egui's `TextEditState`. Each key turns the text into a
//! `Vec<char>` and back only when it changed: editors hold kilobytes, so that costs nothing visible.

use crate::ui::editor::INDENT;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Mode {
    #[default]
    Normal,
    Insert,
    Visual,
    VisualLine,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum K {
    Ch(char),
    Esc,
    Enter,
    Backspace,
    Left,
    Right,
    Up,
    Down,
    CtrlR,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum History {
    Undo,
    Redo,
}

#[derive(Default, Debug)]
pub(crate) struct Out {
    pub(crate) changed: bool,
    pub(crate) yank: Option<String>,
    pub(crate) history: Option<History>,
    /// A paste that waits for the clipboard: the keys to run again once its text is known.
    pub(crate) paste: Option<Vec<char>>,
}

#[derive(Clone, Default)]
struct Search {
    pat: Vec<char>,
    back: bool,
    whole: bool,
}

#[derive(Clone, Default)]
pub(crate) struct Vim {
    pub(crate) mode: Mode,
    /// The fixed end of a Visual selection.
    pub(crate) anchor: usize,
    pending: Vec<char>,
    /// Column `j` and `k` aim for, kept across short lines. `$` sets it to usize::MAX.
    want_col: Option<usize>,
    last_find: Option<(char, char)>,
    search: Option<Search>,
    /// The `/` or `?` line being typed: direction and text.
    typing: Option<(bool, String)>,
    /// The last yank and whether it was whole lines. Kept beside the clipboard to remember linewise.
    reg: (String, bool),
    dot: Vec<char>,
    dot_text: String,
    replaying: bool,
    /// Set while the clipboard has not been read yet: `p` and `P` then hand their keys back in
    /// `Out::paste` instead of running, because the editor reads the clipboard asynchronously.
    pub(crate) defer_paste: bool,
    ins_at: usize,
    ins_len: usize,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Excl,
    Incl,
    Line,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum M {
    H,
    L,
    J,
    K,
    Plus,
    Minus,
    W(bool),
    B(bool),
    E(bool),
    Zero,
    Caret,
    Dollar,
    BigG,
    GG,
    Find(char, char),
    Rep(bool),
    Pct,
    ParaN,
    ParaP,
    Next(bool),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Target {
    M(M),
    Obj(bool, char),
    Lines,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum What {
    Move(M),
    Op(char, Target),
    Obj(bool, char),
    Simple(char),
    Replace(char),
    Search(bool),
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct Cmd {
    n: usize,
    has: bool,
    what: What,
}

enum P<T> {
    More,
    Bad,
    Ok(T),
}

impl<T> P<T> {
    fn map<U>(self, f: impl FnOnce(T) -> U) -> P<U> {
        match self {
            P::More => P::More,
            P::Bad => P::Bad,
            P::Ok(t) => P::Ok(f(t)),
        }
    }
}

impl Vim {
    /// The keys typed so far for a command, or the search line, for the mode badge.
    pub(crate) fn pending(&self) -> String {
        match &self.typing {
            Some((back, s)) => format!("{}{s}", if *back { '?' } else { '/' }),
            None => self.pending.iter().collect(),
        }
    }

    /// True while Esc means something to the editor rather than to the page around it.
    pub(crate) fn wants_esc(&self) -> bool {
        self.mode != Mode::Normal || !self.pending.is_empty() || self.typing.is_some()
    }

    /// Runs one key on the text as chars. The editor converts its text once per frame, not per key.
    pub(crate) fn key(&mut self, b: &mut Vec<char>, cur: &mut usize, k: K, clip: &mut dyn FnMut() -> Option<String>) -> Out {
        let mut c = (*cur).min(b.len());
        // The text can shrink outside Vim (Cmd+Z, a paste), which would leave a Visual end past it.
        self.anchor = self.anchor.min(b.len());
        let mut out = Out::default();
        self.step(b, &mut c, k, clip, &mut out);
        *cur = c;
        out
    }

    /// Back to Normal with nothing pending, keeping the register, the last search and `.`.
    pub(crate) fn reset(&mut self) {
        self.mode = Mode::Normal;
        self.pending.clear();
        self.typing = None;
        // The caret moved by a click or an undo, so `j` and `k` aim for its new column.
        self.want_col = None;
    }

    /// The selection to show as (fixed end, moving end), end-exclusive like egui's ranges: Visual
    /// includes the char under the cursor, and Visual Line whole lines.
    pub(crate) fn selection(&self, b: &[char], cur: usize) -> (usize, usize) {
        let (a, c) = (self.anchor.min(b.len()), cur.min(b.len()));
        match self.mode {
            Mode::Visual if c >= a => (a, (c + 1).min(b.len())),
            Mode::Visual => ((a + 1).min(b.len()), c),
            Mode::VisualLine if c >= a => (ls(b, a), le(b, c)),
            Mode::VisualLine => (le(b, a), ls(b, c)),
            _ => (c, c),
        }
    }

    fn step(&mut self, b: &mut Vec<char>, c: &mut usize, k: K, clip: &mut dyn FnMut() -> Option<String>, out: &mut Out) {
        if self.mode == Mode::Insert {
            if k == K::Esc {
                self.leave_insert(b, c);
            }
            return;
        }
        if let Some((back, mut s)) = self.typing.take() {
            match k {
                K::Enter => {
                    if !s.is_empty() {
                        self.search = Some(Search { pat: s.chars().collect(), back, whole: false });
                    }
                    if let Some((t, _)) = self.eval(b, *c, M::Next(false), 1, false, false) {
                        *c = t;
                    }
                }
                K::Backspace if s.pop().is_some() => self.typing = Some((back, s)),
                K::Ch(ch) => {
                    s.push(ch);
                    self.typing = Some((back, s));
                }
                K::Esc | K::Backspace => {}
                _ => self.typing = Some((back, s)),
            }
            *c = self.settle(b, *c);
            return;
        }
        // Backspace drops a half-typed command, and Enter or an arrow never becomes the char that
        // r, f or t wait for.
        if !self.pending.is_empty() && !matches!(k, K::Ch(_) | K::Esc | K::CtrlR) && (k == K::Backspace || self.pending.last().is_some_and(|p| "rfFtTg".contains(*p))) {
            self.pending.clear();
            return;
        }
        let ch = match k {
            K::Esc => {
                self.pending.clear();
                if self.mode != Mode::Normal {
                    self.mode = Mode::Normal;
                    *c = clamp(b, *c);
                }
                return;
            }
            K::CtrlR => {
                self.pending.clear();
                out.history = Some(History::Redo);
                return;
            }
            K::Ch(ch) => ch,
            K::Enter => '+',
            K::Backspace | K::Left => 'h',
            K::Right => 'l',
            K::Up => 'k',
            K::Down => 'j',
        };
        self.pending.push(ch);
        let visual = matches!(self.mode, Mode::Visual | Mode::VisualLine);
        match parse(&self.pending, visual) {
            P::More => {}
            P::Bad => self.pending.clear(),
            P::Ok(cmd) => {
                let keys = std::mem::take(&mut self.pending);
                self.run(b, c, cmd, &keys, clip, out);
            }
        }
        *c = self.settle(b, *c);
    }

    fn settle(&self, b: &[char], c: usize) -> usize {
        match self.mode {
            Mode::Normal => clamp(b, c),
            _ => c.min(b.len()),
        }
    }

    fn run(&mut self, b: &mut Vec<char>, c: &mut usize, cmd: Cmd, keys: &[char], clip: &mut dyn FnMut() -> Option<String>, out: &mut Out) {
        let n = cmd.n;
        if self.defer_paste && matches!(cmd.what, What::Simple('p' | 'P')) {
            out.paste = Some(keys.to_vec());
            return;
        }
        if matches!(self.mode, Mode::Visual | Mode::VisualLine) {
            return self.run_visual(b, c, cmd, clip, out);
        }
        let change = match cmd.what {
            What::Move(m) => {
                if let Some((t, _)) = self.eval(b, *c, m, n, cmd.has, false) {
                    *c = t;
                }
                false
            }
            What::Op(op, t) => {
                self.op(b, c, op, t, n, cmd.has, out);
                op != 'y'
            }
            What::Obj(..) => false,
            What::Replace(ch) => {
                let e = le(b, *c);
                if *c + n <= e {
                    b[*c..*c + n].fill(ch);
                    *c += n - 1;
                    out.changed = true;
                }
                true
            }
            What::Search(back) => {
                self.typing = Some((back, String::new()));
                false
            }
            What::Simple(s) => self.simple(b, c, s, n, cmd.has, clip, out),
        };
        if change && !self.replaying {
            self.dot = keys.to_vec();
            self.dot_text.clear();
        }
    }

    /// Runs a one-key command and says whether `.` should repeat it.
    fn simple(&mut self, b: &mut Vec<char>, c: &mut usize, s: char, n: usize, has: bool, clip: &mut dyn FnMut() -> Option<String>, out: &mut Out) -> bool {
        match s {
            'x' => self.op(b, c, 'd', Target::M(M::L), n, has, out),
            'X' => self.op(b, c, 'd', Target::M(M::H), n, has, out),
            'D' => self.op(b, c, 'd', Target::M(M::Dollar), n, has, out),
            'C' => self.op(b, c, 'c', Target::M(M::Dollar), n, has, out),
            's' => self.op(b, c, 'c', Target::M(M::L), n, has, out),
            'S' => self.op(b, c, 'c', Target::Lines, n, has, out),
            'Y' => {
                self.op(b, c, 'y', Target::Lines, n, has, out);
                return false;
            }
            'J' => out.changed |= join(b, c, n.max(2)),
            '~' => {
                let e = le(b, *c).min(*c + n);
                if *c < e {
                    case(b, *c, e, '~');
                    out.changed = true;
                    *c = e;
                }
            }
            'p' | 'P' => self.paste(b, c, s == 'p', n, clip, out),
            'i' => self.enter_insert(b, *c),
            'a' => {
                if b.get(*c).is_some_and(|&ch| ch != '\n') {
                    *c += 1;
                }
                self.enter_insert(b, *c);
            }
            'I' => {
                *c = fnb(b, ls(b, *c));
                self.enter_insert(b, *c);
            }
            'A' => {
                *c = le(b, *c);
                self.enter_insert(b, *c);
            }
            'o' | 'O' => {
                let s0 = ls(b, *c);
                let ind: Vec<char> = b[s0..fnb(b, s0)].to_vec();
                let k = ind.len();
                if s == 'o' {
                    let e = le(b, *c);
                    b.splice(e..e, std::iter::once('\n').chain(ind));
                    *c = e + 1 + k;
                } else {
                    b.splice(s0..s0, ind.into_iter().chain(std::iter::once('\n')));
                    *c = s0 + k;
                }
                out.changed = true;
                self.enter_insert(b, *c);
            }
            'v' | 'V' => {
                self.mode = if s == 'v' { Mode::Visual } else { Mode::VisualLine };
                self.anchor = *c;
                return false;
            }
            'u' => {
                out.history = Some(History::Undo);
                return false;
            }
            '.' => {
                self.repeat(b, c, clip, out);
                return false;
            }
            '*' | '#' => {
                if let Some((s0, e0)) = word_obj(b, *c, true, false).filter(|&(s0, e0)| e0 > s0 && class(b[s0], false) == 1) {
                    self.search = Some(Search { pat: b[s0..e0].to_vec(), back: s == '#', whole: true });
                    let from = if s == '#' { s0 } else { *c };
                    if let Some(t) = search(b, from, &b[s0..e0].to_vec(), s == '#', true) {
                        *c = t;
                    }
                }
                return false;
            }
            _ => return false,
        }
        true
    }

    fn run_visual(&mut self, b: &mut Vec<char>, c: &mut usize, cmd: Cmd, clip: &mut dyn FnMut() -> Option<String>, out: &mut Out) {
        let line = self.mode == Mode::VisualLine;
        let (lo, hi) = (self.anchor.min(*c), self.anchor.max(*c));
        // The selection includes the char under the cursor.
        let (s, e) = if line { (ls(b, lo), le(b, hi)) } else { (lo, (hi + 1).min(b.len())) };
        match cmd.what {
            What::Move(m) => {
                if let Some((t, _)) = self.eval(b, *c, m, cmd.n, cmd.has, false) {
                    *c = t;
                }
                return;
            }
            What::Obj(inner, o) => {
                if let Some((os, oe)) = object(b, *c, inner, o).filter(|&(os, oe)| oe > os) {
                    self.mode = Mode::Visual;
                    (self.anchor, *c) = (os, oe - 1);
                }
                return;
            }
            What::Simple(k) => match k {
                'o' => {
                    std::mem::swap(&mut self.anchor, c);
                    return;
                }
                'v' | 'V' => {
                    let to = if k == 'v' { Mode::Visual } else { Mode::VisualLine };
                    self.mode = if self.mode == to { Mode::Normal } else { to };
                    return;
                }
                'd' | 'x' | 'c' | 's' | 'y' => {
                    let op = match k {
                        'x' => 'd',
                        's' => 'c',
                        k => k,
                    };
                    self.mode = Mode::Normal;
                    self.operate(b, c, op, s, e, line, out);
                    return;
                }
                '>' | '<' => {
                    indent(b, ls(b, lo), le(b, hi), k == '>', cmd.n);
                    *c = fnb(b, ls(b, lo));
                    out.changed = true;
                }
                '~' | 'u' | 'U' => {
                    case(b, s, e, k);
                    *c = s;
                    out.changed = true;
                }
                'J' => {
                    *c = lo;
                    let lines = b[lo..hi.min(b.len())].iter().filter(|&&ch| ch == '\n').count() + 1;
                    out.changed |= join(b, c, lines.max(2));
                }
                'p' | 'P' => {
                    let (s, e) = if line && e < b.len() { (s, e + 1) } else { (s, e) };
                    b.drain(s..e);
                    *c = s;
                    out.changed = true;
                    self.mode = Mode::Normal;
                    self.paste(b, c, false, 1, clip, out);
                }
                _ => return,
            },
            _ => return,
        }
        self.mode = Mode::Normal;
    }

    fn op(&mut self, b: &mut Vec<char>, c: &mut usize, op: char, t: Target, n: usize, has: bool, out: &mut Out) {
        let start = *c;
        match t {
            Target::Lines => {
                let last = line_down(b, start, n - 1).unwrap_or(ls(b, start));
                self.operate(b, c, op, start, last, true, out);
            }
            Target::Obj(inner, o) => match object(b, start, inner, o) {
                Some((s, e)) => self.operate(b, c, op, s, e, false, out),
                None => {}
            },
            Target::M(m) => {
                // `cw` on a word changes to its end, like `ce`, so the space after it stays. On the
                // word's last char that end is the char itself, where `e` would reach the next word.
                if let M::W(big) = m
                    && op == 'c'
                    && b.get(start).is_some_and(|ch| !ch.is_whitespace())
                {
                    let last = |i: usize| i + 1 >= b.len() || class(b[i + 1], big) != class(b[i], big);
                    let mut to = start;
                    for k in 0..n {
                        if !(k == 0 && last(to)) {
                            to = end_word(b, to, big);
                        }
                    }
                    self.operate(b, c, op, start, to + 1, false, out);
                    return;
                }
                let Some((mut to, kind)) = self.eval(b, start, m, n, has, true) else {
                    if op == 'c' && matches!(m, M::L) {
                        self.enter_insert(b, start);
                    }
                    return;
                };
                // `dw` on the last word of a line stops at the line end instead of joining the next line.
                if matches!(m, M::W(_)) && kind == Kind::Excl && to > start && le(b, start) > start && b[start..to].contains(&'\n') {
                    to = le(b, start);
                }
                match kind {
                    Kind::Line => self.operate(b, c, op, start, to, true, out),
                    Kind::Excl => self.operate(b, c, op, start.min(to), start.max(to), false, out),
                    Kind::Incl => self.operate(b, c, op, start.min(to), start.max(to) + 1, false, out),
                }
            }
        }
    }

    /// Applies `op` to `a..z`. When `line`, the range is every line `a` and `z` touch.
    fn operate(&mut self, b: &mut Vec<char>, c: &mut usize, op: char, a: usize, z: usize, line: bool, out: &mut Out) {
        let (a, z) = (a.min(z), a.max(z).min(b.len()));
        if line || op == '>' || op == '<' {
            let (s, e) = (ls(b, a), le(b, z));
            if op == '>' || op == '<' {
                indent(b, s, e, op == '>', 1);
                *c = fnb(b, s);
                out.changed = true;
                return;
            }
            let mut t: String = b[s..e].iter().collect();
            t.push('\n');
            self.yank(t, true, out);
            match op {
                'y' => *c = (*c).min(a.max(s)),
                'd' => {
                    let (ds, de) = if e < b.len() {
                        (s, e + 1)
                    } else if s > 0 {
                        (s - 1, e)
                    } else {
                        (s, e)
                    };
                    b.drain(ds..de);
                    *c = fnb(b, ls(b, ds.min(b.len())));
                    out.changed = true;
                }
                'c' => {
                    // The first line's indent stays, as with autoindent.
                    let cs = fnb(b, s);
                    b.drain(cs..e);
                    *c = cs;
                    out.changed = true;
                    self.enter_insert(b, cs);
                }
                _ => {}
            }
            return;
        }
        let (s, mut e) = (a, z);
        // A block that spans lines keeps its closing bracket on its own line when changed.
        if op == 'c' && e > s + 1 && b[e - 1] == '\n' {
            e -= 1;
        }
        if s < e {
            let t: String = b[s..e].iter().collect();
            self.yank(t, false, out);
        }
        match op {
            'y' => *c = s,
            'd' | 'c' => {
                if s < e {
                    b.drain(s..e);
                    out.changed = true;
                }
                *c = s;
                if op == 'c' {
                    self.enter_insert(b, s);
                }
            }
            _ => {}
        }
    }

    fn yank(&mut self, t: String, line: bool, out: &mut Out) {
        self.reg = (t.clone(), line);
        out.yank = Some(t);
    }

    fn paste(&mut self, b: &mut Vec<char>, c: &mut usize, after: bool, n: usize, clip: &mut dyn FnMut() -> Option<String>, out: &mut Out) {
        // The clipboard wins, so text copied in another app pastes too; it is linewise when it is our own
        // last line yank, or when it ends in a newline.
        let (t, line) = match clip().filter(|t| !t.is_empty()) {
            Some(t) => {
                let line = if t == self.reg.0 { self.reg.1 } else { t.ends_with('\n') };
                (t, line)
            }
            None => self.reg.clone(),
        };
        if t.is_empty() {
            return;
        }
        let mut body: Vec<char> = t.repeat(n).chars().collect();
        out.changed = true;
        if line {
            if body.last() != Some(&'\n') {
                body.push('\n');
            }
            let at = if after {
                let e = le(b, *c);
                if e >= b.len() {
                    // The last line has no newline to insert after, so the pasted lines bring one in front.
                    body.pop();
                    let p = b.len() + 1;
                    b.push('\n');
                    b.extend(body);
                    *c = fnb(b, p);
                    return;
                }
                e + 1
            } else {
                ls(b, *c)
            };
            b.splice(at..at, body);
            *c = fnb(b, at);
        } else {
            let at = if after && b.get(*c).is_some_and(|&ch| ch != '\n') { *c + 1 } else { *c };
            let k = body.len();
            b.splice(at..at, body);
            *c = at + k - 1;
        }
    }

    fn enter_insert(&mut self, b: &[char], c: usize) {
        self.mode = Mode::Insert;
        self.want_col = None;
        (self.ins_at, self.ins_len) = (c, b.len());
    }

    fn leave_insert(&mut self, b: &[char], c: &mut usize) {
        // What was typed is kept for `.` when it went in as one run from where Insert began.
        if !self.replaying {
            let grew = b.len().checked_sub(self.ins_len);
            self.dot_text = match grew {
                Some(g) if *c >= self.ins_at && *c - self.ins_at == g && *c <= b.len() => b[self.ins_at..*c].iter().collect(),
                _ => String::new(),
            };
        }
        self.mode = Mode::Normal;
        if *c > ls(b, *c) {
            *c -= 1;
        }
    }

    fn repeat(&mut self, b: &mut Vec<char>, c: &mut usize, clip: &mut dyn FnMut() -> Option<String>, out: &mut Out) {
        if self.dot.is_empty() {
            return;
        }
        self.replaying = true;
        for k in self.dot.clone() {
            self.step(b, c, K::Ch(k), clip, out);
        }
        if self.mode == Mode::Insert {
            let t: Vec<char> = self.dot_text.chars().collect();
            let k = t.len();
            if k > 0 {
                b.splice(*c..*c, t);
                *c += k;
                out.changed = true;
            }
            self.leave_insert(b, c);
        }
        self.replaying = false;
    }

    /// Where motion `m` lands from `c`, or None when it cannot move. `op` is set when an operator
    /// waits for it, which lets `l` reach the line end.
    fn eval(&mut self, b: &[char], c: usize, m: M, n: usize, has: bool, op: bool) -> Option<(usize, Kind)> {
        if !matches!(m, M::J | M::K) {
            self.want_col = None;
        }
        let (s, e) = (ls(b, c), le(b, c));
        let r = match m {
            M::H => (c > s).then(|| (c - n.min(c - s), Kind::Excl)),
            M::L => {
                let lim = if op { e } else { e.saturating_sub(1).max(s) };
                (c < lim).then(|| ((c + n).min(lim), Kind::Excl))
            }
            M::J | M::K => {
                let col = *self.want_col.get_or_insert(c - s);
                let t = if m == M::J { line_down(b, c, n) } else { line_up(b, c, n) }?;
                Some((t + col.min(le(b, t) - t), Kind::Line))
            }
            M::Plus => line_down(b, c, n).map(|t| (fnb(b, t), Kind::Line)),
            M::Minus => line_up(b, c, n).map(|t| (fnb(b, t), Kind::Line)),
            M::W(big) => {
                let t = (0..n).fold(c, |t, _| next_word(b, t, big));
                (t != c).then_some((t, Kind::Excl))
            }
            M::B(big) => {
                let t = (0..n).fold(c, |t, _| prev_word(b, t, big));
                (t != c).then_some((t, Kind::Excl))
            }
            M::E(big) => {
                let t = (0..n).fold(c, |t, _| end_word(b, t, big));
                (t != c).then_some((t, Kind::Incl))
            }
            M::Zero => Some((s, Kind::Excl)),
            M::Caret => Some((fnb(b, s), Kind::Excl)),
            M::Dollar => {
                self.want_col = Some(usize::MAX);
                let last = if n > 1 { line_down(b, c, n - 1).unwrap_or(s) } else { s };
                Some((le(b, last), Kind::Excl))
            }
            M::BigG => Some((fnb(b, if has { line_n(b, n) } else { ls(b, b.len()) }), Kind::Line)),
            M::GG => Some((fnb(b, if has { line_n(b, n) } else { 0 }), Kind::Line)),
            M::Find(kind, ch) => {
                self.last_find = Some((kind, ch));
                find_in_line(b, c, kind, ch, n, false)
            }
            M::Rep(rev) => {
                let (kind, ch) = self.last_find?;
                // `,` runs the same find the other way: f and F, t and T differ only in case.
                let kind = if !rev {
                    kind
                } else if kind.is_ascii_lowercase() {
                    kind.to_ascii_uppercase()
                } else {
                    kind.to_ascii_lowercase()
                };
                find_in_line(b, c, kind, ch, n, true)
            }
            M::Pct => match_bracket(b, c).map(|t| (t, Kind::Incl)),
            M::ParaN => Some(((0..n).fold(c, |t, _| para_next(b, t)), Kind::Excl)),
            M::ParaP => Some(((0..n).fold(c, |t, _| para_prev(b, t)), Kind::Excl)),
            M::Next(rev) => {
                let sr = self.search.as_ref()?;
                let back = sr.back ^ rev;
                let mut t = c;
                for _ in 0..n {
                    t = search(b, t, &sr.pat, back, sr.whole)?;
                }
                Some((t, Kind::Excl))
            }
        };
        r
    }
}

fn count(p: &[char], i: &mut usize) -> Option<usize> {
    let st = *i;
    while *i < p.len() && p[*i].is_ascii_digit() && !(*i == st && p[*i] == '0') {
        *i += 1;
    }
    (st < *i).then(|| p[st..*i].iter().collect::<String>().parse::<usize>().unwrap_or(1).clamp(1, 9999))
}

fn motion(p: &[char]) -> P<M> {
    let Some(&c) = p.first() else { return P::More };
    let m = match c {
        'h' => M::H,
        'l' | ' ' => M::L,
        'j' => M::J,
        'k' => M::K,
        '+' => M::Plus,
        '-' => M::Minus,
        'w' => M::W(false),
        'W' => M::W(true),
        'b' => M::B(false),
        'B' => M::B(true),
        'e' => M::E(false),
        'E' => M::E(true),
        '0' => M::Zero,
        '^' => M::Caret,
        '$' => M::Dollar,
        'G' => M::BigG,
        ';' => M::Rep(false),
        ',' => M::Rep(true),
        '%' => M::Pct,
        '}' => M::ParaN,
        '{' => M::ParaP,
        'n' => M::Next(false),
        'N' => M::Next(true),
        'g' => {
            return match p.get(1) {
                None => P::More,
                Some('g') => P::Ok(M::GG),
                _ => P::Bad,
            };
        }
        'f' | 'F' | 't' | 'T' => {
            return match p.get(1) {
                None => P::More,
                Some(&ch) => P::Ok(M::Find(c, ch)),
            };
        }
        _ => return P::Bad,
    };
    P::Ok(m)
}

fn parse(p: &[char], visual: bool) -> P<Cmd> {
    let mut i = 0;
    let c1 = count(p, &mut i);
    let Some(&c) = p.get(i) else { return P::More };
    i += 1;
    let cmd = |n: Option<usize>, what: What| Cmd { n: n.unwrap_or(1), has: n.is_some(), what };
    let mk = |n: Option<usize>, what: What| P::Ok(cmd(n, what));
    if visual {
        match c {
            'd' | 'x' | 'c' | 's' | 'y' | '>' | '<' | '~' | 'u' | 'U' | 'p' | 'P' | 'J' | 'o' | 'v' | 'V' => return mk(c1, What::Simple(c)),
            'i' | 'a' => {
                return match p.get(i) {
                    None => P::More,
                    Some(&o) => mk(c1, What::Obj(c == 'i', o)),
                };
            }
            _ => {}
        }
    } else {
        match c {
            'd' | 'c' | 'y' | '>' | '<' => {
                let c2 = count(p, &mut i);
                let n = match (c1, c2) {
                    (None, None) => None,
                    (a, b) => Some(a.unwrap_or(1) * b.unwrap_or(1)),
                };
                let Some(&m) = p.get(i) else { return P::More };
                if m == c {
                    return mk(n, What::Op(c, Target::Lines));
                }
                if m == 'i' || m == 'a' {
                    return match p.get(i + 1) {
                        None => P::More,
                        Some(&o) => mk(n, What::Op(c, Target::Obj(m == 'i', o))),
                    };
                }
                return motion(&p[i..]).map(|mm| cmd(n, What::Op(c, Target::M(mm))));
            }
            'r' => {
                return match p.get(i) {
                    None => P::More,
                    Some(&ch) => mk(c1, What::Replace(ch)),
                };
            }
            '/' | '?' => return mk(c1, What::Search(c == '?')),
            'x' | 'X' | 'D' | 'C' | 'Y' | 's' | 'S' | 'J' | '~' | 'p' | 'P' | 'i' | 'a' | 'I' | 'A' | 'o' | 'O' | 'v' | 'V' | 'u' | '.' | '*' | '#' => return mk(c1, What::Simple(c)),
            _ => {}
        }
    }
    motion(&p[i - 1..]).map(|m| cmd(c1, What::Move(m)))
}

/// 0 for whitespace, 1 for word chars, 2 for punctuation. `$` is a word char so `$match` and JS names hold together.
fn class(c: char, big: bool) -> u8 {
    if c.is_whitespace() {
        0
    } else if big || c.is_alphanumeric() || c == '_' || c == '$' {
        1
    } else {
        2
    }
}

fn ls(b: &[char], i: usize) -> usize {
    b[..i.min(b.len())].iter().rposition(|&c| c == '\n').map_or(0, |p| p + 1)
}

fn le(b: &[char], i: usize) -> usize {
    let i = i.min(b.len());
    b[i..].iter().position(|&c| c == '\n').map_or(b.len(), |p| i + p)
}

fn blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// First non-blank of the line starting at `s`.
fn fnb(b: &[char], s: usize) -> usize {
    let mut j = s;
    while j < b.len() && blank(b[j]) {
        j += 1;
    }
    j
}

/// In Normal mode the cursor sits on a char, never on a line's newline unless the line is empty.
pub(crate) fn clamp(b: &[char], c: usize) -> usize {
    let c = c.min(b.len());
    if c > ls(b, c) && (c == b.len() || b[c] == '\n') { c - 1 } else { c }
}

/// Start of the line `n` lines below, as far as the text goes; None when already on the last line.
fn line_down(b: &[char], i: usize, n: usize) -> Option<usize> {
    let mut s = ls(b, i);
    let mut moved = 0;
    while moved < n {
        let e = le(b, s);
        if e >= b.len() {
            break;
        }
        s = e + 1;
        moved += 1;
    }
    (moved > 0).then_some(s)
}

fn line_up(b: &[char], i: usize, n: usize) -> Option<usize> {
    let mut s = ls(b, i);
    let mut moved = 0;
    while moved < n && s > 0 {
        s = ls(b, s - 1);
        moved += 1;
    }
    (moved > 0).then_some(s)
}

/// Start of line `n`, counted from 1, or of the last line.
fn line_n(b: &[char], n: usize) -> usize {
    line_down(b, 0, n.saturating_sub(1)).unwrap_or(0)
}

fn next_word(b: &[char], i: usize, big: bool) -> usize {
    let n = b.len();
    if i >= n {
        return n;
    }
    let cl = class(b[i], big);
    let mut j = i;
    if cl != 0 {
        while j < n && class(b[j], big) == cl {
            j += 1;
        }
    }
    // An empty line counts as a word, as in Vim.
    while j < n && class(b[j], big) == 0 {
        if b[j] == '\n' && j + 1 < n && b[j + 1] == '\n' {
            return j + 1;
        }
        j += 1;
    }
    j
}

fn prev_word(b: &[char], i: usize, big: bool) -> usize {
    if i == 0 {
        return 0;
    }
    let mut j = i - 1;
    while j > 0 && class(b[j], big) == 0 {
        if b[j] == '\n' && b[j - 1] == '\n' {
            return j;
        }
        j -= 1;
    }
    let cl = class(b[j], big);
    if cl == 0 {
        return j;
    }
    while j > 0 && class(b[j - 1], big) == cl {
        j -= 1;
    }
    j
}

fn end_word(b: &[char], i: usize, big: bool) -> usize {
    let n = b.len();
    let mut j = i + 1;
    while j < n && class(b[j], big) == 0 {
        j += 1;
    }
    if j >= n {
        return n.saturating_sub(1).max(i);
    }
    let cl = class(b[j], big);
    while j + 1 < n && class(b[j + 1], big) == cl {
        j += 1;
    }
    j
}

fn find_in_line(b: &[char], c: usize, kind: char, ch: char, n: usize, rep: bool) -> Option<(usize, Kind)> {
    let (s, e) = (ls(b, c), le(b, c));
    let fwd = kind.is_ascii_lowercase();
    let till = kind.eq_ignore_ascii_case(&'t');
    // A repeated `t` starts one further, or it would stay put before the same char.
    let skip = usize::from(till && rep);
    let i = if fwd { (c + 1 + skip..e).filter(|&i| b[i] == ch).nth(n - 1)? } else { (s..c.saturating_sub(skip)).rev().filter(|&i| b[i] == ch).nth(n - 1)? };
    Some(match (fwd, till) {
        (true, true) => (i - 1, Kind::Incl),
        (true, false) => (i, Kind::Incl),
        (false, true) => (i + 1, Kind::Excl),
        (false, false) => (i, Kind::Excl),
    })
}

fn pair_of(ch: char) -> Option<(char, char, bool)> {
    Some(match ch {
        '(' => ('(', ')', true),
        ')' => ('(', ')', false),
        '[' => ('[', ']', true),
        ']' => ('[', ']', false),
        '{' => ('{', '}', true),
        '}' => ('{', '}', false),
        _ => return None,
    })
}

/// The first index of `it` where a `down` closes the nesting, with every `up` on the way opening a level.
fn depth_find(b: &[char], it: impl Iterator<Item = usize>, up: char, down: char) -> Option<usize> {
    let mut d = 0usize;
    for j in it {
        if b[j] == up {
            d += 1;
        } else if b[j] == down {
            if d == 0 {
                return Some(j);
            }
            d -= 1;
        }
    }
    None
}

fn match_bracket(b: &[char], c: usize) -> Option<usize> {
    let e = le(b, c);
    let i = (c..e).find(|&i| pair_of(b[i]).is_some())?;
    let (open, close, fwd) = pair_of(b[i])?;
    if fwd { depth_find(b, i + 1..b.len(), open, close) } else { depth_find(b, (0..i).rev(), close, open) }
}

fn para_next(b: &[char], i: usize) -> usize {
    let n = b.len();
    let mut s = ls(b, i);
    while s < n && b[s] == '\n' {
        s += 1;
    }
    loop {
        let e = le(b, s);
        if e >= n {
            return n;
        }
        s = e + 1;
        if s >= n || b[s] == '\n' {
            return s.min(n);
        }
    }
}

fn para_prev(b: &[char], i: usize) -> usize {
    let mut s = ls(b, i);
    while s > 0 && le(b, s) == s {
        s = ls(b, s - 1);
    }
    while s > 0 {
        s = ls(b, s - 1);
        if le(b, s) == s {
            return s;
        }
    }
    0
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Next match of `pat` after `from` (or before it when `back`), wrapping around. Smart case: a pattern
/// with a capital letter matches case exactly.
fn search(b: &[char], from: usize, pat: &[char], back: bool, whole: bool) -> Option<usize> {
    let n = b.len();
    if pat.is_empty() || pat.len() > n {
        return None;
    }
    let fold = !pat.iter().any(|c| c.is_uppercase());
    let at = |i: usize| {
        i + pat.len() <= n
            && pat.iter().enumerate().all(|(k, &p)| if fold { lower(b[i + k]) == lower(p) } else { b[i + k] == p })
            && (!whole || ((i == 0 || class(b[i - 1], false) != 1) && (i + pat.len() == n || class(b[i + pat.len()], false) != 1)))
    };
    let from = from.min(n - 1);
    if back { (0..from).rev().chain((from..n).rev()).find(|&i| at(i)) } else { (from + 1..n).chain(0..=from).find(|&i| at(i)) }
}

fn object(b: &[char], c: usize, inner: bool, o: char) -> Option<(usize, usize)> {
    match o {
        'w' | 'W' => word_obj(b, c, inner, o == 'W'),
        '"' | '\'' | '`' => quote_obj(b, c, inner, o),
        '(' | ')' | 'b' => bracket_obj(b, c, inner, '(', ')'),
        '[' | ']' => bracket_obj(b, c, inner, '[', ']'),
        '{' | '}' | 'B' => bracket_obj(b, c, inner, '{', '}'),
        '<' | '>' => bracket_obj(b, c, inner, '<', '>'),
        _ => None,
    }
}

fn word_obj(b: &[char], c: usize, inner: bool, big: bool) -> Option<(usize, usize)> {
    let c = c.min(b.len().checked_sub(1)?);
    if b[c] == '\n' {
        return None;
    }
    let cl = class(b[c], big);
    let same = |i: usize| b[i] != '\n' && class(b[i], big) == cl;
    let (mut s, mut e) = (c, c + 1);
    while s > 0 && same(s - 1) {
        s -= 1;
    }
    while e < b.len() && same(e) {
        e += 1;
    }
    if inner {
        return Some((s, e));
    }
    let ws = |i: usize| blank(b[i]);
    if cl != 0 {
        // `aw` takes the spaces after the word, or before it when there are none after.
        let mut e2 = e;
        while e2 < b.len() && ws(e2) {
            e2 += 1;
        }
        if e2 > e {
            return Some((s, e2));
        }
        let mut s2 = s;
        while s2 > 0 && ws(s2 - 1) {
            s2 -= 1;
        }
        Some((s2, e))
    } else {
        let mut e2 = e;
        if e2 < b.len() && b[e2] != '\n' {
            let cl2 = class(b[e2], big);
            while e2 < b.len() && b[e2] != '\n' && class(b[e2], big) == cl2 {
                e2 += 1;
            }
        }
        Some((s, e2))
    }
}

fn quote_obj(b: &[char], c: usize, inner: bool, q: char) -> Option<(usize, usize)> {
    let (s, e) = (ls(b, c), le(b, c));
    let qs: Vec<usize> = (s..e).filter(|&i| b[i] == q && (i == s || b[i - 1] != '\\')).collect();
    let pairs = || qs.chunks(2).filter(|p| p.len() == 2);
    let p = pairs().find(|p| p[0] <= c && c <= p[1]).or_else(|| pairs().find(|p| p[0] > c))?;
    let (a, z) = (p[0], p[1]);
    if inner {
        return Some((a + 1, z));
    }
    let mut z2 = z + 1;
    while z2 < e && blank(b[z2]) {
        z2 += 1;
    }
    Some((a, z2))
}

fn bracket_obj(b: &[char], c: usize, inner: bool, open: char, close: char) -> Option<(usize, usize)> {
    let c = c.min(b.len().checked_sub(1)?);
    // On a closing bracket the block is the one it closes; scanning starts just before it.
    let upto = if b[c] == close { c } else { c + 1 };
    let o = depth_find(b, (0..upto).rev(), close, open)?;
    let z = depth_find(b, o + 1..b.len(), open, close)?;
    if !inner {
        return Some((o, z + 1));
    }
    let mut s = o + 1;
    let mut e = z;
    // A block written over several lines leaves its brackets on their own lines.
    if b.get(s) == Some(&'\n') {
        s += 1;
        let zs = ls(b, z);
        if zs > s && b[zs..z].iter().all(|&ch| blank(ch)) {
            e = zs;
        }
    }
    Some((s, e.max(s)))
}

fn join(b: &mut Vec<char>, c: &mut usize, lines: usize) -> bool {
    let mut changed = false;
    for _ in 1..lines {
        let e = le(b, *c);
        if e >= b.len() {
            break;
        }
        let mut k = e + 1;
        while k < b.len() && blank(b[k]) {
            k += 1;
        }
        let space = !(e == ls(b, e) || b[e - 1] == ' ' || k >= b.len() || b[k] == '\n' || b[k] == ')');
        b.splice(e..k, space.then_some(' '));
        *c = e;
        changed = true;
    }
    changed
}

/// Indents or outdents every line from the one starting at `s` to the one ending at `e`, `n` levels.
/// The block is rebuilt and spliced once: a splice per line would shift the rest of the text each time.
fn indent(b: &mut Vec<char>, s: usize, e: usize, right: bool, n: usize) {
    let unit: Vec<char> = INDENT.repeat(n).chars().collect();
    let mut out = Vec::with_capacity(e - s + unit.len());
    for (k, line) in b[s..e].split(|&c| c == '\n').enumerate() {
        if k > 0 {
            out.push('\n');
        }
        let cut = if right {
            if !line.is_empty() {
                out.extend_from_slice(&unit);
            }
            0
        } else {
            match line.iter().take(unit.len()).take_while(|&&c| c == ' ').count() {
                0 if line.first() == Some(&'\t') => 1,
                spaces => spaces,
            }
        };
        out.extend_from_slice(&line[cut..]);
    }
    b.splice(s..e, out);
}

fn case(b: &mut [char], s: usize, e: usize, how: char) {
    let e = e.min(b.len());
    for ch in &mut b[s..e] {
        let up = match how {
            'u' => false,
            'U' => true,
            _ => !ch.is_uppercase(),
        };
        *ch = if up { ch.to_uppercase().next().unwrap_or(*ch) } else { lower(*ch) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `keys` on `before`, where `|` marks the cursor, and returns the text with the cursor marked.
    /// `<` and `>` inside keys are literal; `\x1b` is Esc and `\r` is Enter.
    fn run_with(v: &mut Vim, before: &str, keys: &str, clip: &mut Option<String>) -> String {
        let mut cur = before.chars().position(|c| c == '|').expect("cursor mark");
        let mut b: Vec<char> = before.replacen('|', "", 1).chars().collect();
        for ch in keys.chars() {
            let k = match ch {
                '\x1b' => K::Esc,
                '\r' => K::Enter,
                '\x08' => K::Backspace,
                '\x12' => K::CtrlR,
                ch => K::Ch(ch),
            };
            // Insert mode typing goes to the editor, not the engine, so the test types it itself.
            if v.mode == Mode::Insert && k != K::Esc {
                b.insert(cur, ch);
                cur += 1;
                continue;
            }
            let c2 = clip.clone();
            let out = v.key(&mut b, &mut cur, k, &mut || c2.clone());
            if let Some(y) = out.yank {
                *clip = Some(y);
            }
        }
        b.insert(cur.min(b.len()), '|');
        b.into_iter().collect()
    }

    fn run(before: &str, keys: &str) -> String {
        run_with(&mut Vim::default(), before, keys, &mut None)
    }

    #[test]
    fn motions() {
        assert_eq!(run("|foo bar baz", "w"), "foo |bar baz");
        assert_eq!(run("|foo bar baz", "2w"), "foo bar |baz");
        assert_eq!(run("foo bar |baz", "b"), "foo |bar baz");
        assert_eq!(run("|foo bar", "e"), "fo|o bar");
        assert_eq!(run("|a.b c", "W"), "a.b |c");
        assert_eq!(run("|$match: 1", "w"), "$match|: 1");
        assert_eq!(run("  foo |bar", "0"), "|  foo bar");
        assert_eq!(run("  foo |bar", "^"), "  |foo bar");
        assert_eq!(run("|foo bar", "$"), "foo ba|r");
        assert_eq!(run("|a\nb\nc", "G"), "a\nb\n|c");
        assert_eq!(run("a\nb\n|c", "gg"), "|a\nb\nc");
        assert_eq!(run("|a\nb\nc", "2G"), "a\n|b\nc");
        assert_eq!(run("|a,b,c", "f,;"), "a,b|,c");
        assert_eq!(run("a,b,|c", "F,"), "a,b|,c");
        assert_eq!(run("|a,b,c", "t,;"), "a,|b,c");
        assert_eq!(run("|{ a: [1] }", "%"), "{ a: [1] |}");
        assert_eq!(run("{ a: [1] |}", "%"), "|{ a: [1] }");
        assert_eq!(run("|a\n\nb\n\nc", "}"), "a\n|\nb\n\nc");
        assert_eq!(run("a\n\nb\n\n|c", "{"), "a\n\nb\n|\nc");
    }

    #[test]
    fn up_down_keep_the_column() {
        assert_eq!(run("abc|d\nx\nabcd", "j"), "abcd\n|x\nabcd");
        assert_eq!(run("abc|d\nx\nabcd", "jj"), "abcd\nx\nabc|d");
        assert_eq!(run("abcd\nx\nab|cd", "kk"), "ab|cd\nx\nabcd");
        assert_eq!(run("|ab\nlonger", "$j"), "ab\nlonge|r");
        assert_eq!(run("|a\nb", "\r"), "a\n|b");
    }

    #[test]
    fn operators() {
        assert_eq!(run("foo b|ar baz", "dw"), "foo b|baz");
        assert_eq!(run("foo |bar baz", "dw"), "foo |baz");
        assert_eq!(run("|foo bar baz", "d2w"), "|baz");
        assert_eq!(run("|foo bar baz", "2dw"), "|baz");
        assert_eq!(run("|abcdef", "3x"), "|def");
        assert_eq!(run("abc|def", "X"), "ab|def");
        assert_eq!(run("ab|cdef", "D"), "a|b");
        assert_eq!(run("a\n|b\nc", "dd"), "a\n|c");
        assert_eq!(run("a\nb\n|c", "dd"), "a\n|b");
        assert_eq!(run("|a\nb\nc", "2dd"), "|c");
        assert_eq!(run("|a\nb\nc", "dj"), "|c");
        assert_eq!(run("|foo(bar)", "dt("), "|(bar)");
        assert_eq!(run("|foo(bar)", "df("), "|bar)");
        assert_eq!(run("|a b", "d$"), "|");
        assert_eq!(run("x |{ a: 1 } y", "d%"), "x | y");
    }

    #[test]
    fn dw_on_the_last_word_stops_at_the_line_end() {
        assert_eq!(run("foo |bar\nbaz", "dw"), "foo| \nbaz");
    }

    #[test]
    fn change_and_insert() {
        assert_eq!(run("foo |bar baz", "cwqux\x1b"), "foo qu|x baz");
        assert_eq!(run("foo b|ar baz", "ciwX\x1b"), "foo |X baz");
        assert_eq!(run("{ a: [1, 2|] }", "ci[\x1b"), "{ a: |[] }");
        assert_eq!(run("{ a: \"hel|lo\" }", "ci\"x\x1b"), "{ a: \"|x\" }");
        assert_eq!(run("f(a, |b)", "da("), "|f");
        assert_eq!(run("|  foo\nbar", "ccx\x1b"), "  |x\nbar");
        assert_eq!(run("ab|c", "C1\x1b"), "ab|1");
        assert_eq!(run("|abc", "s1\x1b"), "|1bc");
        assert_eq!(run("|abc", "ax\x1b"), "a|xbc");
        assert_eq!(run("a|bc", "Ix\x1b"), "|xabc");
        assert_eq!(run("a|bc", "Ax\x1b"), "abc|x");
        assert_eq!(run("  |a\nb", "ox\x1b"), "  a\n  |x\nb");
        assert_eq!(run("  |a\nb", "Ox\x1b"), "  |x\n  a\nb");
        assert_eq!(run("ab|c", "i\x1b"), "a|bc");
    }

    #[test]
    fn cw_on_a_last_letter_changes_only_that_word() {
        assert_eq!(run("let |a = foo;", "cwb\x1b"), "let |b = foo;");
        assert_eq!(run("fo|o bar", "cwx\x1b"), "fo|x bar");
        assert_eq!(run("|a b c", "2cwx\x1b"), "|x c");
        assert_eq!(run("|ab cd", "cwx\x1b"), "|x cd");
    }

    #[test]
    fn keys_that_are_not_chars_cancel_a_waiting_command() {
        assert_eq!(run("|abc", "r\x08"), "|abc");
        assert_eq!(run("|abc", "r\rx"), "|bc");
        assert_eq!(run("|a,b", "f\r;"), "|a,b");
        assert_eq!(run("|abc", "d\x08x"), "|bc");
        assert_eq!(run("a|bc\nd", "2\x1bj"), "abc\n|d");
    }

    #[test]
    fn a_deferred_paste_hands_its_keys_back_and_runs_later() {
        let mut v = Vim { defer_paste: true, ..Default::default() };
        let (mut b, mut cur): (Vec<char>, usize) = ("ab".chars().collect(), 0);
        let out = v.key(&mut b, &mut cur, K::Ch('2'), &mut || None);
        assert!(out.paste.is_none());
        let out = v.key(&mut b, &mut cur, K::Ch('p'), &mut || Some("X".into()));
        assert_eq!(out.paste, Some(vec!['2', 'p']));
        assert!(!out.changed);
        v.defer_paste = false;
        let mut out = Out::default();
        for k in ['2', 'p'] {
            out = v.key(&mut b, &mut cur, K::Ch(k), &mut || Some("X".into()));
        }
        assert_eq!(b.iter().collect::<String>(), "aXXb");
        assert!(out.changed);
    }

    #[test]
    fn multiline_block_change_keeps_brackets_on_their_lines() {
        assert_eq!(run("{\n  a|: 1\n}", "di{"), "{\n|}");
        assert_eq!(run("{\n  a|: 1\n}", "ci{x\x1b"), "{\n|x\n}");
    }

    #[test]
    fn yank_and_paste() {
        let mut clip = None;
        let mut v = Vim::default();
        assert_eq!(run_with(&mut v, "|a\nb", "yyp", &mut clip), "a\n|a\nb");
        assert_eq!(clip.as_deref(), Some("a\n"));
        assert_eq!(run_with(&mut Vim::default(), "|foo bar", "yiwP", &mut None), "fo|ofoo bar");
        assert_eq!(run_with(&mut Vim::default(), "|ab", "xp", &mut None), "b|a");
        assert_eq!(run_with(&mut Vim::default(), "a\n|b", "ddP", &mut None), "|b\na");
        assert_eq!(run_with(&mut Vim::default(), "|a\nb", "jddkP", &mut None), "|b\na");
        assert_eq!(run_with(&mut Vim::default(), "a\n|b", "yyp", &mut None), "a\nb\n|b");
        // Text copied elsewhere pastes as it is, linewise only when it ends in a newline.
        let mut clip = Some("XY".to_string());
        assert_eq!(run_with(&mut Vim::default(), "|ab", "p", &mut clip), "aX|Yb");
        let mut clip = Some("line\n".to_string());
        assert_eq!(run_with(&mut Vim::default(), "|ab\ncd", "p", &mut clip), "ab\n|line\ncd");
    }

    #[test]
    fn dot_repeats_the_last_change() {
        assert_eq!(run("|abcdef", "x.."), "|def");
        assert_eq!(run("|a b c", "dw."), "|c");
        assert_eq!(run("|foo foo", "cwbar\x1bw."), "bar ba|r");
        assert_eq!(run("|a\nb", "A;\x1bj."), "a;\nb|;");
        assert_eq!(run("|a\nb\nc", ">>j."), "  a\n  |b\nc");
    }

    #[test]
    fn misc() {
        assert_eq!(run("|a\n  b", "J"), "a| b");
        assert_eq!(run("|a\nb\nc", "3J"), "a b| c");
        assert_eq!(run("|ab", "~~"), "A|B");
        assert_eq!(run("|abc", "2rx"), "x|xc");
        assert_eq!(run("|a\nb", ">j"), "  |a\n  b");
        assert_eq!(run("    |a", "<<"), "  |a");
        assert_eq!(run("|ab", "u").as_str(), "|ab");
    }

    #[test]
    fn visual() {
        assert_eq!(run("|foo bar", "vld"), "|o bar");
        assert_eq!(run("foo |bar baz", "veyP"), "foo ba|rbar baz");
        assert_eq!(run("a\n|b\nc\nd", "Vjd"), "a\n|d");
        assert_eq!(run("|a\nb\nc", "Vj>"), "  |a\n  b\nc");
        assert_eq!(run("foo b|ar baz", "viwc1\x1b"), "foo |1 baz");
        assert_eq!(run("|abc", "vlU"), "|ABc");
        assert_eq!(run("a|bc", "vlo\x1b"), "a|bc");
        assert_eq!(run("|ab cd", "vex"), "| cd");
    }

    #[test]
    fn search_moves_and_repeats() {
        assert_eq!(run("|foo bar foo bar", "/bar\r"), "foo |bar foo bar");
        assert_eq!(run("|foo bar foo bar", "/bar\rn"), "foo bar foo |bar");
        assert_eq!(run("|foo bar foo bar", "/bar\rnn"), "foo |bar foo bar");
        assert_eq!(run("foo bar foo |bar", "?foo\r"), "foo bar |foo bar");
        assert_eq!(run("|Foo foo", "/foo\r"), "Foo |foo");
        assert_eq!(run("|a Foo foo", "/Foo\rn"), "a |Foo foo");
        assert_eq!(run("|ab abc ab", "*"), "ab abc |ab");
        assert_eq!(run("ab abc |ab", "#"), "|ab abc ab");
        assert_eq!(run("|ab", "/zz\r"), "|ab");
        assert_eq!(run("|ab", "/x\x08\x08l"), "a|b");
    }

    #[test]
    fn pending_and_escape() {
        let mut v = Vim::default();
        let mut b: Vec<char> = "abc".chars().collect();
        let mut cur = 0;
        v.key(&mut b, &mut cur, K::Ch('2'), &mut || None);
        v.key(&mut b, &mut cur, K::Ch('d'), &mut || None);
        assert_eq!(v.pending(), "2d");
        assert!(v.wants_esc());
        v.key(&mut b, &mut cur, K::Esc, &mut || None);
        assert!(!v.wants_esc());
        v.key(&mut b, &mut cur, K::Ch('x'), &mut || None);
        assert_eq!(b, ['b', 'c']);
        let out = v.key(&mut b, &mut cur, K::CtrlR, &mut || None);
        assert_eq!(out.history, Some(History::Redo));
    }

    #[test]
    fn empty_text_does_not_panic() {
        for keys in ["x", "dd", "dw", "p", "J", "~", "ciw", "di(", "vd", "Vd", ">>", "G", "%", "}", "{", "*", "/a\r", "o\x1b", "yy", "D", "rq"] {
            run("|", keys);
        }
        for keys in ["x", "dd", "dw", "p", "J", "~", "ciw", "di(", "vd", "Vd", ">>", "G", "$", "%", "e", "b", "*", "o\x1b", "yyp", "D"] {
            run("a\n|", keys);
            run("|\n\n", keys);
        }
    }
}
