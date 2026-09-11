//! Explicit, session-only adblock subscription updates.
//!
//! The catalog is fixed so this code is not a general URL fetcher. Downloads
//! are HTTPS-only, bounded before parsing, validated as ABP lists and compiled
//! off the UI thread. The caller swaps in the completed pipeline only after the
//! whole selected set succeeds.

use std::collections::HashSet;
use std::time::Duration;

use browser_network::{default_pipeline_with_adblock_lists, RequestPipeline};
use browser_privacy::trackers::TrackerEngine;
use ureq::{Agent, Proxy};

use crate::proxy::StartupProxy;

const MAX_LIST_BYTES: usize = 4 * 1024 * 1024;
const MAX_LIST_RULES: usize = 150_000;
const MAX_RULE_BYTES: usize = 16 * 1024;
const MIN_LIST_RULES: usize = 100;
const UPDATE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum SubscriptionId {
    EasyList,
    EasyPrivacy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CatalogEntry {
    pub id: SubscriptionId,
    pub name: &'static str,
    pub description: &'static str,
    pub url: &'static str,
    pub license: &'static str,
    pub homepage: &'static str,
}

pub(crate) const CATALOG: [CatalogEntry; 2] = [
    CatalogEntry {
        id: SubscriptionId::EasyList,
        name: "EasyList",
        description: "Community-maintained advertising filters",
        url: "https://easylist.to/easylist/easylist.txt",
        license: "GPL-3.0-or-later AND CC-BY-SA-3.0",
        homepage: "https://easylist.to/",
    },
    CatalogEntry {
        id: SubscriptionId::EasyPrivacy,
        name: "EasyPrivacy",
        description: "Optional tracking and telemetry filters",
        url: "https://easylist.to/easylist/easyprivacy.txt",
        license: "GPL-3.0-or-later AND CC-BY-SA-3.0",
        homepage: "https://easylist.to/",
    },
];

pub(crate) struct CompiledUpdate {
    pub selected: Vec<SubscriptionId>,
    pub pipeline: RequestPipeline,
    pub total_bytes: usize,
    pub total_rules: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum UpdateStatus {
    Idle,
    Updating,
    Applied {
        selected: Vec<SubscriptionId>,
        total_bytes: usize,
        total_rules: usize,
    },
    Failed(String),
}

impl UpdateStatus {
    pub fn is_updating(&self) -> bool {
        matches!(self, Self::Updating)
    }
}

pub(crate) fn compile_selected(
    selected: Vec<SubscriptionId>,
    startup_proxy: Option<&StartupProxy>,
) -> Result<CompiledUpdate, String> {
    validate_selection(&selected)?;

    let mut texts = Vec::with_capacity(selected.len());
    let mut total_bytes = 0usize;
    let mut total_rules = 0usize;
    if !selected.is_empty() {
        let agent = update_agent(startup_proxy)?;
        for id in &selected {
            let entry = catalog_entry(*id);
            let (text, rules) = download_list(&agent, entry)?;
            total_bytes += text.len();
            total_rules += rules;
            texts.push(text);
        }
    }

    let pipeline = default_pipeline_with_adblock_lists(
        TrackerEngine::builtin(),
        texts.iter().map(String::as_str),
    );
    Ok(CompiledUpdate {
        selected,
        pipeline,
        total_bytes,
        total_rules,
    })
}

fn validate_selection(selected: &[SubscriptionId]) -> Result<(), String> {
    if selected.len() > CATALOG.len() {
        return Err("Too many filter subscriptions were selected".to_owned());
    }
    let unique: HashSet<_> = selected.iter().copied().collect();
    if unique.len() != selected.len() {
        return Err("A filter subscription was selected more than once".to_owned());
    }
    Ok(())
}

fn catalog_entry(id: SubscriptionId) -> &'static CatalogEntry {
    CATALOG
        .iter()
        .find(|entry| entry.id == id)
        .expect("every subscription id belongs to the fixed catalog")
}

fn update_agent(startup_proxy: Option<&StartupProxy>) -> Result<Agent, String> {
    let proxy = startup_proxy
        .map(|proxy| {
            // The updater deliberately routes every catalog request through an
            // explicit startup proxy. Ignoring its bypass list is fail-closed:
            // it may reduce connectivity, but can never create a direct leak.
            Proxy::new(proxy.uri()).map_err(|error| format!("Invalid update proxy: {error}"))
        })
        .transpose()?;
    let config = Agent::config_builder()
        .https_only(true)
        .proxy(proxy)
        .max_redirects(0)
        .max_response_header_size(32 * 1024)
        .timeout_global(Some(UPDATE_TIMEOUT))
        .user_agent(concat!("rust-browser/", env!("CARGO_PKG_VERSION")))
        .accept("text/plain")
        .accept_encoding("")
        .build();
    Ok(Agent::new_with_config(config))
}

fn download_list(agent: &Agent, entry: &CatalogEntry) -> Result<(String, usize), String> {
    let mut response = agent
        .get(entry.url)
        .call()
        .map_err(|error| format!("{} download failed: {error}", entry.name))?;
    ensure_success_status(entry, response.status().as_u16())?;

    if response
        .body()
        .content_length()
        .is_some_and(|length| length > MAX_LIST_BYTES as u64)
    {
        return Err(format!("{} exceeds the 4 MiB size limit", entry.name));
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !content_type
        .split(';')
        .next()
        .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("text/plain"))
    {
        return Err(format!("{} did not return plain text", entry.name));
    }

    let bytes = response
        .body_mut()
        .with_config()
        .limit((MAX_LIST_BYTES + 1) as u64)
        .read_to_vec()
        .map_err(|error| format!("{} response could not be read: {error}", entry.name))?;
    if bytes.len() > MAX_LIST_BYTES {
        return Err(format!("{} exceeds the 4 MiB size limit", entry.name));
    }
    let (text, rule_count) = validate_filter_list(&bytes)
        .map_err(|error| format!("{} was rejected: {error}", entry.name))?;
    Ok((text, rule_count))
}

fn ensure_success_status(entry: &CatalogEntry, status: u16) -> Result<(), String> {
    if (200..=299).contains(&status) {
        Ok(())
    } else {
        Err(format!(
            "{} returned HTTP status {status}; redirects are not allowed",
            entry.name
        ))
    }
}

fn validate_filter_list(bytes: &[u8]) -> Result<(String, usize), String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "the list is not valid UTF-8".to_owned())?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.is_empty() {
        return Err("the list is empty".to_owned());
    }
    if text
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err("the list contains an unsupported control character".to_owned());
    }

    let mut lines = text.lines();
    let header = lines
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .ok_or_else(|| "the list is empty".to_owned())?;
    if !(header.starts_with("[Adblock Plus ") && header.ends_with(']')) {
        return Err("the ABP list header is missing".to_owned());
    }

    let mut rules = 0usize;
    for line in text.lines() {
        if line.len() > MAX_RULE_BYTES {
            return Err("a rule exceeds the 16 KiB line limit".to_owned());
        }
        let line = line.trim();
        if line.is_empty() || line.starts_with('!') || line.starts_with('[') {
            continue;
        }
        rules += 1;
        if rules > MAX_LIST_RULES {
            return Err("the list exceeds the 150,000-rule limit".to_owned());
        }
    }
    if rules < MIN_LIST_RULES {
        return Err("the list contains fewer than 100 network rules".to_owned());
    }
    Ok((text.to_owned(), rules))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_list(rule_count: usize) -> Vec<u8> {
        let mut text = String::from("\u{feff}[Adblock Plus 2.0]\r\n! Test list\r\n");
        for index in 0..rule_count {
            text.push_str(&format!("||ads{index}.example^\n"));
        }
        text.into_bytes()
    }

    #[test]
    fn catalog_is_fixed_unique_and_https_only() {
        let ids: HashSet<_> = CATALOG.iter().map(|entry| entry.id).collect();
        assert_eq!(ids.len(), CATALOG.len());
        assert!(CATALOG
            .iter()
            .all(|entry| entry.url.starts_with("https://")));
    }

    #[test]
    fn validation_accepts_utf8_bom_crlf_and_supported_headers() {
        let (text, rules) = validate_filter_list(&valid_list(MIN_LIST_RULES)).unwrap();
        assert!(text.starts_with("[Adblock Plus 2.0]"));
        assert_eq!(rules, MIN_LIST_RULES);

        let mut older_header = valid_list(MIN_LIST_RULES);
        let position = older_header
            .windows(b"2.0".len())
            .position(|window| window == b"2.0")
            .unwrap();
        older_header[position] = b'1';
        older_header[position + 2] = b'1';
        assert!(validate_filter_list(&older_header).is_ok());
    }

    #[test]
    fn validation_rejects_html_short_invalid_or_pathological_lists() {
        assert!(validate_filter_list(b"<html>not a list</html>").is_err());
        assert!(validate_filter_list(b"[Adblock Plus 2.0]\n||one.example^\n").is_err());
        assert!(validate_filter_list(b"[Adblock Plus 2.0]\n\0bad\n").is_err());

        let long_rule = format!("[Adblock Plus 2.0]\n||{}^\n", "a".repeat(MAX_RULE_BYTES));
        assert!(validate_filter_list(long_rule.as_bytes()).is_err());
    }

    #[test]
    fn selection_must_not_contain_duplicates() {
        assert!(validate_selection(&[SubscriptionId::EasyList]).is_ok());
        assert!(validate_selection(&[SubscriptionId::EasyList, SubscriptionId::EasyList]).is_err());
    }

    #[test]
    fn updater_disables_redirects_and_uses_only_an_explicit_proxy() {
        let direct = update_agent(None).unwrap();
        assert!(direct.config().proxy().is_none());
        assert!(direct.config().https_only());
        assert_eq!(direct.config().max_redirects(), 0);
        assert_eq!(direct.config().max_response_header_size(), 32 * 1024);

        let startup =
            StartupProxy::parse("http://proxy.example:8765", Some("easylist.to,127.0.0.1"))
                .unwrap();
        let proxied = update_agent(Some(&startup)).unwrap();
        assert!(proxied.config().proxy().is_some());
    }

    #[test]
    fn updater_rejects_redirect_and_error_statuses() {
        let entry = &CATALOG[0];
        assert!(ensure_success_status(entry, 200).is_ok());
        assert!(ensure_success_status(entry, 299).is_ok());
        assert!(ensure_success_status(entry, 301).is_err());
        assert!(ensure_success_status(entry, 308).is_err());
        assert!(ensure_success_status(entry, 500).is_err());
    }

    #[test]
    fn empty_selection_restores_the_builtin_pipeline_without_network() {
        let update = compile_selected(Vec::new(), None).unwrap();
        assert!(update.selected.is_empty());
        assert_eq!(update.total_bytes, 0);
        assert_eq!(update.total_rules, 0);
        assert_eq!(
            update.pipeline.layer_names(),
            vec![
                "scheme-validation",
                "private-network",
                "mixed-content",
                "tracker-blocking",
                "ad-blocking",
            ]
        );
    }

    #[test]
    #[ignore = "requires access to the official EasyList service"]
    fn official_catalog_downloads_validate_and_compile() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let update = compile_selected(
            vec![SubscriptionId::EasyList, SubscriptionId::EasyPrivacy],
            None,
        )
        .unwrap();
        assert_eq!(update.selected.len(), 2);
        assert!(update.total_bytes > 100_000);
        assert!(update.total_rules >= MIN_LIST_RULES * 2);
    }
}
