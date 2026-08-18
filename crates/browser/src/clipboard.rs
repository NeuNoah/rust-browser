//! System clipboard access for the embedder.
//!
//! `arboard` provides a safe cross-platform API. Access is serialized
//! behind a mutex (the Windows clipboard must not be touched from
//! multiple threads at once) and if the system clipboard is
//! unavailable we fall back to an in-process string so that Servo's
//! copy/paste round trip still works.

use std::sync::{Mutex, OnceLock};

use log::debug;
use servo::{ClipboardDelegate, StringRequest, WebView};

/// In-process fallback clipboard.
static FALLBACK: OnceLock<Mutex<String>> = OnceLock::new();

/// Lazily initialized system clipboard.
static CLIPBOARD: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();

/// The clipboard delegate used by every `WebView`.
pub struct SystemClipboard;

impl SystemClipboard {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SystemClipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipboardDelegate for SystemClipboard {
    fn clear(&self, _webview: WebView) {
        match with_system_clipboard(|clipboard| clipboard.clear()) {
            Ok(()) => debug!("clipboard cleared"),
            Err(error) => {
                debug!("system clipboard unavailable ({error}); using in-process fallback");
                with_fallback(|text| text.clear());
            }
        }
    }

    fn get_text(&self, _webview: WebView, request: StringRequest) {
        match with_system_clipboard(|clipboard| clipboard.get_text()) {
            Ok(text) => {
                debug!("clipboard get_text: {} chars", text.chars().count());
                request.success(text);
            }
            Err(error) => {
                debug!("system clipboard unavailable ({error}); using in-process fallback");
                with_fallback(|text| request.success(text.clone()));
            }
        }
    }

    fn set_text(&self, _webview: WebView, new_contents: String) {
        debug!("clipboard set_text: {} chars", new_contents.chars().count());
        match with_system_clipboard(|clipboard| clipboard.set_text(&new_contents)) {
            Ok(()) => {}
            Err(error) => {
                debug!("system clipboard unavailable ({error}); using in-process fallback");
                with_fallback(|text| *text = new_contents);
            }
        }
    }
}

fn with_system_clipboard<T>(
    callback: impl FnOnce(&mut arboard::Clipboard) -> Result<T, arboard::Error>,
) -> Result<T, arboard::Error> {
    let mut guard = CLIPBOARD.get_or_init(|| Mutex::new(None)).lock().unwrap();
    if guard.is_none() {
        *guard = Some(arboard::Clipboard::new()?);
    }
    callback(guard.as_mut().expect("just initialized"))
}

fn with_fallback<T>(callback: impl FnOnce(&mut String) -> T) -> T {
    let mut guard = FALLBACK
        .get_or_init(|| Mutex::new(String::new()))
        .lock()
        .unwrap();
    callback(&mut guard)
}
