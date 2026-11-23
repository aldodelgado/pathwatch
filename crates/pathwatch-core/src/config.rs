//! Configuration: parsing, layering, validation, and derivation of the stream
//! plan. See `planning/01-design-spec.md` §12.
//!
//! Layers, lowest to highest: the built-in configuration (the example file
//! compiled into the binary), the user's file, then explicit overrides from
//! the command line. Tables merge key by key; arrays replace wholesale.

use std::collections::BTreeMap;
use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::model::{Family, Protocol};

/// The built-in configuration and the text printed by `config example`.
pub const EXAMPLE_CONFIG: &str = include_str!("../../../planning/examples/pathwatch.example.toml");

pub const REDACTED: &str = "‹redacted:nextdns-profile›";

// ───────────────────────────── durations ─────────────────────────────

/// A duration written as `"250ms"`, `"1s"`, `"5m"`, `"24h"`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Dur(pub Duration);

impl Dur {
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let split = s.find(|c: char| !c.is_ascii_digit()).ok_or_else(|| format!("duration {s:?} needs a unit (ms, s, m, h)"))?;
        let (num, unit) = s.split_at(split);
        let n: u64 = num.parse().map_err(|_| format!("duration {s:?} must start with a whole number"))?;
        let ms = match unit {
            "ms" => n,
            "s" => n * 1_000,
            "m" => n * 60_000,
            "h" => n * 3_600_000,
            _ => return Err(format!("duration {s:?} has an unknown unit {unit:?} (use ms, s, m, h)")),
        };
        Ok(Dur(Duration::from_millis(ms)))
    }
}

impl fmt::Display for Dur {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = self.0.as_millis() as u64;
        if ms != 0 && ms.is_multiple_of(3_600_000) {
            write!(f, "{}h", ms / 3_600_000)
        } else if ms != 0 && ms.is_multiple_of(60_000) {
            write!(f, "{}m", ms / 60_000)
        } else if ms != 0 && ms.is_multiple_of(1_000) {
            write!(f, "{}s", ms / 1_000)
        } else {
            write!(f, "{ms}ms")
        }
    }
}

impl fmt::Debug for Dur {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl Serialize for Dur {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Dur {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Dur::parse(&s).map_err(serde::de::Error::custom)
    }
}

// ───────────────────────────── schema ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub config_version: u32,
    pub general: General,
    pub source: SourceCfg,
    pub limits: Limits,
    pub icmp: IcmpCfg,
    pub dns: DnsCfg,
    pub bootstrap: Bootstrap,
    pub providers: BTreeMap<String, Provider>,
    pub controls: Controls,
    pub trace: TraceCfg,
    pub history: HistoryCfg,
    pub incidents: Incidents,
    pub ui: UiCfg,
    pub recording: Recording,
    pub redaction: Redaction,
    pub logging: Logging,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct General {
    pub families: Vec<String>,
    pub default_mode: String,
    pub default_family: String,
    pub default_window: Dur,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCfg {
    pub interface: String,
    pub address_v4: String,
    pub address_v6: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub max_transactions_per_second: u32,
    pub burst: u32,
    pub max_in_flight: u32,
    pub max_concurrent_dns: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IcmpCfg {
    pub enabled: bool,
    pub interval: Dur,
    pub deadline: Dur,
    pub payload_bytes: u16,
    pub late_window: Dur,
    pub backend: String,
    pub retries: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DnsCfg {
    pub interval: Dur,
    pub deadline: Dur,
    pub retries: u8,
    pub edns_udp_size: u16,
    pub recursion_desired: bool,
    pub tcp_fallback: bool,
    pub query: Vec<QueryCfg>,
    pub udp: DnsUdpCfg,
    pub tcp: DnsTcpCfg,
    pub dot: DnsEncCfg,
    pub doh: DnsDohCfg,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryCfg {
    pub name: String,
    #[serde(rename = "type")]
    pub qtype: String,
    pub expect: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DnsUdpCfg {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DnsTcpCfg {
    pub enabled: bool,
    pub interval: Dur,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DnsEncCfg {
    pub enabled: bool,
    pub fresh_interval: Dur,
    pub fresh_handshake: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DnsDohCfg {
    pub enabled: bool,
    pub method: String,
    pub fresh_interval: Dur,
    pub fresh_handshake: String,
    pub proxy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bootstrap {
    pub resolver: String,
    pub servers: Vec<String>,
    pub refresh: Dur,
    pub on_failure: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub name: String,
    pub slot: u8,
    pub enabled: bool,
    pub color: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id_env: Option<String>,
    #[serde(default)]
    pub targets: Vec<Target>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub id: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub protocols: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow: Option<bool>,
    /// Static bootstrap: dial these addresses instead of resolving the
    /// hostname. The hostname remains the TLS server name and is still what
    /// the certificate is verified against. One address per family is used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Controls {
    pub gateway: Control,
    pub local_dns: Control,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Control {
    pub enabled: bool,
    pub address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceCfg {
    pub protocol: String,
    pub max_hops: u8,
    pub rounds: u32,
    pub round_interval: Dur,
    pub max_probes: u32,
    pub max_rate: u32,
    pub reverse_dns: bool,
    pub stop_on_leave: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryCfg {
    pub samples: Dur,
    pub records: Dur,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Incidents {
    pub cooldown: Dur,
    pub no_reply_run: NoReplyRun,
    pub elevated_no_reply: ElevatedNoReply,
    pub latency_shift: LatencyShift,
    pub dns: DnsIncidents,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoReplyRun {
    pub open_after_consecutive: u32,
    pub close_after_consecutive: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElevatedNoReply {
    pub window: Dur,
    pub open_at: f64,
    pub close_at: f64,
    pub close_hold: Dur,
    pub min_finalised: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LatencyShift {
    pub recent: Dur,
    pub baseline: Dur,
    pub open_ratio: f64,
    pub open_min_delta: Dur,
    pub close_ratio: f64,
    pub min_recent: u32,
    pub min_baseline: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DnsIncidents {
    pub open_after_consecutive_no_response: u32,
    pub close_after_consecutive_success: u32,
    pub error_responses_in_last_10: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiCfg {
    pub color: String,
    pub glyphs: String,
    pub theme: String,
    pub max_fps: u32,
    pub idle_fps: u32,
    pub reduced_motion: bool,
    pub mouse: bool,
    pub mask_secrets: bool,
    pub keys: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recording {
    pub enabled: bool,
    pub directory: String,
    pub rotate_size: String,
    pub rotate_age: Dur,
    pub max_total: String,
    pub min_free_disk: String,
    pub fsync: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Redaction {
    pub profile_ids: bool,
    pub local_addresses: bool,
    pub hostnames: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Logging {
    pub level: String,
    pub file: String,
    pub max_size: String,
    pub max_files: u32,
}

// ───────────────────────────── loading ─────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub key: String,
    pub message: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.key, self.message)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{origin}: {message}")]
    Parse { origin: String, message: String },
    #[error("configuration is invalid ({} problem(s))", .0.len())]
    Invalid(Vec<Problem>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Default,
    File,
    Flag,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Default => "default",
            Origin::File => "file",
            Origin::Flag => "flag",
        }
    }
}

/// A validated configuration together with where each setting came from.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    pub path: Option<PathBuf>,
    pub warnings: Vec<String>,
    effective: toml::Table,
    file: toml::Table,
    flags: Vec<String>,
}

/// Reads the environment. Injected so tests never depend on the real one.
pub type EnvFn<'a> = &'a dyn Fn(&str) -> Option<String>;

pub fn real_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

impl Loaded {
    /// Loads and validates. `file_text` is the user's file, if any; `overrides`
    /// are `(dotted.key, value)` pairs from the command line.
    pub fn from_layers(file_text: Option<&str>, path: Option<&Path>, overrides: &[(String, toml::Value)]) -> Result<Self, ConfigError> {
        let mut effective: toml::Table =
            EXAMPLE_CONFIG.parse().map_err(|e: toml::de::Error| ConfigError::Parse { origin: "built-in configuration".into(), message: e.to_string() })?;
        let origin = path.map_or_else(|| "configuration".to_string(), |p| p.display().to_string());
        let file: toml::Table = match file_text {
            Some(text) => text.parse().map_err(|e: toml::de::Error| ConfigError::Parse { origin: origin.clone(), message: e.to_string() })?,
            None => toml::Table::new(),
        };
        merge(&mut effective, &file);
        let mut flags = Vec::new();
        for (key, value) in overrides {
            set_path(&mut effective, key, value.clone());
            flags.push(key.clone());
        }
        let config: Config = toml::Value::Table(effective.clone()).try_into().map_err(|e: toml::de::Error| ConfigError::Parse { origin, message: e.message().to_string() })?;
        let problems = validate(&config);
        if !problems.is_empty() {
            return Err(ConfigError::Invalid(problems));
        }
        let mut warnings = Vec::new();
        // A setting that changes nothing is accepted, and not in silence.
        if let Ok(defaults) = EXAMPLE_CONFIG.parse::<toml::Table>() {
            for key in NO_EFFECT {
                if let Some(set) = value_at(&file, key)
                    && value_at(&defaults, key) != Some(set)
                {
                    warnings.push(format!("{key} is set and has no effect in this build; see `pathwatch config reference`"));
                }
            }
            if let Some(toml::Value::Table(providers)) = file.get("providers") {
                for (name, p) in providers {
                    let key = format!("providers.{name}.color");
                    if let Some(set) = p.get("color")
                        && value_at(&defaults, &key) != Some(set)
                    {
                        warnings.push(format!("{key} is set and has no effect in this build: a card's colour goes by its slot"));
                    }
                }
            }
        }
        if let Some(text) = file_text
            && contains_literal_profile(text)
            && let Some(p) = path
            && readable_by_others(p)
        {
            warnings.push(format!("{} contains a NextDNS profile id and is readable by other users; restrict it (chmod 600) or use profile_id_env", p.display()));
        }
        Ok(Self { config, path: path.map(Path::to_path_buf), warnings, effective, file, flags })
    }

    pub fn load(path: Option<&Path>, overrides: &[(String, toml::Value)]) -> Result<Self, ConfigError> {
        match path {
            Some(p) => {
                let text = std::fs::read_to_string(p).map_err(|source| ConfigError::Io { path: p.to_path_buf(), source })?;
                Self::from_layers(Some(&text), Some(p), overrides)
            }
            None => Self::from_layers(None, None, overrides),
        }
    }

    pub fn origin_of(&self, dotted: &str) -> Origin {
        if self.flags.iter().any(|f| f == dotted) {
            Origin::Flag
        } else if has_path(&self.file, dotted) {
            Origin::File
        } else {
            Origin::Default
        }
    }

    /// Every effective scalar setting as `(dotted.key, value, origin)`, with
    /// secrets masked unless `show_secrets`.
    pub fn effective_settings(&self, show_secrets: bool) -> Vec<(String, String, Origin)> {
        let mut out = Vec::new();
        flatten(&self.effective, String::new(), &mut |key, value| {
            let shown = match value {
                toml::Value::String(s) if !show_secrets => toml::Value::String(redact(s)).to_string(),
                other => other.to_string(),
            };
            // Array-of-table members inherit the origin of their array.
            let origin_key = key.split('[').next().unwrap_or(key);
            out.push((key.to_string(), shown, self.origin_of(origin_key)));
        });
        out
    }

    /// The effective configuration as TOML.
    pub fn effective_toml(&self, show_secrets: bool) -> String {
        let mut table = self.effective.clone();
        if !show_secrets {
            redact_table(&mut table);
        }
        toml::to_string(&table).expect("a parsed table serialises")
    }

    pub fn plan(&self, env: EnvFn<'_>) -> Plan {
        Plan::derive(&self.config, env)
    }
}

fn merge(base: &mut toml::Table, over: &toml::Table) {
    for (k, v) in over {
        match (base.get_mut(k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            _ => {
                base.insert(k.clone(), v.clone());
            }
        }
    }
}

fn set_path(table: &mut toml::Table, dotted: &str, value: toml::Value) {
    let mut parts = dotted.split('.').peekable();
    let mut cur = table;
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            cur.insert(part.to_string(), value);
            return;
        }
        cur = match cur.entry(part.to_string()).or_insert_with(|| toml::Value::Table(toml::Table::new())) {
            toml::Value::Table(t) => t,
            other => {
                *other = toml::Value::Table(toml::Table::new());
                other.as_table_mut().unwrap()
            }
        };
    }
}

/// Settings that are read and checked and then change nothing in this
/// build. Listed in one place so that the reference says so of each, and so
/// that setting one is answered with a warning instead of silence.
pub const NO_EFFECT: [&str; 13] = [
    "general.default_mode",
    "general.default_family",
    "general.default_window",
    "limits.max_concurrent_dns",
    "bootstrap.servers",
    "ui.max_fps",
    "ui.idle_fps",
    "ui.mask_secrets",
    "ui.keys",
    "logging.level",
    "logging.file",
    "logging.max_size",
    "logging.max_files",
];

/// Whether a reload applies a change to this setting while running.
pub fn reloadable(key: &str) -> bool {
    RELOADABLE.iter().any(|r| key == *r || key.strip_prefix(r).is_some_and(|rest| rest.starts_with('.')))
}

fn value_at<'a>(table: &'a toml::Table, dotted: &str) -> Option<&'a toml::Value> {
    let (mut cur, mut parts) = (table, dotted.split('.').peekable());
    while let Some(part) = parts.next() {
        match cur.get(part)? {
            v if parts.peek().is_none() => return Some(v),
            toml::Value::Table(t) => cur = t,
            _ => return None,
        }
    }
    None
}

fn has_path(table: &toml::Table, dotted: &str) -> bool {
    let mut cur = table;
    let mut parts = dotted.split('.').peekable();
    while let Some(part) = parts.next() {
        match cur.get(part) {
            None => return false,
            Some(_) if parts.peek().is_none() => return true,
            Some(toml::Value::Table(t)) => cur = t,
            Some(_) => return false,
        }
    }
    false
}

fn flatten(table: &toml::Table, prefix: String, f: &mut dyn FnMut(&str, &toml::Value)) {
    for (k, v) in table {
        let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        match v {
            toml::Value::Table(t) => flatten(t, key, f),
            toml::Value::Array(items) if items.iter().all(toml::Value::is_table) && !items.is_empty() => {
                for (i, item) in items.iter().enumerate() {
                    flatten(item.as_table().unwrap(), format!("{key}[{i}]"), f);
                }
            }
            other => f(&key, other),
        }
    }
}

fn redact_table(table: &mut toml::Table) {
    for (_, v) in table.iter_mut() {
        match v {
            toml::Value::String(s) => *s = redact(s),
            toml::Value::Table(t) => redact_table(t),
            toml::Value::Array(items) => {
                for item in items {
                    match item {
                        toml::Value::Table(t) => redact_table(t),
                        toml::Value::String(s) => *s = redact(s),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// Settings a reload applies while running: everything that decides which
/// streams exist and how often they probe. A change anywhere else needs a
/// restart, and a reload that contains one is refused whole rather than
/// applied in part.
const RELOADABLE: [&str; 12] =
    ["providers", "controls", "general.families", "icmp.enabled", "icmp.interval", "icmp.deadline", "dns.interval", "dns.deadline", "dns.udp", "dns.tcp", "dns.doh", "dns.dot"];

/// The settings that differ between two configurations and cannot be
/// applied by a reload, as dotted keys. Empty when a reload can apply
/// everything that changed.
pub fn restart_only_changes(old: &Config, new: &Config) -> Vec<String> {
    fn walk(path: &str, a: &serde_json::Value, b: &serde_json::Value, out: &mut Vec<String>) {
        if RELOADABLE.contains(&path) || a == b {
            return;
        }
        match (a, b) {
            (serde_json::Value::Object(x), serde_json::Value::Object(y)) => {
                let keys: std::collections::BTreeSet<&String> = x.keys().chain(y.keys()).collect();
                for k in keys {
                    let child = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                    walk(&child, x.get(k).unwrap_or(&serde_json::Value::Null), y.get(k).unwrap_or(&serde_json::Value::Null), out);
                }
            }
            _ => out.push(path.to_string()),
        }
    }
    let mut out = Vec::new();
    let value = |c: &Config| serde_json::to_value(c).unwrap_or(serde_json::Value::Null);
    walk("", &value(old), &value(new), &mut out);
    out
}

/// Masks a NextDNS profile id wherever it appears in an endpoint string:
/// the path of `https://dns.nextdns.io/<id>` and the first label of
/// `<id>.dns.nextdns.io`. The `{profile_id}` template is left as written.
pub fn redact(s: &str) -> String {
    const HOST: &str = "dns.nextdns.io";
    if let Some(pos) = s.find(&format!("{HOST}/")) {
        let start = pos + HOST.len() + 1;
        let rest = &s[start..];
        if !rest.is_empty() && !rest.starts_with('{') {
            let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            return format!("{}{REDACTED}{}", &s[..start], &rest[end..]);
        }
    }
    if let Some(pos) = s.find(&format!(".{HOST}")) {
        let label_start = s[..pos].rfind(['/', '@', ' ']).map_or(0, |i| i + 1);
        let label = &s[label_start..pos];
        if !label.is_empty() && !label.starts_with('{') {
            return format!("{}{REDACTED}{}", &s[..label_start], &s[pos..]);
        }
    }
    s.to_string()
}

fn contains_literal_profile(text: &str) -> bool {
    text.lines().filter(|l| !l.trim_start().starts_with('#')).any(|l| redact(l) != l)
}

fn readable_by_others(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o077 != 0)
}

// ───────────────────────────── validation ─────────────────────────────

fn is_placeholder(s: &str) -> bool {
    s.contains('<') && s.contains('>')
}

const EXPECTATIONS: [&str; 5] = ["noerror", "answer", "nxdomain", "blocked", "any"];
pub const QUERY_TYPES: [&str; 8] = ["A", "AAAA", "NS", "TXT", "MX", "CNAME", "SOA", "PTR"];

/// Checks everything that can be checked without touching the network.
/// Reports every problem, not just the first.
pub fn validate(c: &Config) -> Vec<Problem> {
    let mut p = Vec::new();
    let mut bad = |key: &str, message: String| p.push(Problem { key: key.to_string(), message });

    if c.config_version != 1 {
        bad("config_version", format!("unsupported version {} (this build reads version 1)", c.config_version));
    }
    if c.general.families.is_empty() {
        bad("general.families", "list at least one of \"ipv4\", \"ipv6\"".into());
    }
    for f in &c.general.families {
        if f != "ipv4" && f != "ipv6" {
            bad("general.families", format!("unknown family {f:?} (use \"ipv4\" or \"ipv6\")"));
        }
    }
    if !["icmp", "dns", "encrypted"].contains(&c.general.default_mode.as_str()) {
        bad("general.default_mode", format!("unknown mode {:?} (use icmp, dns, or encrypted)", c.general.default_mode));
    }
    if !["ipv4", "ipv6"].contains(&c.general.default_family.as_str()) {
        bad("general.default_family", format!("unknown family {:?}", c.general.default_family));
    }
    for (key, value) in [("source.interface", &c.source.interface), ("source.address_v4", &c.source.address_v4), ("source.address_v6", &c.source.address_v6)] {
        if !value.is_empty() {
            bad(key, "source binding is not implemented in this build (planned for milestone 3); leave it empty".into());
        }
    }

    if c.limits.max_transactions_per_second == 0 || c.limits.burst == 0 || c.limits.max_in_flight == 0 {
        bad("limits", "limits must be positive".into());
    }
    if c.icmp.interval.0 < Duration::from_millis(200) {
        bad("icmp.interval", format!("{} is below the 200ms minimum", c.icmp.interval));
    }
    if !(250..=10_000).contains(&(c.icmp.deadline.0.as_millis() as u64)) {
        bad("icmp.deadline", format!("{} is outside 250ms–10s", c.icmp.deadline));
    }
    for (key, value) in
        [("recording.rotate_size", &c.recording.rotate_size), ("recording.max_total", &c.recording.max_total), ("recording.min_free_disk", &c.recording.min_free_disk)]
    {
        if let Err(e) = crate::recorder::parse_size(value) {
            bad(key, e);
        }
    }
    if c.trace.protocol != "icmp" {
        bad("trace.protocol", format!("{:?} is not built; route tracing uses \"icmp\"", c.trace.protocol));
    }
    if !c.trace.stop_on_leave {
        bad("trace.stop_on_leave", "a trace always stops when its view is left; keeping it running is not built".into());
    }
    if c.trace.max_hops == 0 || c.trace.rounds == 0 || c.trace.max_probes == 0 || c.trace.max_rate == 0 {
        bad("trace", "max_hops, rounds, max_probes and max_rate must all be at least 1".into());
    }
    if c.redaction.local_addresses {
        bad("redaction.local_addresses", "not built as a setting; use `pathwatch export --redact-local` when sharing a recording".into());
    }
    if c.redaction.hostnames {
        bad("redaction.hostnames", "hostname redaction is not built".into());
    }
    if !["rotate", "never"].contains(&c.recording.fsync.as_str()) {
        bad("recording.fsync", format!("{:?} is not built; use \"rotate\" or \"never\"", c.recording.fsync));
    }
    if !(16..=1400).contains(&c.icmp.payload_bytes) {
        bad("icmp.payload_bytes", format!("{} is outside 16–1400", c.icmp.payload_bytes));
    }
    if !["auto", "datagram"].contains(&c.icmp.backend.as_str()) {
        let why = if c.icmp.backend == "raw" { "the raw backend is not implemented in this build" } else { "use auto or datagram" };
        bad("icmp.backend", format!("unsupported backend {:?}: {why}", c.icmp.backend));
    }
    if c.icmp.retries != 0 {
        bad("icmp.retries", "ICMP retries are not implemented; the only supported value is 0".into());
    }

    if c.dns.deadline.0 >= c.dns.interval.0 {
        bad("dns.deadline", format!("deadline {} must be shorter than dns.interval {} so a stream never overlaps itself", c.dns.deadline, c.dns.interval));
    }
    if c.dns.deadline.0 >= c.dns.tcp.interval.0 {
        bad("dns.tcp.interval", format!("interval {} must be longer than dns.deadline {} so a stream never overlaps itself", c.dns.tcp.interval, c.dns.deadline));
    }
    if c.dns.retries != 0 {
        bad("dns.retries", "DNS retries are not implemented in this build; the only supported value is 0".into());
    }
    if c.dns.edns_udp_size < 512 {
        bad("dns.edns_udp_size", format!("{} is below 512", c.dns.edns_udp_size));
    }
    if c.dns.query.is_empty() {
        bad("dns.query", "at least one query scenario is required".into());
    }
    for (i, q) in c.dns.query.iter().enumerate() {
        if !EXPECTATIONS.contains(&q.expect.as_str()) {
            bad(&format!("dns.query[{i}].expect"), format!("unknown expectation {:?} (use {})", q.expect, EXPECTATIONS.join(", ")));
        }
        if !QUERY_TYPES.contains(&q.qtype.as_str()) {
            bad(&format!("dns.query[{i}].type"), format!("unsupported record type {:?} (use {})", q.qtype, QUERY_TYPES.join(", ")));
        }
        if q.name.is_empty() {
            bad(&format!("dns.query[{i}].name"), "name is empty (use \".\" for the root)".into());
        }
    }
    if c.dns.doh.method != "post" {
        bad("dns.doh.method", format!("{:?} is not implemented in this build; use \"post\"", c.dns.doh.method));
    }
    if c.dns.doh.proxy != "direct" {
        bad("dns.doh.proxy", format!("{:?} is not implemented in this build; use \"direct\"", c.dns.doh.proxy));
    }
    for (key, v) in [("dns.doh.fresh_handshake", &c.dns.doh.fresh_handshake), ("dns.dot.fresh_handshake", &c.dns.dot.fresh_handshake)] {
        if v != "full" {
            bad(key, format!("{v:?} is not implemented in this build; use \"full\""));
        }
    }
    for (key, v) in [("dns.doh.fresh_interval", &c.dns.doh.fresh_interval), ("dns.dot.fresh_interval", &c.dns.dot.fresh_interval)] {
        if v.0 <= FRESH_DEADLINE {
            bad(key, format!("must be longer than the {}s cap on a fresh-connection probe", FRESH_DEADLINE.as_secs()));
        }
    }
    if c.bootstrap.resolver != "system" {
        bad("bootstrap.resolver", format!("{:?} is not implemented in this build; use \"system\"", c.bootstrap.resolver));
    }
    if !["fail", "keep"].contains(&c.bootstrap.on_failure.as_str()) {
        bad("bootstrap.on_failure", format!("unknown value {:?} (use fail or keep)", c.bootstrap.on_failure));
    }

    if !["auto", "truecolor", "256", "16", "never"].contains(&c.ui.color.as_str()) {
        bad("ui.color", format!("unknown value {:?} (use auto, truecolor, 256, 16, or never)", c.ui.color));
    }
    if !["default", "high-contrast", "mono"].contains(&c.ui.theme.as_str()) {
        bad("ui.theme", format!("unknown value {:?} (use default, high-contrast, or mono)", c.ui.theme));
    }
    if !["unicode", "ascii"].contains(&c.ui.glyphs.as_str()) {
        bad("ui.glyphs", format!("unknown value {:?} (use unicode or ascii)", c.ui.glyphs));
    }

    let mut slots = BTreeMap::new();
    for (pkey, prov) in &c.providers {
        let pk = format!("providers.{pkey}");
        if !(1..=4).contains(&prov.slot) {
            bad(&format!("{pk}.slot"), format!("slot {} is outside 1–4", prov.slot));
        }
        if let Some(other) = slots.insert(prov.slot, pkey) {
            bad(&format!("{pk}.slot"), format!("slot {} is also used by providers.{other}", prov.slot));
        }
        let mut ids = std::collections::BTreeSet::new();
        for (i, t) in prov.targets.iter().enumerate() {
            let tk = format!("{pk}.targets[{i}]");
            if !ids.insert(&t.id) {
                bad(&format!("{tk}.id"), format!("duplicate target id {:?}", t.id));
            }
            let given = [t.address.is_some(), t.url.is_some(), t.hostname.is_some()].iter().filter(|b| **b).count();
            if given != 1 {
                bad(&tk, "give exactly one of address, url, hostname".into());
            }
            if t.protocols.is_empty() {
                bad(&format!("{tk}.protocols"), "list at least one protocol".into());
            }
            for name in &t.protocols {
                match Protocol::parse(name) {
                    None => bad(&format!("{tk}.protocols"), format!("unknown protocol {name:?} (use icmp, dns-udp, dns-tcp, dot, doh)")),
                    Some(proto) => {
                        if t.role.as_deref() == Some("router") && proto != Protocol::Icmp {
                            bad(&format!("{tk}.protocols"), format!("a router target accepts only \"icmp\", not {name:?}: a router is a hop, not a resolver"));
                        }
                        match proto {
                            Protocol::Doh if t.url.is_none() => bad(&format!("{tk}.protocols"), "\"doh\" needs a url".into()),
                            Protocol::Dot if t.hostname.is_none() => bad(&format!("{tk}.protocols"), "\"dot\" needs a hostname".into()),
                            Protocol::Icmp | Protocol::DnsUdp | Protocol::DnsTcp if t.address.is_none() => bad(&format!("{tk}.protocols"), format!("{name:?} needs an address")),
                            _ => {}
                        }
                    }
                }
            }
            if !t.addresses.is_empty() {
                if t.address.is_some() {
                    bad(&format!("{tk}.addresses"), "static addresses apply to a url or hostname target; this target already has a literal address".into());
                }
                if t.follow == Some(true) {
                    bad(&format!("{tk}.follow"), "follow has no effect with static addresses: the hostname is never resolved".into());
                }
                for a in &t.addresses {
                    if is_placeholder(a) {
                        if prov.enabled && t.enabled {
                            bad(&format!("{tk}.addresses"), format!("still a placeholder ({a}); set a real address or remove it"));
                        }
                    } else if a.parse::<IpAddr>().is_err() {
                        bad(&format!("{tk}.addresses"), format!("{a:?} is not an IP address"));
                    }
                }
            }
            if let Some(role) = &t.role
                && role != "router"
                && role != "resolver"
            {
                bad(&format!("{tk}.role"), format!("unknown role {role:?} (use router or resolver)"));
            }
            if let Some(url) = &t.url
                && !is_placeholder(url)
                && let Err(e) = parse_https_url(&url.replace("{profile_id}", "x"))
            {
                bad(&format!("{tk}.url"), e);
            }
            // Placeholders are tolerated only where nothing would be sent.
            if prov.enabled && t.enabled {
                for (field, value) in [("address", &t.address), ("url", &t.url), ("hostname", &t.hostname)] {
                    if let Some(v) = value
                        && is_placeholder(v)
                    {
                        bad(&format!("{tk}.{field}"), format!("still a placeholder ({v}); set a real value or disable this target"));
                    }
                }
            }
        }
    }
    for (key, ctl) in [("controls.gateway", &c.controls.gateway), ("controls.local_dns", &c.controls.local_dns)] {
        if ctl.address != "auto" && ctl.address.parse::<IpAddr>().is_err() {
            bad(&format!("{key}.address"), format!("{:?} is neither \"auto\" nor an IP address", ctl.address));
        }
    }
    for key in c.providers.keys().filter(|k| crate::localnet::is_control(k)) {
        bad(&format!("providers.{key}"), "this name is reserved for a control; choose another provider key".into());
    }

    // Aggregate traffic against the global budget.
    let plan = Plan::derive(c, &|_| Some("x".into()));
    if plan.steady_rate > c.limits.max_transactions_per_second as f64 {
        p.push(Problem {
            key: "limits.max_transactions_per_second".into(),
            message: format!(
                "the configured streams need {:.1} transactions/s, above the limit of {}; lengthen intervals, disable streams, or raise the limit",
                plan.steady_rate, c.limits.max_transactions_per_second
            ),
        });
    }
    p
}

/// Parses `https://host[:port]/path` into (host, port, path-and-query).
pub fn parse_https_url(url: &str) -> Result<(String, u16, String), String> {
    let uri: http::Uri = url.parse().map_err(|e| format!("not a valid URL: {e}"))?;
    if uri.scheme_str() != Some("https") {
        return Err("DoH requires an https:// URL".into());
    }
    let host = uri.host().ok_or("URL has no host")?.trim_matches(['[', ']']).to_string();
    let port = uri.port_u16().unwrap_or(443);
    let path = uri.path_and_query().map_or("/", |p| p.as_str()).to_string();
    Ok((host, port, path))
}

// ───────────────────────────── stream plan ─────────────────────────────

/// Overall cap on a fresh-connection probe: connect + handshake + first query.
pub const FRESH_DEADLINE: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// A literal address: no resolution, one destination epoch.
    Ip(IpAddr),
    /// A hostname to resolve off the timing path.
    Host(String),
    /// A DoH endpoint. `host` is both the TLS server name and the HTTP authority.
    Url { host: String, port: u16, path: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanState {
    Collecting,
    /// Nothing is sent; the card says what to configure. Never shown as loss.
    NeedsConfig(String),
    Unsupported(String),
    Disabled,
}

impl PlanState {
    pub fn label(&self) -> &'static str {
        match self {
            PlanState::Collecting => "collecting",
            PlanState::NeedsConfig(_) => "needs_config",
            PlanState::Unsupported(_) => "unsupported",
            PlanState::Disabled => "disabled",
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            PlanState::NeedsConfig(r) | PlanState::Unsupported(r) => Some(r),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StreamPlan {
    pub id: Arc<str>,
    pub provider: Arc<str>,
    pub target: Arc<str>,
    pub protocol: Protocol,
    pub family: Family,
    /// The target as configured, secrets redacted. Safe to record and display.
    pub configured: Arc<str>,
    pub endpoint: Endpoint,
    /// Set when the target has static addresses: the address to dial for this
    /// stream's family. The hostname is then never resolved.
    pub pinned: Option<IpAddr>,
    pub follow: bool,
    pub interval: Duration,
    pub deadline: Duration,
    pub phase: Duration,
    /// A fresh-connection cohort: new connection per probe, closed afterwards.
    pub fresh: bool,
    pub state: PlanState,
}

#[derive(Debug, Clone)]
pub struct ProviderPlan {
    pub key: Arc<str>,
    pub name: String,
    pub slot: u8,
    pub state: PlanState,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub providers: Vec<ProviderPlan>,
    pub streams: Vec<StreamPlan>,
    /// Probe transactions per second across all collecting streams.
    pub steady_rate: f64,
}

impl Plan {
    pub fn collecting(&self) -> impl Iterator<Item = &StreamPlan> {
        self.streams.iter().filter(|s| s.state == PlanState::Collecting)
    }

    /// Adds the enabled controls as streams: ICMP echo to the default
    /// gateway, and DNS over UDP to the first resolver the system lists.
    /// They are streams to the collector and nothing more: no provider, no
    /// card. `auto` uses what was discovered on this host; when that failed
    /// the stream is present but unsupported, with the reason, and nothing
    /// is sent.
    pub fn add_controls(&mut self, c: &Config, local: &crate::localnet::LocalNet) {
        use crate::localnet::{GATEWAY, LOCAL_DNS};
        let families: Vec<Family> =
            [("ipv4", Family::V4), ("ipv6", Family::V6)].into_iter().filter(|(name, _)| c.general.families.iter().any(|f| f == name)).map(|(_, f)| f).collect();
        let mut add = |provider: &'static str, protocol: Protocol, family: Family, found: Result<IpAddr, String>, literal: bool| {
            let (interval, deadline) = if protocol == Protocol::Icmp { (c.icmp.interval.0, c.icmp.deadline.0) } else { (c.dns.interval.0, c.dns.deadline.0) };
            let (endpoint, configured, state) = match found {
                Ok(ip) => (Endpoint::Ip(ip), ip.to_string(), PlanState::Collecting),
                Err(why) => (Endpoint::Ip(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)), "auto".to_string(), PlanState::Unsupported(why)),
            };
            let target = if literal { "configured" } else { "auto" };
            self.streams.push(StreamPlan {
                id: format!("{provider}/{target}/{}/{}", protocol.as_str(), family.as_str()).into(),
                provider: provider.into(),
                target: target.into(),
                protocol,
                family,
                configured: configured.into(),
                endpoint,
                pinned: None,
                follow: false,
                interval,
                deadline,
                // Between the provider streams' slots rather than on top of one.
                phase: interval / 3,
                fresh: false,
                state,
            });
        };
        if c.controls.gateway.enabled {
            match c.controls.gateway.address.parse::<IpAddr>() {
                Ok(ip) => add(GATEWAY, Protocol::Icmp, Family::of(&ip), Ok(ip), true),
                Err(_) => {
                    families.iter().for_each(|f| add(GATEWAY, Protocol::Icmp, *f, local.gateway(*f).clone().map_err(|why| format!("no default gateway found: {why}")), false))
                }
            }
        }
        if c.controls.local_dns.enabled {
            match c.controls.local_dns.address.parse::<IpAddr>() {
                Ok(ip) => add(LOCAL_DNS, Protocol::DnsUdp, Family::of(&ip), Ok(ip), true),
                Err(_) => {
                    for f in &families {
                        // The first resolver of the family, as the system orders them.
                        let first = match &local.resolvers {
                            Ok(list) => list.iter().copied().find(|ip| Family::of(ip) == *f).ok_or_else(|| format!("the system lists no IPv{} resolver", f.as_str())),
                            Err(why) => Err(format!("no system resolver found: {why}")),
                        };
                        add(LOCAL_DNS, Protocol::DnsUdp, *f, first, false);
                    }
                }
            }
        }
        // The controls send too: they are in the rate the plan reports.
        self.steady_rate = self.streams.iter().filter(|s| s.state == PlanState::Collecting).map(|s| 1.0 / s.interval.as_secs_f64()).sum();
    }

    pub fn derive(c: &Config, env: EnvFn<'_>) -> Self {
        let families: Vec<Family> = c
            .general
            .families
            .iter()
            .filter_map(|f| match f.as_str() {
                "ipv4" => Some(Family::V4),
                "ipv6" => Some(Family::V6),
                _ => None,
            })
            .collect();

        let mut providers: Vec<(&String, &Provider)> = c.providers.iter().collect();
        providers.sort_by_key(|(_, p)| p.slot);

        let mut plan_providers = Vec::new();
        let mut streams = Vec::new();
        for (pkey, prov) in providers {
            let pkey_arc: Arc<str> = pkey.as_str().into();
            let has_placeholders = prov.targets.iter().any(|t| [&t.address, &t.url, &t.hostname].into_iter().flatten().any(|v| is_placeholder(v) || v.contains("{profile_id}")));
            if !prov.enabled {
                let state = if has_placeholders || prov.targets.is_empty() {
                    PlanState::NeedsConfig(format!("no target configured; set your endpoint under [providers.{pkey}] and enable it"))
                } else {
                    PlanState::Disabled
                };
                plan_providers.push(ProviderPlan { key: pkey_arc, name: prov.name.clone(), slot: prov.slot, state });
                continue;
            }
            let profile = prov.profile_id_env.as_deref().and_then(env);
            let before = streams.len();
            for t in prov.targets.iter().filter(|t| t.enabled) {
                let raw = t.address.as_ref().or(t.url.as_ref()).or(t.hostname.as_ref()).cloned().unwrap_or_default();
                let needs_profile = raw.contains("{profile_id}");
                let missing_profile = needs_profile && profile.is_none();
                let real = match &profile {
                    Some(id) => raw.replace("{profile_id}", id),
                    None => raw.clone(),
                };
                let configured: Arc<str> = if needs_profile { raw.replace("{profile_id}", REDACTED).into() } else { redact(&raw).into() };
                for name in &t.protocols {
                    let Some(protocol) = Protocol::parse(name) else { continue };
                    let endpoint = match protocol {
                        Protocol::Doh => match parse_https_url(&real) {
                            Ok((host, port, path)) => Endpoint::Url { host, port, path },
                            Err(_) => continue,
                        },
                        _ => match real.parse::<IpAddr>() {
                            Ok(ip) => Endpoint::Ip(ip),
                            Err(_) => Endpoint::Host(real.clone()),
                        },
                    };
                    // Static addresses replace resolution: the first address of
                    // each family is dialled, and a family without one gets no stream.
                    let pinned: Vec<IpAddr> = t.addresses.iter().filter_map(|a| a.parse().ok()).collect();
                    let pin_for = |f: Family| pinned.iter().copied().find(|ip| Family::of(ip) == f);
                    let stream_families: Vec<Family> = match &endpoint {
                        _ if !pinned.is_empty() => families.iter().copied().filter(|f| pin_for(*f).is_some()).collect(),
                        Endpoint::Ip(ip) => vec![Family::of(ip)],
                        Endpoint::Url { host, .. } => match host.parse::<IpAddr>() {
                            Ok(ip) => vec![Family::of(&ip)],
                            Err(_) => families.clone(),
                        },
                        Endpoint::Host(_) => families.clone(),
                    };
                    let (enabled, interval, deadline) = match protocol {
                        Protocol::Icmp => (c.icmp.enabled, c.icmp.interval.0, c.icmp.deadline.0),
                        Protocol::DnsUdp => (c.dns.udp.enabled, c.dns.interval.0, c.dns.deadline.0),
                        Protocol::DnsTcp => (c.dns.tcp.enabled, c.dns.tcp.interval.0, c.dns.deadline.0),
                        Protocol::Dot => (c.dns.dot.enabled, c.dns.interval.0, c.dns.deadline.0),
                        Protocol::Doh => (c.dns.doh.enabled, c.dns.interval.0, c.dns.deadline.0),
                    };
                    for family in stream_families {
                        let state = if missing_profile {
                            PlanState::NeedsConfig(format!("environment variable {} is not set", prov.profile_id_env.as_deref().unwrap_or("(profile_id_env is not configured)")))
                        } else if !enabled || !families.contains(&family) {
                            PlanState::Disabled
                        } else {
                            PlanState::Collecting
                        };
                        let base = format!("{pkey}/{}/{}/{}", t.id, protocol.as_str(), family.as_str());
                        let mut push = |id: String, interval: Duration, deadline: Duration, fresh: bool| {
                            streams.push(StreamPlan {
                                id: id.into(),
                                provider: pkey_arc.clone(),
                                target: t.id.as_str().into(),
                                protocol,
                                family,
                                // Say where it is dialled, so a pinned stream is
                                // never mistaken for a resolved one.
                                configured: match pin_for(family) {
                                    Some(ip) => format!("{configured} via {ip}").into(),
                                    None => configured.clone(),
                                },
                                // DNS over TLS dials the target's hostname on its own
                                // port (853 unless set), with that name as server name.
                                endpoint: match (&endpoint, protocol) {
                                    (Endpoint::Host(host), Protocol::Dot) => Endpoint::Url { host: host.clone(), port: t.port.unwrap_or(853), path: String::new() },
                                    _ => endpoint.clone(),
                                },
                                pinned: pin_for(family),
                                follow: pinned.is_empty() && t.follow.unwrap_or(!matches!(endpoint, Endpoint::Ip(_))),
                                interval,
                                deadline,
                                phase: Duration::ZERO,
                                fresh,
                                state: state.clone(),
                            });
                        };
                        push(base.clone(), interval, deadline, false);
                        match protocol {
                            Protocol::Doh => push(format!("{base}/fresh"), c.dns.doh.fresh_interval.0, FRESH_DEADLINE, true),
                            Protocol::Dot => push(format!("{base}/fresh"), c.dns.dot.fresh_interval.0, FRESH_DEADLINE, true),
                            _ => {}
                        }
                    }
                }
            }
            let mine = &streams[before..];
            let state = if mine.iter().any(|s| s.state == PlanState::Collecting) {
                PlanState::Collecting
            } else if let Some(reason) = mine.iter().find_map(|s| match &s.state {
                PlanState::NeedsConfig(r) => Some(r.clone()),
                _ => None,
            }) {
                PlanState::NeedsConfig(reason)
            } else if mine.is_empty() {
                PlanState::NeedsConfig("no enabled target".into())
            } else {
                PlanState::Disabled
            };
            plan_providers.push(ProviderPlan { key: pkey_arc, name: prov.name.clone(), slot: prov.slot, state });
        }

        // Stagger streams that share an interval evenly across it, in plan order.
        let mut groups: BTreeMap<Duration, Vec<usize>> = BTreeMap::new();
        for (i, s) in streams.iter().enumerate() {
            if s.state == PlanState::Collecting {
                groups.entry(s.interval).or_default().push(i);
            }
        }
        for (interval, members) in &groups {
            for (k, &i) in members.iter().enumerate() {
                streams[i].phase = crate::schedule::stagger_phase(k, members.len(), *interval);
            }
        }
        let steady_rate = streams.iter().filter(|s| s.state == PlanState::Collecting).map(|s| 1.0 / s.interval.as_secs_f64()).sum();
        Self { providers: plan_providers, streams, steady_rate }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn load(text: &str) -> Result<Loaded, ConfigError> {
        Loaded::from_layers(Some(text), None, &[])
    }

    fn problems(text: &str) -> Vec<Problem> {
        match load(text) {
            Err(ConfigError::Invalid(p)) => p,
            other => panic!("expected validation problems, got {other:?}"),
        }
    }

    #[test]
    fn durations_round_trip() {
        for s in ["250ms", "1s", "5m", "24h", "90s", "1500ms"] {
            assert_eq!(Dur::parse(s).unwrap().to_string(), s);
        }
        assert!(Dur::parse("5").is_err());
        assert!(Dur::parse("5 minutes").is_err());
    }

    #[test]
    fn built_in_configuration_is_valid_and_matches_the_documented_defaults() {
        let l = Loaded::from_layers(None, None, &[]).expect("the example configuration must validate");
        let c = &l.config;
        assert_eq!(c.icmp.interval.0, Duration::from_secs(1));
        assert_eq!(c.icmp.deadline.0, Duration::from_secs(2));
        assert_eq!(c.icmp.payload_bytes, 56);
        assert_eq!(c.dns.interval.0, Duration::from_secs(5));
        assert_eq!(c.dns.deadline.0, Duration::from_secs(3));
        assert_eq!(c.dns.retries, 0);
        assert_eq!(c.dns.edns_udp_size, 1232);
        assert_eq!(c.limits.max_transactions_per_second, 25);
        assert_eq!(c.dns.query.len(), 3);
        assert!(!c.ui.mouse && !c.recording.enabled);
        // Cloudflare's DoH address is pinned by default (decided 2026-11-21,
        // after a filtering resolver redirected the hostname); Google's is not.
        let plan = l.plan(&|_| None);
        let doh =
            |provider: &str, family| plan.streams.iter().find(|s| &*s.provider == provider && s.protocol == Protocol::Doh && s.family == family && !s.fresh).expect("a DoH stream");
        assert_eq!(doh("cloudflare", Family::V4).pinned, Some("1.1.1.1".parse().unwrap()));
        assert_eq!(doh("cloudflare", Family::V6).pinned, Some("2606:4700:4700::1111".parse().unwrap()));
        assert!(!doh("cloudflare", Family::V4).follow && doh("google", Family::V4).pinned.is_none() && doh("google", Family::V4).follow);
    }

    #[test]
    fn default_plan_has_four_providers_and_never_invents_targets() {
        let l = Loaded::from_layers(None, None, &[]).unwrap();
        let plan = l.plan(&no_env);
        let states: Vec<(&str, &str)> = plan.providers.iter().map(|p| (&*p.key, p.state.label())).collect();
        assert_eq!(states, vec![("cloudflare", "collecting"), ("google", "collecting"), ("nextdns", "needs_config"), ("arelion", "needs_config")]);
        // Disabled providers contribute no streams at all: nothing can be sent to them.
        assert!(plan.streams.iter().all(|s| &*s.provider == "cloudflare" || &*s.provider == "google"));
        let collecting: Vec<&str> = plan.collecting().map(|s| &*s.id).collect();
        assert_eq!(collecting.len(), 16, "{collecting:#?}");
        assert!(collecting.contains(&"cloudflare/resolver-v4/icmp/4"));
        assert!(collecting.contains(&"google/doh/doh/6/fresh"));
        // 4 ICMP at 1/s + 4 DNS/UDP + 4 DoH at 1/5s + 4 fresh at 1/300s.
        assert!((plan.steady_rate - (4.0 + 0.8 + 0.8 + 4.0 / 300.0)).abs() < 1e-9);
    }

    #[test]
    fn streams_sharing_an_interval_are_staggered() {
        let plan = Loaded::from_layers(None, None, &[]).unwrap().plan(&no_env);
        let mut icmp: Vec<u128> = plan.collecting().filter(|s| s.protocol == Protocol::Icmp).map(|s| s.phase.as_millis()).collect();
        icmp.sort_unstable();
        assert_eq!(icmp, vec![0, 250, 500, 750]);
    }

    #[test]
    fn unknown_keys_are_errors() {
        let e = load("[icmp]\nintervall = \"1s\"\n").unwrap_err();
        assert!(matches!(&e, ConfigError::Parse { message, .. } if message.contains("intervall")), "{e:?}");
    }

    #[test]
    fn placeholder_in_an_enabled_target_is_rejected_by_name() {
        let p = problems(
            "[providers.arelion]\nenabled = true\n[[providers.arelion.targets]]\nid = \"router\"\nrole = \"router\"\naddress = \"<arelion-router>\"\nprotocols = [\"icmp\"]\n",
        );
        assert!(p.iter().any(|x| x.key == "providers.arelion.targets[0].address" && x.message.contains("placeholder")), "{p:?}");
    }

    #[test]
    fn reports_every_problem() {
        let p = problems("[dns]\ndeadline = \"5s\"\n[icmp]\npayload_bytes = 4\n[general]\nfamilies = []\n");
        let keys: Vec<&str> = p.iter().map(|x| x.key.as_str()).collect();
        assert!(keys.contains(&"dns.deadline") && keys.contains(&"icmp.payload_bytes") && keys.contains(&"general.families"), "{keys:?}");
    }

    #[test]
    fn rate_above_the_global_limit_is_rejected() {
        let p = problems("[limits]\nmax_transactions_per_second = 5\n");
        assert!(p.iter().any(|x| x.key == "limits.max_transactions_per_second" && x.message.contains("5.6")), "{p:?}");
    }

    #[test]
    fn router_targets_accept_only_icmp() {
        let p = problems(
            "[providers.arelion]\nenabled = true\n[[providers.arelion.targets]]\nid = \"router\"\nrole = \"router\"\naddress = \"198.51.100.7\"\nprotocols = [\"icmp\", \"dns-udp\"]\n",
        );
        assert!(p.iter().any(|x| x.message.contains("router target accepts only")), "{p:?}");
    }

    const NEXTDNS_ON: &str =
        "[providers.nextdns]\nenabled = true\n[[providers.nextdns.targets]]\nid = \"profile-doh\"\nurl = \"https://dns.nextdns.io/{profile_id}\"\nprotocols = [\"doh\"]\n";

    #[test]
    fn missing_profile_environment_variable_is_needs_config_not_an_error() {
        let plan = load(NEXTDNS_ON).unwrap().plan(&no_env);
        let nd = plan.providers.iter().find(|p| &*p.key == "nextdns").unwrap();
        assert!(matches!(&nd.state, PlanState::NeedsConfig(r) if r.contains("PATHWATCH_NEXTDNS_PROFILE")), "{:?}", nd.state);
        assert!(plan.collecting().all(|s| &*s.provider != "nextdns"));
    }

    #[test]
    fn profile_id_reaches_the_endpoint_but_never_the_recorded_string() {
        let env = |name: &str| (name == "PATHWATCH_NEXTDNS_PROFILE").then(|| "abc123".to_string());
        let l = load(NEXTDNS_ON).unwrap();
        let plan = l.plan(&env);
        let s = plan.collecting().find(|s| &*s.id == "nextdns/profile-doh/doh/4").expect("stream is collecting");
        assert_eq!(s.endpoint, Endpoint::Url { host: "dns.nextdns.io".into(), port: 443, path: "/abc123".into() });
        assert_eq!(&*s.configured, "https://dns.nextdns.io/‹redacted:nextdns-profile›");
        assert!(!l.effective_toml(false).contains("abc123"));
    }

    #[test]
    fn literal_profile_ids_are_masked() {
        assert_eq!(redact("https://dns.nextdns.io/abc123"), format!("https://dns.nextdns.io/{REDACTED}"));
        assert_eq!(redact("https://dns.nextdns.io/abc123/Router"), format!("https://dns.nextdns.io/{REDACTED}/Router"));
        assert_eq!(redact("abc123.dns.nextdns.io"), format!("{REDACTED}.dns.nextdns.io"));
        assert_eq!(redact("https://dns.nextdns.io/{profile_id}"), "https://dns.nextdns.io/{profile_id}");
        assert_eq!(redact("https://dns.google/dns-query"), "https://dns.google/dns-query");
        let l = load("[providers.nextdns]\nenabled = true\n[[providers.nextdns.targets]]\nid = \"profile-doh\"\nurl = \"https://dns.nextdns.io/abc123\"\nprotocols = [\"doh\"]\n")
            .unwrap();
        assert!(!l.effective_toml(false).contains("abc123"));
        assert!(l.effective_toml(true).contains("abc123"));
        assert!(l.effective_settings(false).iter().all(|(_, v, _)| !v.contains("abc123")));
    }

    const PINNED: &str = "[[providers.cloudflare.targets]]\nid = \"doh\"\nurl = \"https://cloudflare-dns.com/dns-query\"\naddresses = [\"1.1.1.1\", \"2606:4700:4700::1111\", \"1.0.0.1\"]\nprotocols = [\"doh\"]\n";

    #[test]
    fn static_addresses_pin_one_address_per_family_and_skip_resolution() {
        let plan = load(PINNED).unwrap().plan(&no_env);
        let v4 = plan.collecting().find(|s| &*s.id == "cloudflare/doh/doh/4").unwrap();
        let v6 = plan.collecting().find(|s| &*s.id == "cloudflare/doh/doh/6/fresh").unwrap();
        assert_eq!(v4.pinned, Some("1.1.1.1".parse().unwrap()), "the first address of the family");
        assert_eq!(v6.pinned, Some("2606:4700:4700::1111".parse().unwrap()));
        assert!(!v4.follow, "a pinned stream never follows a hostname");
        // The hostname stays the TLS server name; only the dialled address changes.
        assert_eq!(v4.endpoint, Endpoint::Url { host: "cloudflare-dns.com".into(), port: 443, path: "/dns-query".into() });
        assert_eq!(&*v4.configured, "https://cloudflare-dns.com/dns-query via 1.1.1.1");

        // A family with no static address gets no stream rather than a resolved one.
        let only_v4 = load(&PINNED.replace(", \"2606:4700:4700::1111\"", "")).unwrap().plan(&no_env);
        assert!(only_v4.streams.iter().all(|s| !s.id.starts_with("cloudflare/doh/doh/6")));
    }

    #[test]
    fn a_reload_can_change_what_is_probed_and_nothing_else() {
        let base = Loaded::from_layers(None, None, &[]).unwrap().config;
        let with = |text: &str| Loaded::from_layers(Some(text), None, &[]).unwrap_or_else(|e| panic!("{text}: {e:?}")).config;
        assert_eq!(restart_only_changes(&base, &base), Vec::<String>::new());
        // Targets, providers, controls, cadence, and which protocols run: applied live.
        for reloadable in [
            "[[providers.cloudflare.targets]]\nid = \"only\"\naddress = \"1.0.0.1\"\nprotocols = [\"icmp\"]\n",
            "[providers.google]\nenabled = false\n",
            "[controls.gateway]\nenabled = true\n",
            "[icmp]\ninterval = \"2s\"\ndeadline = \"1s\"\n",
            "[dns]\ninterval = \"10s\"\n",
            "[dns.doh]\nenabled = false\n",
            "[dns.tcp]\nenabled = true\ninterval = \"30s\"\n",
            "[general]\nfamilies = [\"ipv4\"]\n",
        ] {
            assert_eq!(restart_only_changes(&base, &with(reloadable)), Vec::<String>::new(), "{reloadable}");
        }
        // Anything else is named, key by key, so the refusal can say what to restart for.
        assert_eq!(restart_only_changes(&base, &with("[icmp]\npayload_bytes = 64\n")), vec!["icmp.payload_bytes"]);
        assert_eq!(restart_only_changes(&base, &with("[limits]\nburst = 3\n[recording]\nenabled = true\n")), vec!["limits.burst", "recording.enabled"]);
        assert_eq!(restart_only_changes(&base, &with("[ui]\ncolor = \"never\"\n[icmp]\ninterval = \"2s\"\n")), vec!["ui.color"], "a reloadable change beside it does not hide it");
        assert_eq!(restart_only_changes(&base, &with("[incidents.no_reply_run]\nopen_after_consecutive = 9\n")), vec!["incidents.no_reply_run.open_after_consecutive"]);
    }

    #[test]
    fn dns_over_tls_targets_are_off_by_default_and_plan_two_streams_each_when_enabled() {
        // Built in: the targets exist and are switched off, so nothing is sent.
        let plan = Loaded::from_layers(None, None, &[]).unwrap().plan(&|_| None);
        assert!(plan.collecting().all(|s| s.protocol != Protocol::Dot), "no DoT stream collects unless a target is enabled");

        let on = "[[providers.cloudflare.targets]]\nid = \"dot\"\nhostname = \"one.one.one.one\"\nprotocols = [\"dot\"]\naddresses = [\"1.1.1.1\"]\n";
        let l = Loaded::from_layers(Some(on), None, &[]).unwrap();
        let plan = l.plan(&|_| None);
        let dot: Vec<&StreamPlan> = plan.collecting().filter(|s| s.protocol == Protocol::Dot).collect();
        assert_eq!(dot.iter().map(|s| (&*s.id, s.fresh)).collect::<Vec<_>>(), vec![("cloudflare/dot/dot/4", false), ("cloudflare/dot/dot/4/fresh", true)]);
        // The hostname is the TLS server name; the port is 853 unless set; the address is the one given.
        assert_eq!(dot[0].endpoint, Endpoint::Url { host: "one.one.one.one".into(), port: 853, path: String::new() });
        assert_eq!(dot[0].pinned, Some("1.1.1.1".parse().unwrap()));
        assert_eq!((dot[0].interval, dot[1].interval), (l.config.dns.interval.0, l.config.dns.dot.fresh_interval.0));
        // A port of its own, and the protocol switched off as a whole.
        let custom = Loaded::from_layers(Some("[[providers.google.targets]]\nid = \"dot\"\nhostname = \"dns.google\"\nport = 8853\nprotocols = [\"dot\"]\n"), None, &[])
            .unwrap()
            .plan(&|_| None);
        assert!(custom.collecting().any(|s| matches!(&s.endpoint, Endpoint::Url { port: 8853, .. })));
        let off = Loaded::from_layers(Some(&format!("{on}[dns.dot]\nenabled = false\n")), None, &[]).unwrap().plan(&|_| None);
        assert!(off.collecting().all(|s| s.protocol != Protocol::Dot));
        // A name is required: it is what the certificate is checked against.
        let p = problems("[[providers.cloudflare.targets]]\nid = \"dot\"\naddress = \"1.1.1.1\"\nprotocols = [\"dot\"]\n");
        assert!(p.iter().any(|x| x.message.contains("needs a hostname")), "{p:?}");
    }

    #[test]
    fn dns_over_tcp_is_off_until_enabled_and_then_probes_on_its_own_interval() {
        // Nothing in the defaults asks for it.
        let plan = Loaded::from_layers(None, None, &[]).unwrap().plan(&|_| None);
        assert!(plan.streams.iter().all(|s| s.protocol != Protocol::DnsTcp));

        // A target that lists it while the protocol is switched off: the stream is there, and sends nothing.
        let target = "[[providers.cloudflare.targets]]\nid = \"resolver-v4\"\naddress = \"1.1.1.1\"\nprotocols = [\"dns-udp\", \"dns-tcp\"]\n";
        let plan = Loaded::from_layers(Some(target), None, &[]).unwrap().plan(&|_| None);
        let tcp = plan.streams.iter().find(|s| s.protocol == Protocol::DnsTcp).expect("a DNS/TCP stream is planned");
        assert_eq!(tcp.state, PlanState::Disabled);

        // Switched on: one stream beside the UDP one, on its own slower interval, to the same address.
        let l = Loaded::from_layers(Some(&format!("{target}[dns.tcp]\nenabled = true\n")), None, &[]).unwrap();
        let plan = l.plan(&|_| None);
        let tcp: Vec<&StreamPlan> = plan.collecting().filter(|s| s.protocol == Protocol::DnsTcp).collect();
        assert_eq!(tcp.len(), 1);
        assert_eq!((&*tcp[0].id, tcp[0].interval, tcp[0].deadline, tcp[0].fresh), ("cloudflare/resolver-v4/dns-tcp/4", Duration::from_secs(60), l.config.dns.deadline.0, false));
        assert_eq!(tcp[0].endpoint, Endpoint::Ip("1.1.1.1".parse().unwrap()));
        assert!(plan.collecting().any(|s| &*s.id == "cloudflare/resolver-v4/dns-udp/4"));
        // It is in the budget at its rate; its size cannot be known from the configuration.
        let rows = crate::traffic::budget(&l.config, &plan);
        let row = rows.iter().find(|r| r.kind == crate::traffic::Kind::DnsTcp).expect("a DNS/TCP budget row");
        assert!((row.transactions_per_s - 1.0 / 60.0).abs() < 1e-9 && row.bytes_out_per_s.is_none(), "{row:?}");

        let p = problems("[ui]\ntheme = \"neon\"\n");
        assert!(p.iter().any(|x| x.key == "ui.theme" && x.message.contains("default, high-contrast, or mono")), "{p:?}");
        // A probe must be over before the next one starts.
        let p = problems("[dns.tcp]\ninterval = \"2s\"\n");
        assert!(p.iter().any(|x| x.key == "dns.tcp.interval"), "{p:?}");
        let p = problems("[dns.dot]\nfresh_interval = \"5s\"\n");
        assert!(p.iter().any(|x| x.key == "dns.dot.fresh_interval"), "{p:?}");
        // It dials an address; a name alone is not enough.
        let p = problems("[[providers.cloudflare.targets]]\nid = \"r\"\nhostname = \"one.one.one.one\"\nprotocols = [\"dns-tcp\"]\n");
        assert!(p.iter().any(|x| x.message.contains("needs an address")), "{p:?}");
    }

    #[test]
    fn a_setting_that_changes_nothing_is_answered_with_a_warning_once_it_is_changed() {
        // The whole built-in file, unchanged: every such key is present, and none is a change.
        let l = Loaded::from_layers(Some(EXAMPLE_CONFIG), None, &[]).unwrap();
        assert_eq!(l.warnings, Vec::<String>::new());
        let l = Loaded::from_layers(
            Some("[general]\ndefault_window = \"1h\"\n[logging]\nlevel = \"debug\"\n[ui.keys]\nquit = [\"x\"]\n[providers.google]\ncolor = \"#ffffff\"\n"),
            None,
            &[],
        )
        .unwrap();
        let about: Vec<&str> = l.warnings.iter().map(|w| w.split(' ').next().unwrap()).collect();
        assert_eq!(about, vec!["general.default_window", "ui.keys", "logging.level", "providers.google.color"], "{:?}", l.warnings);
        assert!(l.warnings.iter().all(|w| w.contains("has no effect in this build")));
        // One that does something is not mentioned.
        let l = Loaded::from_layers(Some("[ui]\ntheme = \"mono\"\nmouse = true\n"), None, &[]).unwrap();
        assert_eq!(l.warnings, Vec::<String>::new());
        // Every key named is a real one.
        let defaults: toml::Table = EXAMPLE_CONFIG.parse().unwrap();
        assert!(NO_EFFECT.iter().all(|k| value_at(&defaults, k).is_some()));
        assert!(
            reloadable("dns.tcp.enabled") && reloadable("providers.google.enabled") && reloadable("icmp.interval") && !reloadable("icmp.payload_bytes") && !reloadable("dns.query")
        );
    }

    #[test]
    fn recording_sizes_are_validated() {
        let p = problems("[recording]\nrotate_size = \"big\"\nmax_total = \"1 parsec\"\nfsync = \"interval\"\n");
        let keys: Vec<&str> = p.iter().map(|x| x.key.as_str()).collect();
        assert!(keys.contains(&"recording.rotate_size") && keys.contains(&"recording.max_total") && keys.contains(&"recording.fsync"), "{p:?}");
        assert!(!keys.contains(&"recording.min_free_disk"));
    }

    #[test]
    fn static_addresses_are_validated() {
        let p = problems(&PINNED.replace("1.0.0.1", "one.one.one.one"));
        assert!(p.iter().any(|x| x.key == "providers.cloudflare.targets[0].addresses" && x.message.contains("not an IP address")), "{p:?}");
        let p = problems("[[providers.cloudflare.targets]]\nid = \"r\"\naddress = \"1.1.1.1\"\naddresses = [\"1.0.0.1\"]\nprotocols = [\"icmp\"]\n");
        assert!(p.iter().any(|x| x.message.contains("already has a literal address")), "{p:?}");
        let p = problems(&PINNED.replace("protocols", "follow = true\nprotocols"));
        assert!(p.iter().any(|x| x.key == "providers.cloudflare.targets[0].follow"), "{p:?}");
    }

    #[test]
    fn provenance_distinguishes_default_file_and_flag() {
        let l = Loaded::from_layers(Some("[icmp]\ndeadline = \"1s\"\n"), None, &[("general.default_family".into(), toml::Value::String("ipv6".into()))]).unwrap();
        assert_eq!(l.origin_of("icmp.deadline"), Origin::File);
        assert_eq!(l.origin_of("icmp.interval"), Origin::Default);
        assert_eq!(l.origin_of("general.default_family"), Origin::Flag);
        assert_eq!(l.config.icmp.deadline.0, Duration::from_secs(1));
        assert_eq!(l.config.general.default_family, "ipv6");
    }

    #[test]
    fn not_yet_implemented_settings_are_rejected_rather_than_ignored() {
        let p = problems("[source]\ninterface = \"en0\"\n[dns.doh]\nproxy = \"env\"\n");
        let keys: Vec<&str> = p.iter().map(|x| x.key.as_str()).collect();
        assert!(keys.contains(&"source.interface") && keys.contains(&"dns.doh.proxy"), "{keys:?}");
    }

    #[test]
    fn controls_become_streams_without_a_provider_and_say_why_when_they_cannot_run() {
        use crate::localnet::LocalNet;
        let on = "[controls.gateway]\nenabled = true\n[controls.local_dns]\nenabled = true\n";
        let l = Loaded::from_layers(Some(on), None, &[]).expect("controls are valid configuration now");
        let before = l.plan(&|_| None);
        // Off by default: the built-in plan has no control stream.
        assert!(Loaded::from_layers(None, None, &[]).unwrap().plan(&|_| None).streams.iter().all(|s| !crate::localnet::is_control(&s.provider)));

        // A network with an IPv4 gateway, a loopback stub resolver, and no IPv6.
        let local = LocalNet {
            gateway_v4: Ok("192.168.1.1".parse().unwrap()),
            gateway_v6: Err("the routing table has no IPv6 default route".into()),
            resolvers: Ok(vec!["127.0.0.53".parse().unwrap()]),
        };
        let mut plan = before.clone();
        plan.add_controls(&l.config, &local);
        assert_eq!(plan.providers.len(), before.providers.len(), "a control is never a provider and never has a card");
        let controls: Vec<&StreamPlan> = plan.streams.iter().filter(|s| crate::localnet::is_control(&s.provider)).collect();
        let summary: Vec<(&str, &str, bool)> = controls.iter().map(|s| (&*s.id, &*s.configured, s.state == PlanState::Collecting)).collect();
        assert_eq!(
            summary,
            vec![
                ("gateway/auto/icmp/4", "192.168.1.1", true),
                ("gateway/auto/icmp/6", "auto", false),
                ("local-dns/auto/dns-udp/4", "127.0.0.53", true),
                ("local-dns/auto/dns-udp/6", "auto", false)
            ]
        );
        assert_eq!(controls[1].state.reason(), Some("no default gateway found: the routing table has no IPv6 default route"));
        assert_eq!(controls[3].state.reason(), Some("the system lists no IPv6 resolver"));
        assert_eq!((controls[0].interval, controls[2].interval), (l.config.icmp.interval.0, l.config.dns.interval.0));
        assert!(plan.collecting().filter(|s| crate::localnet::is_control(&s.provider)).count() == 2);
        assert!((plan.steady_rate - before.steady_rate - (1.0 + 0.2)).abs() < 1e-9, "one echo a second and one query every five are in the reported rate");

        // Nothing discovered: the rows exist, say why, and send nothing.
        let mut plan = before.clone();
        plan.add_controls(&l.config, &LocalNet::unknown("not looked up"));
        assert!(plan.streams.iter().filter(|s| crate::localnet::is_control(&s.provider)).all(|s| matches!(&s.state, PlanState::Unsupported(why) if why.contains("not looked up"))));

        // A literal address is used as given, in its own family only.
        let literal = Loaded::from_layers(Some("[controls.gateway]\nenabled = true\naddress = \"10.0.0.1\"\n"), None, &[]).unwrap();
        let mut plan = literal.plan(&|_| None);
        plan.add_controls(&literal.config, &LocalNet::unknown("unused"));
        let ids: Vec<&str> = plan.streams.iter().filter(|s| crate::localnet::is_control(&s.provider)).map(|s| &*s.id).collect();
        assert_eq!(ids, vec!["gateway/configured/icmp/4"]);

        // Bad addresses and reserved names are configuration errors.
        let p = problems("[controls.gateway]\naddress = \"the router\"\n[providers.gateway]\nname = \"Mine\"\nslot = 4\nenabled = false\ncolor = \"#FFFFFF\"\n");
        let keys: Vec<&str> = p.iter().map(|x| x.key.as_str()).collect();
        assert!(keys.contains(&"controls.gateway.address") && keys.contains(&"providers.gateway"), "{p:?}");
    }
}
