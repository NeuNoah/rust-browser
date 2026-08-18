//! Resource type classification for a request.

/// What kind of resource a request is loading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceType {
    /// Top-level document navigation.
    Document,
    /// `<script>` elements and imports.
    Script,
    /// `<img>`, CSS background images, favicons.
    Image,
    /// CSS stylesheets.
    Stylesheet,
    /// Web fonts.
    Font,
    /// `<video>`/`<audio>` and media elements.
    Media,
    /// `XMLHttpRequest`.
    Xhr,
    /// `fetch()`.
    Fetch,
    /// Everything else (beacons, prefetch, subresources).
    Other,
}

impl ResourceType {
    /// Classify a request from its resource-type string as reported by
    /// the engine (e.g. `"script"`, `"image"`, `"document"`).
    pub fn from_engine_string(s: &str) -> Self {
        match s {
            "document" | "main_frame" | "sub_frame" => Self::Document,
            "script" => Self::Script,
            "image" | "imageset" | "favicon" => Self::Image,
            "stylesheet" => Self::Stylesheet,
            "font" => Self::Font,
            "media" | "video" | "audio" => Self::Media,
            "xmlhttprequest" | "xhr" => Self::Xhr,
            "fetch" | "beacon" => Self::Fetch,
            _ => Self::Other,
        }
    }

    /// Whether a block of this resource is "content-destroying" (the
    /// page layout depends on it) vs. "resource-level" (scripts,
    /// images, beacons). Trackers are almost always the latter.
    pub fn is_layout_critical(self) -> bool {
        matches!(self, Self::Document | Self::Stylesheet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_engine_strings() {
        assert_eq!(
            ResourceType::from_engine_string("script"),
            ResourceType::Script
        );
        assert_eq!(
            ResourceType::from_engine_string("image"),
            ResourceType::Image
        );
        assert_eq!(
            ResourceType::from_engine_string("document"),
            ResourceType::Document
        );
        assert_eq!(ResourceType::from_engine_string("xhr"), ResourceType::Xhr);
        assert_eq!(
            ResourceType::from_engine_string("fetch"),
            ResourceType::Fetch
        );
        assert_eq!(
            ResourceType::from_engine_string("weird/type"),
            ResourceType::Other
        );
    }

    #[test]
    fn layout_critical() {
        assert!(ResourceType::Document.is_layout_critical());
        assert!(ResourceType::Stylesheet.is_layout_critical());
        assert!(!ResourceType::Script.is_layout_critical());
        assert!(!ResourceType::Image.is_layout_critical());
    }
}
