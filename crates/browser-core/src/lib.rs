//! `browser-core` — the browser's state model and navigation logic.
//!
//! This crate owns the tab model and the navigation orchestration. It
//! deliberately knows nothing about the rendering engine: tabs carry
//! plain state, and the UI layer maps engine events (Servo `WebView`s)
//! onto this model.
//!
//! No global state: everything lives in [`BrowserCore`], which the UI
//! owns exclusively.

#![forbid(unsafe_code)]

pub mod blocking_stats;
pub mod core;
pub mod navigation;
pub mod search;
pub mod tabs;

pub use blocking_stats::{BlockingKind, BlockingStatsStore, SiteBlockingStats};
pub use core::BrowserCore;
pub use navigation::{normalize_input, NavigationCommand, NavigationError};
pub use search::{SearchEngine, SearchError};
pub use tabs::{CoreError, LoadState, Tab, TabId, TabManager};
