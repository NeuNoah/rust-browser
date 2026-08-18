//! The request pipeline: layers, context and decisions.

use url::Url;

use crate::ResourceType;

/// Full description of one request being evaluated.
#[derive(Debug, Clone)]
pub struct RequestContext {
    /// The request URL.
    pub url: Url,
    /// The URL of the page that initiated the request (the top-level
    /// document for top-level navigations).
    pub initiator: Option<Url>,
    /// What kind of resource this is.
    pub resource_type: ResourceType,
    /// Whether this is a top-level document load.
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
            resource_type,
            is_top_level,
        }
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
        if self.layers.iter().any(|l| l.name() == layer.name()) {
            return Err(RequestPipelineError::DuplicateLayer(layer.name()));
        }
        self.layers.push(Box::new(layer));
        Ok(self)
    }

    /// Evaluate one request against all layers.
    pub fn evaluate(&self, context: &RequestContext) -> PipelineDecision {
        for layer in &self.layers {
            match layer.check(context) {
                LayerOutcome::Pass => (),
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
}
