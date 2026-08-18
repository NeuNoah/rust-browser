//! The top-level browser state owned by the UI.
//!
//! `BrowserCore` bundles the tab manager and the security/privacy
//! policies that the UI consults when handling navigation input. The
//! engine is deliberately absent: the UI maps engine events onto this
//! state.

use url::Url;

use crate::navigation::{command_for_input, NavigationCommand, NavigationError};
use crate::tabs::{CoreError, LoadState, TabId, TabManager};
use browser_security::navigation::NavigationPolicy;

/// The root state object. Owned exclusively by the UI thread.
#[derive(Debug)]
pub struct BrowserCore {
    pub tabs: TabManager,
    pub navigation_policy: NavigationPolicy,
}

impl Default for BrowserCore {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserCore {
    pub fn new() -> Self {
        Self {
            tabs: TabManager::new(),
            navigation_policy: NavigationPolicy::default(),
        }
    }

    /// Translate raw address-bar input into a validated command.
    pub fn command_from_input(
        &self,
        input: &str,
        in_new_tab: bool,
    ) -> Result<NavigationCommand, NavigationError> {
        command_for_input(input, &self.navigation_policy, in_new_tab)
    }

    /// Create the initial tab of a fresh session.
    pub fn start_session(&mut self) -> TabId {
        self.tabs.create_tab(crate::navigation::default_start_url())
    }

    /// Record that a load started for `tab`.
    pub fn load_started(&mut self, tab: TabId) -> Result<(), CoreError> {
        self.tabs.update(tab, |tab| {
            tab.load_state = LoadState::Loading;
            tab.navigation_seq += 1;
        })
    }

    /// Record a committed URL (from the engine's location-changed
    /// event) for `tab`.
    pub fn location_changed(&mut self, tab: TabId, url: Url) -> Result<(), CoreError> {
        self.tabs.update(tab, |tab| {
            tab.url = url;
        })
    }

    /// Record that the load finished for `tab`.
    pub fn load_finished(&mut self, tab: TabId) -> Result<(), CoreError> {
        self.tabs.update(tab, |tab| {
            tab.load_state = LoadState::Loaded;
        })
    }

    /// Record that the load failed for `tab`.
    pub fn load_failed(&mut self, tab: TabId) -> Result<(), CoreError> {
        self.tabs.update(tab, |tab| {
            tab.load_state = LoadState::Failed;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_lifecycle() {
        let mut core = BrowserCore::new();
        let tab = core.start_session();
        assert_eq!(core.tabs.active_tab_id(), Some(tab));
        assert_eq!(core.tabs.get(tab).unwrap().url.as_str(), "about:blank");

        let command = core
            .command_from_input("https://example.com", false)
            .unwrap();
        let NavigationCommand::Load(url) = command else {
            panic!("expected Load command");
        };
        core.load_started(tab).unwrap();
        core.location_changed(tab, url).unwrap();
        core.load_finished(tab).unwrap();
        let tab = core.tabs.get(tab).unwrap();
        assert_eq!(tab.load_state, LoadState::Loaded);
        assert_eq!(tab.url.as_str(), "https://example.com/");
        assert_eq!(tab.navigation_seq, 1);
    }

    #[test]
    fn denied_input_does_not_touch_state() {
        let mut core = BrowserCore::new();
        let tab = core.start_session();
        let before = core.tabs.get(tab).unwrap().clone();
        assert!(core
            .command_from_input("javascript:alert(1)", false)
            .is_err());
        let after = core.tabs.get(tab).unwrap();
        assert_eq!(after.url, before.url);
        assert_eq!(after.load_state, before.load_state);
        assert_eq!(after.navigation_seq, before.navigation_seq);
    }
}
