//! Navigation orchestration: from user input to a validated command.
//!
//! [`normalize_input`] completes user input (`example.com` →
//! `https://example.com/`) and classifies it. The resulting
//! [`NavigationCommand`] is then checked against the security policy
//! before anything is handed to the engine.

use url::Url;

use browser_security::navigation::{NavigationDecision, NavigationPolicy};
use browser_security::schemes::SchemeKind;

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

    // Strip a leading "search terms → engine" ambiguity: a string
    // containing spaces cannot be a valid host, so we treat it as a
    // search query. No search engine is wired in yet; the query is
    // forwarded as a URL query to the default engine later (Phase 6).
    if input.chars().any(char::is_whitespace) {
        // No search integration yet: reject clearly.
        return Err(NavigationError::InvalidUrl);
    }

    // Already has a scheme? RFC 3986: a scheme must start with a
    // letter, so "192.168.0.1:8080" is a bare host with a port, not
    // a scheme.
    let has_scheme = input
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
    in_new_tab: bool,
) -> Result<NavigationCommand, NavigationError> {
    let url = normalize_input(input)?;
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
            command_for_input("example.com", &policy, false).unwrap(),
            NavigationCommand::Load(Url::parse("https://example.com/").unwrap())
        );
        assert_eq!(
            command_for_input("example.com", &policy, true).unwrap(),
            NavigationCommand::NewTab(Url::parse("https://example.com/").unwrap())
        );
        assert!(matches!(
            command_for_input("javascript:alert(1)", &policy, false),
            Err(NavigationError::Denied(_))
        ));
        assert!(matches!(
            command_for_input("data:text/html,x", &policy, false),
            Err(NavigationError::Denied(_))
        ));
        assert!(matches!(
            command_for_input("https://user:pass@example.com", &policy, false),
            Err(NavigationError::Denied(_))
        ));
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
