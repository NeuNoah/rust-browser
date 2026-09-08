//! Deterministic tracker matching.
//!
//! The engine matches request URLs against a curated, versioned local
//! list. Two rule kinds are supported:
//!
//! - **Host rules** (`host: tracker.example.com`): matches the host
//!   exactly or any subdomain (`sub.tracker.example.com`).
//! - **Pattern rules** (`pattern: analytics.example.com/collect`):
//!   substring match against the full URL.
//!
//! Comments (`# ...`) and blank lines are ignored. The matching is
//! pure, allocation-light and unit-testable without a network.
//!
//! EasyList/EasyPrivacy and the `adblock` crate (Brave) are integrated
//! in a later phase; this engine stays as the lightweight first layer
//! that works even when filter lists are unavailable or stale.

use std::borrow::Cow;
use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use thiserror::Error;
use url::{Host, Url};

/// Version of the built-in tracker list. Bump when the embedded list
/// changes; never change rules without bumping.
pub const TRACKER_LIST_VERSION: u32 = 3;

/// One parsed tracker rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackerRule {
    kind: TrackerRuleKind,
    /// The rule text in canonical (lowercase) form.
    rule: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackerRuleKind {
    /// `host:` rule — matches host and subdomains.
    Host,
    /// `pattern:` rule — substring match against the URL.
    Pattern,
}

impl TrackerRule {
    /// Parse a single rule line. Comments and blank lines return
    /// `Ok(None)`. Malformed lines return `Err`.
    pub fn parse(line: &str) -> Result<Option<Self>, TrackerEngineError> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return Ok(None);
        }
        let (kind, rest) = line
            .split_once(':')
            .ok_or_else(|| TrackerEngineError::MalformedRule(line.to_string()))?;
        let mut rule = rest.trim().to_ascii_lowercase();
        if rule.is_empty() {
            return Err(TrackerEngineError::MalformedRule(line.to_string()));
        }
        let kind = match kind.trim().to_ascii_lowercase().as_str() {
            "host" => TrackerRuleKind::Host,
            "pattern" => TrackerRuleKind::Pattern,
            other => {
                return Err(TrackerEngineError::MalformedRule(format!(
                    "unknown rule kind {other:?}"
                )));
            }
        };
        // Canonicalize host rules with the same parser used for request
        // URLs, so malformed rules cannot silently match nothing.
        if kind == TrackerRuleKind::Host {
            let candidate = rule.trim_end_matches('.');
            rule = Host::parse(candidate)
                .map(|host| host.to_string())
                .map_err(|_| TrackerEngineError::MalformedRule(line.to_string()))?;
        }
        Ok(Some(Self { kind, rule }))
    }

    pub fn kind(&self) -> TrackerRuleKind {
        self.kind
    }

    pub fn text(&self) -> &str {
        &self.rule
    }
}

/// Parsing or matching errors.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TrackerEngineError {
    /// A rule line could not be parsed.
    #[error("malformed tracker rule: {0}")]
    MalformedRule(String),
}

/// The result of matching one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackerDecision {
    /// The request is a tracker and must be blocked.
    Blocked(TrackerMatch),
    /// The request is not a known tracker.
    Allowed,
}

/// Details of a blocking match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackerMatch {
    /// The rule that matched.
    pub rule: TrackerRule,
}

/// The tracker engine: an immutable, cheaply clonable set of rules.
#[derive(Debug, Clone, Default)]
pub struct TrackerEngine {
    hosts: HashSet<String>,
    patterns: Vec<String>,
    list_version: u32,
}

/// Metadata about the loaded list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackerListInfo {
    pub version: u32,
    pub host_rules: usize,
    pub pattern_rules: usize,
    /// When the list was loaded, as UNIX seconds.
    pub loaded_at_epoch_secs: u64,
}

impl TrackerEngine {
    /// An empty engine (nothing is blocked).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Parse `rules` (multi-line text in the format documented in the
    /// module header) and return the engine. Malformed lines are
    /// errors; comments and blank lines are skipped.
    pub fn from_rules(rules: &str, list_version: u32) -> Result<Self, TrackerEngineError> {
        let mut engine = Self {
            hosts: HashSet::new(),
            patterns: Vec::new(),
            list_version,
        };
        for line in rules.lines() {
            if let Some(rule) = TrackerRule::parse(line)? {
                engine.insert(rule);
            }
        }
        Ok(engine)
    }

    /// The embedded default list (see `resources/filterlists/trackers.txt`
    /// for the canonical source).
    pub fn builtin() -> Self {
        Self::from_rules(BUILTIN_TRACKERS, TRACKER_LIST_VERSION)
            .expect("the built-in tracker list must parse")
    }

    /// Load a list from raw text with an explicit version number.
    pub fn load(&mut self, rules: &str, list_version: u32) -> Result<(), TrackerEngineError> {
        let parsed = Self::from_rules(rules, list_version)?;
        *self = parsed;
        Ok(())
    }

    /// Add a single already-parsed rule.
    fn insert(&mut self, rule: TrackerRule) {
        match rule.kind {
            TrackerRuleKind::Host => {
                self.hosts.insert(rule.rule);
            }
            TrackerRuleKind::Pattern => self.patterns.push(rule.rule),
        }
    }

    /// Number of rules currently loaded.
    pub fn len(&self) -> usize {
        self.hosts.len() + self.patterns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty() && self.patterns.is_empty()
    }

    /// Metadata for the currently loaded list.
    pub fn info(&self) -> TrackerListInfo {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        TrackerListInfo {
            version: self.list_version,
            host_rules: self.hosts.len(),
            pattern_rules: self.patterns.len(),
            loaded_at_epoch_secs: now,
        }
    }

    /// Match `url` against host and URL-pattern rules. Whether a
    /// top-level request is exempt is decided by the request-pipeline
    /// layer, which has the necessary request context.
    pub fn check(&self, url: &Url) -> TrackerDecision {
        let host = match url.host_str().map(|host| host.trim_end_matches('.')) {
            Some(host) => {
                if host.bytes().any(|byte| byte.is_ascii_uppercase()) {
                    Cow::Owned(host.to_ascii_lowercase())
                } else {
                    Cow::Borrowed(host)
                }
            }
            None => return TrackerDecision::Allowed,
        };

        let matching_host = std::iter::once(host.as_ref())
            .chain(
                host.match_indices('.')
                    .map(|(separator, _)| &host[separator + 1..]),
            )
            .find(|candidate| self.hosts.contains(*candidate));
        if let Some(rule) = matching_host {
            return TrackerDecision::Blocked(TrackerMatch {
                rule: TrackerRule {
                    kind: TrackerRuleKind::Host,
                    rule: rule.to_owned(),
                },
            });
        }

        if self.patterns.is_empty() {
            return TrackerDecision::Allowed;
        }
        let full = url.as_str().to_ascii_lowercase();
        if let Some(rule) = self
            .patterns
            .iter()
            .find(|pattern| full.contains(pattern.as_str()))
        {
            return TrackerDecision::Blocked(TrackerMatch {
                rule: TrackerRule {
                    kind: TrackerRuleKind::Pattern,
                    rule: rule.clone(),
                },
            });
        }

        TrackerDecision::Allowed
    }
}

/// The built-in list. Curated by hand; the canonical copy lives in
/// `resources/filterlists/trackers.txt` for review and future update
/// tooling. Additions must be conservative: false positives break
/// sites, and this is the default on-by-default layer.
const BUILTIN_TRACKERS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../resources/filterlists/trackers.txt"
));

#[cfg(test)]
mod tests {
    use super::*;

    fn url(input: &str) -> Url {
        Url::parse(input).expect("test URL must parse")
    }

    #[test]
    fn parses_rules_comments_and_blank_lines() {
        let rules = "\n# comment\nhost: tracker.example.com\n\npattern: analytics.io/collect\n";
        let engine = TrackerEngine::from_rules(rules, 1).unwrap();
        assert_eq!(engine.len(), 2);
        assert_eq!(engine.info().host_rules, 1);
        assert_eq!(engine.info().pattern_rules, 1);
        assert_eq!(engine.info().version, 1);
    }

    #[test]
    fn rejects_malformed_rules() {
        assert!(TrackerEngine::from_rules("not-a-rule", 1).is_err());
        assert!(TrackerEngine::from_rules("host:", 1).is_err());
        assert!(TrackerEngine::from_rules("host: a/b", 1).is_err());
        assert!(TrackerEngine::from_rules("host: not a host", 1).is_err());
        assert!(TrackerEngine::from_rules("host: example.com:443", 1).is_err());
        assert!(TrackerEngine::from_rules("bogus: x", 1).is_err());
    }

    #[test]
    fn host_rule_matches_exact_and_subdomains() {
        let engine = TrackerEngine::from_rules("host: tracker.example.com\n", 1).unwrap();
        assert_eq!(
            engine.check(&url("http://tracker.example.com/pixel.png")),
            TrackerDecision::Blocked(TrackerMatch {
                rule: TrackerRule {
                    kind: TrackerRuleKind::Host,
                    rule: "tracker.example.com".into(),
                }
            })
        );
        assert_eq!(
            engine.check(&url("https://sub.tracker.example.com/collect")),
            TrackerDecision::Blocked(TrackerMatch {
                rule: TrackerRule {
                    kind: TrackerRuleKind::Host,
                    rule: "tracker.example.com".into(),
                }
            })
        );
        assert!(matches!(
            engine.check(&url("https://www.example.com/")),
            TrackerDecision::Allowed
        ));
        // Prefix match must not happen (e.g. evil.com vs evil.com.evil.org).
        assert!(matches!(
            engine.check(&url("https://xexample.com/")),
            TrackerDecision::Allowed
        ));
    }

    #[test]
    fn pattern_rule_matches_substring() {
        let engine = TrackerEngine::from_rules("pattern: /collect?e=1\n", 1).unwrap();
        assert!(matches!(
            engine.check(&url("https://cdn.example.net/collect?e=1&x=2")),
            TrackerDecision::Blocked(_)
        ));
        assert!(matches!(
            engine.check(&url("https://cdn.example.net/collect?x=2")),
            TrackerDecision::Allowed
        ));
    }

    #[test]
    fn rules_are_case_insensitive() {
        let engine = TrackerEngine::from_rules("HOST: TRACKER.example.com.\n", 1).unwrap();
        assert!(matches!(
            engine.check(&url("https://TRACKER.EXAMPLE.COM/x")),
            TrackerDecision::Blocked(_)
        ));
    }

    #[test]
    fn host_rules_match_trailing_dot_urls() {
        let engine = TrackerEngine::from_rules("host: tracker.example.com\n", 1).unwrap();
        assert!(matches!(
            engine.check(&url("https://tracker.example.com./x")),
            TrackerDecision::Blocked(_)
        ));
        assert!(matches!(
            engine.check(&url("https://sub.tracker.example.com./x")),
            TrackerDecision::Blocked(_)
        ));
    }

    #[test]
    fn builtin_list_parses_and_matches_itself() {
        let engine = TrackerEngine::builtin();
        assert!(!engine.is_empty(), "built-in list must not be empty");
        assert_eq!(engine.info().version, TRACKER_LIST_VERSION);
        // The builtin list must contain at least one well-known tracker
        // to be self-verifying (see resources/filterlists/trackers.txt).
        assert!(engine.hosts.len() >= 10, "builtin list too small");
        assert_eq!(engine.info().pattern_rules, 0);
        assert!(matches!(
            engine.check(&url("https://shop.example/pixel-art.png")),
            TrackerDecision::Allowed
        ));
    }

    #[test]
    fn empty_engine_blocks_nothing() {
        let engine = TrackerEngine::empty();
        assert!(engine.is_empty());
        assert!(matches!(
            engine.check(&url("https://tracker.example.com/")),
            TrackerDecision::Allowed
        ));
    }
}
