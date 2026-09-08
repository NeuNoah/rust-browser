//! Explicit, session-only search provider selection.
//!
//! No provider is selected by default. Search URLs are constructed only
//! after a user choice and only for a bounded query submitted through the
//! address bar; there is no suggestion or keystroke network path.

use url::Url;

const MAX_SEARCH_QUERY_CHARS: usize = 512;

/// Search providers whose query endpoints are built into the browser.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SearchEngine {
    /// Do not forward address-bar text to any search provider.
    #[default]
    Disabled,
    DuckDuckGo,
    Brave,
}

impl SearchEngine {
    /// Stable order used by the settings UI.
    pub const CHOICES: [Self; 3] = [Self::Disabled, Self::DuckDuckGo, Self::Brave];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Disabled => "Disabled (no query forwarding)",
            Self::DuckDuckGo => "DuckDuckGo",
            Self::Brave => "Brave Search",
        }
    }

    /// Construct a provider URL without ever interpolating raw text into
    /// the URL syntax. `url` percent-encodes the query parameter.
    pub fn search_url(self, raw_query: &str) -> Result<Url, SearchError> {
        if self == Self::Disabled {
            return Err(SearchError::Disabled);
        }
        if raw_query.chars().any(char::is_control) {
            return Err(SearchError::ControlCharacter);
        }
        let query = raw_query.split_whitespace().collect::<Vec<_>>().join(" ");
        if query.is_empty() {
            return Err(SearchError::EmptyQuery);
        }
        if query.chars().count() > MAX_SEARCH_QUERY_CHARS {
            return Err(SearchError::QueryTooLong);
        }

        let (base, parameter) = match self {
            Self::Disabled => unreachable!("disabled provider returned above"),
            Self::DuckDuckGo => ("https://duckduckgo.com/", "q"),
            Self::Brave => ("https://search.brave.com/search", "q"),
        };
        let mut url = Url::parse(base).expect("static search provider URL must parse");
        url.query_pairs_mut().append_pair(parameter, &query);
        Ok(url)
    }
}

/// Why address-bar text could not become a search request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SearchError {
    #[error("search is disabled; choose a provider in Settings")]
    Disabled,
    #[error("search query is empty")]
    EmptyQuery,
    #[error("search query contains a control character")]
    ControlCharacter,
    #[error("search query is too long")]
    QueryTooLong,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_is_disabled_by_default() {
        assert_eq!(SearchEngine::default(), SearchEngine::Disabled);
        assert_eq!(
            SearchEngine::default().search_url("private query"),
            Err(SearchError::Disabled)
        );
    }

    #[test]
    fn providers_encode_one_bounded_query_parameter() {
        for engine in [SearchEngine::DuckDuckGo, SearchEngine::Brave] {
            let url = engine.search_url("  rust & privacy  ").unwrap();
            assert_eq!(url.scheme(), "https");
            assert_eq!(
                url.query_pairs().collect::<Vec<_>>(),
                vec![("q".into(), "rust & privacy".into())]
            );
        }
    }

    #[test]
    fn queries_are_empty_control_free_and_bounded() {
        assert_eq!(
            SearchEngine::DuckDuckGo.search_url("   "),
            Err(SearchError::EmptyQuery)
        );
        assert_eq!(
            SearchEngine::DuckDuckGo.search_url("line\nbreak"),
            Err(SearchError::ControlCharacter)
        );
        assert_eq!(
            SearchEngine::DuckDuckGo.search_url(&"x".repeat(MAX_SEARCH_QUERY_CHARS + 1)),
            Err(SearchError::QueryTooLong)
        );
    }
}
