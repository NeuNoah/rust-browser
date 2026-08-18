//! URL scheme classification and allow-listing.
//!
//! Schemes are the first security boundary of a browser: a
//! `javascript:` URL in the address bar is an XSS vector, a `file:`
//! URL opened from web content leaks local files, and `data:` URLs can
//! smuggle content past navigational checks. Classification is
//! exhaustive: unknown schemes are explicitly represented instead of
//! being silently treated as HTTP.

use url::Url;

/// Classification of a URL scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemeKind {
    /// `http:` — permitted, but subject to HTTPS-first policy in Phase 6.
    Http,
    /// `https:` — preferred scheme.
    Https,
    /// `file:` — local file access. Only allowed from user-initiated
    /// navigation, never from web content (enforced by [`crate::navigation`]).
    File,
    /// `data:` — allowed for user-initiated navigation only when the
    /// media type is a safe text or image type.
    Data,
    /// `about:` — internal pages (e.g. `about:blank`).
    About,
    /// `blob:` — valid inside a renderer, never from the URL bar.
    Blob,
    /// `javascript:` — never executable via navigation.
    Javascript,
    /// `mailto:` — delegated to external handlers, never loaded as a page.
    Mailto,
    /// `chrome:`/`browser:` — internal browser pages (reserved).
    Internal,
    /// Anything else. Treated as untrusted; navigation is denied.
    Other,
}

impl SchemeKind {
    /// Returns `true` for schemes that may be entered in the URL bar and
    /// result in a page load.
    ///
    /// `blob:` and `javascript:` are deliberately excluded: they are
    /// renderer-internal concepts and must never come from user input.
    pub fn is_navigable_from_address_bar(self) -> bool {
        matches!(self, Self::Http | Self::Https | Self::File | Self::About)
    }

    /// Returns `true` for schemes that web content may navigate to.
    ///
    /// `file:` is excluded: content-initiated navigation to local files
    /// is a classic local file disclosure primitive.
    pub fn is_navigable_from_content(self) -> bool {
        matches!(self, Self::Http | Self::Https | Self::About)
    }

    /// Returns `true` for schemes that produce network traffic.
    pub fn is_network(self) -> bool {
        matches!(self, Self::Http | Self::Https)
    }
}

/// Classify the scheme of `url`.
///
/// Unknown or malformed schemes are reported as [`SchemeKind::Other`];
/// callers must treat `Other` as untrusted.
pub fn classify_scheme(url: &Url) -> SchemeKind {
    match url.scheme() {
        "http" => SchemeKind::Http,
        "https" => SchemeKind::Https,
        "file" => SchemeKind::File,
        "data" => SchemeKind::Data,
        "about" => SchemeKind::About,
        "blob" => SchemeKind::Blob,
        "javascript" => SchemeKind::Javascript,
        "mailto" => SchemeKind::Mailto,
        "chrome" | "browser" => SchemeKind::Internal,
        _ => SchemeKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(input: &str) -> Url {
        Url::parse(input).expect("test URL must parse")
    }

    #[test]
    fn classifies_common_schemes() {
        assert_eq!(
            classify_scheme(&url("http://example.com/")),
            SchemeKind::Http
        );
        assert_eq!(
            classify_scheme(&url("https://example.com/")),
            SchemeKind::Https
        );
        assert_eq!(
            classify_scheme(&url("file:///C:/tmp/a.txt")),
            SchemeKind::File
        );
        assert_eq!(
            classify_scheme(&url("data:text/plain,hi")),
            SchemeKind::Data
        );
        assert_eq!(classify_scheme(&url("about:blank")), SchemeKind::About);
        assert_eq!(
            classify_scheme(&url("blob:https://example.com/id")),
            SchemeKind::Blob
        );
        assert_eq!(
            classify_scheme(&url("javascript:alert(1)")),
            SchemeKind::Javascript
        );
        assert_eq!(classify_scheme(&url("mailto:a@b.c")), SchemeKind::Mailto);
        assert_eq!(
            classify_scheme(&url("chrome://settings")),
            SchemeKind::Internal
        );
    }

    #[test]
    fn unknown_scheme_is_other() {
        assert_eq!(
            classify_scheme(&url("gopher://example.com")),
            SchemeKind::Other
        );
        assert_eq!(
            classify_scheme(&url("ftp://example.com")),
            SchemeKind::Other
        );
        assert_eq!(classify_scheme(&url("weird://x")), SchemeKind::Other);
    }

    #[test]
    fn address_bar_navigability() {
        assert!(SchemeKind::Https.is_navigable_from_address_bar());
        assert!(SchemeKind::Http.is_navigable_from_address_bar());
        assert!(SchemeKind::File.is_navigable_from_address_bar());
        assert!(SchemeKind::About.is_navigable_from_address_bar());
        assert!(!SchemeKind::Javascript.is_navigable_from_address_bar());
        assert!(!SchemeKind::Data.is_navigable_from_address_bar());
        assert!(!SchemeKind::Blob.is_navigable_from_address_bar());
        assert!(!SchemeKind::Other.is_navigable_from_address_bar());
        assert!(!SchemeKind::Mailto.is_navigable_from_address_bar());
    }

    #[test]
    fn content_navigability() {
        assert!(SchemeKind::Https.is_navigable_from_content());
        assert!(SchemeKind::Http.is_navigable_from_content());
        assert!(!SchemeKind::File.is_navigable_from_content());
        assert!(!SchemeKind::Data.is_navigable_from_content());
        assert!(!SchemeKind::Javascript.is_navigable_from_content());
    }

    #[test]
    fn network_classification() {
        assert!(SchemeKind::Http.is_network());
        assert!(SchemeKind::Https.is_network());
        assert!(!SchemeKind::File.is_network());
        assert!(!SchemeKind::About.is_network());
    }
}
