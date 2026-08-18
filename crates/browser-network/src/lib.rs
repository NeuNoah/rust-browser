//! `browser-network` — the request pipeline.
//!
//! Every network request passes through a pipeline of independent,
//! testable layers before it is handed to the engine:
//!
//! ```text
//! URL
//!  ↓
//! URL Validation        (browser-security)
//!  ↓
//! Security Policy       (schemes, credentials, mixed content)
//!  ↓
//! Privacy Policy        (cookie rules — Phase 9 enforcement)
//!  ↓
//! Tracker Blocking      (browser-privacy)
//!  ↓
//! Adblock               (Phase 7 — Brave adblock crate)
//!  ↓
//! Network Request
//!  ↓
//! Servo
//! ```
//!
//! Each layer is a [`Layer`]; the pipeline composes them and short-
//! circuits on the first block. The pipeline is pure: it performs no
//! I/O and blocks no threads, so it is fully unit-testable.

pub mod layers;
pub mod pipeline;
pub mod resource_type;

pub use layers::{default_pipeline, MixedContentLayer, SchemeValidationLayer, TrackerLayer};
pub use pipeline::{
    block_reason, Layer, LayerOutcome, PipelineDecision, RequestContext, RequestPipeline,
    RequestPipelineError,
};
pub use resource_type::ResourceType;
