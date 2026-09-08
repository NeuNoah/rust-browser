//! Concrete pipeline layers.
//!
//! The five layers that make up the default pipeline:
//!
//! 1. [`SchemeValidationLayer`] — the navigation/security policy from
//!    `browser-security`: no `javascript:`, `data:`, `file:`, `blob:`
//!    or credentials, regardless of source.
//! 2. [`PrivateNetworkLayer`] — public pages cannot reach explicit
//!    local-name, loopback or numeric private-network targets.
//! 3. [`MixedContentLayer`] — no plain-HTTP resources on HTTPS
//!    pages. (Full mixed-content policy, including upgrade-in-place,
//!    arrives with the networking phase.)
//! 4. [`TrackerLayer`] — the tracker engine from `browser-privacy`.
//!    Servo's raw document flag is not trusted; only an
//!    embedder-authenticated top-level request is exempted here.
//! 5. [`AdblockLayer`] — Brave's `adblock` engine, compiled from the
//!    configured ABP-compatible network filter lists.

use adblock::lists::{ParseOptions, RuleTypes};
use adblock::request::Request as AdblockRequest;
use adblock::{Engine as AdblockEngine, FilterSet};
use browser_privacy::TrackerEngine;
use browser_security::navigation::{NavigationDecision, NavigationPolicy};
use url::{Host, Url};

use crate::pipeline::{Layer, LayerOutcome, RequestContext};

pub const TRACKER_LAYER_NAME: &str = "tracker-blocking";
pub const ADBLOCK_LAYER_NAME: &str = "ad-blocking";
const BUILTIN_ADBLOCK_LIST: &str = include_str!("../../../resources/filterlists/adblock.txt");
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

/// Layer 4: tracker blocking.
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
        TRACKER_LAYER_NAME
    }

    fn check(&self, context: &RequestContext) -> LayerOutcome {
        if context.is_top_level {
            return LayerOutcome::Pass;
        }
        match self.engine.check(&context.url) {
            browser_privacy::TrackerDecision::Allowed => LayerOutcome::Pass,
            browser_privacy::TrackerDecision::Blocked(_) => {
                // Tracker rules are third-party protections. Applying
                // them to the committed site's own host would make a
                // listed domain unvisitable and turn broad patterns
                // into false positives on routes such as `/pixel-art`.
                let same_top_level_host = context
                    .top_level_url
                    .as_ref()
                    .and_then(Url::host_str)
                    .zip(context.url.host_str())
                    .is_some_and(|(top, target)| {
                        top.trim_end_matches('.')
                            .eq_ignore_ascii_case(target.trim_end_matches('.'))
                    });
                if same_top_level_host {
                    LayerOutcome::Pass
                } else {
                    LayerOutcome::Block("request matches a known tracker")
                }
            }
        }
    }
}

/// Layer 5: ABP-compatible network filtering using Brave's adblock
/// engine. The engine is built once from complete lists; changing a
/// subscription therefore means constructing a replacement pipeline,
/// never mutating request-time state.
pub struct AdblockLayer {
    engine: AdblockEngine,
}

impl AdblockLayer {
    /// Build the small, deterministic list shipped with the browser.
    /// Remote subscriptions are intentionally not fetched here.
    pub fn builtin() -> Self {
        Self::from_filter_lists([BUILTIN_ADBLOCK_LIST])
    }

    /// Compile one or more complete ABP-compatible lists. Only network
    /// rules are retained because cosmetic filtering is not exposed by
    /// Servo's resource callback used by this pipeline.
    pub fn from_filter_lists<'a>(lists: impl IntoIterator<Item = &'a str>) -> Self {
        let mut filter_set = FilterSet::new(false);
        let options = ParseOptions {
            rule_types: RuleTypes::NetworkOnly,
            ..ParseOptions::default()
        };
        for list in lists {
            filter_set.add_filter_list(list.to_owned(), options);
        }
        Self {
            engine: AdblockEngine::new_with_filter_set(filter_set),
        }
    }

    fn request_type(context: &RequestContext) -> &'static str {
        match context.resource_type {
            crate::ResourceType::Document if context.is_top_level => "document",
            crate::ResourceType::Document => "sub_frame",
            crate::ResourceType::Script => "script",
            crate::ResourceType::Image => "image",
            crate::ResourceType::Stylesheet => "stylesheet",
            crate::ResourceType::Font => "font",
            crate::ResourceType::Media => "media",
            crate::ResourceType::Xhr | crate::ResourceType::Fetch => "xmlhttprequest",
            crate::ResourceType::Other => "other",
        }
    }
}

impl Layer for AdblockLayer {
    fn name(&self) -> &'static str {
        ADBLOCK_LAYER_NAME
    }

    fn check(&self, context: &RequestContext) -> LayerOutcome {
        // A network filter list must not make its own publisher's site
        // impossible to visit. Subframe documents remain filterable.
        if context.is_top_level {
            return LayerOutcome::Pass;
        }

        // Prefer the embedder-authenticated committed document. The
        // initiator is only fallback metadata and can be suppressed by
        // a page's Referrer-Policy.
        let source_url = context
            .top_level_url
            .as_ref()
            .or(context.initiator.as_ref())
            .map(Url::as_str)
            .unwrap_or("");
        let Ok(request) = AdblockRequest::new(
            context.url.as_str(),
            source_url,
            Self::request_type(context),
            "GET",
        ) else {
            // Scheme and URL validity are owned by earlier security
            // layers; an unsupported adblock request is not a reason to
            // override their decision or invent a second policy here.
            return LayerOutcome::Pass;
        };

        if self.engine.check_network_request(&request).should_block() {
            LayerOutcome::Block("request matches an ad-blocking rule")
        } else {
            LayerOutcome::Pass
        }
    }
}

/// Layer 2: prevent a public page from issuing requests to explicit
/// loopback/private-network targets. This is a URL-level PNA guard;
/// DNS rebinding still requires enforcement after name resolution.
#[derive(Debug, Default)]
pub struct PrivateNetworkLayer;

impl Layer for PrivateNetworkLayer {
    fn name(&self) -> &'static str {
        "private-network"
    }

    fn check(&self, context: &RequestContext) -> LayerOutcome {
        if context.is_top_level {
            return LayerOutcome::Pass;
        }
        if is_potentially_local_network_target(&context.url) {
            let mut has_explicit_local_source = false;
            for source in context.top_level_url.iter().chain(context.initiator.iter()) {
                match network_location(source) {
                    NetworkLocation::Public => {
                        return LayerOutcome::Block("public page requested a local-network target");
                    }
                    NetworkLocation::Local => has_explicit_local_source = true,
                    NetworkLocation::Unknown => {}
                }
            }
            if has_explicit_local_source {
                LayerOutcome::Pass
            } else {
                LayerOutcome::Block("unattributed or opaque request targeted the local network")
            }
        } else {
            LayerOutcome::Pass
        }
    }
}

#[derive(Clone, Copy)]
enum NetworkLocation {
    Local,
    Public,
    Unknown,
}

fn network_location(url: &Url) -> NetworkLocation {
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        NetworkLocation::Unknown
    } else if is_explicitly_local_network_url(url) {
        NetworkLocation::Local
    } else {
        NetworkLocation::Public
    }
}

fn is_potentially_local_network_target(url: &Url) -> bool {
    is_explicitly_local_network_url(url)
        || matches!(url.host(), Some(Host::Domain(domain)) if !domain.trim_end_matches('.').contains('.'))
}

fn is_explicitly_local_network_url(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.');
            domain.eq_ignore_ascii_case("localhost")
                || domain.to_ascii_lowercase().ends_with(".localhost")
                || domain.to_ascii_lowercase().ends_with(".local")
        }
        Some(Host::Ipv4(address)) => is_local_ipv4(address),
        Some(Host::Ipv6(address)) => {
            let segments = address.segments();
            address.to_ipv4_mapped().is_some_and(is_local_ipv4)
                || address.is_loopback()
                || address.is_unspecified()
                || address.is_multicast()
                || (segments[0] & 0xfe00) == 0xfc00
                || (segments[0] & 0xffc0) == 0xfe80
        }
        None => false,
    }
}

fn is_local_ipv4(address: std::net::Ipv4Addr) -> bool {
    let octets = address.octets();
    address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_unspecified()
        || address.is_multicast()
        || address.is_broadcast()
        || octets[0] == 0
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 198 && (18..=19).contains(&octets[1]))
}

/// Layer 3: mixed-content prevention.
#[derive(Debug, Default)]
pub struct MixedContentLayer;

impl Layer for MixedContentLayer {
    fn name(&self) -> &'static str {
        "mixed-content"
    }

    fn check(&self, context: &RequestContext) -> LayerOutcome {
        if context.is_top_level || context.url.scheme() != "http" {
            return LayerOutcome::Pass;
        }
        let has_secure_context = context
            .top_level_url
            .iter()
            .chain(context.initiator.iter())
            .any(|url| url.scheme() == "https");
        let has_plain_http_context = context
            .top_level_url
            .iter()
            .chain(context.initiator.iter())
            .any(|url| url.scheme() == "http");
        if has_secure_context {
            LayerOutcome::Block("HTTP resource in an HTTPS context (mixed content)")
        } else if !has_plain_http_context {
            LayerOutcome::Block("HTTP resource in an opaque or unattributed context")
        } else {
            LayerOutcome::Pass
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
        .add_layer(PrivateNetworkLayer)
        .expect("fresh pipeline cannot contain duplicates");
    pipeline
        .add_layer(MixedContentLayer)
        .expect("fresh pipeline cannot contain duplicates");
    pipeline
        .add_tracker_layer(TrackerLayer::new(tracker_engine))
        .expect("fresh pipeline cannot contain duplicates");
    pipeline
        .add_layer(AdblockLayer::builtin())
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
    fn tracker_layer_requires_an_authenticated_top_level_signal() {
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
        // An iframe document is not exempt merely because its resource
        // type is `Document`.
        assert_eq!(
            layer.check(&ctx(
                "https://tracker.example.com/",
                None,
                ResourceType::Document,
                false
            )),
            LayerOutcome::Block("request matches a known tracker")
        );
        // A top-level signal authenticated by the embedder is exempt.
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
    fn adblock_layer_blocks_third_party_ads_and_respects_exceptions() {
        let layer = AdblockLayer::from_filter_lists([
            "||ads.example^$third-party\n@@||ads.example/allowed.js$script,domain=shop.example\n",
        ]);

        let blocked = ctx(
            "https://ads.example/banner.js",
            None,
            ResourceType::Script,
            false,
        )
        .with_top_level_url(Some(Url::parse("https://shop.example/").unwrap()));
        assert_eq!(
            layer.check(&blocked),
            LayerOutcome::Block("request matches an ad-blocking rule")
        );

        let excepted = ctx(
            "https://ads.example/allowed.js",
            None,
            ResourceType::Script,
            false,
        )
        .with_top_level_url(Some(Url::parse("https://shop.example/").unwrap()));
        assert_eq!(layer.check(&excepted), LayerOutcome::Pass);

        let first_party = ctx(
            "https://ads.example/banner.js",
            None,
            ResourceType::Script,
            false,
        )
        .with_top_level_url(Some(Url::parse("https://ads.example/").unwrap()));
        assert_eq!(layer.check(&first_party), LayerOutcome::Pass);
    }

    #[test]
    fn adblock_uses_trusted_top_level_when_referrer_is_suppressed() {
        let layer = AdblockLayer::from_filter_lists(["||ads.example^$third-party\n"]);
        let request = ctx(
            "https://ads.example/banner.png",
            None,
            ResourceType::Image,
            false,
        )
        .with_top_level_url(Some(Url::parse("https://news.example/").unwrap()));

        assert_eq!(
            layer.check(&request),
            LayerOutcome::Block("request matches an ad-blocking rule")
        );
    }

    #[test]
    fn adblock_does_not_block_authenticated_top_level_navigation() {
        let layer = AdblockLayer::from_filter_lists(["||ads.example^\n"]);
        let request = ctx("https://ads.example/", None, ResourceType::Document, true);
        assert_eq!(layer.check(&request), LayerOutcome::Pass);
    }

    #[test]
    fn tracker_override_cannot_bypass_the_adblock_layer() {
        let tracker_engine =
            TrackerEngine::from_rules("host: ads.example\n", 1).expect("valid tracker rule");
        let pipeline = default_pipeline(tracker_engine);
        let request = ctx(
            "https://ads.example/banner.js",
            None,
            ResourceType::Script,
            false,
        )
        .with_top_level_url(Some(Url::parse("https://shop.example/").unwrap()));

        assert_eq!(
            pipeline.evaluate_with_tracker_override(&request, true),
            PipelineDecision::Block {
                layer: ADBLOCK_LAYER_NAME,
                reason: "request matches an ad-blocking rule",
            }
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
            LayerOutcome::Block("HTTP resource in an HTTPS context (mixed content)")
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
        // Merely being a Document is not enough to claim top-level.
        assert_eq!(
            layer.check(&ctx(
                "http://destination.example.com/",
                Some("https://site.example.com"),
                ResourceType::Document,
                false
            )),
            LayerOutcome::Block("HTTP resource in an HTTPS context (mixed content)")
        );
        // A signal authenticated by the embedder can exempt a genuine
        // first navigation in a fresh browser-created WebView.
        assert_eq!(
            layer.check(&ctx(
                "http://destination.example.com/",
                Some("https://site.example.com"),
                ResourceType::Document,
                true
            )),
            LayerOutcome::Pass
        );

        let opaque = ctx("http://cdn.example/x", None, ResourceType::Script, false)
            .with_top_level_url(Some(Url::parse("about:blank").unwrap()));
        assert_eq!(
            layer.check(&opaque),
            LayerOutcome::Block("HTTP resource in an opaque or unattributed context")
        );
    }

    #[test]
    fn trusted_top_level_url_survives_a_suppressed_referrer() {
        let layer = MixedContentLayer;
        let context = ctx(
            "http://cdn.example.com/lib.js",
            None,
            ResourceType::Script,
            false,
        )
        .with_top_level_url(Some(Url::parse("https://site.example/").unwrap()));
        assert_eq!(
            layer.check(&context),
            LayerOutcome::Block("HTTP resource in an HTTPS context (mixed content)")
        );
    }

    #[test]
    fn public_pages_cannot_reach_explicit_private_network_targets() {
        let layer = PrivateNetworkLayer;
        for target in [
            "http://127.0.0.1:8000/",
            "https://localhost/",
            "https://localhost./",
            "http://sub.localhost./",
            "http://192.168.1.1/",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://printer.local/",
            "http://router/",
        ] {
            let context = ctx(target, None, ResourceType::Fetch, false)
                .with_top_level_url(Some(Url::parse("https://public.example/").unwrap()));
            assert_eq!(
                layer.check(&context),
                LayerOutcome::Block("public page requested a local-network target"),
                "target {target}"
            );
        }

        let local_context = ctx("http://192.168.1.1/", None, ResourceType::Fetch, false)
            .with_top_level_url(Some(Url::parse("http://localhost/").unwrap()));
        assert_eq!(layer.check(&local_context), LayerOutcome::Pass);

        let inherited_public_context = ctx(
            "http://192.168.1.1/",
            Some("http://attacker.example/"),
            ResourceType::Fetch,
            false,
        )
        .with_top_level_url(Some(Url::parse("about:blank").unwrap()));
        assert_eq!(
            layer.check(&inherited_public_context),
            LayerOutcome::Block("public page requested a local-network target")
        );

        let opaque_context = ctx("http://127.0.0.1/", None, ResourceType::Fetch, false)
            .with_top_level_url(Some(Url::parse("about:blank").unwrap()));
        assert_eq!(
            layer.check(&opaque_context),
            LayerOutcome::Block("unattributed or opaque request targeted the local network")
        );

        let named_local_context = ctx("http://router/", None, ResourceType::Fetch, false)
            .with_top_level_url(Some(Url::parse("http://printer.local/").unwrap()));
        assert_eq!(layer.check(&named_local_context), LayerOutcome::Pass);

        let trusted_initial_navigation =
            ctx("http://192.168.1.1/", None, ResourceType::Document, true)
                .with_top_level_url(Some(Url::parse("about:blank").unwrap()));
        assert_eq!(layer.check(&trusted_initial_navigation), LayerOutcome::Pass);

        let ambiguous_source = ctx("http://192.168.1.1/", None, ResourceType::Fetch, false)
            .with_top_level_url(Some(Url::parse("http://attacker/").unwrap()));
        assert_eq!(
            layer.check(&ambiguous_source),
            LayerOutcome::Block("public page requested a local-network target")
        );

        assert_eq!(
            layer.check(&ctx("http://127.0.0.1/", None, ResourceType::Fetch, false)),
            LayerOutcome::Block("unattributed or opaque request targeted the local network")
        );
    }

    #[test]
    fn first_party_pattern_routes_are_not_false_positives() {
        let engine = TrackerEngine::from_rules("pattern: /tracking\n", 1).unwrap();
        let layer = TrackerLayer::new(engine);
        let first_party = ctx(
            "https://shop.example/tracking/settings",
            None,
            ResourceType::Document,
            false,
        )
        .with_top_level_url(Some(Url::parse("https://shop.example/").unwrap()));
        assert_eq!(layer.check(&first_party), LayerOutcome::Pass);

        let third_party = ctx(
            "https://metrics.example/tracking",
            None,
            ResourceType::Image,
            false,
        )
        .with_top_level_url(Some(Url::parse("https://shop.example/").unwrap()));
        assert_eq!(
            layer.check(&third_party),
            LayerOutcome::Block("request matches a known tracker")
        );
    }

    #[test]
    fn a_listed_host_remains_visitable_as_a_first_party() {
        let engine = TrackerEngine::from_rules("host: tracker.example\n", 1).unwrap();
        let layer = TrackerLayer::new(engine);
        let context = ctx(
            "https://tracker.example/page",
            None,
            ResourceType::Document,
            true,
        )
        .with_top_level_url(Some(Url::parse("https://tracker.example/page").unwrap()));
        assert_eq!(layer.check(&context), LayerOutcome::Pass);
    }

    #[test]
    fn default_pipeline_composes_all_layers() {
        let engine = TrackerEngine::builtin();
        let pipeline = default_pipeline(engine);
        assert_eq!(
            pipeline.layer_names(),
            vec![
                "scheme-validation",
                "private-network",
                "mixed-content",
                "tracker-blocking",
                "ad-blocking",
            ]
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
