//! `browser-security` — engine-independent security policies.
//!
//! This crate implements security-relevant policy decisions that must be
//! testable in isolation and must never depend on the rendering engine:
//!
//! - [`schemes`]: classification and allow-listing of URL schemes.
//! - [`navigation`]: which URLs may be navigated to from the URL bar and
//!   from web content.
//! - [`download`]: safe handling of download file names and target paths.
//!
//! Nothing in this crate performs network I/O or parses untrusted input
//! with ad-hoc string manipulation: all URL handling goes through the
//! `url` crate.

#![forbid(unsafe_code)]

pub mod download;
pub mod navigation;
pub mod schemes;

pub use download::{
    sanitize_filename, DownloadPolicy, DownloadPolicyError, DownloadReceipt, SafeDownloadError,
    SafeDownloadTarget, SafeDownloadWriter,
};
pub use navigation::{NavigationDecision, NavigationPolicy, NavigationPolicyError};
pub use schemes::{classify_scheme, SchemeKind};
