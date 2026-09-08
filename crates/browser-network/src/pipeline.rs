//! The request pipeline: layers, context and decisions.

use url::Url;

use crate::ResourceType;

/// Full description of one request being evaluated.
#[derive(Debug, Clone)]
pub struct RequestContext {
    /// The request URL.
    pub url: Url,
    /// Referrer metadata reported for the request. A page can suppress
    /// this with Referrer-Policy, so security layers must not treat it
    /// as a trusted top-level principal.
    pub initiator: Option<Url>,
    /// The committed top-level URL supplied by the embedder, when a
    /// request belongs to a WebView.
    pub top_level_url: Option<Url>,
    /// What kind of resource this is.
    pub resource_type: ResourceType,
    /// Whether the embedder has authenticated this as a top-level
    /// document load. Callers must not copy an engine flag that also
    /// labels iframe documents.
    pub is_top_level: bool,
}

impl RequestContext {
    pub fn new(
        url: Url,
        initiator: Option<Url>,
        resource_type: ResourceType,
        is_top_level: bool,
    ) -> Self {
        Self {
            url,
            initiator,
            top_level_url: None,
            resource_type,
            is_top_level,
        }
    }

    /// Attach the embedder's committed top-level URL. This is separate
    /// from referrer metadata so a page cannot erase the security
    /// context with `Referrer-Policy: no-referrer`.
    pub fn with_top_level_url(mut self, top_level_url: Option<Url>) -> Self {
        self.top_level_url = top_level_url;
        self
    }
}

/// The outcome of one layer's evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerOutcome {
    /// The layer does not block this request.
    Pass,
    /// The layer blocks the request, with a human-readable reason.
    Block(&'static str),
}

/// A single pipeline layer. Layers must be pure and deterministic.
pub trait Layer: Send + Sync {
    /// A stable name for logging and debugging.
    fn name(&self) -> &'static str;
    /// Evaluate the request.
    fn check(&self, context: &RequestContext) -> LayerOutcome;
}

/// The final decision of the pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineDecision {
    /// Every layer passed; the request may proceed.
    Allow,
    /// A layer blocked the request.
    Block {
        /// The layer that blocked.
        layer: &'static str,
        /// Why it blocked.
        reason: &'static str,
    },
}

/// Error type for pipeline construction/configuration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestPipelineError {
    /// A layer was added twice (duplicate name).
    #[error("duplicate layer: {0}")]
    DuplicateLayer(&'static str),
}

/// Convenience constructor for `Block` outcomes.
pub fn block_reason(reason: &'static str) -> LayerOutcome {
    LayerOutcome::Block(reason)
}

/// The composed pipeline. Layers run in insertion order and the
/// pipeline short-circuits on the first block.
#[derive(Default)]
pub struct RequestPipeline {
    layers: Vec<Box<dyn Layer>>,
    /// Index of the concrete tracker layer, when one was registered via
    /// `add_tracker_layer`. Keeping this identity structurally prevents
    /// an unrelated layer with the same display name from inheriting a
    /// privacy override.
    tracker_layer: Option<usize>,
}

impl RequestPipeline {
    /// A pipeline with no layers (allows everything).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Append a layer. Layer names must be unique.
    pub fn add_layer(
        &mut self,
        layer: impl Layer + 'static,
    ) -> Result<&mut Self, RequestPipelineError> {
        self.add_boxed_layer(Box::new(layer), false)
    }

    /// Append the concrete tracker layer that may be skipped by a
    /// per-site tracker override. At most one such layer is permitted.
    pub fn add_tracker_layer(
        &mut self,
        layer: crate::layers::TrackerLayer,
    ) -> Result<&mut Self, RequestPipelineError> {
        self.add_boxed_layer(Box::new(layer), true)
    }

    fn add_boxed_layer(
        &mut self,
        layer: Box<dyn Layer>,
        is_tracker_layer: bool,
    ) -> Result<&mut Self, RequestPipelineError> {
        if self.layers.iter().any(|l| l.name() == layer.name()) {
            return Err(RequestPipelineError::DuplicateLayer(layer.name()));
        }
        if is_tracker_layer {
            if self.tracker_layer.is_some() {
                return Err(RequestPipelineError::DuplicateLayer(layer.name()));
            }
            self.tracker_layer = Some(self.layers.len());
        }
        self.layers.push(layer);
        Ok(self)
    }

    /// Evaluate one request against all layers.
    pub fn evaluate(&self, context: &RequestContext) -> PipelineDecision {
        self.evaluate_with_tracker_override(context, false)
    }

    /// Evaluate every layer while allowing only a tracker-layer block
    /// to be overridden. Evaluation continues after that layer, so an
    /// opt-out from tracking protection can never bypass later
    /// security checks.
    pub fn evaluate_with_tracker_override(
        &self,
        context: &RequestContext,
        allow_trackers: bool,
    ) -> PipelineDecision {
        for (index, layer) in self.layers.iter().enumerate() {
            match layer.check(context) {
                LayerOutcome::Pass => (),
                LayerOutcome::Block(_) if allow_trackers && self.tracker_layer == Some(index) => {}
                LayerOutcome::Block(reason) => {
                    return PipelineDecision::Block {
                        layer: layer.name(),
                        reason,
                    };
                }
            }
        }
        PipelineDecision::Allow
    }

    /// The names of the layers in order.
    pub fn layer_names(&self) -> Vec<&'static str> {
        self.layers.iter().map(|l| l.name()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AllowAll;
    impl Layer for AllowAll {
        fn name(&self) -> &'static str {
            "allow-all"
        }
        fn check(&self, _: &RequestContext) -> LayerOutcome {
            LayerOutcome::Pass
        }
    }

    struct BlockScripts;
    impl Layer for BlockScripts {
        fn name(&self) -> &'static str {
            "block-scripts"
        }
        fn check(&self, context: &RequestContext) -> LayerOutcome {
            if context.resource_type == ResourceType::Script {
                LayerOutcome::Block("scripts are blocked by this test layer")
            } else {
                LayerOutcome::Pass
            }
        }
    }

    struct TrackerBlock;
    impl Layer for TrackerBlock {
        fn name(&self) -> &'static str {
            crate::layers::TRACKER_LAYER_NAME
        }
        fn check(&self, _: &RequestContext) -> LayerOutcome {
            LayerOutcome::Block("tracker")
        }
    }

    fn ctx(url: &str, rt: ResourceType) -> RequestContext {
        RequestContext::new(Url::parse(url).unwrap(), None, rt, false)
    }

    #[test]
    fn empty_pipeline_allows_everything() {
        let pipeline = RequestPipeline::empty();
        assert_eq!(
            pipeline.evaluate(&ctx("https://example.com/x.js", ResourceType::Script)),
            PipelineDecision::Allow
        );
    }

    #[test]
    fn layers_run_in_order_and_short_circuit() {
        let mut pipeline = RequestPipeline::empty();
        pipeline.add_layer(AllowAll).unwrap();
        pipeline.add_layer(BlockScripts).unwrap();

        let script = ctx("https://example.com/tracker.js", ResourceType::Script);
        assert_eq!(
            pipeline.evaluate(&script),
            PipelineDecision::Block {
                layer: "block-scripts",
                reason: "scripts are blocked by this test layer",
            }
        );

        let image = ctx("https://example.com/pic.png", ResourceType::Image);
        assert_eq!(pipeline.evaluate(&image), PipelineDecision::Allow);
    }

    #[test]
    fn duplicate_layer_is_rejected() {
        let mut pipeline = RequestPipeline::empty();
        pipeline.add_layer(AllowAll).unwrap();
        assert!(matches!(
            pipeline.add_layer(AllowAll),
            Err(RequestPipelineError::DuplicateLayer("allow-all"))
        ));
    }

    #[test]
    fn tracker_override_continues_through_later_layers() {
        let mut pipeline = RequestPipeline::empty();
        let tracker = crate::layers::TrackerLayer::new(
            browser_privacy::TrackerEngine::from_rules("host: tracker.example\n", 1).unwrap(),
        );
        pipeline.add_tracker_layer(tracker).unwrap();
        pipeline.add_layer(BlockScripts).unwrap();
        let script = ctx("https://tracker.example/script.js", ResourceType::Script);

        assert_eq!(
            pipeline.evaluate_with_tracker_override(&script, true),
            PipelineDecision::Block {
                layer: "block-scripts",
                reason: "scripts are blocked by this test layer",
            }
        );
        assert_eq!(
            pipeline.evaluate_with_tracker_override(&script, false),
            PipelineDecision::Block {
                layer: crate::layers::TRACKER_LAYER_NAME,
                reason: "request matches a known tracker",
            }
        );
    }

    #[test]
    fn a_layer_cannot_spoof_tracker_override_identity_by_name() {
        let mut pipeline = RequestPipeline::empty();
        pipeline.add_layer(TrackerBlock).unwrap();
        let request = ctx("https://tracker.example/pixel", ResourceType::Image);

        assert_eq!(
            pipeline.evaluate_with_tracker_override(&request, true),
            PipelineDecision::Block {
                layer: crate::layers::TRACKER_LAYER_NAME,
                reason: "tracker",
            }
        );
    }
}
