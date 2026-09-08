//! Navigation orchestration: from user input to a validated command.
//!
//! [`normalize_input`] completes user input (`example.com` →
//! `https://example.com/`) and classifies it. The resulting
//! [`NavigationCommand`] is then checked against the security policy
//! before anything is handed to the engine.

use url::{Host, Url};

use browser_security::navigation::{NavigationDecision, NavigationPolicy};
use browser_security::schemes::SchemeKind;

use crate::search::{SearchEngine, SearchError};

/// A validated navigation request, ready to be executed by the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavigationCommand {
    /// Load this URL in the active tab.
    Load(Url),
    /// Navigate back in history.
    Back,
    /// Navigate forward in history.
    Forward,
    /// Reload the current page.
    Reload,
    /// Open a new tab with this URL.
    NewTab(Url),
}

/// Why navigation input was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NavigationError {
    /// The input could not be parsed as a URL.
    #[error("cannot parse URL")]
    InvalidUrl,
    /// The input was empty.
    #[error("empty input")]
    EmptyInput,
    /// Search-like input could not be forwarded under the current setting.
    #[error(transparent)]
    Search(#[from] SearchError),
    /// The URL failed the navigation policy.
    #[error("navigation denied: {0}")]
    Denied(browser_security::navigation::NavigationPolicyError),
}

/// Complete user input into a URL. Bare host names get HTTPS (never
/// HTTP-first); anything with a scheme is passed through verbatim.
pub fn normalize_input(input: &str) -> Result<Url, NavigationError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(NavigationError::EmptyInput);
    }

    // Keep URL normalization network-neutral. `command_for_input` may
    // separately classify whitespace as a query, but only after the user
    // has selected a provider. URL-only settings call this function directly.
    if input.chars().any(char::is_whitespace) {
        return Err(NavigationError::InvalidUrl);
    }

    // Already has a scheme? RFC 3986: a scheme must start with a
    // letter, so "192.168.0.1:8080" is a bare host with a port, not
    // a scheme.
    let looks_like_host_with_port = input.split_once(':').is_some_and(|(host, remainder)| {
        let port = remainder.split(['/', '?', '#']).next().unwrap_or_default();
        let host_is_unambiguous = match Host::parse(host) {
            Ok(Host::Domain(domain)) => {
                domain.contains('.') || domain.eq_ignore_ascii_case("localhost")
            }
            Ok(Host::Ipv4(_) | Host::Ipv6(_)) => true,
            Err(_) => false,
        };
        host_is_unambiguous
            && !port.is_empty()
            && port.chars().all(|character| character.is_ascii_digit())
            && port.parse::<u16>().is_ok()
    });
    let has_scheme = !looks_like_host_with_port
        && input
            .split_once(':')
            .map(|(scheme, _)| {
                let mut chars = scheme.chars();
                match chars.next() {
                    Some(first) if first.is_ascii_alphabetic() => {}
                    _ => return false,
                }
                chars.all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
            })
            .unwrap_or(false);

    let candidate = if has_scheme {
        input.to_string()
    } else {
        // Bare host or path: default to HTTPS.
        format!("https://{input}")
    };

    Url::parse(&candidate).map_err(|_| NavigationError::InvalidUrl)
}

/// Validate a completed input and produce a navigation command for the
/// active tab.
pub fn command_for_input(
    input: &str,
    policy: &NavigationPolicy,
    search_engine: SearchEngine,
    in_new_tab: bool,
) -> Result<NavigationCommand, NavigationError> {
    let url = match normalize_input(input) {
        Ok(url) => url,
        Err(NavigationError::InvalidUrl) => match search_query_from_input(input) {
            Some(query) => search_engine.search_url(query)?,
            None => return Err(NavigationError::InvalidUrl),
        },
        Err(error) => return Err(error),
    };
    command_for_url(url, policy, in_new_tab)
}

/// Validate URL-only input. Unlike [`command_for_input`], this never consults
/// a search provider and is suitable for settings and command-line fields.
pub fn command_for_url_input(
    input: &str,
    policy: &NavigationPolicy,
    in_new_tab: bool,
) -> Result<NavigationCommand, NavigationError> {
    command_for_url(normalize_input(input)?, policy, in_new_tab)
}

fn command_for_url(
    url: Url,
    policy: &NavigationPolicy,
    in_new_tab: bool,
) -> Result<NavigationCommand, NavigationError> {
    match policy.check_address_bar(&url) {
        NavigationDecision::Allow(url) => {
            if in_new_tab {
                Ok(NavigationCommand::NewTab(url))
            } else {
                Ok(NavigationCommand::Load(url))
            }
        }
        NavigationDecision::Deny(reason) => Err(NavigationError::Denied(reason)),
    }
}

/// Recognize deliberate search input without forwarding malformed explicit
/// URLs. Whitespace marks a normal query; `?term` supports one-word queries.
fn search_query_from_input(input: &str) -> Option<&str> {
    let input = input.trim();
    if let Some(forced) = input.strip_prefix('?') {
        return Some(forced.trim());
    }
    if !input.chars().any(char::is_whitespace) || input.contains("://") {
        return None;
    }
    Some(input)
}

/// The default start page for new tabs. `about:blank` is the only
/// network-free, privacy-neutral choice until new-tab content exists.
pub fn default_start_url() -> Url {
    Url::parse("about:blank").expect("static URL must parse")
}

/// True when the URL is one of the built-in browser pages.
pub fn is_internal_page(url: &Url) -> bool {
    matches!(
        browser_security::schemes::classify_scheme(url),
        SchemeKind::About | SchemeKind::Internal
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_bare_hosts_with_https() {
        assert_eq!(
            normalize_input("example.com").unwrap().as_str(),
            "https://example.com/"
        );
        assert_eq!(
            normalize_input("example.com/path").unwrap().as_str(),
            "https://example.com/path"
        );
        assert_eq!(
            normalize_input("192.168.0.1:8080").unwrap().as_str(),
            "https://192.168.0.1:8080/"
        );
        assert_eq!(
            normalize_input("example.com:8443/path").unwrap().as_str(),
            "https://example.com:8443/path"
        );
        assert_eq!(
            normalize_input("localhost:3000").unwrap().as_str(),
            "https://localhost:3000/"
        );
        assert_eq!(
            normalize_input("[::1]:3000/path").unwrap().as_str(),
            "https://[::1]:3000/path"
        );
    }

    #[test]
    fn reserved_schemes_are_never_reinterpreted_as_host_port_pairs() {
        for input in ["javascript:123", "about:80", "data:443"] {
            assert_eq!(
                normalize_input(input).unwrap().scheme(),
                input.split(':').next().unwrap()
            );
        }

        let policy = NavigationPolicy::default();
        assert!(matches!(
            command_for_input("javascript:123", &policy, SearchEngine::Disabled, false),
            Err(NavigationError::Denied(_))
        ));
        assert!(matches!(
            command_for_input("about:80", &policy, SearchEngine::Disabled, false),
            Err(NavigationError::Denied(_))
        ));
    }

    #[test]
    fn passes_urls_with_schemes_through() {
        assert_eq!(
            normalize_input("https://example.com/").unwrap().as_str(),
            "https://example.com/"
        );
        assert_eq!(
            normalize_input("about:blank").unwrap().as_str(),
            "about:blank"
        );
        // Unsupported schemes still parse — the policy layer rejects them.
        assert_eq!(
            normalize_input("javascript:alert(1)").unwrap().as_str(),
            "javascript:alert(1)"
        );
    }

    #[test]
    fn rejects_empty_and_whitespace() {
        assert!(matches!(
            normalize_input(""),
            Err(NavigationError::EmptyInput)
        ));
        assert!(matches!(
            normalize_input("   "),
            Err(NavigationError::EmptyInput)
        ));
        assert!(matches!(
            normalize_input("hello world"),
            Err(NavigationError::InvalidUrl)
        ));
        assert!(matches!(
            normalize_input("://"),
            Err(NavigationError::InvalidUrl)
        ));
    }

    #[test]
    fn command_for_input_validates() {
        let policy = NavigationPolicy::default();
        assert_eq!(
            command_for_input("example.com", &policy, SearchEngine::Disabled, false).unwrap(),
            NavigationCommand::Load(Url::parse("https://example.com/").unwrap())
        );
        assert_eq!(
            command_for_input("example.com", &policy, SearchEngine::Disabled, true).unwrap(),
            NavigationCommand::NewTab(Url::parse("https://example.com/").unwrap())
        );
        assert!(matches!(
            command_for_input(
                "javascript:alert(1)",
                &policy,
                SearchEngine::Disabled,
                false
            ),
            Err(NavigationError::Denied(_))
        ));
        assert!(matches!(
            command_for_input("data:text/html,x", &policy, SearchEngine::Disabled, false),
            Err(NavigationError::Denied(_))
        ));
        assert!(matches!(
            command_for_input(
                "https://user:pass@example.com",
                &policy,
                SearchEngine::Disabled,
                false
            ),
            Err(NavigationError::Denied(_))
        ));
    }

    #[test]
    fn query_forwarding_requires_an_explicit_provider_choice() {
        let policy = NavigationPolicy::default();
        assert!(matches!(
            command_for_input("rust privacy", &policy, SearchEngine::Disabled, false),
            Err(NavigationError::Search(SearchError::Disabled))
        ));

        let command =
            command_for_input("rust privacy", &policy, SearchEngine::DuckDuckGo, false).unwrap();
        let NavigationCommand::Load(url) = command else {
            panic!("expected search load");
        };
        assert_eq!(url.host_str(), Some("duckduckgo.com"));
        assert_eq!(
            url.query_pairs().find(|(key, _)| key == "q").unwrap().1,
            "rust privacy"
        );
    }

    #[test]
    fn forced_single_word_search_is_supported_without_leaking_bad_urls() {
        let policy = NavigationPolicy::default();
        assert!(command_for_input("?rust", &policy, SearchEngine::Brave, false).is_ok());
        assert!(matches!(
            command_for_input(
                "https://example.com/bad path",
                &policy,
                SearchEngine::Brave,
                false
            ),
            Err(NavigationError::InvalidUrl)
        ));
    }

    #[test]
    fn url_only_fields_never_turn_text_into_a_search() {
        let policy = NavigationPolicy::default();
        assert_eq!(
            command_for_url_input("private query", &policy, false),
            Err(NavigationError::InvalidUrl)
        );
    }

    #[test]
    fn internal_page_detection() {
        assert!(is_internal_page(&Url::parse("about:blank").unwrap()));
        assert!(!is_internal_page(
            &Url::parse("https://example.com/").unwrap()
        ));
    }

    #[test]
    fn start_page_is_blank() {
        assert_eq!(default_start_url().as_str(), "about:blank");
    }
}
