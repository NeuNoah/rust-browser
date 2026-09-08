//! Bounded, session-only per-site blocking statistics.

use std::collections::{HashMap, VecDeque};

/// Why a request was counted as blocked for the visible site.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockingKind {
    Tracker,
    Advertisement,
}

/// Counts shown for one normalized top-level host.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SiteBlockingStats {
    trackers: u64,
    advertisements: u64,
}

impl SiteBlockingStats {
    pub fn trackers(self) -> u64 {
        self.trackers
    }

    pub fn advertisements(self) -> u64 {
        self.advertisements
    }

    pub fn total(self) -> u64 {
        self.trackers.saturating_add(self.advertisements)
    }

    fn record(&mut self, kind: BlockingKind) {
        let counter = match kind {
            BlockingKind::Tracker => &mut self.trackers,
            BlockingKind::Advertisement => &mut self.advertisements,
        };
        *counter = counter.saturating_add(1);
    }
}

/// Session storage with a fixed upper bound on attacker-influenced hosts.
#[derive(Debug)]
pub struct BlockingStatsStore {
    sites: HashMap<String, SiteBlockingStats>,
    insertion_order: VecDeque<String>,
    max_sites: usize,
}

impl BlockingStatsStore {
    pub fn new(max_sites: usize) -> Self {
        Self {
            sites: HashMap::new(),
            insertion_order: VecDeque::new(),
            max_sites,
        }
    }

    /// Record one block for an already-normalized top-level host.
    /// Empty or overlong values fail closed instead of consuming capacity.
    pub fn record(&mut self, host: &str, kind: BlockingKind) {
        if self.max_sites == 0 || host.is_empty() || host.len() > 253 {
            return;
        }
        if let Some(stats) = self.sites.get_mut(host) {
            stats.record(kind);
            return;
        }
        if self.sites.len() == self.max_sites {
            if let Some(oldest) = self.insertion_order.pop_front() {
                self.sites.remove(&oldest);
            }
        }
        let mut stats = SiteBlockingStats::default();
        stats.record(kind);
        self.sites.insert(host.to_owned(), stats);
        self.insertion_order.push_back(host.to_owned());
    }

    pub fn for_site(&self, host: &str) -> SiteBlockingStats {
        self.sites.get(host).copied().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_tracker_and_advertisement_counts() {
        let mut store = BlockingStatsStore::new(8);
        store.record("example.com", BlockingKind::Tracker);
        store.record("example.com", BlockingKind::Advertisement);
        store.record("example.com", BlockingKind::Advertisement);

        assert_eq!(
            store.for_site("example.com"),
            SiteBlockingStats {
                trackers: 1,
                advertisements: 2,
            }
        );
        assert_eq!(store.for_site("example.com").total(), 3);
        assert_eq!(
            store.for_site("other.example"),
            SiteBlockingStats::default()
        );
    }

    #[test]
    fn evicts_the_oldest_site_at_the_fixed_capacity() {
        let mut store = BlockingStatsStore::new(2);
        store.record("first.example", BlockingKind::Tracker);
        store.record("second.example", BlockingKind::Tracker);
        store.record("first.example", BlockingKind::Advertisement);
        store.record("third.example", BlockingKind::Advertisement);

        assert_eq!(
            store.for_site("first.example"),
            SiteBlockingStats::default()
        );
        assert_eq!(store.for_site("second.example").total(), 1);
        assert_eq!(store.for_site("third.example").total(), 1);
    }

    #[test]
    fn rejects_hosts_that_cannot_be_valid_dns_names() {
        let mut store = BlockingStatsStore::new(2);
        store.record("", BlockingKind::Tracker);
        store.record(&"x".repeat(254), BlockingKind::Advertisement);

        assert!(store.sites.is_empty());
        assert!(store.insertion_order.is_empty());
    }
}
