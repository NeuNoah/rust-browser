//! Tab state and the tab manager.

use url::Url;

/// An opaque, monotonically increasing tab identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabId(u64);

impl TabId {
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// Loading state of a tab's document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadState {
    /// No page loaded yet (new tab).
    Empty,
    /// A load is in progress.
    Loading,
    /// The load finished successfully.
    Loaded,
    /// The load failed.
    Failed,
}

/// Everything a tab knows about its current document.
#[derive(Debug, Clone)]
pub struct Tab {
    pub id: TabId,
    /// The URL currently shown (may be `about:blank` initially).
    pub url: Url,
    /// Document title, if reported by the engine.
    pub title: Option<String>,
    pub load_state: LoadState,
    /// Monotonic counter bumped on every committed navigation; used by
    /// the UI to avoid acting on stale navigation results.
    pub navigation_seq: u64,
}

impl Tab {
    fn new(id: TabId, url: Url) -> Self {
        Self {
            id,
            url,
            title: None,
            load_state: LoadState::Empty,
            navigation_seq: 0,
        }
    }
}

/// Errors from the core state model.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    #[error("no such tab")]
    NoSuchTab,
    #[error("the core has no active tab")]
    NoActiveTab,
}

/// The tab manager: owned state, no interior mutability.
#[derive(Debug, Default)]
pub struct TabManager {
    tabs: Vec<Tab>,
    active: Option<TabId>,
    next_id: u64,
}

impl TabManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a tab and activate it.
    pub fn create_tab(&mut self, url: Url) -> TabId {
        let id = TabId(self.next_id);
        self.next_id += 1;
        self.tabs.push(Tab::new(id, url));
        self.active = Some(id);
        id
    }

    /// Close a tab. The active tab falls back to the previous one, or
    /// `None` when the last tab was closed.
    pub fn close_tab(&mut self, id: TabId) -> Result<(), CoreError> {
        let index = self
            .tabs
            .iter()
            .position(|tab| tab.id == id)
            .ok_or(CoreError::NoSuchTab)?;
        self.tabs.remove(index);
        if self.active == Some(id) {
            self.active = None;
            // Activate the neighboring tab instead of leaving a hole.
            if !self.tabs.is_empty() {
                self.active = Some(self.tabs[index.saturating_sub(1)].id);
            }
        }
        Ok(())
    }

    /// Set the active tab.
    pub fn activate(&mut self, id: TabId) -> Result<(), CoreError> {
        if self.tabs.iter().any(|tab| tab.id == id) {
            self.active = Some(id);
            Ok(())
        } else {
            Err(CoreError::NoSuchTab)
        }
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|id| self.get(id))
    }

    pub fn active_tab_id(&self) -> Option<TabId> {
        self.active
    }

    pub fn get(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    pub fn get_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Apply a mutation to a tab; convenience for the core.
    pub fn update(&mut self, id: TabId, f: impl FnOnce(&mut Tab)) -> Result<(), CoreError> {
        let tab = self.get_mut(id).ok_or(CoreError::NoSuchTab)?;
        f(tab);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(input: &str) -> Url {
        Url::parse(input).expect("test URL must parse")
    }

    #[test]
    fn creates_and_activates_tabs() {
        let mut manager = TabManager::new();
        assert!(manager.is_empty());
        let a = manager.create_tab(url("https://a.example/"));
        let b = manager.create_tab(url("https://b.example/"));
        assert_eq!(manager.len(), 2);
        assert_eq!(manager.active_tab_id(), Some(b));
        assert_eq!(
            manager.active_tab().unwrap().url.as_str(),
            "https://b.example/"
        );
        assert!(a < b, "ids must be monotonic");
    }

    #[test]
    fn closes_and_falls_back() {
        let mut manager = TabManager::new();
        let a = manager.create_tab(url("https://a.example/"));
        let b = manager.create_tab(url("https://b.example/"));
        manager.close_tab(b).unwrap();
        assert_eq!(manager.active_tab_id(), Some(a));
        manager.close_tab(a).unwrap();
        assert!(manager.is_empty());
        assert_eq!(manager.active_tab_id(), None);
    }

    #[test]
    fn activate_validates() {
        let mut manager = TabManager::new();
        let a = manager.create_tab(url("https://a.example/"));
        assert!(manager.activate(TabId(999)).is_err());
        assert_eq!(manager.activate(a), Ok(()));
    }

    #[test]
    fn tab_state_transitions() {
        let mut manager = TabManager::new();
        let id = manager.create_tab(url("about:blank"));
        manager
            .update(id, |tab| {
                tab.load_state = LoadState::Loading;
                tab.navigation_seq += 1;
            })
            .unwrap();
        let tab = manager.get(id).unwrap();
        assert_eq!(tab.load_state, LoadState::Loading);
        assert_eq!(tab.navigation_seq, 1);
    }
}
