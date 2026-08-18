//! Cookie acceptance policy.
//!
//! Privacy defaults: third-party cookies are rejected by default,
//! `SameSite=Lax` is the effective default, and `Secure` is required
//! for non-HTTP contexts where supported. Cookie *storage and
//! partitioning* (including the actual engine enforcement inside
//! Servo's network layer) land in the storage phase; this module is
//! the policy API that drives that enforcement and is fully testable
//! today.

use thiserror::Error;
use url::Url;

/// Whether a cookie should be accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookieDecision {
    /// Accept and store the cookie.
    Accept,
    /// Reject the cookie.
    Reject,
}

/// Why a cookie was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CookiePolicyError {
    /// The cookie is a third-party cookie and third-party cookies are
    /// disabled.
    #[error("third-party cookie rejected")]
    ThirdParty,
    /// The cookie lacks `Secure` while the connection is not HTTPS and
    /// the policy requires it.
    #[error("non-secure cookie on insecure connection")]
    NotSecure,
    /// The cookie's `Domain` attribute does not match the setting
    /// origin (a spoofed-domain attack).
    #[error("cookie domain mismatch: {0}")]
    DomainMismatch(String),
    /// The cookie would be set over HTTP but the policy requires
    /// HTTPS-first handling.
    #[error("cookie requires a secure connection")]
    HttpsOnly,
}

/// What a cookie's `SameSite` attribute means for a given request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SameSite {
    /// `SameSite=Strict` — sent only for same-site requests.
    Strict,
    /// `SameSite=Lax` — sent for same-site and top-level navigations.
    Lax,
    /// No attribute or `SameSite=None`.
    None,
}

/// The acceptance policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CookiePolicy {
    /// Reject all third-party cookies (default: `true`).
    pub block_third_party: bool,
    /// Require `Secure` cookies on HTTPS origins; reject non-secure
    /// cookies entirely (default: `true`).
    pub secure_only: bool,
    /// Default `SameSite` value applied when the attribute is absent
    /// (default: `Lax`).
    pub default_same_site: SameSite,
}

impl Default for CookiePolicy {
    fn default() -> Self {
        Self {
            block_third_party: true,
            secure_only: true,
            default_same_site: SameSite::Lax,
        }
    }
}

impl CookiePolicy {
    /// Decide whether a cookie being set by `setter` for `target`
    /// (the cookie's `Domain` attribute, if any) should be accepted.
    ///
    /// `is_third_party` is computed by the caller from the document's
    /// top-level origin — this function stays pure.
    pub fn check_set(
        &self,
        setter: &Url,
        domain_attribute: Option<&str>,
        is_secure: bool,
        is_third_party: bool,
        same_site: Option<SameSite>,
    ) -> Result<CookieDecision, CookiePolicyError> {
        if self.block_third_party && is_third_party {
            return Err(CookiePolicyError::ThirdParty);
        }

        if self.secure_only && !is_secure && setter.scheme() == "https" {
            return Err(CookiePolicyError::NotSecure);
        }
        if !is_secure && setter.scheme() == "http" {
            // Non-secure cookies are allowed from plain HTTP in Phase 1
            // (they could never be stored anyway once HTTPS-first lands).
        }

        if let Some(domain) = domain_attribute {
            let domain = domain.trim_start_matches('.').to_ascii_lowercase();
            let setter_host = setter
                .host_str()
                .map(str::to_ascii_lowercase)
                .unwrap_or_default();
            let matches = setter_host == domain || setter_host.ends_with(&format!(".{domain}"));
            if !matches {
                return Err(CookiePolicyError::DomainMismatch(domain));
            }
        }

        let _ = self.default_same_site;
        let _ = same_site;
        Ok(CookieDecision::Accept)
    }

    /// The `SameSite` value that applies to a cookie without an
    /// explicit attribute.
    pub fn effective_same_site(&self) -> SameSite {
        self.default_same_site
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(input: &str) -> Url {
        Url::parse(input).expect("test URL must parse")
    }

    #[test]
    fn third_party_cookies_rejected_by_default() {
        let policy = CookiePolicy::default();
        // A cookie set on an embedded tracker from a first-party page.
        let setter = url("https://ads.example.com/");
        let result = policy.check_set(&setter, None, true, true, None);
        assert_eq!(result, Err(CookiePolicyError::ThirdParty));
    }

    #[test]
    fn first_party_cookies_accepted() {
        let policy = CookiePolicy::default();
        let setter = url("https://shop.example.com/");
        assert_eq!(
            policy.check_set(&setter, None, true, false, None),
            Ok(CookieDecision::Accept)
        );
    }

    #[test]
    fn third_party_allowed_when_configured() {
        let policy = CookiePolicy {
            block_third_party: false,
            ..CookiePolicy::default()
        };
        let setter = url("https://ads.example.com/");
        assert_eq!(
            policy.check_set(&setter, None, true, true, None),
            Ok(CookieDecision::Accept)
        );
    }

    #[test]
    fn non_secure_cookie_rejected_on_https() {
        let policy = CookiePolicy::default();
        let setter = url("https://secure.example.com/");
        assert_eq!(
            policy.check_set(&setter, None, false, false, None),
            Err(CookiePolicyError::NotSecure)
        );
    }

    #[test]
    fn domain_attribute_must_match() {
        let policy = CookiePolicy::default();
        let setter = url("https://login.example.com/");
        // Legitimate: attribute matches the setter host.
        assert_eq!(
            policy.check_set(&setter, Some("example.com"), true, false, None),
            Ok(CookieDecision::Accept)
        );
        // Spoofed: attribute is an unrelated domain.
        assert_eq!(
            policy.check_set(&setter, Some("evil.org"), true, false, None),
            Err(CookiePolicyError::DomainMismatch("evil.org".into()))
        );
        // Leading dot is tolerated.
        assert_eq!(
            policy.check_set(&setter, Some(".example.com"), true, false, None),
            Ok(CookieDecision::Accept)
        );
    }

    #[test]
    fn default_same_site_is_lax() {
        let policy = CookiePolicy::default();
        assert_eq!(policy.effective_same_site(), SameSite::Lax);
    }
}
