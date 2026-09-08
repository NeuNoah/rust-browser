//! System clipboard access for the embedder.
//!
//! `arboard` provides a safe cross-platform API. Access is serialized
//! behind a mutex (the Windows clipboard must not be touched from
//! multiple threads at once) and if the system clipboard is
//! unavailable the request fails closed. Servo 0.5 does not yet enforce
//! Clipboard API permissions, so access is also gated on a short-lived
//! user action granted by the embedder.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use log::{debug, warn};
use servo::{ClipboardDelegate, StringRequest, WebView, WebViewId};

/// Lazily initialized system clipboard.
static CLIPBOARD: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();

const GRANT_LIFETIME: Duration = Duration::from_secs(2);
const MAX_PENDING_GRANTS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClipboardAccess {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardOperation {
    Clear,
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GrantUse {
    Deny,
    Allow,
    AllowAndConsume,
}

#[derive(Debug, Clone, Copy)]
struct ClipboardGrant {
    access: ClipboardAccess,
    expires_at: Instant,
    clear_seen: bool,
}

impl ClipboardGrant {
    fn new(access: ClipboardAccess, now: Instant) -> Self {
        Self {
            access,
            expires_at: now + GRANT_LIFETIME,
            clear_seen: false,
        }
    }

    fn authorize(&mut self, operation: ClipboardOperation, now: Instant) -> GrantUse {
        if now > self.expires_at {
            return GrantUse::Deny;
        }
        match (self.access, operation) {
            (ClipboardAccess::Read, ClipboardOperation::Read) => GrantUse::AllowAndConsume,
            (ClipboardAccess::Write, ClipboardOperation::Clear) if !self.clear_seen => {
                self.clear_seen = true;
                GrantUse::Allow
            }
            (ClipboardAccess::Write, ClipboardOperation::Write) => GrantUse::AllowAndConsume,
            _ => GrantUse::Deny,
        }
    }
}

/// The clipboard delegate used by every `WebView`.
pub struct SystemClipboard {
    grants: RefCell<HashMap<WebViewId, VecDeque<ClipboardGrant>>>,
}

impl SystemClipboard {
    pub fn new() -> Self {
        Self {
            grants: RefCell::new(HashMap::new()),
        }
    }

    pub(crate) fn grant(&self, webview_id: WebViewId, access: ClipboardAccess) {
        let mut grants = self.grants.borrow_mut();
        let queue = grants.entry(webview_id).or_default();
        if queue.len() >= MAX_PENDING_GRANTS {
            queue.pop_front();
        }
        queue.push_back(ClipboardGrant::new(access, Instant::now()));
    }

    pub(crate) fn revoke(&self, webview_id: WebViewId) {
        self.grants.borrow_mut().remove(&webview_id);
    }

    pub(crate) fn revoke_all(&self) {
        self.grants.borrow_mut().clear();
    }

    fn authorize(&self, webview_id: WebViewId, operation: ClipboardOperation) -> bool {
        let mut grants = self.grants.borrow_mut();
        let Some(queue) = grants.get_mut(&webview_id) else {
            return false;
        };
        let authorized = authorize_grant_queue(queue, operation, Instant::now());
        if queue.is_empty() {
            grants.remove(&webview_id);
        }
        authorized
    }
}

fn authorize_grant_queue(
    queue: &mut VecDeque<ClipboardGrant>,
    operation: ClipboardOperation,
    now: Instant,
) -> bool {
    queue.retain(|grant| now <= grant.expires_at);
    let Some(index) = queue.iter().position(|grant| match operation {
        ClipboardOperation::Read => grant.access == ClipboardAccess::Read,
        ClipboardOperation::Clear => grant.access == ClipboardAccess::Write && !grant.clear_seen,
        ClipboardOperation::Write => grant.access == ClipboardAccess::Write,
    }) else {
        return false;
    };
    let decision = queue[index].authorize(operation, now);
    if decision == GrantUse::AllowAndConsume {
        queue.remove(index);
    }
    decision != GrantUse::Deny
}

impl Default for SystemClipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipboardDelegate for SystemClipboard {
    fn clear(&self, webview: WebView) {
        if !self.authorize(webview.id(), ClipboardOperation::Clear) {
            debug!("Denied clipboard clear without a recent user action");
            return;
        }
        match with_system_clipboard(|clipboard| clipboard.clear()) {
            Ok(()) => debug!("clipboard cleared"),
            Err(error) => {
                warn!("system clipboard clear failed: {error}");
            }
        }
    }

    fn get_text(&self, webview: WebView, request: StringRequest) {
        if !self.authorize(webview.id(), ClipboardOperation::Read) {
            debug!("Denied clipboard read without a recent paste action");
            request.failure("Clipboard read requires an explicit paste action".to_owned());
            return;
        }
        match with_system_clipboard(|clipboard| clipboard.get_text()) {
            Ok(text) => {
                debug!("clipboard get_text: {} chars", text.chars().count());
                request.success(text);
            }
            Err(error) => {
                warn!("system clipboard read failed: {error}");
                request.failure(format!("System clipboard unavailable: {error}"));
            }
        }
    }

    fn set_text(&self, webview: WebView, new_contents: String) {
        if !self.authorize(webview.id(), ClipboardOperation::Write) {
            debug!("Denied clipboard write without a recent copy or cut action");
            return;
        }
        debug!("clipboard set_text: {} chars", new_contents.chars().count());
        match with_system_clipboard(|clipboard| clipboard.set_text(&new_contents)) {
            Ok(()) => {}
            Err(error) => {
                warn!("system clipboard write failed: {error}");
            }
        }
    }
}

fn with_system_clipboard<T>(
    callback: impl FnOnce(&mut arboard::Clipboard) -> Result<T, arboard::Error>,
) -> Result<T, arboard::Error> {
    let mut guard = lock_unpoisoned(CLIPBOARD.get_or_init(|| Mutex::new(None)));
    if guard.is_none() {
        *guard = Some(arboard::Clipboard::new()?);
    }
    callback(guard.as_mut().expect("just initialized"))
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_grant_is_one_shot_and_operation_specific() {
        let now = Instant::now();
        let mut grant = ClipboardGrant::new(ClipboardAccess::Read, now);
        assert_eq!(
            grant.authorize(ClipboardOperation::Write, now),
            GrantUse::Deny
        );

        let mut grant = ClipboardGrant::new(ClipboardAccess::Read, now);
        assert_eq!(
            grant.authorize(ClipboardOperation::Read, now),
            GrantUse::AllowAndConsume
        );
    }

    #[test]
    fn write_grant_allows_clear_then_one_write() {
        let now = Instant::now();
        let mut grant = ClipboardGrant::new(ClipboardAccess::Write, now);
        assert_eq!(
            grant.authorize(ClipboardOperation::Clear, now),
            GrantUse::Allow
        );
        assert_eq!(
            grant.authorize(ClipboardOperation::Clear, now),
            GrantUse::Deny
        );
        assert_eq!(
            grant.authorize(ClipboardOperation::Write, now),
            GrantUse::AllowAndConsume
        );
    }

    #[test]
    fn expired_grant_fails_closed() {
        let now = Instant::now();
        let mut grant = ClipboardGrant::new(ClipboardAccess::Read, now);
        assert_eq!(
            grant.authorize(
                ClipboardOperation::Read,
                now + GRANT_LIFETIME + Duration::from_nanos(1)
            ),
            GrantUse::Deny
        );
    }

    #[test]
    fn queued_copy_and_paste_grants_do_not_overwrite_each_other() {
        let now = Instant::now();
        let mut queue = VecDeque::from([
            ClipboardGrant::new(ClipboardAccess::Write, now),
            ClipboardGrant::new(ClipboardAccess::Read, now),
        ]);
        assert!(authorize_grant_queue(
            &mut queue,
            ClipboardOperation::Write,
            now
        ));
        assert!(authorize_grant_queue(
            &mut queue,
            ClipboardOperation::Read,
            now
        ));
        assert!(queue.is_empty());
    }

    #[test]
    fn unrelated_operation_does_not_destroy_a_grant() {
        let now = Instant::now();
        let mut queue = VecDeque::from([ClipboardGrant::new(ClipboardAccess::Write, now)]);
        assert!(!authorize_grant_queue(
            &mut queue,
            ClipboardOperation::Read,
            now
        ));
        assert!(authorize_grant_queue(
            &mut queue,
            ClipboardOperation::Write,
            now
        ));
    }
}
