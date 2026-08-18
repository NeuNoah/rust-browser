//! `browser-privacy` — engine-independent privacy layer.
//!
//! Ad blocking is not the same as tracking protection. This crate
//! implements the privacy primitives that the request pipeline consumes:
//!
//! - [`trackers`]: deterministic tracker matching against a curated,
//!   versioned, local list (hosts and URL patterns).
//! - [`cookies`]: cookie acceptance policy (third-party cookies are
//!   rejected by default).
//! - [`fingerprinting`]: fingerprinting protection configuration.
//!   Protection must reduce uniqueness — never randomize, which makes
//!   a device *more* unique.
//!
//! Nothing in this crate performs network I/O or blocks decision
//! making on external services.

pub mod cookies;
pub mod fingerprinting;
pub mod trackers;

pub use cookies::{CookieDecision, CookiePolicy, CookiePolicyError};
pub use fingerprinting::{FingerprintCategory, FingerprintingConfig, ProtectionLevel};
pub use trackers::{
    TrackerDecision, TrackerEngine, TrackerEngineError, TrackerMatch, TrackerRule, TrackerRuleKind,
};
