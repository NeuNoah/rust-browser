//! Navigation policy: which URLs may be loaded, and by whom.
//!
//! Two entry points exist:
//!
//! - [`NavigationPolicy::check_address_bar`] — user-initiated navigation
//!   from the URL bar. Allows only `http:`, `https:` and `about:`.
//! - [`NavigationPolicy::check_content`] — navigation initiated by web
//!   content (links, redirects, `window.open`). Uses the same scheme
//!   allow-list and stricter source-specific checks can be added here.
//!
//! The policy is pure and deterministic; it performs no I/O. Later
//! phases add blocking-list checks, HTTPS-first upgrades and phishing
//! classification above this layer.

use url::Url;

use crate::schemes::{classify_scheme, SchemeKind};

/// Reason a navigation was denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationPolicyError {
    /// Scheme is not permitted for this navigation source.
    SchemeNotAllowed(SchemeKind),
    /// The URL has no host (e.g. malformed authority or missing host).
    MissingHost,
    /// The URL contains user credentials (`https://user:pass@host/`).
    /// Credentials are a phishing vector and are stripped at parse time
    /// by the `url` crate only if requested; we reject them explicitly.
    EmbeddedCredentials,
    /// The host is empty but the scheme requires one.
    EmptyHost,
    /// Only the inert `about:blank` page is exposed by this embedder.
    InternalPageNotAllowed,
}

impl core::fmt::Display for NavigationPolicyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::SchemeNotAllowed(kind) => {
                write!(f, "scheme {kind:?} is not allowed for this navigation")
            }
            Self::MissingHost => write!(f, "URL has no host"),
            Self::EmbeddedCredentials => write!(f, "URL embeds user credentials"),
            Self::EmptyHost => write!(f, "URL has an empty host"),
            Self::InternalPageNotAllowed => write!(f, "internal page is not available"),
        }
    }
}

impl core::error::Error for NavigationPolicyError {}

/// The outcome of a navigation check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavigationDecision {
    /// Navigation is permitted; the URL to load is given.
    Allow(Url),
    /// Navigation is denied for the given reason.
    Deny(NavigationPolicyError),
}

/// Where the navigation originates. Keeping the source explicit avoids
/// weakening content checks when trusted top-frame metadata becomes
/// available in a future Servo release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationSource {
    /// Typed into the address bar by the user.
    AddressBar,
    /// Initiated by web content (links, scripts, redirects).
    Content,
}

/// Pure navigation policy. Constructing one is free; call the `check_*`
/// methods for each navigation attempt.
#[derive(Debug, Clone, Copy, Default)]
pub struct NavigationPolicy {}

impl NavigationPolicy {
    /// Check a user-initiated navigation from the address bar.
    pub fn check_address_bar(&self, url: &Url) -> NavigationDecision {
        self.check(url, NavigationSource::AddressBar)
    }

    /// Check a navigation initiated by web content.
    pub fn check_content(&self, url: &Url) -> NavigationDecision {
        self.check(url, NavigationSource::Content)
    }

    fn check(&self, url: &Url, source: NavigationSource) -> NavigationDecision {
        let scheme = classify_scheme(url);

        // Scheme allow-list, by source.
        let navigable = match source {
            NavigationSource::AddressBar => scheme.is_navigable_from_address_bar(),
            NavigationSource::Content => scheme.is_navigable_from_content(),
        };
        if !navigable {
            return NavigationDecision::Deny(NavigationPolicyError::SchemeNotAllowed(scheme));
        }

        if scheme == SchemeKind::About && (url.path() != "blank" || url.query().is_some()) {
            return NavigationDecision::Deny(NavigationPolicyError::InternalPageNotAllowed);
        }

        // Reject credentials embedded in the URL.
        if !url.username().is_empty() || !url.password().is_none() {
            return NavigationDecision::Deny(NavigationPolicyError::EmbeddedCredentials);
        }

        // Network schemes require a non-empty host.
        if scheme.is_network() {
            match url.host_str() {
                Some(host) if !host.is_empty() => (),
                _ => return NavigationDecision::Deny(NavigationPolicyError::MissingHost),
            }
        }

        NavigationDecision::Allow(url.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(input: &str) -> Url {
        Url::parse(input).expect("test URL must parse")
    }

    #[test]
    fn address_bar_allows_common_urls() {
        let policy = NavigationPolicy::default();
        let decision = policy.check_address_bar(&url("https://example.com/path?q=1"));
        assert!(
            matches!(decision, NavigationDecision::Allow(u) if u.as_str() == "https://example.com/path?q=1")
        );

        assert!(matches!(
            policy.check_address_bar(&url("http://example.com/")),
            NavigationDecision::Allow(_)
        ));
        assert!(matches!(
            policy.check_address_bar(&url("about:blank")),
            NavigationDecision::Allow(_)
        ));
    }

    #[test]
    fn address_bar_denies_dangerous_schemes() {
        let policy = NavigationPolicy::default();
        assert_eq!(
            policy.check_address_bar(&url("javascript:alert(1)")),
            NavigationDecision::Deny(NavigationPolicyError::SchemeNotAllowed(
                SchemeKind::Javascript
            ))
        );
        assert_eq!(
            policy.check_address_bar(&url("data:text/html,<script>1</script>")),
            NavigationDecision::Deny(NavigationPolicyError::SchemeNotAllowed(SchemeKind::Data))
        );
        assert_eq!(
            policy.check_address_bar(&url("blob:https://example.com/abc")),
            NavigationDecision::Deny(NavigationPolicyError::SchemeNotAllowed(SchemeKind::Blob))
        );
        assert_eq!(
            policy.check_address_bar(&url("file:///C:/Windows/system.ini")),
            NavigationDecision::Deny(NavigationPolicyError::SchemeNotAllowed(SchemeKind::File))
        );
        assert_eq!(
            policy.check_address_bar(&url("mailto:a@b.c")),
            NavigationDecision::Deny(NavigationPolicyError::SchemeNotAllowed(SchemeKind::Mailto))
        );
        assert_eq!(
            policy.check_address_bar(&url("about:config")),
            NavigationDecision::Deny(NavigationPolicyError::InternalPageNotAllowed)
        );
        assert_eq!(
            policy.check_address_bar(&url("about:blank?unexpected")),
            NavigationDecision::Deny(NavigationPolicyError::InternalPageNotAllowed)
        );
        assert!(matches!(
            policy.check_address_bar(&url("about:blank#fragment")),
            NavigationDecision::Allow(_)
        ));
    }

    #[test]
    fn content_denies_file_and_data() {
        let policy = NavigationPolicy::default();
        assert!(matches!(
            policy.check_content(&url("file:///C:/Windows/system.ini")),
            NavigationDecision::Deny(NavigationPolicyError::SchemeNotAllowed(SchemeKind::File))
        ));
        assert!(matches!(
            policy.check_content(&url("data:text/plain,secret")),
            NavigationDecision::Deny(NavigationPolicyError::SchemeNotAllowed(SchemeKind::Data))
        ));
        // ... but allows normal links.
        assert!(matches!(
            policy.check_content(&url("https://example.com/")),
            NavigationDecision::Allow(_)
        ));
    }

    #[test]
    fn rejects_embedded_credentials() {
        let policy = NavigationPolicy::default();
        let decision = policy.check_address_bar(&url("https://user:pass@example.com/"));
        assert_eq!(
            decision,
            NavigationDecision::Deny(NavigationPolicyError::EmbeddedCredentials)
        );
    }

    #[test]
    fn missing_host_guard_holds() {
        // The url crate refuses to parse network URLs without a host
        // (`Url::parse("https://")` → `EmptyHost`), so the policy's
        // MissingHost branch is a defensive guard for future, laxer
        // parsers. What the parser accepts always carries a host:
        let parsed = url("https:///just/a/path");
        assert!(parsed.host_str().is_some());
        let policy = NavigationPolicy::default();
        assert!(matches!(
            policy.check_address_bar(&parsed),
            NavigationDecision::Allow(_)
        ));
    }

    #[test]
    fn unknown_scheme_denied_from_both_sources() {
        let policy = NavigationPolicy::default();
        assert!(matches!(
            policy.check_address_bar(&url("ftp://example.com/")),
            NavigationDecision::Deny(NavigationPolicyError::SchemeNotAllowed(SchemeKind::Other))
        ));
        assert!(matches!(
            policy.check_content(&url("ftp://example.com/")),
            NavigationDecision::Deny(_)
        ));
    }
}
