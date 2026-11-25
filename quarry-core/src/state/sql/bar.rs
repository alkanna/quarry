//! State behind src/app/sql/bar.rs, which draws it.

use super::filter_panel::Panel;
use crate::db::sql::{filter::self, rules::{Grain, Rule}, schema::SchemaCol, value::Kind, Col, PageQuery};
use crate::state::Tab;
use crate::state::complete::Ac;
use crate::state::sql_complete::Wants;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// The clauses a tab keeps besides the WHERE and ORDER BY text, saved with the open tabs.
#[derive(Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Clauses {
    /// SQL mode of the bar instead of Rules mode.
    pub sql: bool,
    pub select: String,
    pub group: String,
    pub having: String,
    pub limit: String,
    pub offset: String,
    pub hint: String,
    pub distinct: bool,
    /// The filter tree as the panel last wrote it, kept only in the saved tabs: groups, order and off rows that the
    /// WHERE text alone loses. Empty at runtime, where the tree lives in `Bar`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tree: String,
}

/// One aggregate of the GROUP BY editor: function index into `FUNCS`, column ("" is `*`) and alias.
#[derive(Clone, Default)]
pub struct Agg {
    pub func: usize,
    pub col: String,
    pub alias: String,
}

/// The state of the GROUP BY editor. It is what the last Apply wrote, so reopening shows it again;
/// text edited by hand in SQL mode is not read back (ponytail: parse it if someone asks).
#[derive(Default)]
pub struct GroupDlg {
    pub keys: Vec<(String, Option<Grain>)>,
    pub aggs: Vec<Agg>,
    pub having: Option<Rule>,
    /// Whether the server reads backslash escapes, for the HAVING preview.
    pub bs: bool,
}

/// Runtime state of the bar. Only `c` is saved.
#[derive(Default)]
pub struct Bar {
    pub c: Clauses,
    /// The filter as the panel edits it. The WHERE text stays the source of truth: the tree writes it on every edit and
    /// is read again from any text that came from elsewhere.
    pub tree: filter::Tree,
    /// A restored tab's saved tree, used once the columns are known and only while it still writes the WHERE text.
    pub pending: String,
    /// The tree as the open tabs save it.
    pub tree_json: String,
    pub panel: Panel,
    /// Set by the panel's Save as filter; `sql_content` opens the Save dialog.
    pub save_filter: bool,
    /// A column whose facts a value cell asked for, and its kind; `sql_content` sends the request.
    pub want_facts: Option<(String, Kind)>,
    /// The WHERE text the tree was last written as; a different `filter_txt` came from elsewhere.
    pub src: String,
    /// The tree was read again, so the text is written back once, in the tree's spelling.
    pub rewrite: bool,
    pub fold_open: bool,
    pub gdlg: GroupDlg,
    /// The table's own columns, kept when a grouped answer replaces the result columns.
    pub table_cols: Arc<Vec<Col>>,
    /// The open suggestion list of whichever bar input has it, and what the inputs asked the app to look up.
    pub ac: Ac,
    pub wants: Wants,
    /// Set by the Explain button; `sql_content` runs the same entry point as Cmd+E.
    pub explain: bool,
    /// Initial open states; only the offline demo sets them.
    pub preset: Preset,
    /// The clause whose popover the "+" opened, because a clause with no chip has nothing else to hang it on.
    pub adding: Option<Add>,
    /// Whether the bar holds another query than the page's, worked out once a frame by `bar` so the results need not
    /// build the query again.
    pub dirty: bool,
    /// A WHERE text as it ran and as the rules respelled it.
    pub respelled: Option<(String, String)>,
    /// What `bar` derives from the columns, kept while they and the primary key stay the same.
    pub memo: Option<ColsMemo>,
    /// Whether the WHERE text read as a tree, for the text, the column pairs and the mode it was read with.
    pub parsable: Option<(String, Arc<Vec<(String, Kind)>>, bool, bool)>,
}

impl Bar {
    /// The query that ran, with its WHERE spelled as the rules write it when that is how the bar shows it now. The history keeps
    /// the text as it ran, so only this comparison reads the other spelling.
    pub fn ran_as_shown<'a>(&self, ran: &'a PageQuery) -> std::borrow::Cow<'a, PageQuery> {
        match &self.respelled {
            Some((from, to)) if ran.filter == *from => std::borrow::Cow::Owned(PageQuery { filter: to.clone(), ..ran.clone() }),
            _ => std::borrow::Cow::Borrowed(ran),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Add {
    Order,
    Columns,
    Group,
}

pub struct ColsMemo {
    pub cols: Arc<Vec<Col>>,
    pub pk: Vec<String>,
    pub fallback: Arc<Vec<SchemaCol>>,
    pub pairs: Arc<Vec<(String, Kind)>>,
}

/// What the bar shows open from the first frame: the suggestion list, the ORDER BY popover and the
/// text typed into its first column picker.
#[derive(Default)]
pub struct Preset {
    pub ac: bool,
    pub order: bool,
    pub pick: Option<String>,
}

/// What the worker is asked for, from the tab's texts.
pub fn page_query(t: &Tab) -> PageQuery {
    let c = t.sql.as_ref().map(|s| s.bar.c.clone()).unwrap_or_default();
    PageQuery { filter: t.filter_txt.clone(), order: t.sort_txt.clone(), select: split_top(&c.select), distinct: c.distinct, group: split_top(&c.group), having: c.having, limit: c.limit.trim().parse().ok(), skip: c.offset.trim().parse().unwrap_or(0), index_hint: c.hint }
}

/// Splits at commas outside quotes and parentheses: select lists and GROUP BY lists.
pub fn split_top(text: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth, mut quote) = (Vec::new(), String::new(), 0, None::<char>);
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"' | '`') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            (None, ',') if depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}
