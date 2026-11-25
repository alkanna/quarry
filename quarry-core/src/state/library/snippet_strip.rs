//! State behind src/app/library/snippet_strip.rs, which draws it.

use crate::library::snippet::{self, Lang, Parsed};
use crate::state::Tab;
use std::ops::Range;

/// The text of a tab that holds values: the SQL Query editor, the Shell editor, or the Documents bar's filter, sort and
/// projection, read as one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Src {
    Query,
    Shell,
    Bar,
}

pub struct MakeValue {
    pub uid: u64,
    pub src: Src,
    pub range: Range<usize>,
    pub literal: String,
    pub name: String,
    pub ty: String,
    pub default: Option<String>,
    pub keep: bool,
}

pub fn texts(t: &Tab, src: Src) -> Vec<&str> {
    match src {
        Src::Query => t.sql.as_ref().map(|s| vec![s.query.as_str()]).unwrap_or_default(),
        Src::Shell => vec![t.shell.code.as_str()],
        Src::Bar => vec![&t.filter_txt, &t.sort_txt, &t.proj_txt],
    }
}

pub fn text_mut(t: &mut Tab, src: Src) -> Option<&mut String> {
    match src {
        Src::Query => t.sql.as_mut().map(|s| &mut s.query),
        Src::Shell => Some(&mut t.shell.code),
        Src::Bar => None,
    }
}

/// The values of `src`, or None when it has none or holds a snippet the Cmd+K form already filled. Text without `{{`,
/// most tabs, costs one scan per text.
pub fn parsed(t: &Tab, src: Src, l: Lang) -> Option<Parsed> {
    let texts = texts(t, src);
    if t.snip.off || !texts.iter().any(|s| s.contains("{{")) {
        return None;
    }
    Some(snippet::merge(texts.into_iter().map(|s| snippet::parse(s, l)))).filter(Parsed::is_snippet)
}

/// `text`, all or part of `src`, with the tab's values filled in, or why it can't be. Each placeholder takes the type the
/// whole of `src` gives its name.
pub fn fill_tab(t: &Tab, text: &str, src: Src, l: Lang, bs: bool) -> Result<String, String> {
    if !text.contains("{{") {
        return Ok(text.to_string());
    }
    match parsed(t, src, l) {
        Some(full) => snippet::fill_named(text, &full, &t.snip.vals, l, bs),
        None => Ok(text.to_string()),
    }
}
