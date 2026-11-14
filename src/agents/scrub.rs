//! Compiled scrub rules and what they do to documents, rows and text before anything leaves Quarry: the Secrets
//! and PII presets plus the user's own rules. A rule that does not compile is an error for the whole set, never
//! skipped: a silently missing rule would leak.
//!
//! Speed: preset key patterns are one RegexSet run once per key, and preset value detectors sit behind one
//! RegexSet prefilter, so a string pays for one DFA pass plus only the detectors whose cheap pattern it contains.

use super::config::{Action, AgentsConfig, ConnPolicy, Matcher, PresetMode, PresetOverride, Presets, RuleSpec};
use super::detectors::{self, Spans};
use super::presets::{self, Finder, Item, norm_key};
use hmac::{Hmac, KeyInit, Mac};
use mongodb::bson::{Bson, Document};
use regex::{Regex, RegexSet};
use sha2::Sha256;
use std::fmt;
use std::ops::Range;
use std::sync::LazyLock;

/// A rule set that does not compile. `rule` is the position across global then connection rules, None for a
/// preset item.
#[derive(Clone, Debug, PartialEq)]
pub struct RuleErr {
    pub rule: Option<usize>,
    pub msg: String,
}

impl fmt::Display for RuleErr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.rule {
            Some(i) => write!(f, "Rule {} does not compile: {}", i + 1, self.msg),
            None => write!(f, "A preset does not compile: {}", self.msg),
        }
    }
}

/// Which rule changed a value, for the tester in Settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fired {
    Preset(&'static str),
    Custom(usize),
}

#[derive(Debug)]
enum Seg {
    /// `**`: any number of levels, none included.
    Deep,
    Pat(String),
}

#[derive(Debug)]
enum Kind {
    // The glob source is read only by `hidden_paths`, for tests.
    Path(Vec<Seg>, #[cfg_attr(not(test), allow(dead_code))] String),
    Name(Regex),
    Value(Regex),
    Detect(super::config::Detector),
}

#[derive(Debug)]
struct NsGlob {
    db: String,
    coll: Option<String>,
}

struct Rule {
    index: usize,
    scope: Vec<NsGlob>,
    kind: Kind,
    action: Action,
    mode: PresetMode,
}

struct KeyItem {
    id: &'static str,
    action: Action,
    mode: PresetMode,
}

struct ValueItem {
    id: &'static str,
    action: Action,
    find: Finder,
    not_under: Option<Regex>,
}

struct GateItem {
    id: &'static str,
    action: Action,
    key: Regex,
    test: fn(&str) -> bool,
}

pub struct Rules {
    custom: Vec<Rule>,
    key_set: RegexSet,
    key_items: Vec<KeyItem>,
    value_set: RegexSet,
    value_items: Vec<ValueItem>,
    gates: Vec<GateItem>,
    key: [u8; 32],
    /// Binary values leave as their size only; on by default in `AgentsConfig`.
    pub binary_as_size: bool,
}

/// The rules that apply to one namespace. Presets apply everywhere; custom rules follow their scope.
pub struct NsRules<'a> {
    rules: &'a Rules,
    custom: Vec<&'a Rule>,
}

fn regex(p: &str) -> Result<Regex, String> {
    regex::RegexBuilder::new(p).size_limit(1 << 20).dfa_size_limit(1 << 20).build().map_err(|e| e.to_string())
}

fn set(p: &[&str]) -> Result<RegexSet, RuleErr> {
    regex::RegexSetBuilder::new(p).size_limit(8 << 20).dfa_size_limit(8 << 20).build().map_err(|e| RuleErr { rule: None, msg: e.to_string() })
}

fn path_glob(p: &str) -> Result<Vec<Seg>, String> {
    if p.trim().is_empty() {
        return Err("the field path is empty".into());
    }
    p.split('.').map(|s| match s {
        "" => Err(format!("`{p}` has an empty level")),
        "**" => Ok(Seg::Deep),
        s => Ok(Seg::Pat(s.into())),
    }).collect()
}

fn ns_glob(g: &str) -> Result<NsGlob, String> {
    let g = g.trim();
    match g.split_once('.') {
        None if !g.is_empty() => Ok(NsGlob { db: g.into(), coll: None }),
        Some((db, coll)) if !db.is_empty() && !coll.is_empty() => Ok(NsGlob { db: db.into(), coll: Some(coll.into()) }),
        _ => Err(format!("`{g}` is not a database or database.collection pattern")),
    }
}

/// `*` matches any run of characters, so `card*` and `*_token` work inside one level.
pub(crate) fn wild(pat: &str, s: &str) -> bool {
    let (p, s) = (pat.as_bytes(), s.as_bytes());
    let (mut pi, mut si, mut star, mut mark) = (0, 0, None, 0);
    while si < s.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            pi += 1;
            mark = si;
        } else if pi < p.len() && p[pi] == s[si] {
            pi += 1;
            si += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

fn ns_hit(g: &NsGlob, db: &str, coll: &str) -> bool {
    wild(&g.db, db) && g.coll.as_deref().is_none_or(|c| wild(c, coll))
}

/// An all-digit level is an array index or a numeric key. Either may be skipped, so `items.ssn` covers `items.0.ssn`,
/// and either may be matched, so `metrics.*.salary` and `metrics.2024.salary` cover a key named 2024.
fn number(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit())
}

fn full(g: &[Seg], p: &[&str]) -> bool {
    match g.split_first() {
        None => p.iter().all(|s| number(s)),
        Some((Seg::Deep, rest)) => (0..=p.len()).any(|i| full(rest, &p[i..])),
        Some((Seg::Pat(s), rest)) => p.split_first().is_some_and(|(h, t)| (wild(s, h) && full(rest, t)) || (number(h) && full(g, t))),
    }
}

/// Whether the glob can match `p` followed by at least one more level, so `p` holds a hidden field below it.
fn below(g: &[Seg], p: &[&str]) -> bool {
    match g.split_first() {
        None => false,
        Some((Seg::Deep, rest)) => p.is_empty() || (0..=p.len()).any(|i| below(rest, &p[i..])),
        Some((Seg::Pat(s), rest)) => match p.split_first() {
            None => true,
            Some((h, t)) => (wild(s, h) && below(rest, t)) || (number(h) && below(g, t)),
        },
    }
}

/// Update positionals (`$`, `$[]`, `$[i]`) name no field and are dropped. Number levels stay, and `full` and
/// `below` let them be skipped or matched (see `number`).
pub(crate) fn levels(path: &str) -> Vec<&str> {
    path.split('.').filter(|s| !s.is_empty() && !s.starts_with('$')).collect()
}

impl Rules {
    /// `over` is the connection's preset override; `global` and `conn` are the user's own rules.
    pub fn compile(presets: &Presets, over: &PresetOverride, global: &[RuleSpec], conn: &[RuleSpec], hash_key: &[u8; 32]) -> Result<Rules, RuleErr> {
        let mut custom = Vec::new();
        for (index, spec) in global.iter().chain(conn).enumerate() {
            if !spec.on {
                continue;
            }
            let err = |msg: String| RuleErr { rule: Some(index), msg };
            let scope = spec.scope.iter().map(|g| ns_glob(g)).collect::<Result<_, _>>().map_err(err)?;
            let kind = match &spec.matcher {
                Matcher::FieldPath(p) => Kind::Path(path_glob(p).map_err(err)?, p.clone()),
                Matcher::FieldName(r) => Kind::Name(regex(r).map_err(err)?),
                Matcher::ValueRegex(r) => {
                    let re = regex(r).map_err(err)?;
                    // A pattern that matches the empty string would redact between every character.
                    if re.is_match("") {
                        return Err(err("the pattern matches empty text".into()));
                    }
                    Kind::Value(re)
                }
                Matcher::Detector(d) => Kind::Detect(*d),
            };
            if let Action::Replace(t) = &spec.action
                && t.len() > 200
            {
                return Err(err("the replacement text is longer than 200 characters".into()));
            }
            custom.push(Rule { index, scope, kind, action: spec.action.clone(), mode: spec.mode });
        }
        let (mut keys, mut key_items, mut hints, mut value_items, mut gates) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let perr = |id: &str, msg: String| RuleErr { rule: None, msg: format!("{id}: {msg}") };
        for (p, on, items) in [(&presets.secrets, over.secrets.resolve(presets.secrets.on), presets::secrets()), (&presets.pii, over.pii.resolve(presets.pii.on), presets::pii())] {
            if !on {
                continue;
            }
            for Item { id, key, gate, value, not_under, .. } in items.into_iter().filter(|i| !p.off.iter().any(|o| o == i.id)) {
                if let Some(k) = key {
                    keys.push(k);
                    key_items.push(KeyItem { id, action: p.action.clone(), mode: p.mode });
                }
                if let Some((k, test)) = gate {
                    gates.push(GateItem { id, action: p.action.clone(), key: regex(k).map_err(|e| perr(id, e))?, test });
                }
                if let Some((hint, find)) = value {
                    hints.push(hint);
                    let not_under = not_under.map(regex).transpose().map_err(|e| perr(id, e))?;
                    value_items.push(ValueItem { id, action: p.action.clone(), find, not_under });
                }
            }
        }
        Ok(Rules { custom, key_set: set(&keys)?, key_items, value_set: set(&hints)?, value_items, gates, key: *hash_key, binary_as_size: true })
    }

    /// The rules for one connection, or for none (the connection's own rules and override left out).
    pub fn from_config(cfg: &AgentsConfig, conn: Option<&ConnPolicy>, hash_key: &[u8; 32]) -> Result<Rules, RuleErr> {
        let over = conn.map(|c| c.presets).unwrap_or_default();
        let mut r = Rules::compile(&cfg.presets, &over, &cfg.rules, conn.map_or(&[][..], |c| c.rules.as_slice()), hash_key)?;
        r.binary_as_size = cfg.binary_as_size;
        Ok(r)
    }

    pub fn for_ns(&self, db: &str, coll: &str) -> NsRules<'_> {
        NsRules { rules: self, custom: self.custom.iter().filter(|r| r.scope.is_empty() || r.scope.iter().any(|g| ns_hit(g, db, coll))).collect() }
    }

    /// Every rule whatever its scope, for output that belongs to no one namespace (free SQL rows, errors of a
    /// statement that may touch several tables).
    pub fn all(&self) -> NsRules<'_> {
        NsRules { rules: self, custom: self.custom.iter().collect() }
    }

    /// Whether free-form text tools (free SQL, commands, the shell) are unsafe here: a Hide-mode rule matches a
    /// field they could reach. `known` lists the fields per namespace: the full column list from the catalog on SQL
    /// engines, the keys of a schema sample on MongoDB. A sample can miss a rare field, which is why the shell is
    /// always Ask. None, or a list that did not load, counts every Hide-mode path and name rule.
    pub fn hides_any(&self, known: Option<&[((String, String), Vec<String>)]>) -> bool {
        match known {
            None => self.all().hides_data(None),
            Some(tables) => tables.iter().any(|((db, t), cols)| self.for_ns(db, t).hides_data(Some(cols))),
        }
    }
}

impl NsRules<'_> {
    fn path_rules(&self) -> impl Iterator<Item = &&Rule> {
        self.custom.iter().filter(|r| matches!(r.kind, Kind::Path(..) | Kind::Name(_)))
    }

    /// The preset key item that applies to a field with this key, if any.
    fn key_item(&self, key: &str) -> Option<&KeyItem> {
        if self.rules.key_items.is_empty() {
            return None;
        }
        self.rules.key_set.matches(&norm_key(key)).iter().next().map(|i| &self.rules.key_items[i])
    }

    fn key_has(&self, key: &str, mode: PresetMode) -> bool {
        !self.rules.key_items.is_empty() && self.rules.key_set.matches(&norm_key(key)).iter().any(|i| self.rules.key_items[i].mode == mode)
    }

    /// What hides or masks fields here, for display: custom path globs and name patterns, then preset key items.
    #[cfg(test)]
    pub fn hidden_paths(&self) -> Vec<String> {
        let custom = self.path_rules().map(|r| match &r.kind {
            Kind::Path(_, src) => src.clone(),
            Kind::Name(re) => format!("name ~ {}", re.as_str()),
            _ => unreachable!(),
        });
        custom.chain(self.rules.key_items.iter().map(|k| presets::label(k.id).unwrap_or(k.id).to_string())).collect()
    }

    /// Whether any field rule applies here, in either mode. Whole-document constructs ($$ROOT, $objectToArray) are
    /// refused then, since their output carries every field under keys no rule knows.
    pub fn field_rules(&self) -> bool {
        self.path_rules().next().is_some() || !self.rules.key_items.is_empty()
    }

    /// Whether a custom field path rule applies here. Joined documents land under the `as` key, where a path rule
    /// no longer matches, so lookups into such a namespace are refused; name rules still match there.
    pub fn path_rules_here(&self) -> bool {
        self.path_rules().any(|r| matches!(r.kind, Kind::Path(..)))
    }

    /// Whether free-form text tools are unsafe here. Only Hide-mode rules count: masked fields leave scrubbed
    /// whatever reads them. With the field list (catalog columns, or sampled keys on MongoDB) only a rule that
    /// matches one of them counts; without one any Hide-mode path or name rule does.
    pub fn hides_data(&self, known: Option<&[String]>) -> bool {
        match known {
            None => self.path_rules().any(|r| r.mode == PresetMode::Hide) || self.rules.key_items.iter().any(|k| k.mode == PresetMode::Hide),
            Some(cols) => cols.iter().any(|c| self.hides(c)),
        }
    }

    fn matches(&self, path: &str, mode: PresetMode) -> bool {
        let p = levels(path);
        p.iter().any(|s| self.key_has(s, mode))
            || self.path_rules().filter(|r| r.mode == mode).any(|r| match &r.kind {
                Kind::Path(g, _) => (1..=p.len()).any(|n| full(g, &p[..n])),
                Kind::Name(re) => p.iter().any(|s| re.is_match(s)),
                _ => false,
            })
    }

    fn holds(&self, path: &str, mode: PresetMode) -> bool {
        let p = levels(path);
        (p.is_empty() && self.rules.key_items.iter().any(|k| k.mode == mode))
            || self.path_rules().filter(|r| r.mode == mode).any(|r| match &r.kind {
                Kind::Path(g, _) => below(g, &p),
                Kind::Name(_) => p.is_empty(),
                _ => false,
            })
    }

    /// Whether the value at `path` is hidden by a Hide-mode rule: the path or one of its ancestors matches.
    pub fn hides(&self, path: &str) -> bool {
        self.matches(path, PresetMode::Hide)
    }

    /// Whether the value at `path` is masked by a MaskOutput rule: queries may use it, output may not copy it.
    pub fn masks(&self, path: &str) -> bool {
        self.matches(path, PresetMode::MaskOutput)
    }

    /// Whether `path` holds a Hide-mode field below it, so comparing or computing on it whole would read the hidden
    /// value too. The root (empty path) holds every one. Name rules and preset keys can apply at any level, which
    /// only the root is known to hold.
    pub fn hides_below(&self, path: &str) -> bool {
        self.holds(path, PresetMode::Hide)
    }

    /// Like `hides_below`, for MaskOutput fields.
    pub fn masks_below(&self, path: &str) -> bool {
        self.holds(path, PresetMode::MaskOutput)
    }

    pub fn scrub_doc(&self, d: &mut Document) {
        self.doc(d, &mut String::new(), &mut Vec::new());
    }

    /// The scrubbed copy of `d` and which preset item or custom rule fired on which field, for the tester in Settings.
    pub fn explain_scrub(&self, d: &Document) -> (Document, Vec<(String, Fired)>) {
        let (mut out, mut hits) = (d.clone(), Vec::new());
        self.doc(&mut out, &mut String::new(), &mut hits);
        (out, hits)
    }

    /// Value rules and detectors only: free text has no field names.
    pub fn scrub_text(&self, s: &str) -> String {
        let mut hits = Vec::new();
        self.text(s.to_string(), "", "", &mut hits).unwrap_or_else(|| "[hidden]".into())
    }

    /// `path` is the path of `d` and comes back unchanged; one buffer serves every field instead of a string each.
    fn doc(&self, d: &mut Document, path: &mut String, hits: &mut Vec<(String, Fired)>) {
        let base = path.len();
        let mut dropped = Vec::new();
        for (k, v) in d.iter_mut() {
            if base > 0 {
                path.push('.');
            }
            path.push_str(k);
            if !self.value(v, path, k, hits) {
                dropped.push(k.clone());
            }
            path.truncate(base);
        }
        for k in dropped {
            d.remove(&k);
        }
    }

    /// The whole-value rule for a field, by its path or key: custom path and name rules first, as they are the
    /// user's more specific choice, then preset keys, then gated preset keys.
    fn field_rule(&self, v: &Bson, path: &str, key: &str) -> Option<(Fired, &Action)> {
        let p = levels(path);
        let custom = self.path_rules().find(|r| match &r.kind {
            Kind::Path(g, _) => full(g, &p),
            Kind::Name(re) => re.is_match(key),
            _ => false,
        });
        if let Some(r) = custom {
            return Some((Fired::Custom(r.index), &r.action));
        }
        if let Some(k) = self.key_item(key) {
            return Some((Fired::Preset(k.id), &k.action));
        }
        let Bson::String(s) = v else { return None };
        if self.rules.gates.is_empty() {
            return None;
        }
        let n = norm_key(key);
        self.rules.gates.iter().find(|g| g.key.is_match(&n) && (g.test)(s)).map(|g| (Fired::Preset(g.id), &g.action))
    }

    /// Scrubs one value in place. False when a Remove rule drops it.
    fn value(&self, v: &mut Bson, path: &mut String, key: &str, hits: &mut Vec<(String, Fired)>) -> bool {
        if let Some((fired, action)) = self.field_rule(v, path, key) {
            hits.push((path.to_string(), fired));
            let nested = matches!(v, Bson::Document(_) | Bson::Array(_));
            let text = as_text(v);
            *v = match action {
                Action::Remove => return false,
                Action::Replace(t) => Bson::String(t.clone()),
                Action::Hash => Bson::String(hash(&self.rules.key, &text)),
                // A masked JSON object shows nothing useful and may show a short value whole.
                Action::Mask(_) | Action::Redact if nested => Bson::String("[hidden]".into()),
                Action::Mask(n) => Bson::String(mask(&text, *n)),
                Action::Redact => Bson::String("[hidden]".into()),
            };
            return true;
        }
        match v {
            Bson::Document(d) => self.doc(d, path, hits),
            Bson::Array(a) => {
                // Elements keep the array's path and key, so `items.sku` and an `emails` key reach them.
                let mut keep = Vec::with_capacity(a.len());
                for mut e in std::mem::take(a) {
                    let ok = match &mut e {
                        Bson::Document(d) => {
                            self.doc(d, path, hits);
                            true
                        }
                        _ => self.value(&mut e, path, key, hits),
                    };
                    if ok {
                        keep.push(e);
                    }
                }
                *a = keep;
            }
            Bson::Binary(b) if self.rules.binary_as_size => *v = Bson::String(format!("<binary {} bytes>", b.bytes.len())),
            _ => return self.leaf(v, path, key, hits),
        }
        true
    }

    /// Value rules on a string, or on a whole number's digits since card and phone numbers are often stored as
    /// numbers.
    fn leaf(&self, v: &mut Bson, path: &str, key: &str, hits: &mut Vec<(String, Fired)>) -> bool {
        let text = match v {
            Bson::String(s) | Bson::Symbol(s) | Bson::JavaScriptCode(s) => std::mem::take(s),
            Bson::Int64(n) => n.to_string(),
            Bson::Int32(n) => n.to_string(),
            Bson::RegularExpression(r) => r.pattern.clone(),
            _ => return true,
        };
        let before = hits.len();
        let out = self.text(text.clone(), path, key, hits);
        match (out, &mut *v) {
            (None, _) => return false,
            (Some(t), Bson::String(s) | Bson::Symbol(s) | Bson::JavaScriptCode(s)) => *s = t,
            (Some(t), _) if hits.len() > before => *v = Bson::String(t),
            _ => {}
        }
        true
    }

    /// Custom value rules, then the preset detectors whose prefilter pattern the text contains. None when a Remove
    /// rule drops the whole field.
    fn text(&self, mut s: String, path: &str, key: &str, hits: &mut Vec<(String, Fired)>) -> Option<String> {
        if s.is_empty() {
            return Some(s);
        }
        for r in &self.custom {
            let spans: Spans = match &r.kind {
                Kind::Value(re) => re.find_iter(&s).filter(|m| !m.is_empty()).map(|m| m.range()).collect(),
                Kind::Detect(d) => detectors::find(*d, &s),
                _ => continue,
            };
            if !spans.is_empty() {
                hits.push((path.to_string(), Fired::Custom(r.index)));
                s = self.apply(&r.action, &s, &spans)?;
            }
        }
        if self.rules.value_items.is_empty() {
            return Some(s);
        }
        let candidates = self.rules.value_set.matches(&s);
        if !candidates.matched_any() {
            return Some(s);
        }
        let mut norm = None;
        for i in candidates.iter() {
            let item = &self.rules.value_items[i];
            if let Some(nu) = &item.not_under
                && !key.is_empty()
                && nu.is_match(norm.get_or_insert_with(|| norm_key(key)))
            {
                continue;
            }
            // PEM search is plain substring search, linear on any size, and must see a block's end however far.
            let spans = if item.id == "secrets.pem" { (item.find)(&s) } else { detectors::windowed(&s, item.find) };
            if !spans.is_empty() {
                hits.push((path.to_string(), Fired::Preset(item.id)));
                s = self.apply(&item.action, &s, &spans)?;
            }
        }
        Some(s)
    }

    /// The text with every span handled by the action. None for Remove, which drops the whole field.
    fn apply(&self, action: &Action, s: &str, spans: &[Range<usize>]) -> Option<String> {
        Some(match action {
            Action::Remove => return None,
            Action::Replace(t) => splice(s, spans, |_| t.clone()),
            Action::Mask(n) => splice(s, spans, |m| mask(m, *n)),
            Action::Hash => splice(s, spans, |m| hash(&self.rules.key, m)),
            Action::Redact => splice(s, spans, |_| "[hidden]".into()),
        })
    }
}

fn splice(s: &str, spans: &[Range<usize>], f: impl Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(s.len());
    let mut at = 0;
    for r in spans {
        if r.start < at {
            continue;
        }
        out.push_str(&s[at..r.start]);
        out.push_str(&f(&s[r.clone()]));
        at = r.end;
    }
    out.push_str(&s[at..]);
    out
}

fn as_text(v: &Bson) -> String {
    match v {
        Bson::String(s) => s.clone(),
        Bson::DateTime(d) => d.try_to_rfc3339_string().unwrap_or_else(|_| d.timestamp_millis().to_string()),
        other => other.clone().into_relaxed_extjson().to_string(),
    }
}

/// Spaces and dashes stay so a masked card still reads as a card. Short values are starred whole: keeping the last
/// four characters of a four-letter name would keep all of it.
pub fn mask(s: &str, keep: u8) -> String {
    let sep = |c: char| c.is_whitespace() || matches!(c, '-' | '.' | '(' | ')' | '/');
    let total = s.chars().filter(|&c| !sep(c)).count();
    let keep = if total > 2 * keep as usize { keep as usize } else { 0 };
    let mut seen = 0;
    s.chars().map(|c| if sep(c) { c } else {
        seen += 1;
        if total - seen < keep { c } else { '*' }
    }).collect()
}

/// Keyed, so a list of hashes cannot be reversed by hashing guesses without the key, and stable, so equal values
/// still match across documents and joins.
pub fn hash(key: &[u8; 32], s: &str) -> String {
    let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC takes any key length");
    m.update(s.as_bytes());
    let out = m.finalize().into_bytes();
    format!("h:{}", super::link::hex(&out[..8]))
}

static ECHO: LazyLock<Regex> = LazyLock::new(|| {
    regex::RegexBuilder::new(r#"(?i)(invalid input syntax for (?:type )?[\w ]+|invalid input value for enum [\w.]+|duplicate entry|incorrect [\w ]+ value|truncated incorrect [\w ]+ value|for value)(\s*:?\s*)(?:"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')"#).size_limit(1 << 20).build().expect("echo regex")
});
static PG_KEY: LazyLock<Regex> = LazyLock::new(|| regex::RegexBuilder::new(r"Key \(([^)]*)\)=\(").build().expect("key regex"));

/// Database errors quote the values that caused them: the duplicate key, the failing row, the text that did not
/// cast. Those parts go first, then the detectors run on what is left.
pub fn scrub_error(raw: &str, rules: &NsRules) -> String {
    let mut s = raw.to_string();
    for key in ["dup key:", "keyValue", "errInfo"] {
        s = cut_after(&s, key, b'{', b'}');
    }
    s = cut_after(&s, "Failing row contains", b'(', b')');
    let mut kept = Vec::new();
    for line in s.lines() {
        // psql writes DETAIL, `db::sql::se_at` writes Detail.
        let t = line.trim_start();
        if t.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("DETAIL:")) {
            kept.push(format!("{}: [hidden]", &t[..6]));
        } else {
            kept.push(line.to_string());
        }
    }
    s = kept.join("\n");
    s = ECHO.replace_all(&s, "${1}${2}[hidden]").into_owned();
    while let Some(m) = PG_KEY.find(&s) {
        let open = m.end() - 1;
        let close = balanced(s.as_bytes(), open, b'(', b')');
        let cols = PG_KEY.captures(&s[m.start()..]).map(|c| c[1].to_string()).unwrap_or_default();
        s = format!("{}Key ({cols})=[hidden]{}", &s[..m.start()], &s[close..]);
    }
    rules.scrub_text(&s)
}

/// Replaces the bracketed payload that follows each `key` with `[hidden]`. An unclosed payload runs to the end.
fn cut_after(s: &str, key: &str, open: u8, close: u8) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find(key) {
        let after = i + key.len();
        let b = rest.as_bytes();
        let mut j = after;
        while j < b.len() && matches!(b[j], b' ' | b'"' | b':' | b'\'') {
            j += 1;
        }
        out.push_str(&rest[..after]);
        if b.get(j) == Some(&open) {
            let end = balanced(b, j, open, close);
            out.push_str(&rest[after..j]);
            out.push(open as char);
            out.push_str(" [hidden] ");
            out.push(close as char);
            rest = &rest[end..];
        } else {
            rest = &rest[after..];
        }
    }
    out.push_str(rest);
    out
}

/// Index just past the bracket closing the one at `at`, skipping quoted text.
fn balanced(b: &[u8], at: usize, open: u8, close: u8) -> usize {
    let (mut depth, mut i, mut quote) = (0i32, at, None::<u8>);
    while i < b.len() {
        let c = b[i];
        match quote {
            Some(_) if c == b'\\' => i += 1,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == b'"' || c == b'\'' => quote = Some(c),
            None if c == open => depth += 1,
            None if c == close => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            None => {}
        }
        i += 1;
    }
    b.len()
}

#[cfg(test)]
pub(crate) mod tests_support {
    use crate::agents::config::{Action, Preset, PresetMode, Presets};

    /// Both presets off, so tests of custom rules see only their own rules.
    pub fn no_presets() -> Presets {
        let p = |action| Preset { on: false, action, mode: PresetMode::Hide, off: Vec::new() };
        Presets { secrets: p(Action::Replace("hidden".into())), pii: p(Action::Mask(4)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::config::{Detector, Tri};
    use mongodb::bson::{Binary, doc, spec::BinarySubtype};

    const KEY: [u8; 32] = [7; 32];

    fn rule(matcher: Matcher, action: Action) -> RuleSpec {
        RuleSpec { on: true, matcher, scope: Vec::new(), action, mode: PresetMode::Hide }
    }

    fn path(p: &str, a: Action) -> RuleSpec {
        rule(Matcher::FieldPath(p.into()), a)
    }

    fn off() -> Presets {
        tests_support::no_presets()
    }

    /// Custom rules alone, presets off, so each test sees only the rule it is about.
    fn one(r: RuleSpec) -> Rules {
        Rules::compile(&off(), &PresetOverride::default(), &[r], &[], &KEY).unwrap()
    }

    fn defaults() -> Rules {
        Rules::compile(&Presets::default(), &PresetOverride::default(), &[], &[], &KEY).unwrap()
    }

    fn scrubbed(rules: &Rules, mut d: Document) -> Document {
        rules.for_ns("shop", "users").scrub_doc(&mut d);
        d
    }

    #[test]
    fn actions() {
        let d = doc! { "a": "4111 1111 1111 1111", "b": 5 };
        assert_eq!(scrubbed(&one(path("a", Action::Remove)), d.clone()), doc! { "b": 5 });
        assert_eq!(scrubbed(&one(path("a", Action::Replace("x".into()))), d.clone()), doc! { "a": "x", "b": 5 });
        assert_eq!(scrubbed(&one(path("a", Action::Mask(4))), d.clone()), doc! { "a": "**** **** **** 1111", "b": 5 });
        assert_eq!(scrubbed(&one(path("a", Action::Redact)), d.clone()), doc! { "a": "[hidden]", "b": 5 });
        let h = scrubbed(&one(path("a", Action::Hash)), d.clone());
        let v = h.get_str("a").unwrap();
        assert!(v.starts_with("h:") && v.len() == 18, "{v}");
        assert_eq!(scrubbed(&one(path("b", Action::Mask(0))), d), doc! { "a": "4111 1111 1111 1111", "b": "*" });
        assert_eq!(scrubbed(&one(path("a", Action::Mask(4))), doc! { "a": { "x": 1 } }), doc! { "a": "[hidden]" });
        assert_eq!(mask("+1 555-123-4567", 2), "** ***-***-**67");
        assert_eq!(mask("Ana", 4), "***");
        assert_eq!(mask("12345678", 4), "********", "half or more kept would be too much");
    }

    #[test]
    fn hashing_is_keyed_and_stable() {
        assert_eq!(hash(&KEY, "ana@x.io"), hash(&KEY, "ana@x.io"));
        assert_ne!(hash(&KEY, "ana@x.io"), hash(&KEY, "bob@x.io"));
        assert_ne!(hash(&KEY, "ana@x.io"), hash(&[8; 32], "ana@x.io"));
        let r = one(rule(Matcher::Detector(Detector::Email), Action::Hash));
        let d = scrubbed(&r, doc! { "m": "to ana@x.io", "n": ["ana@x.io"] });
        assert_eq!(d.get_str("m").unwrap(), format!("to {}", hash(&KEY, "ana@x.io")));
        assert_eq!(d.get_array("n").unwrap()[0].as_str().unwrap(), hash(&KEY, "ana@x.io"));
    }

    #[test]
    fn value_rules_act_on_matches_only() {
        let r = one(rule(Matcher::ValueRegex(r"\d{3}-\d{2}-\d{4}".into()), Action::Redact));
        assert_eq!(scrubbed(&r, doc! { "note": "ssn 123-45-6789 ok" }), doc! { "note": "ssn [hidden] ok" });
        let r = one(rule(Matcher::Detector(Detector::CardNumber), Action::Mask(4)));
        assert_eq!(scrubbed(&r, doc! { "c": 4111111111111111i64 }), doc! { "c": "************1111" });
        let r = one(rule(Matcher::Detector(Detector::PemPrivateKey), Action::Remove));
        assert_eq!(scrubbed(&r, doc! { "k": "-----BEGIN PRIVATE KEY-----\nabc", "x": ["-----BEGIN PRIVATE KEY-----", "ok"] }), doc! { "x": ["ok"] });
    }

    #[test]
    fn path_globs() {
        let r = one(path("customer.*.ssn", Action::Remove));
        assert_eq!(scrubbed(&r, doc! { "customer": { "us": { "ssn": 1, "n": 2 }, "ssn": 3 } }), doc! { "customer": { "us": { "n": 2 }, "ssn": 3 } });
        let r = one(path("**.ssn", Action::Remove));
        assert_eq!(scrubbed(&r, doc! { "ssn": 1, "a": { "b": { "ssn": 2, "c": 3 } } }), doc! { "a": { "b": { "c": 3 } } });
        let r = one(path("card*", Action::Remove));
        assert_eq!(scrubbed(&r, doc! { "card_no": 1, "cardholder": 2, "x": 3 }), doc! { "x": 3 });
        let r = one(path("a.**", Action::Remove));
        assert_eq!(scrubbed(&r, doc! { "a": { "b": 1 }, "c": 2 }), doc! { "c": 2 });
        for rule in ["metrics.2024.salary", "metrics.*.salary", "metrics.salary"] {
            let r = one(path(rule, Action::Remove));
            assert_eq!(scrubbed(&r, doc! { "metrics": { "2024": { "salary": 1, "n": 2 } } }), doc! { "metrics": { "2024": { "n": 2 } } }, "{rule}");
        }
    }

    #[test]
    fn arrays_keep_the_parent_path() {
        let r = one(path("items.secret", Action::Replace("x".into())));
        assert_eq!(scrubbed(&r, doc! { "items": [{ "secret": 1, "n": 1 }, { "n": 2 }, [{ "secret": 2 }]] }), doc! { "items": [{ "secret": "x", "n": 1 }, { "n": 2 }, [{ "secret": "x" }]] });
        let r = one(path("tags", Action::Remove));
        assert_eq!(scrubbed(&r, doc! { "tags": ["a"], "n": 1 }), doc! { "n": 1 });
    }

    #[test]
    fn binary_as_size() {
        let mut rules = one(path("zz", Action::Remove));
        let bin = Bson::Binary(Binary { subtype: BinarySubtype::Generic, bytes: vec![1, 2, 3] });
        assert_eq!(scrubbed(&rules, doc! { "b": bin.clone(), "l": [bin.clone()] }), doc! { "b": "<binary 3 bytes>", "l": ["<binary 3 bytes>"] });
        rules.binary_as_size = false;
        assert_eq!(scrubbed(&rules, doc! { "b": bin.clone() }), doc! { "b": bin });
    }

    #[test]
    fn scopes() {
        let mut r = path("ssn", Action::Remove);
        r.scope = vec!["shop.users".into(), "hr".into(), "log*.audit_*".into()];
        let rules = one(r);
        let hit = |db: &str, c: &str| rules.for_ns(db, c).hides("ssn");
        assert!(hit("shop", "users") && hit("hr", "anything") && hit("logs", "audit_2024"));
        assert!(!hit("shop", "orders") && !hit("shopx", "users") && !hit("logs", "events"));
    }

    #[test]
    fn fails_closed() {
        let bad = [
            rule(Matcher::FieldName("(".into()), Action::Remove),
            rule(Matcher::ValueRegex("a{99999999}".into()), Action::Remove),
            rule(Matcher::ValueRegex("x*".into()), Action::Redact),
            path("", Action::Remove),
            path("a..b", Action::Remove),
            RuleSpec { scope: vec![".x".into()], ..path("a", Action::Remove) },
        ];
        let global = [path("ok", Action::Remove)];
        for b in bad {
            let e = Rules::compile(&Presets::default(), &PresetOverride::default(), &global, &[b.clone()], &KEY).err().unwrap_or_else(|| panic!("{b:?} compiled"));
            assert_eq!(e.rule, Some(1));
        }
        let off_rule = RuleSpec { on: false, ..rule(Matcher::FieldName("(".into()), Action::Remove) };
        assert!(Rules::compile(&off(), &PresetOverride::default(), &[off_rule], &[], &KEY).is_ok());
    }

    #[test]
    fn presets_by_default() {
        let d = scrubbed(&defaults(), doc! {
            "user": { "Password": "p4ss", "name": "Ana", "firstName": "Anabelle", "email": "ana@example.org" },
            "list": [{ "accessToken": "t" }],
            "note": "call +33 6 12 34 56 78 or write ana@example.org",
            "card": 4111111111111111i64,
            "dsn": "postgres://app:s3cret@db:5432/x",
            "version": "1.2.3.4",
            "remote": "10.1.2.3",
            "order": "ORD-1234567812345678",
            "created": "2024-01-02T10:11:12Z",
        });
        assert_eq!(d.get_document("user").unwrap(), &doc! { "Password": "hidden", "name": "Ana", "firstName": "********", "email": "**********e.org" });
        assert_eq!(d.get_array("list").unwrap()[0].as_document().unwrap(), &doc! { "accessToken": "hidden" });
        assert_eq!(d.get_str("note").unwrap(), "call *** * ** ** 56 78 or write **********e.org");
        assert_eq!(d.get_str("card").unwrap(), "************1111", "card is a PII key");
        assert_eq!(d.get_str("dsn").unwrap(), "postgres://app:hidden@db:5432/x");
        assert_eq!(d.get_str("version").unwrap(), "1.2.3.4", "an IP-looking version is left alone");
        assert_eq!(d.get_str("remote").unwrap(), "**.*.*.*");
        assert_eq!(d.get_str("order").unwrap(), "ORD-1234567812345678");
        assert_eq!(d.get_str("created").unwrap(), "2024-01-02T10:11:12Z");
    }

    #[test]
    fn preset_items_turn_off_and_connections_override() {
        let mut p = Presets::default();
        p.pii.off = vec!["pii.names.person".into(), "pii.email".into()];
        let r = Rules::compile(&p, &PresetOverride::default(), &[], &[], &KEY).unwrap();
        assert_eq!(scrubbed(&r, doc! { "firstName": "Anabelle", "note": "ana@example.org" }), doc! { "firstName": "Anabelle", "note": "ana@example.org" });
        let r = Rules::compile(&Presets::default(), &PresetOverride { secrets: Tri::Inherit, pii: Tri::Off }, &[], &[], &KEY).unwrap();
        assert_eq!(scrubbed(&r, doc! { "email": "ana@example.org", "pwd": "x" }), doc! { "email": "ana@example.org", "pwd": "hidden" });
        let r = Rules::compile(&off(), &PresetOverride { secrets: Tri::On, pii: Tri::Inherit }, &[], &[], &KEY).unwrap();
        assert_eq!(scrubbed(&r, doc! { "email": "ana@example.org", "pwd": "x" }), doc! { "email": "ana@example.org", "pwd": "hidden" });
    }

    #[test]
    fn custom_rules_win_over_presets() {
        let r = Rules::compile(&Presets::default(), &PresetOverride::default(), &[path("email", Action::Hash)], &[], &KEY).unwrap();
        let d = scrubbed(&r, doc! { "email": "ana@example.org" });
        assert_eq!(d.get_str("email").unwrap(), hash(&KEY, "ana@example.org"));
    }

    #[test]
    fn random_values_only_under_secret_keys() {
        let d = scrubbed(&defaults(), doc! { "signature": "9f8e7d6c5b4a39281706aBcD", "sku": "9f8e7d6c5b4a39281706aBcD", "signing_key": "pending review" });
        assert_eq!(d, doc! { "signature": "hidden", "sku": "9f8e7d6c5b4a39281706aBcD", "signing_key": "pending review" });
    }

    #[test]
    fn hidden_paths_and_ancestors() {
        let rules = Rules::compile(&off(), &PresetOverride::default(), &[path("customer.ssn", Action::Remove), rule(Matcher::FieldName("^pin$".into()), Action::Remove)], &[], &KEY).unwrap();
        let ns = rules.for_ns("a", "b");
        assert!(ns.hides("customer.ssn") && ns.hides("customer.ssn.x") && ns.hides("customer.0.ssn") && ns.hides("a.pin"));
        assert!(!ns.hides("customer") && !ns.hides("customer.name"));
        assert!(ns.hides_below("customer") && ns.hides_below("") && !ns.hides_below("customer.name") && !ns.hides_below("other"));
        assert_eq!(ns.hidden_paths().len(), 2);
        let d = defaults();
        let ns = d.for_ns("a", "b");
        assert!(ns.hides("user.password") && ns.hides_below("") && !ns.hides("user.name"));
        assert!(!ns.hides("contact.phoneNumber") && ns.masks("contact.phoneNumber") && ns.masks_below("") && !ns.masks("user.password"));
        assert!(ns.field_rules() && !ns.path_rules_here());
        let r = Rules::compile(&off(), &PresetOverride::default(), &[RuleSpec { mode: PresetMode::MaskOutput, ..path("customer.email", Action::Remove) }], &[], &KEY).unwrap();
        let ns = r.for_ns("a", "b");
        assert!(ns.masks("customer.email") && !ns.hides("customer.email") && ns.masks_below("customer") && !ns.hides_below("customer") && ns.path_rules_here());
    }

    #[test]
    fn hides_data_counts_hide_mode_only() {
        let rules = defaults();
        let ns = rules.for_ns("shop", "users");
        assert!(ns.hides_data(None), "no field list: any Hide-mode key item counts");
        assert!(ns.hides_data(Some(&["id".into(), "password".into()])));
        assert!(!ns.hides_data(Some(&["id".into(), "email".into()])), "PII is MaskOutput by default");
        assert!(!ns.hides_data(Some(&["id".into(), "total".into(), "status".into()])));
        assert!(rules.hides_any(Some(&[(("shop".into(), "users".into()), vec!["_id".into(), "profile.apiKey".into()])])), "a sampled MongoDB key");
        assert!(!rules.hides_any(Some(&[(("shop".into(), "users".into()), vec!["_id".into(), "profile.email".into()])])));
        let mut p = Presets::default();
        p.secrets.on = false;
        assert!(!Rules::compile(&p, &PresetOverride::default(), &[], &[], &KEY).unwrap().hides_any(None), "masked fields alone never turn free text off");
        let t = |cols: &[&str]| (("shop".to_string(), "orders".to_string()), cols.iter().map(|c| c.to_string()).collect::<Vec<_>>());
        assert!(!rules.hides_any(Some(&[t(&["id", "total"])])));
        assert!(rules.hides_any(Some(&[t(&["id"]), t(&["api_key"])])));
        assert!(rules.hides_any(None));
        let mut p = Presets::default();
        p.secrets.off = presets::secrets().iter().filter(|i| i.key.is_some()).map(|i| i.id.to_string()).collect();
        p.pii.on = false;
        let values_only = Rules::compile(&p, &PresetOverride::default(), &[], &[], &KEY).unwrap();
        assert!(!values_only.hides_any(None), "value detectors hide no field");
    }

    #[test]
    fn explain_names_the_item_or_rule() {
        let rules = Rules::compile(&Presets::default(), &PresetOverride::default(), &[path("a.b", Action::Remove)], &[], &KEY).unwrap();
        let (_, hits) = rules.for_ns("x", "y").explain_scrub(&doc! { "a": { "b": 1 }, "pwd": "x", "c": "4111 1111 1111 1111" });
        assert_eq!(hits, vec![("a.b".to_string(), Fired::Custom(0)), ("pwd".to_string(), Fired::Preset("secrets.names.password")), ("c".to_string(), Fired::Preset("pii.card"))]);
    }

    #[test]
    fn errors_lose_their_values() {
        let rules = defaults();
        let ns = rules.all();
        let e = scrub_error(r#"E11000 duplicate key error collection: shop.users index: email_1 dup key: { email: "ana@x.io" }"#, &ns);
        assert_eq!(e, "E11000 duplicate key error collection: shop.users index: email_1 dup key: { [hidden] }");
        let e = scrub_error(r#"Write error {"keyValue": {"ssn": "123-45-6789"}, "errInfo": {"details": {"x": "}"}}, "code": 11000}"#, &ns);
        assert!(!e.contains("123") && !e.contains("details") && e.contains("11000"), "{e}");
        let e = scrub_error("ERROR: new row violates check constraint\nDETAIL: Failing row contains (1, ana@x.io, 4111).", &ns);
        assert_eq!(e, "ERROR: new row violates check constraint\nDETAIL: [hidden]");
        let e = scrub_error("ERROR: new row for relation \"t\" violates check constraint: Failing row contains (1, (a), x).", &ns);
        assert_eq!(e, "ERROR: new row for relation \"t\" violates check constraint: Failing row contains ( [hidden] ).");
        let e = scrub_error(r#"invalid input syntax for type integer: "s3cret""#, &ns);
        assert_eq!(e, "invalid input syntax for type integer: [hidden]");
        let e = scrub_error("Duplicate entry 'ana@x.io' for key 'users.email'", &ns);
        assert_eq!(e, "Duplicate entry [hidden] for key 'users.email'");
        let e = scrub_error("Incorrect integer value: 'abc' for column 'n' at row 1", &ns);
        assert_eq!(e, "Incorrect integer value: [hidden] for column 'n' at row 1");
        let e = scrub_error("duplicate key value violates unique constraint \"u\" Key (email)=(ana@x.io) already exists.", &ns);
        assert_eq!(e, "duplicate key value violates unique constraint \"u\" Key (email)=[hidden] already exists.");
        assert_eq!(scrub_error("connect to mongodb://app:pw123@db failed for ana@x.io", &ns), "connect to mongodb://app:hidden@db failed for *****.**");
    }

    /// Run on winbox with `cargo test --release agents::scrub::tests::throughput -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn throughput() {
        let rules = defaults();
        let ns = rules.for_ns("shop", "users");
        let docs: Vec<Document> = (0..10_000)
            .map(|i| doc! {
                "_id": i, "firstName": "Anabelle", "lastName": format!("Doe{i}"), "email": format!("user{i}@example.org"),
                "phone": "+33 6 12 34 56 78", "password": "x", "status": "active", "total": i as f64 * 1.5, "version": "1.2.3.4",
                "address": { "street": "12 rue de la Paix", "city": "Paris", "zip": "75002" },
                "tags": ["alpha", "beta", "gamma", "delta"],
                "items": [{ "sku": format!("SKU-{i:08}"), "qty": 2, "price": 9.99 }, { "sku": "SKU-00000002", "qty": 1, "price": 19.5 }],
                "note": "Customer called about order ORD-20240102-0001 and asked for a refund to card 4111 1111 1111 1111; reach them at ana@example.org or +44 20 7946 0958. Shipped from warehouse 10.0.0.12 on 2024-01-02T10:11:12Z. Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.",
                "meta": { "source": "web", "ua": "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/605.1.15", "uuid": "550e8400-e29b-41d4-a716-446655440000" },
            })
            .collect();
        let bytes: usize = docs.iter().map(|d| mongodb::bson::to_vec(d).map_or(0, |v| v.len())).sum();
        let start = std::time::Instant::now();
        for d in &docs {
            let mut d = d.clone();
            ns.scrub_doc(&mut d);
        }
        let secs = start.elapsed().as_secs_f64();
        println!("scrubbed 10000 docs of {} bytes on average in {secs:.3} s: {:.0} docs/s", bytes / docs.len(), 10_000.0 / secs);
    }
}
