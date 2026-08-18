//! Concrete pipeline layers.
//!
//! The three layers that make up the default pipeline:
//!
//! 1. [`SchemeValidationLayer`] — the navigation/security policy from
//!    `browser-security`: no `javascript:`, `data:`, `file:`, `blob:`
//!    or credentials, regardless of source.
//! 2. [`TrackerLayer`] — the tracker engine from `browser-privacy`.
//!    Top-level navigations are exempt (users may visit a tracker's
//!    own site); every subresource from a tracker host is blocked.
//! 3. [`MixedContentLayer`] — no plain-HTTP subresources on HTTPS
//!    pages. (Full mixed-content policy, including upgrade-in-place,
//!    arrives with the networking phase.)

use browser_privacy::TrackerEngine;
use browser_security::navigation::{NavigationDecision, NavigationPolicy};

use crate::pipeline::{Layer, LayerOutcome, RequestContext};
/// Layer 1: scheme and credential validation for every request.
#[derive(Debug, Default)]
pub struct SchemeValidationLayer {
    policy: NavigationPolicy,
}

impl SchemeValidationLayer {
    pub fn new() -> Self {
        Self {
            policy: NavigationPolicy::default(),
        }
    }
}

impl Layer for SchemeValidationLayer {
    fn name(&self) -> &'static str {
        "scheme-validation"
    }

    fn check(&self, context: &RequestContext) -> LayerOutcome {
        match self.policy.check_content(&context.url) {
            NavigationDecision::Allow(_) => LayerOutcome::Pass,
            NavigationDecision::Deny(_) => LayerOutcome::Block("URL rejected by navigation policy"),
        }
    }
}

/// Layer 2: tracker blocking.
#[derive(Debug, Clone)]
pub struct TrackerLayer {
    engine: TrackerEngine,
}

impl TrackerLayer {
    pub fn new(engine: TrackerEngine) -> Self {
        Self { engine }
    }

    pub fn engine(&self) -> &TrackerEngine {
        &self.engine
    }
}

impl Layer for TrackerLayer {
    fn name(&self) -> &'static str {
        "tracker-blocking"
    }

    fn check(&self, context: &RequestContext) -> LayerOutcome {
        // Top-level navigations to a tracker's own site are allowed —
        // the user asked for that page. Everything else from a tracker
        // host is blocked.
        if context.is_top_level {
            return LayerOutcome::Pass;
        }
        match self.engine.check(&context.url) {
            browser_privacy::TrackerDecision::Allowed => LayerOutcome::Pass,
            browser_privacy::TrackerDecision::Blocked(_) => {
                LayerOutcome::Block("request matches a known tracker")
            }
        }
    }
}

/// Layer 3: mixed-content prevention.
#[derive(Debug, Default)]
pub struct MixedContentLayer;

impl Layer for MixedContentLayer {
    fn name(&self) -> &'static str {
        "mixed-content"
    }

    fn check(&self, context: &RequestContext) -> LayerOutcome {
        // A plain-HTTP request inside an HTTPS page.
        if context.url.scheme() != "http" {
            return LayerOutcome::Pass;
        }
        match &context.initiator {
            Some(initiator) if initiator.scheme() == "https" => {
                LayerOutcome::Block("HTTP subresource on an HTTPS page (mixed content)")
            }
            _ => LayerOutcome::Pass,
        }
    }
}

/// Convenience: the default pipeline with the standard layers in the
/// standard order. Equivalent to building it manually.
pub fn default_pipeline(tracker_engine: TrackerEngine) -> crate::RequestPipeline {
    let mut pipeline = crate::RequestPipeline::empty();
    pipeline
        .add_layer(SchemeValidationLayer::new())
        .expect("fresh pipeline cannot contain duplicates");
    pipeline
        .add_layer(TrackerLayer::new(tracker_engine))
        .expect("fresh pipeline cannot contain duplicates");
    pipeline
        .add_layer(MixedContentLayer)
        .expect("fresh pipeline cannot contain duplicates");
    pipeline
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    use crate::pipeline::PipelineDecision;
    use crate::{RequestContext, ResourceType};

    fn ctx(url: &str, initiator: Option<&str>, rt: ResourceType, top: bool) -> RequestContext {
        RequestContext::new(
            Url::parse(url).unwrap(),
            initiator.map(|i| Url::parse(i).unwrap()),
            rt,
            top,
        )
    }

    #[test]
    fn scheme_layer_blocks_dangerous_urls() {
        let layer = SchemeValidationLayer::new();
        assert_eq!(
            layer.check(&ctx(
                "javascript:alert(1)",
                Some("https://example.com"),
                ResourceType::Script,
                false
            )),
            LayerOutcome::Block("URL rejected by navigation policy")
        );
        assert_eq!(
            layer.check(&ctx(
                "data:text/html,<b>x</b>",
                Some("https://example.com"),
                ResourceType::Document,
                false
            )),
            LayerOutcome::Block("URL rejected by navigation policy")
        );
        assert_eq!(
            layer.check(&ctx(
                "https://user:pass@example.com/",
                None,
                ResourceType::Document,
                true
            )),
            LayerOutcome::Block("URL rejected by navigation policy")
        );
        assert_eq!(
            layer.check(&ctx(
                "https://example.com/app.js",
                Some("https://example.com"),
                ResourceType::Script,
                false
            )),
            LayerOutcome::Pass
        );
    }

    #[test]
    fn tracker_layer_blocks_subresources_only() {
        let engine = TrackerEngine::from_rules("host: tracker.example.com\n", 1).unwrap();
        let layer = TrackerLayer::new(engine);

        // Subresource from tracker → blocked.
        assert_eq!(
            layer.check(&ctx(
                "https://tracker.example.com/pixel.gif",
                Some("https://shop.example.com"),
                ResourceType::Image,
                false
            )),
            LayerOutcome::Block("request matches a known tracker")
        );
        // Top-level navigation to the same host → allowed.
        assert_eq!(
            layer.check(&ctx(
                "https://tracker.example.com/",
                None,
                ResourceType::Document,
                true
            )),
            LayerOutcome::Pass
        );
        // First-party subresource → allowed.
        assert_eq!(
            layer.check(&ctx(
                "https://shop.example.com/logo.png",
                Some("https://shop.example.com"),
                ResourceType::Image,
                false
            )),
            LayerOutcome::Pass
        );
    }

    #[test]
    fn mixed_content_layer_blocks_http_on_https() {
        let layer = MixedContentLayer;
        assert_eq!(
            layer.check(&ctx(
                "http://cdn.example.com/lib.js",
                Some("https://site.example.com"),
                ResourceType::Script,
                false
            )),
            LayerOutcome::Block("HTTP subresource on an HTTPS page (mixed content)")
        );
        // Same URL with an HTTP initiator → allowed.
        assert_eq!(
            layer.check(&ctx(
                "http://cdn.example.com/lib.js",
                Some("http://site.example.com"),
                ResourceType::Script,
                false
            )),
            LayerOutcome::Pass
        );
        // HTTPS subresource on HTTPS page → allowed.
        assert_eq!(
            layer.check(&ctx(
                "https://cdn.example.com/lib.js",
                Some("https://site.example.com"),
                ResourceType::Script,
                false
            )),
            LayerOutcome::Pass
        );
    }

    #[test]
    fn default_pipeline_composes_all_layers() {
        let engine = TrackerEngine::builtin();
        let pipeline = default_pipeline(engine);
        assert_eq!(
            pipeline.layer_names(),
            vec!["scheme-validation", "tracker-blocking", "mixed-content",]
        );

        // A real-world composite: tracker script on an HTTPS page.
        let decision = pipeline.evaluate(&ctx(
            "https://www.googletagmanager.com/gtm.js",
            Some("https://news.example.com"),
            ResourceType::Script,
            false,
        ));
        assert!(matches!(decision, PipelineDecision::Block { .. }));
    }
}
