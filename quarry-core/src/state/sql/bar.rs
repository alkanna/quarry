//! State behind src/app/sql/bar.rs, which draws it.

use super::filter_panel::Panel;
use crate::db::sql::{filter::self, rules::{Grain, Rule}, schema::SchemaCol, value::Kind, Col, PageQuery};
use crate::state::Tab;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// The clauses a tab keeps besides the WHERE and ORDER BY text, saved with the open tabs.
#[derive(Default, Clone, PartialEq, Debug, Serialize, Deserialize)]
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

/// Runtime state of the bar that Core reads. Only `c` is saved. What only drawing needs (suggestions, popovers, the group editor)
/// is `BarUi` in src/app/sql/bar.rs.
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
    /// A column whose facts a value cell asked for, and its kind; `sql_content` sends the request.
    pub want_facts: Option<(String, Kind)>,
    /// The WHERE text the tree was last written as; a different `filter_txt` came from elsewhere.
    pub src: String,
    /// The tree was read again, so the text is written back once, in the tree's spelling.
    pub rewrite: bool,
    /// The table's own columns, kept when a grouped answer replaces the result columns.
    pub table_cols: Arc<Vec<Col>>,
    /// Initial open states; only the offline demo sets them.
    pub preset: Preset,
    /// Whether the bar holds another query than the page's, worked out once a frame by `bar` so the results need not
    /// build the query again.
    pub dirty: bool,
    /// A WHERE text as it ran and as the rules respelled it.
    pub respelled: Option<(String, String)>,
    /// What `bar` derives from the columns, kept while they and the primary key stay the same.
    pub memo: Option<ColsMemo>,
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
    query_of(&t.filter_txt, &t.sort_txt, c)
}

/// The same from texts and clauses the bar is still editing, which Core has not been told of yet.
pub fn query_of(filter: &str, sort: &str, c: Clauses) -> PageQuery {
    PageQuery { filter: filter.to_string(), order: sort.to_string(), select: split_top(&c.select), distinct: c.distinct, group: split_top(&c.group), having: c.having, limit: c.limit.trim().parse().ok(), skip: c.offset.trim().parse().unwrap_or(0), index_hint: c.hint }
}

/// What the bar derives from the columns and the text before it draws: the memo, the tree read from the WHERE text, and
/// whether the bar holds another query than the one that ran. Reading the text as rules spells it their way, which is what
/// is written back; it is the same query, so it must not count as an edit of the one that ran.
pub fn sync(t: &mut Tab, d: Dialect, bs: bool) {
    let mut q = page_query(t);
    let Some(s) = t.sql.as_mut() else { return };
    let cols = if s.bar.table_cols.is_empty() { s.cols.clone() } else { s.bar.table_cols.clone() };
    let memo = match s.bar.memo.take() {
        Some(m) if Arc::ptr_eq(&m.cols, &cols) && m.pk == s.pk => m,
        _ => ColsMemo {
            fallback: Arc::new(cols.iter().map(|c| SchemaCol { name: c.name.clone(), ty: c.ty.clone(), kind: c.kind, key: if s.pk.contains(&c.src) { "PRI".into() } else { String::new() }, nullable: c.nullable, fk: false }).collect()),
            pairs: Arc::new(cols.iter().map(|c| (c.name.clone(), c.kind)).collect()),
            pk: s.pk.clone(),
            cols: cols.clone(),
        },
    };
    let pairs = memo.pairs.clone();
    let b = &mut s.bar;
    b.memo = Some(memo);
    if read_tree(b, &t.filter_txt, &pairs, d, bs, false) && !pairs.is_empty() && !b.c.sql {
        let out = filter::to_sql(&b.tree, d, bs);
        if out != t.filter_txt {
            b.respelled = Some((t.filter_txt.clone(), out.clone()));
        }
        q.filter = out;
    }
    b.dirty = q != *b.ran_as_shown(&s.ran);
    // After a read, so a text read in another spelling is shown as the tree writes it.
    if !b.c.sql && std::mem::take(&mut b.rewrite) {
        write_tree(b, &mut t.filter_txt, &pairs, d, bs);
    }
}

/// The panel's own state that the drawing changes; the counts are Core's.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct PanelView {
    pub open: bool,
    pub sel: Option<u32>,
    pub pick: Option<u32>,
    pub ops: Option<u32>,
    pub hover: Option<u32>,
}

/// What a frame of the bar changed, each part only when it differs from what Core held. `written` is the panel's own edit of a
/// row, after which the WHERE text is written from the tree.
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub struct BarEdit {
    pub filter: Option<String>,
    pub sort: Option<String>,
    pub clauses: Option<Clauses>,
    pub tree: Option<filter::Tree>,
    pub written: bool,
    pub panel: Option<PanelView>,
}

impl BarEdit {
    pub fn is_empty(&self) -> bool {
        self.filter.is_none() && self.sort.is_none() && self.clauses.is_none() && self.tree.is_none() && !self.written && self.panel.is_none()
    }
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

use crate::db;
use crate::db::sql::{filter::Match, rules::{self, Op}, Dialect};
use crate::state::SqlTab;
use mongodb::bson::Bson;

/// A plain column name from a name quoted the way `quote_ident` writes it or a bare word, or None for an expression.
/// `d` picks the quote: on MySQL "a" is a string, and a caller that rewrites the text must not turn it into a column.
/// None reads either quote, for the callers that only look and have no dialect at hand.
pub fn plain_ident(s: &str, d: Option<Dialect>) -> Option<String> {
    let s = s.trim();
    let quotes: &[char] = match d {
        Some(Dialect::Postgres) => &['"'],
        Some(_) => &['`'],
        None => &['`', '"'],
    };
    for &q in quotes {
        if let Some(inner) = s.strip_prefix(q).and_then(|x| x.strip_suffix(q)) {
            // A lone quote inside means several names or an expression, such as "a" || "b".
            let mut out = String::with_capacity(inner.len());
            let mut it = inner.chars();
            while let Some(c) = it.next() {
                if c == q && it.next() != Some(q) {
                    return None;
                }
                out.push(c);
            }
            return (!out.is_empty()).then_some(out);
        }
    }
    // PostgreSQL folds an unquoted name to lower case, and that is the column it reads.
    (!s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$')).then(|| if d == Some(Dialect::Postgres) { s.to_ascii_lowercase() } else { s.to_string() })
}

/// ORDER BY text as (column, descending) keys, or None when it holds anything but plain columns.
pub fn parse_order(text: &str, d: Option<Dialect>) -> Option<Vec<(String, bool)>> {
    split_top(text)
        .iter()
        .map(|k| {
            let up = k.to_ascii_uppercase();
            let (col, desc) = if up.ends_with(" DESC") { (&k[..k.len() - 5], true) } else if up.ends_with(" ASC") { (&k[..k.len() - 4], false) } else { (k.as_str(), false) };
            Some((plain_ident(col, d)?, desc))
        })
        .collect()
}

/// Reads the WHERE text into the tree when it changed from elsewhere (Filter to these, a restored tab, a foreign key row),
/// or always with `force`. A tree that still writes the text stays, so off rows and groups survive a trip through SQL
/// mode, and a restored tab's saved tree comes back the same way. Text that is not a tree switches the bar to SQL. Without
/// columns nothing can be read, so `src` stays behind and the read happens once they arrive.
pub fn read_tree(b: &mut Bar, text: &str, pairs: &[(String, Kind)], d: Dialect, bs: bool, force: bool) -> bool {
    // A saved tree is read even when the text matches `src`, which it does for a restored tab whose rows are all off.
    let read = !b.c.sql && (force || text != b.src || !b.pending.is_empty()) && (!pairs.is_empty() || (text.trim().is_empty() && b.pending.is_empty()));
    if read {
        let pending = std::mem::take(&mut b.pending);
        let keep = pending.is_empty() && filter::to_sql(&b.tree, d, bs) == text;
        let tree = if keep {
            None
        } else if !pending.is_empty() {
            filter::reconcile(&pending, text, pairs, d, bs)
        } else {
            filter::parse(text, pairs, d, bs)
        };
        match tree {
            Some(t) => {
                // Ids start over in a tree read afresh, so a selection would name another row.
                (b.panel.sel, b.panel.pick) = (None, None);
                b.tree = t;
                b.tree_json = filter::to_json(&b.tree);
            }
            None if keep => {}
            None => b.c.sql = true,
        }
        b.src = text.to_string();
        b.rewrite = true;
    }
    read
}

/// Writes the tree out as the WHERE text, so Run, Copy as SQL and the fold line see it. Only a tree read from the current
/// text is written: another is stale and would replace a text not read yet.
pub fn write_tree(b: &mut Bar, text: &mut String, pairs: &[(String, Kind)], d: Dialect, bs: bool) {
    if pairs.is_empty() || b.src != *text {
        return;
    }
    let out = filter::to_sql(&b.tree, d, bs);
    // `src` already equals the text when nothing changed, so it is only copied on a change.
    if out != *text {
        *text = out;
        b.src = text.clone();
    }
}

pub fn tcols(s: &SqlTab) -> &[Col] {
    if s.bar.table_cols.is_empty() { &s.cols } else { &s.bar.table_cols }
}

/// The table's own sort can name an aggregate, which an ungrouped query does not have. It only decides
/// whether the sort stays, so it reads either quote: `ungroup` has no dialect at hand.
pub fn keep_sort(sort: &str, cols: &[Col]) -> bool {
    parse_order(sort, None).is_some_and(|k| k.iter().all(|(c, _)| cols.iter().any(|x| x.name == *c)))
}

pub fn bson_text(v: &Bson) -> Option<String> {
    match v {
        Bson::String(s) => Some(s.clone()),
        Bson::Int32(n) => Some(n.to_string()),
        Bson::Int64(n) => Some(n.to_string()),
        Bson::Double(n) => Some(n.to_string()),
        Bson::Boolean(b) => Some((*b as i32).to_string()),
        _ => None,
    }
}

/// Bytes as a literal of the dialect. On MySQL `rule` picks 0x, the form the rules write for a binary column;
/// otherwise X'..', which the bar does not read as a rule, so a text column never gets it back as a quoted string.
pub fn bytes_lit(b: &[u8], d: Dialect, bs: bool, rule: bool) -> String {
    let pg = d == Dialect::Postgres;
    let hex = db::sql::value::hex_digits(b, !pg);
    if pg {
        db::sql::value::pg_lit(&format!("\\x{hex}"))
    } else if b.is_empty() {
        // 0x without digits is not a literal; an empty string equals empty bytes.
        db::sql::value::lit("", bs)
    } else if rule {
        format!("0x{hex}")
    } else {
        format!("X'{hex}'")
    }
}

/// Clears GROUP BY, HAVING and the generated select list; the sort stays when it names real columns.
pub fn ungroup(t: &mut Tab) {
    let Some(s) = t.sql.as_mut() else { return };
    s.bar.c.group.clear();
    s.bar.c.having.clear();
    s.bar.c.select.clear();
    if !keep_sort(&t.sort_txt, tcols(s)) {
        t.sort_txt.clear();
    }
    t.page = 0;
}

/// Turns the double-clicked group into conditions on the WHERE text and ungroups.
pub fn drill(t: &mut Tab, row: usize, d: Dialect, bs: bool) {
    let Some(doc) = t.docs.get(row).and_then(|r| r.decode().ok()) else { return };
    let Some(s) = t.sql.as_ref() else { return };
    let (select, group_list) = (split_top(&s.bar.c.select), split_top(&s.bar.c.group));
    let mut conds: Vec<String> = Vec::new();
    for g in &group_list {
        let Some(name) = plain_ident(g, Some(d)) else { continue };
        let Some(v) = doc.get(&name) else { continue };
        // A group named after a select alias is an expression such as DATE(col).
        let expr = select.iter().find_map(|i| {
            let up = i.to_ascii_uppercase();
            let at = up.rfind(" AS ")?;
            (plain_ident(&i[at + 4..], Some(d))? == name).then(|| i[..at].trim().to_string())
        });
        let lhs = match (&expr, tcols(s).iter().find(|c| c.name == name)) {
            (Some(e), _) => e.clone(),
            (None, Some(_)) => d.quote_ident(&name),
            _ => continue,
        };
        conds.push(match v {
            Bson::Null => format!("{lhs} IS NULL"),
            // Not through rule_sql by column kind: a MySQL text column with bytes that are not UTF-8 arrives as binary too.
            Bson::Binary(bin) => {
                let binary_col = expr.is_none() && tcols(s).iter().any(|c| c.name == name && rules::group_of(c.kind) == rules::Group::Other);
                format!("{lhs} = {}", bytes_lit(&bin.bytes, d, bs, binary_col))
            }
            v => {
                // The row reader makes no other value; a guessed condition would show the wrong rows, so the drill stops.
                let Some(text) = bson_text(v) else { return };
                match tcols(s).iter().find(|c| c.name == name && expr.is_none()) {
                    Some(c) => rules::rule_sql(&Rule { col: name.clone(), kind: c.kind, op: Op::Eq, values: vec![text], case: rules::default_case(Op::Eq, d) }, d, bs),
                    None => format!("{lhs} = {}", db::sql::value::lit(&text, bs)),
                }
            }
        });
    }
    let pairs: Vec<(String, Kind)> = tcols(s).iter().map(|c| (c.name.clone(), c.kind)).collect();
    t.filter_txt = and_onto(&t.filter_txt, conds, &pairs, d, bs);
    ungroup(t);
}

/// WHERE text `existing` with `conds` after it, joined by AND. Conditions already joined by AND need no parentheses.
pub fn and_onto(existing: &str, conds: impl IntoIterator<Item = String>, pairs: &[(String, Kind)], d: Dialect, bs: bool) -> String {
    let existing = existing.trim();
    let flat = filter::parse(existing, pairs, d, bs).is_some_and(|t| t.root.mode == Match::All);
    // A line comment at the end would swallow the closing parenthesis, so that goes on its own line.
    let nl = if existing.lines().last().is_some_and(|l| l.contains("--") || l.contains('#')) { "\n" } else { "" };
    let existing = if existing.is_empty() { None } else if flat { Some(existing.to_string()) } else { Some(format!("({existing}{nl})")) };
    existing.into_iter().chain(conds).collect::<Vec<_>>().join(" AND ")
}
