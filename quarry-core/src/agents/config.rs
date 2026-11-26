//! What the user decided agents may do, saved signed in `agents/agents.json` rather than on each saved connection:
//! connections.json is rewritten in many places and cannot be signed on its own. Plain data shared by the policy,
//! the scrubber and the Settings pages.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentsConfig {
    /// Off until the user turns it on. The listener runs while Quarry does, so an agent can tell "off" from "closed"; while off it answers nothing else.
    pub enabled: bool,
    /// Keyed by `ConnMeta::id`. A connection missing here is Not shared.
    pub conns: BTreeMap<u64, ConnPolicy>,
    /// What a connection gets when it is first shared.
    pub defaults: ConnPolicy,
    /// The shipped Secrets and PII rule sets.
    pub presets: Presets,
    /// The user's own rules for every connection, applied in order before a connection's own.
    pub rules: Vec<RuleSpec>,
    pub binary_as_size: bool,
    pub limits: Limits,
    pub audit: AuditPrefs,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self { enabled: false, conns: BTreeMap::new(), defaults: ConnPolicy::default(), presets: Presets::default(), rules: Vec::new(), binary_as_size: true, limits: Limits::default(), audit: AuditPrefs::default() }
    }
}

impl AgentsConfig {
    pub fn conn(&self, id: u64) -> Option<&ConnPolicy> {
        self.conns.get(&id).filter(|p| p.access != Access::NotShared)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Access {
    #[default]
    NotShared,
    Read,
    ReadWrite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Never,
    Ask,
    Allow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OpClass {
    ReadData,
    ChangeData,
    ChangeStructure,
    RunScripts,
    ServerUsers,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnPolicy {
    pub access: Access,
    pub read: Mode,
    pub change: Mode,
    pub structure: Mode,
    pub scripts: Mode,
    /// A call that would touch more documents or rows than this asks, even where changes are allowed.
    pub large_change: u64,
    pub scope: Scope,
    /// Turns the Secrets or PII preset on or off for this connection only.
    pub presets: PresetOverride,
    /// Applied after the global rules.
    pub rules: Vec<RuleSpec>,
}

impl Default for ConnPolicy {
    fn default() -> Self {
        Self { access: Access::NotShared, read: Mode::Allow, change: Mode::Ask, structure: Mode::Never, scripts: Mode::Never, large_change: 100, scope: Scope::default(), presets: PresetOverride::default(), rules: Vec::new() }
    }
}

impl ConnPolicy {
    /// Server and users has no tools yet, so it is never allowed whatever was saved.
    pub fn mode(&self, c: OpClass) -> Mode {
        match c {
            OpClass::ReadData => self.read,
            OpClass::ChangeData => self.change,
            OpClass::ChangeStructure => self.structure,
            OpClass::RunScripts => self.scripts,
            OpClass::ServerUsers => Mode::Never,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScopeMode {
    #[default]
    All,
    Only,
    Except,
}

/// Which databases and collections (or tables) agents see, as `db.coll` globs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Scope {
    pub mode: ScopeMode,
    pub globs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RuleSpec {
    pub on: bool,
    pub matcher: Matcher,
    /// `db.coll` globs the rule applies to; empty means everywhere.
    #[serde(default)]
    pub scope: Vec<String>,
    pub action: Action,
    /// Only for field path and field name rules; value rules always just scrub.
    #[serde(default)]
    pub mode: PresetMode,
}

/// What a field rule stops besides changing the value in output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PresetMode {
    /// Agents can neither query nor change the field, nor see it in output: secrets, which a query can probe.
    #[default]
    Hide,
    /// Agents can filter, sort, join on and write the field, but every value they get back is scrubbed, and it
    /// cannot be copied under another key. For personal data, which agents legitimately look up by.
    MaskOutput,
}

/// A shipped rule set. Its items are fixed so an update can improve them; the user can only turn single items off.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub on: bool,
    pub action: Action,
    /// Missing in files written before modes existed, which then get the stricter Hide.
    #[serde(default)]
    pub mode: PresetMode,
    /// Ids of the items the user turned off.
    #[serde(default)]
    pub off: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Presets {
    pub secrets: Preset,
    pub pii: Preset,
}

/// Both on by default: sending personal data to a model provider is the risk agents bring, so privacy comes first.
/// Personal data stays queryable, since looking a customer up by email is the job, but leaves masked.
impl Default for Presets {
    fn default() -> Self {
        Self {
            secrets: Preset { on: true, action: Action::Replace("hidden".into()), mode: PresetMode::Hide, off: Vec::new() },
            pii: Preset { on: true, action: Action::Mask(4), mode: PresetMode::MaskOutput, off: Vec::new() },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tri {
    #[default]
    Inherit,
    On,
    Off,
}

impl Tri {
    pub fn resolve(self, global: bool) -> bool {
        match self {
            Tri::Inherit => global,
            Tri::On => true,
            Tri::Off => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PresetOverride {
    pub secrets: Tri,
    pub pii: Tri,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Matcher {
    /// Dotted path with `*` for one level and `**` for any depth, e.g. `customers.**.ssn`.
    FieldPath(String),
    /// Regex on the last key of a path.
    FieldName(String),
    /// Regex on string values anywhere.
    ValueRegex(String),
    Detector(Detector),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Detector {
    CardNumber,
    Iban,
    Email,
    Phone,
    Ssn,
    Jwt,
    PemPrivateKey,
    CloudKey,
    Ip,
    /// French social security number (NIR) with its key.
    Nir,
    /// UK National Insurance number.
    Nino,
    /// `Bearer ...` and `Authorization: ...` tokens in text.
    BearerToken,
    /// The password of a `scheme://user:pass@host` URL or connection string.
    UrlPassword,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Action {
    Remove,
    Replace(String),
    /// Keep the last N characters, star the rest.
    Mask(u8),
    /// Keyed HMAC, stable so equal values still match across documents.
    Hash,
    /// Replace only the matched part of a string.
    Redact,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub rows: u32,
    pub bytes: u32,
    pub secs: u32,
    pub undo_docs: u32,
    pub undo_call_bytes: u64,
    pub undo_total_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { rows: 200, bytes: 256 << 10, secs: 30, undo_docs: 1000, undo_call_bytes: 16 << 20, undo_total_bytes: 64 << 20 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AuditPrefs {
    pub on: bool,
    pub max_bytes: u64,
}

impl Default for AuditPrefs {
    fn default() -> Self {
        Self { on: true, max_bytes: 50 << 20 }
    }
}
