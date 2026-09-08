//! The application state: window, Servo instance, WebView and the
//! glue between them.
//!
//! `AppState` is shared as `Rc<AppState>` because it is the
//! `WebViewDelegate`. Servo delivers delegate callbacks while the main
//! thread drains `Servo::spin_event_loop`; queued UI events avoid
//! re-entrant borrows while a frame is being assembled.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use euclid::Scale;
use log::{debug, info, warn};
use servo::{
    ColorPicker, ContextMenu, ContextMenuAction, CreateNewWebViewRequest, Cursor,
    DeviceIndependentPixel, DevicePixel, DevicePoint, EmbedderControl, EmbedderControlId,
    FilePicker, ImeEvent, InputEvent, LoadStatus, MouseButton, MouseButtonAction, MouseButtonEvent,
    MouseMoveEvent, NavigationRequest, OffscreenRenderingContext, RenderingContext, RgbColor,
    SelectElement, Servo, ServoBuilder, ServoDelegate, SimpleDialog, WebResourceLoad,
    WebResourceResponse, WebView, WebViewBuilder, WebViewDelegate, WebViewId, WheelDelta,
    WheelEvent, WheelMode, WindowRenderingContext,
};
use url::{Host, Url};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::raw_window_handle::{HasDisplayHandle as _, HasWindowHandle as _};
use winit::window::{CursorIcon, Window, WindowId};

use browser_core::{
    BrowserCore, LoadState, NavigationCommand, NavigationError, SearchEngine, TabId,
};
use browser_network::{default_pipeline, RequestContext, RequestPipeline, ResourceType};
use browser_privacy::trackers::TrackerEngine;

use crate::gui::{Gui, ReaderScrollCommand};
use crate::proxy::StartupProxy;
use crate::reader::{
    article_from_js, reader_url_is_eligible, ReaderArticle, ReaderError, EXTRACTION_SCRIPT,
};
use crate::waker::EventLoopWaker;

const USER_GESTURE_GRANT_LIFETIME: Duration = Duration::from_secs(2);
const MAX_PENDING_USER_ACTION_GRANTS: usize = 8;
const MAX_FILE_FILTERS: usize = 64;
const MAX_FILE_FILTER_LENGTH: usize = 64;
const READER_EXTRACTION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_OUTSTANDING_READER_EVALUATIONS: usize = 4;

fn reader_scroll_command(key_event: &KeyEvent) -> Option<ReaderScrollCommand> {
    match key_event.logical_key {
        Key::Named(NamedKey::Home) => Some(ReaderScrollCommand::Home),
        Key::Named(NamedKey::End) => Some(ReaderScrollCommand::End),
        Key::Named(NamedKey::PageDown) => Some(ReaderScrollCommand::PageDown),
        Key::Named(NamedKey::PageUp) => Some(ReaderScrollCommand::PageUp),
        Key::Named(NamedKey::ArrowDown) => Some(ReaderScrollCommand::ArrowDown),
        Key::Named(NamedKey::ArrowUp) => Some(ReaderScrollCommand::ArrowUp),
        _ => match key_event.physical_key {
            PhysicalKey::Code(KeyCode::Home) => Some(ReaderScrollCommand::Home),
            PhysicalKey::Code(KeyCode::End) => Some(ReaderScrollCommand::End),
            PhysicalKey::Code(KeyCode::PageDown) => Some(ReaderScrollCommand::PageDown),
            PhysicalKey::Code(KeyCode::PageUp) => Some(ReaderScrollCommand::PageUp),
            PhysicalKey::Code(KeyCode::ArrowDown) => Some(ReaderScrollCommand::ArrowDown),
            PhysicalKey::Code(KeyCode::ArrowUp) => Some(ReaderScrollCommand::ArrowUp),
            _ => None,
        },
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UserActionCapability {
    Popup,
    FilePicker,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UserActionGrantKind {
    General,
    PopupOnly,
}

#[derive(Clone, Copy, Debug)]
struct UserActionGrant {
    expires_at: Instant,
    kind: UserActionGrantKind,
    epoch: u64,
}

impl UserActionGrant {
    fn allows(self, capability: UserActionCapability) -> bool {
        self.kind == UserActionGrantKind::General || capability == UserActionCapability::Popup
    }
}

/// Events winit delivers to our application.
pub enum AppEvent {
    /// Servo woke the event loop up; drain its message queues.
    Wake,
    /// egui requested another frame, immediately or after a delay.
    Repaint { delay: Duration, pass: u64 },
}

/// One Servo [`WebView`] owned by a browser tab. The order of this
/// vector mirrors the tab order in the core's `TabManager`. All
/// WebViews share the single offscreen rendering context (servoshell
/// model).
pub(crate) struct TabWebView {
    pub(crate) tab: TabId,
    pub(crate) webview: WebView,
    /// Last viewport sent to this WebView. The rendering context is
    /// shared, so querying its size cannot reveal which tabs received
    /// a resize notification.
    pub(crate) viewport_size: winit::dpi::PhysicalSize<u32>,
}

/// The system-IME target Servo reported for a page text field. The
/// rectangle is relative to the WebView and measured in physical
/// device pixels.
#[derive(Clone, Copy)]
struct PageImeControl {
    id: EmbedderControlId,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

#[derive(Clone, Copy)]
struct TabHistory {
    len: usize,
    current: usize,
}

impl TabHistory {
    fn can_go_back(&self) -> bool {
        self.current > 0 && self.current < self.len
    }

    fn can_go_forward(&self) -> bool {
        self.current < self.len.saturating_sub(1)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReaderRequest {
    generation: u64,
    navigation_seq: u64,
    source_url: Url,
    webview_id: WebViewId,
    deadline: Instant,
}

impl ReaderRequest {
    fn matches_document(&self, navigation_seq: u64, url: &Url) -> bool {
        reader_document_matches(self.navigation_seq, &self.source_url, navigation_seq, url)
    }
}

fn reader_document_matches(
    source_navigation_seq: u64,
    source_url: &Url,
    current_navigation_seq: u64,
    current_url: &Url,
) -> bool {
    source_navigation_seq == current_navigation_seq && source_url == current_url
}

enum ReaderTabState {
    Extracting(ReaderRequest),
    Ready {
        request: ReaderRequest,
        article: Arc<ReaderArticle>,
    },
    Error {
        request: ReaderRequest,
        error: ReaderError,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReaderButtonState {
    Unavailable,
    Waiting,
    Available,
    Extracting,
    Active,
    Error { retryable: bool },
}

#[derive(Clone)]
pub(crate) enum ReaderView {
    Extracting {
        source_url: Url,
    },
    Ready {
        source_url: Url,
        article: Arc<ReaderArticle>,
        generation: u64,
    },
    Error {
        source_url: Url,
        message: &'static str,
        retryable: bool,
    },
}

/// A Servo control together with the tab and opaque request id that
/// own it. Keeping this metadata prevents a hidden tab (or a stale
/// hide notification) from affecting the active tab's UI.
pub(crate) struct PendingControl<T> {
    pub(crate) tab: TabId,
    pub(crate) id: EmbedderControlId,
    pub(crate) control: T,
}

struct PendingFilePicker {
    tab: TabId,
    id: EmbedderControlId,
    control: FilePicker,
    grant: UserActionGrant,
}

fn discard_pending_file_picker(
    pending: &mut Option<PendingFilePicker>,
    tab: TabId,
    id: Option<EmbedderControlId>,
) -> bool {
    let matches = pending
        .as_ref()
        .is_some_and(|control| control.tab == tab && id.is_none_or(|id| control.id == id));
    if matches {
        pending.take();
    }
    matches
}

/// Applies the same request policy to HTTP(S) loads that Servo cannot
/// associate with a WebView (for example service-worker traffic).
struct GlobalRequestDelegate {
    pipeline: Rc<RequestPipeline>,
    proxy_enabled: bool,
}

impl ServoDelegate for GlobalRequestDelegate {
    fn load_web_resource(&self, load: WebResourceLoad) {
        let request = load.request();
        let url = request.url.clone();
        let initiator = request.referrer_url.clone();
        if proxy_default_http_port_is_unsupported(self.proxy_enabled, &url) {
            debug!("Blocked default-port HTTP request through Servo 0.5 proxy connector");
            load.intercept(WebResourceResponse::new(url)).cancel();
            return;
        }
        if global_http_context_is_unsafe(&url, initiator.as_ref()) {
            debug!("Blocked global HTTP request without an explicit HTTP initiator");
            load.intercept(WebResourceResponse::new(url)).finish();
            return;
        }
        let context = RequestContext::new(
            url.clone(),
            initiator,
            ResourceType::from_engine_string(request.destination.as_str()),
            // Servo 0.5 reports this flag for iframe documents too. Do
            // not expose the ambiguous value as a trusted exemption.
            false,
        );
        if let browser_network::PipelineDecision::Block { layer, reason } =
            self.pipeline.evaluate(&context)
        {
            debug!(
                "Blocked global request {} ({layer}: {reason})",
                url_identity(&url)
            );
            load.intercept(WebResourceResponse::new(url)).finish();
        }
    }
}

/// The root of all browser state. See the module docs for the sharing
/// model.
pub struct AppState {
    pub(crate) window: Rc<Window>,
    servo: Servo,
    window_rendering_context: Rc<WindowRenderingContext>,
    pub(crate) rendering_context: Rc<OffscreenRenderingContext>,
    pub(crate) webviews: RefCell<Vec<TabWebView>>,
    gui: RefCell<Gui>,
    core: RefCell<BrowserCore>,
    pipeline: Rc<RequestPipeline>,
    proxy_enabled: bool,
    clipboard: Rc<crate::clipboard::SystemClipboard>,
    active_tab: Cell<Option<TabId>>,
    /// Per-tab history availability, reported by the engine; keyed by
    /// tab so the toolbar reflects the active tab's history.
    tab_history: RefCell<HashMap<TabId, TabHistory>>,
    pub(crate) can_go_back: Cell<bool>,
    pub(crate) can_go_forward: Cell<bool>,
    pub(crate) needs_repaint: Cell<bool>,
    gui_repaint_at: Cell<Option<Instant>>,
    gui_repaint_pass: Cell<u64>,
    last_cursor_position: Cell<Option<PhysicalPosition<f64>>>,
    last_mouse_point: Cell<Option<DevicePoint>>,
    page_pointer_capture: Cell<Option<TabId>>,
    page_buttons_down: RefCell<HashSet<winit::event::MouseButton>>,
    egui_buttons_down: RefCell<HashSet<winit::event::MouseButton>>,
    /// True while the WebView owns the keyboard focus; false while an
    /// egui widget (e.g. the address bar) does.
    page_focus: Cell<bool>,
    restore_page_focus_on_window_focus: Cell<bool>,
    /// The currently pressed modifier keys, tracked via
    /// `WindowEvent::ModifiersChanged`.
    modifiers: Cell<winit::keyboard::ModifiersState>,
    alt_graph: Cell<bool>,
    consumed_shortcuts: RefCell<HashSet<PhysicalKey>>,
    /// Short-lived one-shot grants for popup or file-picker requests,
    /// keyed by the WebView that received the user action.
    user_action_grants: RefCell<HashMap<WebViewId, VecDeque<UserActionGrant>>>,
    /// Invalidates already-reserved page-action tokens whenever focus
    /// moves into browser chrome or another tab.
    user_action_epoch: Cell<u64>,
    /// Browser-created WebViews have no opener or running page script.
    /// Their first intercepted HTTP document can therefore be trusted
    /// as a real top-level navigation despite Servo's ambiguous
    /// `is_for_main_frame` flag. Opener-created WebViews never enter
    /// this set.
    fresh_browser_webviews: RefCell<HashSet<WebViewId>>,
    /// Tabs Servo currently reports as animating.
    animating_tabs: RefCell<HashSet<TabId>>,
    /// Non-off Reader states keyed by tab. Ready/error pages are
    /// throttled; extraction remains scheduled until its bounded timeout.
    reader_states: RefCell<HashMap<TabId, ReaderTabState>>,
    /// Evaluations cannot be canceled by Servo. Tombstones remain until
    /// callbacks arrive, enforcing one per WebView and a global cap.
    reader_inflight: RefCell<HashMap<WebViewId, u64>>,
    next_reader_generation: Cell<u64>,
    ui_events: RefCell<Vec<UiEvent>>,
    /// The URL new tabs load; editable in the settings window.
    start_page: RefCell<Url>,
    /// Per-site tracker override: host → allow trackers. Consulted in
    /// `load_web_resource`; `None` means the default pipeline applies.
    tracker_overrides: Rc<RefCell<HashMap<String, bool>>>,
    /// Per-tab system-IME targets reported by Servo. Composition state
    /// is separate: a focused text field is not necessarily composing.
    page_ime_controls: RefCell<HashMap<TabId, PageImeControl>>,
    ime_composing_tab: Cell<Option<TabId>>,
    /// Used to keep Enter from submitting the URL bar while it is
    /// selecting an IME candidate.
    ui_ime_composing: Cell<bool>,
    /// The embedder controls pending from Servo, rendered by egui.
    /// Each owns its request; dropping it responds "dismissed".
    pub(crate) active_context_menu: RefCell<Option<PendingControl<ContextMenu>>>,
    pub(crate) active_select: RefCell<Option<PendingControl<SelectElement>>>,
    pub(crate) active_select_value: RefCell<Vec<usize>>,
    pub(crate) active_color_picker: RefCell<Option<PendingControl<ColorPicker>>>,
    pub(crate) active_color_value: Cell<[u8; 3]>,
    pub(crate) active_dialog: RefCell<Option<PendingControl<SimpleDialog>>>,
    dialog_focus_request: Cell<Option<EmbedderControlId>>,
    /// Whether the settings window is open.
    pub(crate) settings_open: Cell<bool>,
}

/// Delegate notifications queued for the main thread. Every event
/// carries the tab it belongs to; the active tab is only a display
/// concern.
enum UiEvent {
    Url(TabId, Url),
    PageTitle(TabId, Option<String>),
    LoadStatus(TabId, LoadStatus),
    ReaderResult(TabId, ReaderRequest, Result<ReaderArticle, ReaderError>),
    Closed(TabId),
    Crashed(TabId),
    /// A context menu / select / picker / simple dialog from page content.
    ContextMenu(TabId, EmbedderControlId, ContextMenu),
    SelectElement(TabId, EmbedderControlId, SelectElement),
    ColorPicker(TabId, EmbedderControlId, ColorPicker, [u8; 3]),
    FilePicker(TabId, EmbedderControlId, FilePicker, UserActionGrant),
    SimpleDialog(TabId, EmbedderControlId, SimpleDialog),
    InputMethod(TabId, PageImeControl),
    /// An embedder control was hidden; the pending one with this id
    /// (if any) is dismissed.
    HideEmbedderControl(TabId, EmbedderControlId),
}

/// The winit application.
pub struct App {
    proxy: EventLoopProxy<AppEvent>,
    initial_url: Option<String>,
    initial_tracker_overrides: Vec<String>,
    initial_proxy: Option<StartupProxy>,
    state: Option<Rc<AppState>>,
}

impl App {
    pub fn new(
        event_loop: &EventLoop<AppEvent>,
        initial_url: Option<String>,
        initial_tracker_overrides: Vec<String>,
        initial_proxy: Option<StartupProxy>,
    ) -> Self {
        Self {
            proxy: event_loop.create_proxy(),
            initial_url,
            initial_tracker_overrides,
            initial_proxy,
            state: None,
        }
    }
}

impl AppState {
    /// Create the window, Servo instance, offscreen rendering context,
    /// GUI and the initial WebView.
    pub fn create(
        event_loop: &ActiveEventLoop,
        proxy: EventLoopProxy<AppEvent>,
        initial_url: Option<String>,
        initial_tracker_overrides: Vec<String>,
        initial_proxy: Option<StartupProxy>,
    ) -> Result<Rc<Self>, String> {
        let waker: Box<dyn servo::EventLoopWaker> = Box::new(EventLoopWaker::new(proxy.clone()));
        let pipeline = Rc::new(default_pipeline(TrackerEngine::builtin()));
        let mut tracker_override_map = HashMap::new();
        for input in initial_tracker_overrides {
            let host = normalize_site_host(&input)
                .map_err(|error| format!("Invalid --allow-trackers-for value: {error}"))?;
            tracker_override_map.insert(host, true);
        }
        let tracker_overrides = Rc::new(RefCell::new(tracker_override_map));

        // Do not inherit ambient HTTP(S)_PROXY/NO_PROXY variables. An
        // explicit configuration is validated before the window exists,
        // and Servo's connector returns its proxy errors without a direct
        // connection fallback.
        let proxy_enabled = initial_proxy.is_some();
        let preferences = browser_servo_preferences(initial_proxy.as_ref());

        let servo = ServoBuilder::default()
            .opts(browser_servo_options())
            .preferences(preferences)
            .event_loop_waker(waker)
            .build();
        servo.set_delegate(Rc::new(GlobalRequestDelegate {
            pipeline: pipeline.clone(),
            proxy_enabled,
        }));
        servo.setup_logging();

        let window = Rc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Rust Browser")
                        .with_inner_size(winit::dpi::PhysicalSize::new(1280, 800)),
                )
                .map_err(|error| format!("Could not create window: {error}"))?,
        );

        let window_size = window.inner_size();
        let display_handle = event_loop
            .display_handle()
            .map_err(|error| format!("Could not get display handle: {error}"))?;
        let window_handle = window
            .window_handle()
            .map_err(|error| format!("Could not get window handle: {error}"))?;
        let window_rendering_context = Rc::new(
            WindowRenderingContext::new(display_handle, window_handle, window_size)
                .map_err(|error| format!("Could not create RenderingContext: {error:?}"))?,
        );
        window_rendering_context
            .make_current()
            .map_err(|error| format!("Could not make RenderingContext current: {error:?}"))?;

        let rendering_context = Rc::new(window_rendering_context.offscreen_context(window_size));

        let gui = Gui::new(event_loop, &window, &rendering_context, proxy)?;
        let clipboard = Rc::new(crate::clipboard::SystemClipboard::new());
        let state = Rc::new(Self {
            window: window.clone(),
            servo,
            window_rendering_context,
            rendering_context: rendering_context.clone(),
            webviews: RefCell::new(Vec::new()),
            gui: RefCell::new(gui),
            core: RefCell::new(BrowserCore::new()),
            pipeline,
            proxy_enabled,
            clipboard,
            active_tab: Cell::new(None),
            tab_history: RefCell::new(HashMap::new()),
            can_go_back: Cell::new(false),
            can_go_forward: Cell::new(false),
            needs_repaint: Cell::new(false),
            gui_repaint_at: Cell::new(None),
            gui_repaint_pass: Cell::new(0),
            last_cursor_position: Cell::new(None),
            last_mouse_point: Cell::new(None),
            page_pointer_capture: Cell::new(None),
            page_buttons_down: RefCell::new(HashSet::new()),
            egui_buttons_down: RefCell::new(HashSet::new()),
            page_focus: Cell::new(false),
            restore_page_focus_on_window_focus: Cell::new(false),
            modifiers: Cell::new(winit::keyboard::ModifiersState::empty()),
            alt_graph: Cell::new(false),
            consumed_shortcuts: RefCell::new(HashSet::new()),
            user_action_grants: RefCell::new(HashMap::new()),
            user_action_epoch: Cell::new(0),
            fresh_browser_webviews: RefCell::new(HashSet::new()),
            animating_tabs: RefCell::new(HashSet::new()),
            reader_states: RefCell::new(HashMap::new()),
            reader_inflight: RefCell::new(HashMap::new()),
            next_reader_generation: Cell::new(0),
            ui_events: RefCell::new(Vec::new()),
            start_page: RefCell::new(Url::parse("about:blank").expect("static URL")),
            tracker_overrides,
            page_ime_controls: RefCell::new(HashMap::new()),
            ime_composing_tab: Cell::new(None),
            ui_ime_composing: Cell::new(false),
            active_context_menu: RefCell::new(None),
            active_select: RefCell::new(None),
            active_select_value: RefCell::new(Vec::new()),
            active_color_picker: RefCell::new(None),
            active_color_value: Cell::new([0, 0, 0]),
            active_dialog: RefCell::new(None),
            dialog_focus_request: Cell::new(None),
            settings_open: Cell::new(false),
        });

        let focus_url_bar = initial_url.is_none();
        let initial_url = match initial_url {
            Some(input) => match state.core.borrow().command_from_url_input(&input, false) {
                Ok(NavigationCommand::Load(url)) => url,
                Ok(_) => return Err("Initial input did not resolve to a URL".to_owned()),
                Err(error) => return Err(format!("Invalid initial URL: {error}")),
            },
            None => state.start_page_url(),
        };

        state.create_tab(initial_url);
        if focus_url_bar {
            state.gui.borrow_mut().focus_url_bar();
            state.set_page_focus(false);
        } else {
            state.set_page_focus(true);
        }

        window.request_redraw();
        Ok(state)
    }

    /// Navigate according to address-bar input.
    pub fn navigate(self: &Rc<Self>, input: &str) -> Result<(), NavigationError> {
        // End the immutable RefCell borrow before an action such as
        // `load_url` or `create_tab` mutates the core model.
        let command = self.core.borrow().command_from_input(input, false);
        match command? {
            NavigationCommand::Load(url) => self.load_url(url),
            NavigationCommand::Reload => self.navigate_reload(),
            NavigationCommand::Back => self.navigate_back(),
            NavigationCommand::Forward => self.navigate_forward(),
            NavigationCommand::NewTab(url) => self.create_tab(url),
        }
        Ok(())
    }

    /// Create a new tab, activate it and load `url` into it. All tabs
    /// share the window's single offscreen rendering context (the
    /// servoshell model).
    pub fn create_tab(self: &Rc<Self>, url: Url) {
        info!("New tab: {}", url_identity(&url));
        let tab = self.core.borrow_mut().tabs.create_tab(url.clone());
        // Initial navigation has to be part of WebView construction:
        // calling `WebView::load` before Servo has registered the new
        // browsing context drops the request.
        let webview = WebViewBuilder::new(&self.servo, self.rendering_context.clone())
            .url(url.clone())
            .hidpi_scale_factor(Scale::new(self.window.scale_factor() as f32))
            .delegate(self.clone())
            .clipboard_delegate(self.clipboard.clone())
            .build();
        self.fresh_browser_webviews
            .borrow_mut()
            .insert(webview.id());
        self.webviews.borrow_mut().push(TabWebView {
            tab,
            webview,
            viewport_size: winit::dpi::PhysicalSize::new(0, 0),
        });
        self.activate_tab(tab);
    }

    /// Close a tab. Closing the last tab opens the configured start
    /// page in a fresh tab.
    pub fn close_tab(self: &Rc<Self>, tab: TabId) {
        info!("Close tab {tab:?}");
        let reader_was_visible = self.leave_reader_for_tab(tab, false);
        if self.page_pointer_capture.get() == Some(tab) {
            self.release_page_pointer_capture();
        }
        self.cancel_page_composition(tab);
        self.page_ime_controls.borrow_mut().remove(&tab);
        self.dismiss_controls_for_tab(tab);
        let was_active = self.core.borrow().tabs.active_tab_id() == Some(tab);
        let index = self
            .webviews
            .borrow()
            .iter()
            .position(|tab_webview| tab_webview.tab == tab);
        if let Some(index) = index {
            let removed = self.webviews.borrow_mut().remove(index);
            self.clipboard.revoke(removed.webview.id());
            self.user_action_grants
                .borrow_mut()
                .remove(&removed.webview.id());
            self.fresh_browser_webviews
                .borrow_mut()
                .remove(&removed.webview.id());
        }
        let _ = self.core.borrow_mut().tabs.close_tab(tab);
        self.tab_history.borrow_mut().remove(&tab);
        self.animating_tabs.borrow_mut().remove(&tab);
        let is_empty = self.core.borrow().tabs.is_empty();
        let next_tab = if !is_empty && was_active {
            self.core.borrow().tabs.active_tab_id()
        } else {
            None
        };
        if is_empty {
            self.create_tab(self.start_page_url());
        } else if let Some(next) = next_tab {
            self.activate_tab(next);
        }
        if reader_was_visible && was_active && !self.active_reader_is_visible() {
            self.set_page_focus(true);
        }
    }

    /// The tab that owns `webview`, by engine id.
    fn tab_for(&self, webview: &WebView) -> Option<TabId> {
        self.webviews
            .borrow()
            .iter()
            .find(|tab_webview| tab_webview.webview.id() == webview.id())
            .map(|tab_webview| tab_webview.tab)
    }

    /// Make `tab` the active tab and synchronize the UI with its state.
    pub(crate) fn activate_tab(&self, tab: TabId) {
        let page_was_focused = self.page_focus.get();
        if let Some(previous) = self.active_tab.get().filter(|previous| *previous != tab) {
            self.invalidate_user_action_epoch();
            if self.page_pointer_capture.get().is_some() {
                self.release_page_pointer_capture();
            }
            self.cancel_page_composition(previous);
            self.dismiss_controls_for_tab(previous);
            self.with_webview(previous, |webview| {
                self.clipboard.revoke(webview.id());
                self.user_action_grants.borrow_mut().remove(&webview.id());
            });
            if page_was_focused {
                self.with_webview(previous, WebView::blur);
            }
        }
        if self.core.borrow_mut().tabs.activate(tab).is_err() {
            warn!("cannot activate unknown tab {tab:?}");
            return;
        }
        for tab_webview in self.webviews.borrow().iter() {
            if tab_webview.tab == tab {
                tab_webview.webview.show();
            } else {
                tab_webview.webview.hide();
            }
        }
        let title = self
            .core
            .borrow()
            .tabs
            .get(tab)
            .and_then(|tab_state| tab_state.title.clone());
        if let Some(title) = title {
            self.window.set_title(&title);
        } else {
            self.window.set_title("Rust Browser");
        }
        let history = self.tab_history.borrow().get(&tab).cloned();
        self.can_go_back
            .set(history.as_ref().is_some_and(TabHistory::can_go_back));
        self.can_go_forward
            .set(history.as_ref().is_some_and(TabHistory::can_go_forward));
        self.active_tab.set(Some(tab));
        let target_has_reader = self.reader_states.borrow().contains_key(&tab);
        if page_was_focused && !target_has_reader {
            self.with_webview(tab, WebView::focus);
        }
        if page_was_focused && target_has_reader {
            // The previous WebView was already blurred above. Update the
            // window-global ownership without sending a spurious focus/blur
            // pair to the covered target page.
            self.page_focus.set(false);
            self.with_webview(tab, |webview| {
                self.user_action_grants.borrow_mut().remove(&webview.id());
            });
            self.window.set_ime_allowed(false);
        }
        // Do not retain a pointer shape supplied by the previously
        // visible WebView. The active page will report its cursor on
        // the next pointer move.
        self.window
            .set_cursor(winit::window::Cursor::Icon(CursorIcon::Default));
        self.window.request_redraw();
    }

    /// Switch to the next tab, or the previous one when shift is held.
    fn cycle_tab(&self, backwards: bool) {
        let tabs = self.core.borrow().tabs.tabs().to_vec();
        if tabs.len() < 2 {
            return;
        }
        let Some(active) = self.core.borrow().tabs.active_tab_id() else {
            return;
        };
        let Some(index) = tabs.iter().position(|tab| tab.id == active) else {
            return;
        };
        let next = if backwards {
            (index + tabs.len() - 1) % tabs.len()
        } else {
            (index + 1) % tabs.len()
        };
        self.activate_tab(tabs[next].id);
    }

    /// Run `f` on the active tab's WebView, if there is one.
    fn with_active_webview(&self, f: impl FnOnce(&WebView)) {
        let Some(tab) = self.active_tab.get() else {
            return;
        };
        self.with_webview(tab, f);
    }

    /// Run `f` on one tab's WebView, if it still exists.
    fn with_webview(&self, tab: TabId, f: impl FnOnce(&WebView)) {
        if let Some(tab_webview) = self
            .webviews
            .borrow()
            .iter()
            .find(|tab_webview| tab_webview.tab == tab)
        {
            f(&tab_webview.webview);
        }
    }

    fn queue_ui_event(&self, event: UiEvent) {
        self.ui_events.borrow_mut().push(event);
        self.needs_repaint.set(true);
        self.window.request_redraw();
    }

    /// Keep the embedder focus flag and Servo's WebView focus state in
    /// sync. IME input is disabled whenever browser chrome owns focus.
    fn set_page_focus(&self, focused: bool) {
        let changed = self.page_focus.replace(focused) != focused;
        if !focused {
            // A page gesture must not remain usable after browser
            // chrome, another native window, or an embedder control
            // takes focus. Revoke even when the cached focus flag was
            // already false: a page click and a chrome-focus request can
            // be observed in different Servo/winit batches, and queued
            // file-picker requests must fail closed across that race.
            self.invalidate_user_action_epoch();
            self.with_active_webview(|webview| {
                self.user_action_grants.borrow_mut().remove(&webview.id());
            });
        }
        if changed {
            self.with_active_webview(|webview| {
                if focused {
                    webview.focus();
                } else {
                    webview.blur();
                }
            });
        }
        if !focused {
            self.window.set_ime_allowed(false);
        }
    }

    fn grant_user_action(&self, webview_id: WebViewId, kind: UserActionGrantKind) {
        let mut grants = self.user_action_grants.borrow_mut();
        let queue = grants.entry(webview_id).or_default();
        if queue.len() >= MAX_PENDING_USER_ACTION_GRANTS {
            queue.pop_front();
        }
        queue.push_back(UserActionGrant {
            expires_at: Instant::now() + USER_GESTURE_GRANT_LIFETIME,
            kind,
            epoch: self.user_action_epoch.get(),
        });
    }

    fn invalidate_user_action_epoch(&self) {
        self.user_action_epoch
            .set(self.user_action_epoch.get().wrapping_add(1));
    }

    fn take_user_action_grant(
        &self,
        webview_id: WebViewId,
        capability: UserActionCapability,
    ) -> Option<UserActionGrant> {
        let mut grants = self.user_action_grants.borrow_mut();
        let queue = grants.get_mut(&webview_id)?;
        let grant = take_user_action_grant_queue(queue, Instant::now(), capability);
        if queue.is_empty() {
            grants.remove(&webview_id);
        }
        grant
    }

    fn consume_user_action_grant(
        &self,
        webview_id: WebViewId,
        capability: UserActionCapability,
    ) -> bool {
        self.take_user_action_grant(webview_id, capability)
            .is_some()
    }

    /// The active tab's id, if any.
    pub(crate) fn active_tab_id(&self) -> Option<TabId> {
        self.active_tab.get()
    }

    pub(crate) fn active_tab_url(&self) -> Option<String> {
        self.active_tab.get().and_then(|tab| {
            self.core
                .borrow()
                .tabs
                .get(tab)
                .map(|tab_state| tab_state.url.to_string())
        })
    }

    /// The tab states in tab order, for the tab bar.
    pub(crate) fn tab_states(&self) -> Vec<browser_core::Tab> {
        self.core.borrow().tabs.tabs().to_vec()
    }

    fn load_url(&self, url: Url) {
        info!("Loading {}", url_identity(&url));
        if let Some(tab) = self.active_tab.get() {
            self.leave_reader_for_tab(tab, false);
            self.cancel_page_composition(tab);
            let _ = self.core.borrow_mut().load_started(tab);
        }
        self.with_active_webview(|webview| webview.load(url));
    }

    pub fn navigate_back(&self) {
        if self.can_go_back.get() {
            if let Some(tab) = self.active_tab.get() {
                self.leave_reader_for_tab(tab, false);
            }
            self.with_active_webview(|webview| {
                webview.go_back(1);
            });
        }
    }

    pub fn navigate_forward(&self) {
        if self.can_go_forward.get() {
            if let Some(tab) = self.active_tab.get() {
                self.leave_reader_for_tab(tab, false);
            }
            self.with_active_webview(|webview| {
                webview.go_forward(1);
            });
        }
    }

    pub fn navigate_reload(&self) {
        if let Some(tab) = self.active_tab.get() {
            self.leave_reader_for_tab(tab, false);
        }
        self.with_active_webview(|webview| webview.reload());
    }

    /// Have Servo paint the active WebView into its rendering context.
    pub(crate) fn repaint_webviews(&self) {
        if let Err(error) = self.window_rendering_context.make_current() {
            warn!("Could not make window RenderingContext current: {error:?}");
            return;
        }
        if let Some(tab) = self.active_tab.get() {
            if let Some(tab_webview) = self
                .webviews
                .borrow()
                .iter()
                .find(|tab_webview| tab_webview.tab == tab)
            {
                tab_webview.webview.paint();
            }
        }
    }

    /// Apply queued delegate notifications to the core model and GUI.
    fn process_ui_events(self: &Rc<Self>) {
        let events = std::mem::take(&mut *self.ui_events.borrow_mut());
        // A show followed by hide/close can be present in one Servo
        // batch. Defer the blocking native dialog until the whole batch
        // has established that the request is still current.
        let mut pending_file_picker: Option<PendingFilePicker> = None;
        for event in events {
            let active = self.active_tab.get();
            match event {
                UiEvent::Url(tab, url) => {
                    let reader_was_visible = self.leave_reader_for_tab(tab, false);
                    discard_pending_file_picker(&mut pending_file_picker, tab, None);
                    if Some(tab) == active {
                        self.invalidate_user_action_epoch();
                    }
                    self.with_webview(tab, |webview| {
                        self.user_action_grants.borrow_mut().remove(&webview.id());
                    });
                    let _ = self.core.borrow_mut().location_changed(tab, url.clone());
                    if Some(tab) == active {
                        let mut gui = self.gui.borrow_mut();
                        if !gui.url_dirty {
                            gui.url = url.to_string();
                        }
                    }
                    if reader_was_visible
                        && Some(tab) == active
                        && !self.gui.borrow().has_keyboard_focus()
                    {
                        self.set_page_focus(true);
                    }
                }
                UiEvent::PageTitle(tab, title) => {
                    let title = title.map(|title| text_for_ui(&title, 200));
                    let _ = self.core.borrow_mut().tabs.update(tab, |tab_state| {
                        tab_state.title = title.clone();
                    });
                    if Some(tab) == active {
                        if let Some(title) = title {
                            self.window.set_title(&title);
                        } else {
                            self.window.set_title("Rust Browser");
                        }
                    }
                }
                UiEvent::LoadStatus(tab, status) => {
                    let reader_was_visible = if matches!(status, LoadStatus::Complete) {
                        false
                    } else {
                        if Some(tab) == active {
                            self.invalidate_user_action_epoch();
                        }
                        self.leave_reader_for_tab(tab, false)
                    };
                    let mut core = self.core.borrow_mut();
                    match status {
                        LoadStatus::Complete => {
                            let _ = core.load_finished(tab);
                        }
                        _ => {
                            let _ = core.load_started(tab);
                        }
                    }
                    drop(core);
                    if reader_was_visible
                        && Some(tab) == active
                        && !self.gui.borrow().has_keyboard_focus()
                    {
                        self.set_page_focus(true);
                    }
                }
                UiEvent::ReaderResult(tab, request, result) => {
                    {
                        let mut inflight = self.reader_inflight.borrow_mut();
                        if inflight.get(&request.webview_id) == Some(&request.generation) {
                            inflight.remove(&request.webview_id);
                        }
                    }
                    let pending_matches = self.reader_states.borrow().get(&tab).is_some_and(
                        |state| matches!(state, ReaderTabState::Extracting(current) if current == &request),
                    );
                    let document_matches = self.core.borrow().tabs.get(tab).is_some_and(|state| {
                        request.matches_document(state.navigation_seq, &state.url)
                    });
                    let mut webview_matches = false;
                    self.with_webview(tab, |webview| {
                        webview_matches = webview.id() == request.webview_id
                            && matches!(webview.load_status(), LoadStatus::Complete)
                            && webview.url().as_ref() == Some(&request.source_url);
                    });
                    if !pending_matches || !document_matches || !webview_matches {
                        if pending_matches {
                            let restore_focus =
                                Some(tab) == active && !self.gui.borrow().has_keyboard_focus();
                            self.leave_reader_for_tab(tab, restore_focus);
                        }
                        continue;
                    }
                    match result {
                        Ok(article) => {
                            self.with_webview(tab, |webview| webview.set_throttled(true));
                            self.reader_states.borrow_mut().insert(
                                tab,
                                ReaderTabState::Ready {
                                    request,
                                    article: Arc::new(article),
                                },
                            );
                        }
                        Err(error) => {
                            // Keep an error surface from exposing a fully active,
                            // animating page underneath it. Retry/close always
                            // restores normal scheduling first.
                            self.with_webview(tab, |webview| webview.set_throttled(true));
                            self.reader_states
                                .borrow_mut()
                                .insert(tab, ReaderTabState::Error { request, error });
                        }
                    }
                    if Some(tab) == active {
                        self.set_page_focus(false);
                    }
                }
                UiEvent::Closed(tab) | UiEvent::Crashed(tab) => {
                    self.close_tab(tab);
                }
                UiEvent::ContextMenu(tab, id, menu) => {
                    if Some(tab) == active && !self.active_reader_is_visible() {
                        discard_pending_file_picker(&mut pending_file_picker, tab, None);
                        info!("Context menu requested");
                        self.prepare_embedder_control(tab);
                        *self.active_context_menu.borrow_mut() = Some(PendingControl {
                            tab,
                            id,
                            control: menu,
                        });
                    }
                }
                UiEvent::SelectElement(tab, id, select) => {
                    if Some(tab) == active && !self.active_reader_is_visible() {
                        discard_pending_file_picker(&mut pending_file_picker, tab, None);
                        info!("Select element requested");
                        self.prepare_embedder_control(tab);
                        *self.active_select_value.borrow_mut() = select.selected_options();
                        *self.active_select.borrow_mut() = Some(PendingControl {
                            tab,
                            id,
                            control: select,
                        });
                    }
                }
                UiEvent::ColorPicker(tab, id, picker, color) => {
                    if Some(tab) == active && !self.active_reader_is_visible() {
                        discard_pending_file_picker(&mut pending_file_picker, tab, None);
                        info!("Color picker requested");
                        self.prepare_embedder_control(tab);
                        self.active_color_value.set(color);
                        *self.active_color_picker.borrow_mut() = Some(PendingControl {
                            tab,
                            id,
                            control: picker,
                        });
                    }
                }
                UiEvent::FilePicker(tab, id, picker, grant) => {
                    if Some(tab) == active && pending_file_picker.is_none() {
                        pending_file_picker = Some(PendingFilePicker {
                            tab,
                            id,
                            control: picker,
                            grant,
                        });
                    } else if Some(tab) == active {
                        pending_file_picker.take();
                        debug!("Dismissed competing file-picker requests");
                    }
                }
                UiEvent::SimpleDialog(tab, id, dialog) => {
                    if Some(tab) == active {
                        discard_pending_file_picker(&mut pending_file_picker, tab, None);
                        info!("Simple dialog requested");
                        self.prepare_embedder_control(tab);
                        self.dialog_focus_request.set(Some(id));
                        *self.active_dialog.borrow_mut() = Some(PendingControl {
                            tab,
                            id,
                            control: dialog,
                        });
                    }
                }
                UiEvent::InputMethod(tab, control) => {
                    self.page_ime_controls.borrow_mut().insert(tab, control);
                }
                UiEvent::HideEmbedderControl(tab, id) => {
                    let hides_ime = self
                        .page_ime_controls
                        .borrow()
                        .get(&tab)
                        .is_some_and(|control| control.id == id);
                    if hides_ime {
                        self.page_ime_controls.borrow_mut().remove(&tab);
                        self.cancel_page_composition(tab);
                    }
                    // Drop the matching pending control; its `Drop`
                    // implementation responds "dismissed" to Servo.
                    if discard_pending_file_picker(&mut pending_file_picker, tab, Some(id)) {
                        continue;
                    }
                    if self
                        .active_context_menu
                        .borrow()
                        .as_ref()
                        .is_some_and(|pending| pending.tab == tab && pending.id == id)
                    {
                        self.active_context_menu.borrow_mut().take();
                        if Some(tab) == self.active_tab.get() {
                            self.gui.borrow_mut().surrender_focus();
                            self.restore_page_focus();
                        }
                    } else if self
                        .active_select
                        .borrow()
                        .as_ref()
                        .is_some_and(|pending| pending.tab == tab && pending.id == id)
                    {
                        self.active_select.borrow_mut().take();
                        self.active_select_value.borrow_mut().clear();
                        if Some(tab) == self.active_tab.get() {
                            self.gui.borrow_mut().surrender_focus();
                            self.restore_page_focus();
                        }
                    } else if self
                        .active_dialog
                        .borrow()
                        .as_ref()
                        .is_some_and(|pending| pending.tab == tab && pending.id == id)
                    {
                        self.active_dialog.borrow_mut().take();
                        self.dialog_focus_request.set(None);
                        if Some(tab) == self.active_tab.get() {
                            self.gui.borrow_mut().surrender_focus();
                            self.restore_page_focus();
                        }
                    } else if self
                        .active_color_picker
                        .borrow()
                        .as_ref()
                        .is_some_and(|pending| pending.tab == tab && pending.id == id)
                    {
                        self.active_color_picker.borrow_mut().take();
                        if Some(tab) == self.active_tab.get() {
                            self.gui.borrow_mut().surrender_focus();
                            self.restore_page_focus();
                        }
                    }
                }
            }
        }
        if let Some(pending) = pending_file_picker {
            let authorized = self.active_tab.get() == Some(pending.tab)
                && self.page_focus.get()
                && !self.gui.borrow().has_keyboard_focus()
                && !self.page_control_owns_focus()
                && reserved_user_action_grant_is_valid(
                    pending.grant,
                    Instant::now(),
                    self.user_action_epoch.get(),
                    UserActionCapability::FilePicker,
                );
            if authorized {
                info!("File picker requested");
                self.show_native_file_picker(pending.tab, pending.control);
            } else {
                debug!("Dismissed stale or unauthorized file-picker request");
            }
        }
    }

    fn prepare_embedder_control(&self, tab: TabId) {
        self.cancel_page_composition(tab);
        self.dismiss_transient_controls();
        self.gui.borrow_mut().surrender_focus();
        self.ui_ime_composing.set(false);
        self.set_page_focus(false);
    }

    fn show_native_file_picker(&self, tab: TabId, mut picker: FilePicker) {
        self.prepare_embedder_control(tab);
        let allow_multiple = picker.allow_select_multiple();
        let extensions = normalized_file_filter_extensions(
            picker
                .filter_patterns()
                .iter()
                .map(|pattern| pattern.0.as_str()),
        );
        let title = format!(
            "{} — {}",
            if allow_multiple {
                "Choose files"
            } else {
                "Choose file"
            },
            self.active_page_label()
        );
        let mut dialog = rfd::FileDialog::new()
            .set_parent(self.window.as_ref())
            .set_title(title);
        if !extensions.is_empty() {
            dialog = dialog.add_filter("Accepted files", &extensions);
        }

        let selected = if allow_multiple {
            dialog.pick_files()
        } else {
            dialog.pick_file().map(|path| vec![path])
        };
        if let Some(paths) = selected.filter(|paths| !paths.is_empty()) {
            // The native dialog is the authorization boundary. Do not
            // canonicalize or log the user-selected paths here.
            picker.select(&paths);
            picker.submit();
        } else {
            picker.dismiss();
        }
        if self.active_tab.get() == Some(tab) {
            self.restore_page_focus();
        }
    }

    fn point_in_webview(&self, position: PhysicalPosition<f64>) -> bool {
        let gui = self.gui.borrow();
        let scale = gui.egui_ctx.pixels_per_point();
        // winit reports cursor positions relative to the client area
        // origin, in physical pixels.
        let point = egui::pos2(position.x as f32 / scale, position.y as f32 / scale);
        gui.webview_rect.contains(point)
    }

    /// The cursor position relative to the top-left corner of the
    /// WebView, in physical pixels. Servo expects viewport-relative
    /// coordinates, not window or screen coordinates.
    fn webview_relative_point(&self, position: PhysicalPosition<f64>) -> DevicePoint {
        let gui = self.gui.borrow();
        let scale = gui.egui_ctx.pixels_per_point();
        DevicePoint::new(
            (position.x - gui.webview_rect.min.x as f64 * scale as f64) as f32,
            (position.y - gui.webview_rect.min.y as f64 * scale as f64) as f32,
        )
    }

    /// Reclassify the stationary pointer after a capture ends or an
    /// egui popup changes geometry. `last_mouse_point` is deliberately
    /// a routing marker as well as Servo's relative coordinate.
    fn refresh_pointer_route(&self, notify_page: bool) {
        let Some(position) = self.last_cursor_position.get() else {
            self.last_mouse_point.set(None);
            return;
        };
        if self.page_pointer_capture.get().is_some() {
            if notify_page {
                self.forward_cursor_moved(position);
            } else {
                self.last_mouse_point
                    .set(Some(self.webview_relative_point(position)));
            }
            return;
        }
        let reader_visible = self.active_reader_is_visible();
        let over_egui =
            self.has_modal_dialog() || self.gui.borrow().wants_pointer_at(position, reader_visible);
        if self.egui_buttons_down.borrow().is_empty()
            && self.point_in_webview(position)
            && !over_egui
        {
            if notify_page {
                self.forward_cursor_moved(position);
            } else {
                self.last_mouse_point
                    .set(Some(self.webview_relative_point(position)));
            }
        } else {
            self.last_mouse_point.set(None);
        }
    }

    fn forward_cursor_moved(&self, position: PhysicalPosition<f64>) {
        let point = self.webview_relative_point(position);
        self.last_mouse_point.set(Some(point));
        let target = self.page_pointer_capture.get().or(self.active_tab.get());
        if let Some(tab) = target {
            self.with_webview(tab, |webview| {
                webview
                    .notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(point.into())));
            });
        }
    }

    fn forward_mouse_button(
        &self,
        state: ElementState,
        platform_button: winit::event::MouseButton,
    ) {
        let point = self.last_mouse_point.get();
        let action = match state {
            ElementState::Pressed => MouseButtonAction::Down,
            ElementState::Released => MouseButtonAction::Up,
        };
        let button = match platform_button {
            winit::event::MouseButton::Left => MouseButton::Left,
            winit::event::MouseButton::Middle => MouseButton::Middle,
            winit::event::MouseButton::Right => MouseButton::Right,
            winit::event::MouseButton::Back => MouseButton::Back,
            winit::event::MouseButton::Forward => MouseButton::Forward,
            winit::event::MouseButton::Other(id) => MouseButton::Other(id),
        };
        let target = match state {
            ElementState::Pressed => {
                self.page_buttons_down.borrow_mut().insert(platform_button);
                let tab = self.page_pointer_capture.get().or(self.active_tab.get());
                self.page_pointer_capture.set(tab);
                tab
            }
            ElementState::Released => self.page_pointer_capture.get().or(self.active_tab.get()),
        };
        if let (Some(tab), Some(point)) = (target, point) {
            self.with_webview(tab, |webview| {
                webview.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                    action,
                    button,
                    point.into(),
                )));
            });
        }
        if state == ElementState::Released {
            self.page_buttons_down.borrow_mut().remove(&platform_button);
            if self.page_buttons_down.borrow().is_empty() {
                self.page_pointer_capture.set(None);
            }
        }
    }

    fn release_page_pointer_capture(&self) {
        let buttons: Vec<_> = self.page_buttons_down.borrow().iter().copied().collect();
        for button in buttons {
            self.forward_mouse_button(ElementState::Released, button);
        }
        self.page_buttons_down.borrow_mut().clear();
        self.page_pointer_capture.set(None);
    }

    fn forward_mouse_wheel(&self, delta: &MouseScrollDelta) {
        let Some(point) = self.last_mouse_point.get() else {
            return;
        };
        let delta = wheel_delta_from_winit(delta);
        let event = InputEvent::Wheel(WheelEvent::new(delta, point.into()));
        self.with_active_webview(|webview| {
            webview.notify_input_event(event);
        });
    }

    /// Track the modifier state from the key events themselves. The
    /// Windows backend of winit derives its modifier events from
    /// `GetKeyState`, which input synthesized with `PostMessage` (and
    /// in some sessions even `SendInput`) never updates, so we cannot
    /// rely on `ModifiersChanged` alone.
    fn track_modifier_key(&self, key_event: &KeyEvent) {
        let pressed = key_event.state == ElementState::Pressed;
        let mut modifiers = self.modifiers.get();
        match key_event.logical_key {
            Key::Named(NamedKey::Control) => {
                if pressed {
                    modifiers.insert(ModifiersState::CONTROL);
                } else {
                    modifiers.remove(ModifiersState::CONTROL);
                }
            }
            Key::Named(NamedKey::Shift) => {
                if pressed {
                    modifiers.insert(ModifiersState::SHIFT);
                } else {
                    modifiers.remove(ModifiersState::SHIFT);
                }
            }
            Key::Named(NamedKey::Alt) => {
                if pressed {
                    modifiers.insert(ModifiersState::ALT);
                } else {
                    modifiers.remove(ModifiersState::ALT);
                }
            }
            Key::Named(NamedKey::AltGraph) => {
                self.alt_graph.set(pressed);
            }
            Key::Named(NamedKey::Super) | Key::Named(NamedKey::Meta) => {
                if pressed {
                    modifiers.insert(ModifiersState::SUPER);
                } else {
                    modifiers.remove(ModifiersState::SUPER);
                }
            }
            _ => {}
        }
        self.modifiers.set(modifiers);
    }

    /// Translate a winit key event into a Servo keyboard event and hand
    /// it to the page.
    fn forward_keyboard(&self, key_event: &KeyEvent) {
        let event = keyboard_types::KeyboardEvent {
            state: match key_event.state {
                ElementState::Pressed => keyboard_types::KeyState::Down,
                ElementState::Released => keyboard_types::KeyState::Up,
            },
            key: key_from_winit(key_event.logical_key.clone()),
            code: code_from_physical_key(key_event.physical_key),
            location: match key_event.location {
                winit::keyboard::KeyLocation::Standard => keyboard_types::Location::Standard,
                winit::keyboard::KeyLocation::Left => keyboard_types::Location::Left,
                winit::keyboard::KeyLocation::Right => keyboard_types::Location::Right,
                winit::keyboard::KeyLocation::Numpad => keyboard_types::Location::Numpad,
            },
            modifiers: modifiers_from_winit(self.modifiers.get(), self.alt_graph.get()),
            repeat: key_event.repeat,
            is_composing: self.ime_composing_tab.get() == self.active_tab.get(),
        };
        let clipboard_access =
            clipboard_access_for_key_event(key_event, self.modifiers.get(), self.alt_graph.get());
        let grants_user_action = key_event.state == ElementState::Pressed
            && !key_event.repeat
            && matches!(
                key_event.logical_key,
                Key::Named(NamedKey::Enter | NamedKey::Space)
            );
        self.with_active_webview(|webview| {
            if let Some(access) = clipboard_access {
                self.clipboard.grant(webview.id(), access);
            }
            if grants_user_action {
                self.grant_user_action(webview.id(), UserActionGrantKind::General);
            }
            webview.notify_input_event(InputEvent::Keyboard(
                servo::input_events::KeyboardEvent::new(event),
            ));
        });
    }

    /// Browser shortcuts (Ctrl+T/W/Tab, Ctrl+L, zoom) must win over
    /// both the page and the address bar. Returns true when the event
    /// was consumed.
    fn handle_browser_shortcut(
        self: &Rc<Self>,
        key_event: &KeyEvent,
        emergency_only: bool,
    ) -> bool {
        if key_event.state == ElementState::Released {
            return self
                .consumed_shortcuts
                .borrow_mut()
                .remove(&key_event.physical_key);
        }
        if key_event.repeat {
            return self
                .consumed_shortcuts
                .borrow()
                .contains(&key_event.physical_key);
        }
        let modifiers = self.modifiers.get();
        // AltGraph is commonly reported as Ctrl+Alt. It must still be
        // available for text entry instead of triggering browser UI.
        if !browser_shortcuts_enabled(modifiers, self.alt_graph.get()) {
            return false;
        }
        let tab_lifecycle_shortcut = matches!(
            key_event.physical_key,
            PhysicalKey::Code(KeyCode::KeyT | KeyCode::KeyW | KeyCode::Tab)
        );
        if self.ui_ime_composing.get() && tab_lifecycle_shortcut {
            return false;
        }
        if emergency_only
            && !matches!(
                key_event.physical_key,
                PhysicalKey::Code(KeyCode::KeyW | KeyCode::Tab)
            )
        {
            return false;
        }

        let handled = match key_event.physical_key {
            PhysicalKey::Code(KeyCode::KeyT) => {
                self.create_tab(self.start_page_url());
                self.gui.borrow_mut().focus_url_bar();
                self.set_page_focus(false);
                true
            }
            PhysicalKey::Code(KeyCode::KeyW) => {
                let page_control_owned_focus = self.page_control_owns_focus();
                if let Some(tab) = self.active_tab.get() {
                    self.close_tab(tab);
                }
                if emergency_only || page_control_owned_focus {
                    self.gui.borrow_mut().surrender_focus();
                    self.restore_page_focus();
                }
                true
            }
            PhysicalKey::Code(KeyCode::Tab) => {
                let previous = self.active_tab.get();
                let page_control_owned_focus = self.page_control_owns_focus();
                self.cycle_tab(modifiers.shift_key());
                if (emergency_only || page_control_owned_focus) && self.active_tab.get() != previous
                {
                    self.gui.borrow_mut().surrender_focus();
                    self.restore_page_focus();
                }
                true
            }
            PhysicalKey::Code(KeyCode::KeyL) => {
                self.dismiss_transient_controls();
                if let Some(tab) = self.ime_composing_tab.get() {
                    self.cancel_page_composition(tab);
                }
                self.gui.borrow_mut().focus_url_bar();
                self.set_page_focus(false);
                self.window.request_redraw();
                true
            }
            PhysicalKey::Code(KeyCode::NumpadAdd) => {
                self.zoom_by(0.1);
                true
            }
            _ if matches!(&key_event.logical_key, Key::Character(value) if value == "+" || value == "=") =>
            {
                // Logical matching supports both Ctrl+Shift+= on US
                // layouts and the dedicated + key on German layouts.
                self.zoom_by(0.1);
                true
            }
            PhysicalKey::Code(KeyCode::NumpadSubtract) => {
                self.zoom_by(-0.1);
                true
            }
            _ if matches!(&key_event.logical_key, Key::Character(value) if value == "-") => {
                self.zoom_by(-0.1);
                true
            }
            PhysicalKey::Code(KeyCode::Numpad0) | PhysicalKey::Code(KeyCode::Digit0) => {
                self.reset_zoom();
                true
            }
            _ => false,
        };
        if handled {
            self.consumed_shortcuts
                .borrow_mut()
                .insert(key_event.physical_key);
        }
        handled
    }

    /// Adjust the page zoom of the active tab. Servo clamps to
    /// [0.1, 10.0].
    fn zoom_by(&self, delta: f32) {
        self.with_active_webview(|webview| {
            let next = (webview.page_zoom() + delta).clamp(0.1, 10.0);
            webview.set_page_zoom(next);
            info!("Zoom level: {next:.1}");
        });
    }

    fn reset_zoom(&self) {
        self.with_active_webview(|webview| {
            webview.set_page_zoom(1.0);
            info!("Zoom level: 1.0");
        });
    }

    /// Translate and forward a platform IME event to Servo's explicit
    /// composition lifecycle for the active tab.
    fn forward_page_ime(&self, ime_event: &winit::event::Ime) {
        let Some(tab) = self.active_tab.get() else {
            return;
        };

        let was_composing = self.ime_composing_tab.get() == Some(tab);
        let (is_composing, events) = crate::ime::translate(was_composing, ime_event);
        self.ime_composing_tab.set(is_composing.then_some(tab));
        self.with_webview(tab, |webview| {
            for event in events {
                webview.notify_input_event(InputEvent::Ime(ImeEvent::Composition(event)));
            }
        });
    }

    /// End a composition before its target tab is hidden, closed or
    /// navigated. This prevents a later commit from leaking into a
    /// different tab.
    fn cancel_page_composition(&self, tab: TabId) {
        if self.ime_composing_tab.get() != Some(tab) {
            return;
        }
        self.ime_composing_tab.set(None);
        self.with_webview(tab, |webview| {
            webview
                .notify_input_event(InputEvent::Ime(ImeEvent::Composition(crate::ime::cancel())));
        });
    }

    /// Reconcile egui's window-global IME switch with the active page
    /// field after egui has processed its platform output.
    pub(crate) fn sync_page_ime(&self, webview_rect: egui::Rect, pixels_per_point: f32) {
        if !self.page_focus.get() {
            return;
        }
        let control = self
            .active_tab
            .get()
            .and_then(|tab| self.page_ime_controls.borrow().get(&tab).copied());
        self.window.set_ime_allowed(control.is_some());
        if let Some(control) = control {
            let origin_x = webview_rect.min.x * pixels_per_point;
            let origin_y = webview_rect.min.y * pixels_per_point;
            self.window.set_ime_cursor_area(
                winit::dpi::PhysicalPosition::new(
                    origin_x + control.x as f32,
                    origin_y + control.y as f32,
                ),
                winit::dpi::PhysicalSize::new(control.width, control.height),
            );
        }
    }

    pub(crate) fn ui_ime_composing(&self) -> bool {
        self.ui_ime_composing.get()
    }

    fn active_page_has_ime_target(&self) -> bool {
        self.active_tab
            .get()
            .is_some_and(|tab| self.page_ime_controls.borrow().contains_key(&tab))
    }
}

/// Convert a winit key into the keyboard_types key Servo expects.
fn key_from_winit(key: winit::keyboard::Key) -> keyboard_types::Key {
    match key {
        winit::keyboard::Key::Unidentified(_) => {
            keyboard_types::Key::Named(keyboard_types::NamedKey::Unidentified)
        }
        winit::keyboard::Key::Dead(_) => keyboard_types::Key::Named(keyboard_types::NamedKey::Dead),
        winit::keyboard::Key::Character(text) => keyboard_types::Key::Character(text.to_string()),
        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Space) => {
            keyboard_types::Key::Character(" ".to_owned())
        }
        winit::keyboard::Key::Named(named) => {
            keyboard_types::Key::Named(named_key_from_winit(named))
        }
    }
}

/// Map the named keys that matter for everyday browsing; everything
/// else collapses to `Unidentified`, which is harmless.
fn named_key_from_winit(named: winit::keyboard::NamedKey) -> keyboard_types::NamedKey {
    use winit::keyboard::NamedKey as W;
    match named {
        W::Alt => keyboard_types::NamedKey::Alt,
        W::AltGraph => keyboard_types::NamedKey::AltGraph,
        W::CapsLock => keyboard_types::NamedKey::CapsLock,
        W::Control => keyboard_types::NamedKey::Control,
        W::Fn => keyboard_types::NamedKey::Fn,
        W::FnLock => keyboard_types::NamedKey::FnLock,
        W::NumLock => keyboard_types::NamedKey::NumLock,
        W::ScrollLock => keyboard_types::NamedKey::ScrollLock,
        W::Shift => keyboard_types::NamedKey::Shift,
        W::Symbol => keyboard_types::NamedKey::Symbol,
        W::SymbolLock => keyboard_types::NamedKey::SymbolLock,
        W::Meta => keyboard_types::NamedKey::Meta,
        W::Hyper | W::Super => keyboard_types::NamedKey::Meta,
        W::Enter => keyboard_types::NamedKey::Enter,
        W::Tab => keyboard_types::NamedKey::Tab,
        W::ArrowDown => keyboard_types::NamedKey::ArrowDown,
        W::ArrowLeft => keyboard_types::NamedKey::ArrowLeft,
        W::ArrowRight => keyboard_types::NamedKey::ArrowRight,
        W::ArrowUp => keyboard_types::NamedKey::ArrowUp,
        W::End => keyboard_types::NamedKey::End,
        W::Home => keyboard_types::NamedKey::Home,
        W::PageDown => keyboard_types::NamedKey::PageDown,
        W::PageUp => keyboard_types::NamedKey::PageUp,
        W::Backspace => keyboard_types::NamedKey::Backspace,
        W::Clear => keyboard_types::NamedKey::Clear,
        W::Copy => keyboard_types::NamedKey::Copy,
        W::Cut => keyboard_types::NamedKey::Cut,
        W::Delete => keyboard_types::NamedKey::Delete,
        W::Insert => keyboard_types::NamedKey::Insert,
        W::Paste => keyboard_types::NamedKey::Paste,
        W::Redo => keyboard_types::NamedKey::Redo,
        W::Undo => keyboard_types::NamedKey::Undo,
        W::ContextMenu => keyboard_types::NamedKey::ContextMenu,
        W::Escape => keyboard_types::NamedKey::Escape,
        W::Find => keyboard_types::NamedKey::Find,
        W::Help => keyboard_types::NamedKey::Help,
        W::Pause => keyboard_types::NamedKey::Pause,
        W::PrintScreen => keyboard_types::NamedKey::PrintScreen,
        W::Select => keyboard_types::NamedKey::Select,
        W::ZoomIn => keyboard_types::NamedKey::ZoomIn,
        W::ZoomOut => keyboard_types::NamedKey::ZoomOut,
        W::MediaPlayPause => keyboard_types::NamedKey::MediaPlayPause,
        W::MediaStop => keyboard_types::NamedKey::MediaStop,
        W::MediaTrackNext => keyboard_types::NamedKey::MediaTrackNext,
        W::MediaTrackPrevious => keyboard_types::NamedKey::MediaTrackPrevious,
        W::AudioVolumeDown => keyboard_types::NamedKey::AudioVolumeDown,
        W::AudioVolumeMute => keyboard_types::NamedKey::AudioVolumeMute,
        W::AudioVolumeUp => keyboard_types::NamedKey::AudioVolumeUp,
        W::BrowserBack => keyboard_types::NamedKey::BrowserBack,
        W::BrowserFavorites => keyboard_types::NamedKey::BrowserFavorites,
        W::BrowserForward => keyboard_types::NamedKey::BrowserForward,
        W::BrowserHome => keyboard_types::NamedKey::BrowserHome,
        W::BrowserRefresh => keyboard_types::NamedKey::BrowserRefresh,
        W::BrowserSearch => keyboard_types::NamedKey::BrowserSearch,
        W::BrowserStop => keyboard_types::NamedKey::BrowserStop,
        W::Convert => keyboard_types::NamedKey::Convert,
        W::NonConvert => keyboard_types::NamedKey::NonConvert,
        W::Process => keyboard_types::NamedKey::Process,
        W::SingleCandidate => keyboard_types::NamedKey::SingleCandidate,
        _ => keyboard_types::NamedKey::Unidentified,
    }
}

/// The physical key code of a key press, as the DOM reports it.
fn code_from_physical_key(physical_key: winit::keyboard::PhysicalKey) -> keyboard_types::Code {
    match physical_key {
        winit::keyboard::PhysicalKey::Code(key_code) => {
            // Both sides speak the W3C UI Events code names; fall back
            // to Unidentified for anything we cannot round-trip.
            let name = format!("{key_code:?}");
            keyboard_types::Code::from_str(&name).unwrap_or(keyboard_types::Code::Unidentified)
        }
        winit::keyboard::PhysicalKey::Unidentified(_) => keyboard_types::Code::Unidentified,
    }
}

/// Convert winit modifier state into keyboard_types modifiers.
fn modifiers_from_winit(
    modifiers: winit::keyboard::ModifiersState,
    alt_graph: bool,
) -> keyboard_types::Modifiers {
    let mut result = keyboard_types::Modifiers::empty();
    if modifiers.shift_key() {
        result |= keyboard_types::Modifiers::SHIFT;
    }
    if modifiers.control_key() {
        result |= keyboard_types::Modifiers::CONTROL;
    }
    if modifiers.alt_key() {
        result |= keyboard_types::Modifiers::ALT;
    }
    if modifiers.super_key() {
        result |= keyboard_types::Modifiers::META;
    }
    if alt_graph {
        result |= keyboard_types::Modifiers::ALT_GRAPH;
    }
    result
}

fn browser_shortcuts_enabled(modifiers: ModifiersState, alt_graph: bool) -> bool {
    modifiers.control_key() && !modifiers.alt_key() && !alt_graph
}

fn browser_servo_options() -> servo::Opts {
    // Let pipeline failures reach `notify_crashed` instead of
    // terminating the entire process, and make session-temporary
    // storage an explicit embedder choice.
    servo::Opts {
        hard_fail: false,
        temporary_storage: true,
        ..servo::Opts::default()
    }
}

fn user_action_grant_is_valid(grant: UserActionGrant, now: Instant) -> bool {
    now <= grant.expires_at
}

fn reserved_user_action_grant_is_valid(
    grant: UserActionGrant,
    now: Instant,
    current_epoch: u64,
    capability: UserActionCapability,
) -> bool {
    grant.epoch == current_epoch
        && user_action_grant_is_valid(grant, now)
        && grant.allows(capability)
}

fn take_user_action_grant_queue(
    queue: &mut VecDeque<UserActionGrant>,
    now: Instant,
    capability: UserActionCapability,
) -> Option<UserActionGrant> {
    queue.retain(|grant| user_action_grant_is_valid(*grant, now));
    let index = queue.iter().position(|grant| grant.allows(capability))?;
    queue.remove(index)
}

#[cfg(test)]
fn consume_user_action_grant_queue(
    queue: &mut VecDeque<UserActionGrant>,
    now: Instant,
    capability: UserActionCapability,
) -> bool {
    take_user_action_grant_queue(queue, now, capability).is_some()
}

fn normalize_file_filter_extension(input: &str) -> Option<String> {
    let extension = input.trim();
    if extension.is_empty()
        || extension.len() > MAX_FILE_FILTER_LENGTH
        || extension.starts_with('.')
        || extension.ends_with('.')
        || !extension
            .chars()
            .any(|character| character.is_ascii_alphanumeric())
        || !extension.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-' | '+')
        })
    {
        return None;
    }
    Some(extension.to_ascii_lowercase())
}

fn normalized_file_filter_extensions<'a>(
    patterns: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let mut seen = HashSet::new();
    patterns
        .into_iter()
        .filter_map(normalize_file_filter_extension)
        .filter(|extension| seen.insert(extension.clone()))
        .take(MAX_FILE_FILTERS)
        .collect()
}

fn wheel_delta_from_winit(delta: &MouseScrollDelta) -> WheelDelta {
    let (x, y, mode) = match delta {
        MouseScrollDelta::LineDelta(x, y) => (*x as f64, *y as f64, WheelMode::DeltaLine),
        MouseScrollDelta::PixelDelta(position) => (position.x, position.y, WheelMode::DeltaPixel),
    };
    WheelDelta { x, y, z: 0.0, mode }
}

fn clipboard_access_for_key_event(
    key_event: &KeyEvent,
    modifiers: ModifiersState,
    alt_graph: bool,
) -> Option<crate::clipboard::ClipboardAccess> {
    if key_event.state != ElementState::Pressed || key_event.repeat || alt_graph {
        return None;
    }
    let control = modifiers.control_key() && !modifiers.alt_key();
    let shift = modifiers.shift_key();
    match key_event.physical_key {
        PhysicalKey::Code(KeyCode::KeyV) if control => {
            Some(crate::clipboard::ClipboardAccess::Read)
        }
        PhysicalKey::Code(KeyCode::Insert) if shift => {
            Some(crate::clipboard::ClipboardAccess::Read)
        }
        PhysicalKey::Code(KeyCode::KeyC | KeyCode::KeyX) if control => {
            Some(crate::clipboard::ClipboardAccess::Write)
        }
        PhysicalKey::Code(KeyCode::Insert) if control => {
            Some(crate::clipboard::ClipboardAccess::Write)
        }
        PhysicalKey::Code(KeyCode::Delete) if shift => {
            Some(crate::clipboard::ClipboardAccess::Write)
        }
        _ => None,
    }
}

fn normalize_site_host(input: &str) -> Result<String, String> {
    let input = input.trim().trim_end_matches('.');
    if input.is_empty() {
        return Err("Enter a site host".to_owned());
    }
    Host::parse(input)
        .map(|host| host.to_string())
        .map_err(|_| "Enter a host such as example.com (without a URL path)".to_owned())
}

fn trackers_allowed_for_site(
    overrides: &HashMap<String, bool>,
    top_level_host: Option<&str>,
) -> bool {
    top_level_host.is_some_and(|host| overrides.get(host) == Some(&true))
}

fn site_host_for_request(page: Option<&Url>) -> Option<String> {
    page.and_then(Url::host_str)
        .map(|host| host.trim_end_matches('.').to_ascii_lowercase())
}

pub(crate) fn url_identity(url: &Url) -> String {
    if url.has_host() {
        url.origin().ascii_serialization()
    } else {
        format!("{}:", url.scheme())
    }
}

fn global_http_context_is_unsafe(url: &Url, initiator: Option<&Url>) -> bool {
    url.scheme() == "http" && initiator.is_none_or(|source| source.scheme() != "http")
}

fn proxy_default_http_port_is_unsupported(proxy_enabled: bool, url: &Url) -> bool {
    // hyper-util's Tunnel connector defaults a destination without an
    // explicit non-default port to 443, even for http://. Servo 0.5 uses
    // that tunnel for both proxy preferences, so stop the request before
    // it can be sent to the wrong destination port.
    proxy_enabled && url.scheme() == "http" && url.port().is_none()
}

fn browser_servo_preferences(proxy: Option<&StartupProxy>) -> servo::Preferences {
    let mut preferences = servo::Preferences::default();
    preferences.network_http_proxy_uri.clear();
    preferences.network_https_proxy_uri.clear();
    preferences.network_http_no_proxy.clear();
    if let Some(proxy) = proxy {
        preferences.network_http_proxy_uri = proxy.uri().to_owned();
        preferences.network_https_proxy_uri = proxy.uri().to_owned();
        preferences.network_http_no_proxy = proxy.bypass().to_owned();
    }
    preferences
}

pub(crate) fn text_for_ui(input: &str, max_chars: usize) -> String {
    let mut output = String::with_capacity(input.len().min(max_chars));
    let mut chars = input.chars();
    for character in chars.by_ref().take(max_chars) {
        output.push(if character.is_control() {
            ' '
        } else {
            character
        });
    }
    if chars.next().is_some() {
        output.push('…');
    }
    output
}

/// Map a Servo cursor request to the winit cursor the OS shows.
fn cursor_icon_for(cursor: Cursor) -> CursorIcon {
    match cursor {
        Cursor::None | Cursor::Default => CursorIcon::Default,
        Cursor::Pointer => CursorIcon::Pointer,
        Cursor::ContextMenu => CursorIcon::ContextMenu,
        Cursor::Help => CursorIcon::Help,
        Cursor::Progress => CursorIcon::Progress,
        Cursor::Wait => CursorIcon::Wait,
        Cursor::Cell => CursorIcon::Cell,
        Cursor::Crosshair => CursorIcon::Crosshair,
        Cursor::Text => CursorIcon::Text,
        Cursor::VerticalText => CursorIcon::VerticalText,
        Cursor::Alias => CursorIcon::Alias,
        Cursor::Copy => CursorIcon::Copy,
        Cursor::Move => CursorIcon::Move,
        Cursor::NoDrop => CursorIcon::NoDrop,
        Cursor::NotAllowed => CursorIcon::NotAllowed,
        Cursor::Grab => CursorIcon::Grab,
        Cursor::Grabbing => CursorIcon::Grabbing,
        Cursor::EResize => CursorIcon::EResize,
        Cursor::NResize => CursorIcon::NResize,
        Cursor::NeResize => CursorIcon::NeResize,
        Cursor::NwResize => CursorIcon::NwResize,
        Cursor::SResize => CursorIcon::SResize,
        Cursor::SeResize => CursorIcon::SeResize,
        Cursor::SwResize => CursorIcon::SwResize,
        Cursor::WResize => CursorIcon::WResize,
        Cursor::EwResize => CursorIcon::EwResize,
        Cursor::NsResize => CursorIcon::NsResize,
        Cursor::NeswResize => CursorIcon::NeswResize,
        Cursor::NwseResize => CursorIcon::NwseResize,
        Cursor::ColResize => CursorIcon::ColResize,
        Cursor::RowResize => CursorIcon::RowResize,
        Cursor::AllScroll => CursorIcon::AllScroll,
        Cursor::ZoomIn => CursorIcon::ZoomIn,
        Cursor::ZoomOut => CursorIcon::ZoomOut,
    }
}

impl WebViewDelegate for AppState {
    fn notify_new_frame_ready(&self, _webview: WebView) {
        self.window.request_redraw();
    }

    fn notify_animating_changed(&self, webview: WebView, animating: bool) {
        let Some(tab) = self.tab_for(&webview) else {
            return;
        };
        if animating {
            self.animating_tabs.borrow_mut().insert(tab);
            if self.active_tab.get() == Some(tab) {
                self.window.request_redraw();
            }
        } else {
            self.animating_tabs.borrow_mut().remove(&tab);
        }
    }

    fn notify_url_changed(&self, webview: WebView, url: Url) {
        info!("URL changed: {}", url_identity(&url));
        if let Some(tab) = self.tab_for(&webview) {
            self.queue_ui_event(UiEvent::Url(tab, url));
        }
    }

    fn notify_page_title_changed(&self, webview: WebView, title: Option<String>) {
        if let Some(tab) = self.tab_for(&webview) {
            self.queue_ui_event(UiEvent::PageTitle(tab, title));
        }
    }

    fn notify_load_status_changed(&self, webview: WebView, status: LoadStatus) {
        info!("Load status: {status:?}");
        if let Some(tab) = self.tab_for(&webview) {
            self.queue_ui_event(UiEvent::LoadStatus(tab, status));
        }
    }

    fn notify_history_changed(&self, webview: WebView, entries: Vec<Url>, current: usize) {
        if let Some(tab) = self.tab_for(&webview) {
            let history = TabHistory {
                len: entries.len(),
                current,
            };
            let can_go_back = history.can_go_back();
            let can_go_forward = history.can_go_forward();
            self.tab_history.borrow_mut().insert(tab, history);
            if self.active_tab.get() == Some(tab) {
                self.can_go_back.set(can_go_back);
                self.can_go_forward.set(can_go_forward);
            }
            self.needs_repaint.set(true);
            self.window.request_redraw();
        }
    }

    /// `window.close()` on the page: drop the tab.
    fn notify_closed(&self, webview: WebView) {
        info!("WebView closed by the page");
        if let Some(tab) = self.tab_for(&webview) {
            self.queue_ui_event(UiEvent::Closed(tab));
        }
    }

    /// A pipeline in the WebView panicked: drop the tab instead of
    /// taking down the app. The Servo instance keeps running.
    fn notify_crashed(&self, webview: WebView, _reason: String, _backtrace: Option<String>) {
        warn!("WebView crashed; closing the affected tab");
        if let Some(tab) = self.tab_for(&webview) {
            self.queue_ui_event(UiEvent::Crashed(tab));
        }
    }

    fn notify_cursor_changed(&self, webview: WebView, cursor: Cursor) {
        if self.tab_for(&webview) == self.active_tab.get() {
            self.window
                .set_cursor(winit::window::Cursor::Icon(cursor_icon_for(cursor)));
        }
    }

    fn request_create_new(&self, parent_webview: WebView, request: CreateNewWebViewRequest) {
        if !self.consume_user_action_grant(parent_webview.id(), UserActionCapability::Popup) {
            debug!("Blocked popup without a recent user gesture");
            drop(request);
            return;
        }
        let webview = request
            .builder(self.rendering_context.clone())
            .hidpi_scale_factor(Scale::new(self.window.scale_factor() as f32))
            .delegate(parent_webview.delegate())
            .clipboard_delegate(self.clipboard.clone())
            .build();
        let url = webview
            .url()
            .unwrap_or_else(|| Url::parse("about:blank").expect("static URL"));
        let tab = self.core.borrow_mut().tabs.create_tab(url);
        self.webviews.borrow_mut().push(TabWebView {
            tab,
            webview,
            viewport_size: winit::dpi::PhysicalSize::new(0, 0),
        });
        self.activate_tab(tab);
    }

    fn request_navigation(&self, _webview: WebView, request: NavigationRequest) {
        let url = request.url.clone();
        if self.core.borrow().validate_content_navigation(&url).is_ok() {
            request.allow();
        } else {
            debug!("Blocked content navigation to {}", url_identity(&url));
            request.deny();
        }
    }

    /// Form element UI and dialogs requested by page content. File
    /// selection uses a native system dialog; the remaining supported
    /// controls are rendered by egui. Everything else is dismissed.
    fn show_embedder_control(&self, webview: WebView, control: EmbedderControl) {
        let Some(tab) = self.tab_for(&webview) else {
            return;
        };
        let id = control.id();
        match control {
            EmbedderControl::ContextMenu(menu) => {
                self.queue_ui_event(UiEvent::ContextMenu(tab, id, menu));
            }
            EmbedderControl::SelectElement(select) => {
                self.queue_ui_event(UiEvent::SelectElement(tab, id, select));
            }
            EmbedderControl::ColorPicker(picker) => {
                let color = picker.current_color().unwrap_or(RgbColor {
                    red: 0,
                    green: 0,
                    blue: 0,
                });
                self.queue_ui_event(UiEvent::ColorPicker(
                    tab,
                    id,
                    picker,
                    [color.red, color.green, color.blue],
                ));
            }
            EmbedderControl::FilePicker(picker) => {
                let grant = if self.active_tab.get() == Some(tab)
                    && !self.page_control_owns_focus()
                    && self.page_focus.get()
                {
                    self.take_user_action_grant(webview.id(), UserActionCapability::FilePicker)
                } else {
                    None
                };
                let Some(grant) = grant else {
                    debug!("Blocked ineligible active-tab file picker");
                    drop(picker);
                    return;
                };
                self.queue_ui_event(UiEvent::FilePicker(tab, id, picker, grant));
            }
            EmbedderControl::SimpleDialog(dialog) => {
                self.queue_ui_event(UiEvent::SimpleDialog(tab, id, dialog));
            }
            EmbedderControl::InputMethod(input_method) => {
                let position = input_method.position();
                let width = (position.max.x - position.min.x).max(1) as u32;
                let height = (position.max.y - position.min.y).max(1) as u32;
                self.queue_ui_event(UiEvent::InputMethod(
                    tab,
                    PageImeControl {
                        id: input_method.id(),
                        x: position.min.x,
                        y: position.min.y,
                        width,
                        height,
                    },
                ));
            }
        }
    }

    fn hide_embedder_control(&self, webview: WebView, id: EmbedderControlId) {
        let Some(tab) = self.tab_for(&webview) else {
            return;
        };
        self.queue_ui_event(UiEvent::HideEmbedderControl(tab, id));
    }

    /// Every intercepted HTTP(S) request passes through the privacy
    /// pipeline here. A
    /// blocked request is answered with an empty 200 response so the
    /// page keeps rendering normally.
    fn load_web_resource(&self, webview: WebView, load: WebResourceLoad) {
        let request = load.request();
        let url = request.url.clone();
        let initiator = request.referrer_url.clone();
        let page_url = webview.url();
        // Per-site exceptions are keyed only by the embedder-owned
        // WebView URL. Referrer metadata is page-controlled and must
        // never select a more permissive policy.
        let site_host = site_host_for_request(page_url.as_ref());
        let resource_type = ResourceType::from_engine_string(request.destination.as_str());
        let trusted_initial_document = resource_type == ResourceType::Document
            && self
                .fresh_browser_webviews
                .borrow_mut()
                .remove(&webview.id());
        if proxy_default_http_port_is_unsupported(self.proxy_enabled, &url) {
            debug!("Blocked default-port HTTP request through Servo 0.5 proxy connector");
            load.intercept(WebResourceResponse::new(url)).cancel();
            return;
        }
        let context = RequestContext::new(
            url.clone(),
            initiator,
            resource_type,
            // Never trust Servo's raw flag: it is true for iframe
            // documents too. Only the first document in an opener-free,
            // browser-created WebView is unambiguously top-level.
            trusted_initial_document,
        )
        .with_top_level_url(page_url);
        let allow_trackers =
            trackers_allowed_for_site(&self.tracker_overrides.borrow(), site_host.as_deref());

        match self
            .pipeline
            .evaluate_with_tracker_override(&context, allow_trackers)
        {
            browser_network::PipelineDecision::Allow => {}
            browser_network::PipelineDecision::Block { layer, reason } => {
                debug!("Blocked {} ({layer}: {reason})", url_identity(&url));
                load.intercept(WebResourceResponse::new(url)).finish();
            }
        }
    }
}

impl AppState {
    /// Settings: the URL new tabs load.
    pub(crate) fn start_page_url(&self) -> Url {
        self.start_page.borrow().clone()
    }

    fn set_start_page(&self, url: Url) {
        info!("Start page set to {}", url_identity(&url));
        *self.start_page.borrow_mut() = url;
    }

    /// Validate settings input with the same policy as the address bar
    /// before it can become the URL loaded by future tabs.
    pub(crate) fn apply_start_page(&self, input: &str) -> Result<(), String> {
        let command = self.core.borrow().command_from_url_input(input, false);
        match command {
            Ok(NavigationCommand::Load(url)) => {
                self.set_start_page(url);
                Ok(())
            }
            Ok(_) => Err("Start page did not resolve to a URL".to_owned()),
            Err(error) => Err(format!("Invalid start page: {error}")),
        }
    }

    /// Session-only search selection. Disabled means address-bar query text
    /// never leaves the browser; suggestions are never requested.
    pub(crate) fn search_engine(&self) -> SearchEngine {
        self.core.borrow().search_engine()
    }

    pub(crate) fn set_search_engine(&self, search_engine: SearchEngine) {
        info!("Search provider set to {}", search_engine.label());
        self.core.borrow_mut().set_search_engine(search_engine);
    }

    /// Settings: per-site tracker override.
    pub(crate) fn set_tracker_override(&self, host: &str, allow: bool) -> Result<(), String> {
        let host = normalize_site_host(host)?;
        info!("Tracker override for {host}: allow={allow}");
        self.tracker_overrides.borrow_mut().insert(host, allow);
        Ok(())
    }

    pub(crate) fn remove_tracker_override(&self, host: &str) {
        info!("Tracker override removed for {host}");
        self.tracker_overrides.borrow_mut().remove(host);
    }

    pub(crate) fn tracker_overrides(&self) -> Vec<(String, bool)> {
        let mut entries: Vec<_> = self
            .tracker_overrides
            .borrow()
            .iter()
            .map(|(host, allow)| (host.clone(), *allow))
            .collect();
        entries.sort();
        entries
    }

    pub(crate) fn authorize_context_menu_action(&self, tab: TabId, action: ContextMenuAction) {
        self.with_webview(tab, |webview| match action {
            ContextMenuAction::OpenLinkInNewWebView | ContextMenuAction::OpenImageInNewView => {
                self.grant_user_action(webview.id(), UserActionGrantKind::PopupOnly);
            }
            ContextMenuAction::Paste => {
                self.clipboard
                    .grant(webview.id(), crate::clipboard::ClipboardAccess::Read);
            }
            ContextMenuAction::Cut
            | ContextMenuAction::Copy
            | ContextMenuAction::CopyLink
            | ContextMenuAction::CopyImageLink => {
                self.clipboard
                    .grant(webview.id(), crate::clipboard::ClipboardAccess::Write);
            }
            _ => {}
        });
    }

    pub(crate) fn restore_page_focus(&self) {
        self.ui_ime_composing.set(false);
        self.set_page_focus(!self.active_reader_is_visible());
        self.window.request_redraw();
    }

    fn dismiss_transient_controls(&self) -> bool {
        let dismissed_menu = self.active_context_menu.borrow_mut().take().is_some();
        let dismissed_select = self.active_select.borrow_mut().take().is_some();
        if dismissed_select {
            self.active_select_value.borrow_mut().clear();
        }
        let dismissed_color = self.active_color_picker.borrow_mut().take().is_some();
        let dismissed = dismissed_menu || dismissed_select || dismissed_color;
        if dismissed {
            self.gui.borrow_mut().surrender_focus();
            self.restore_page_focus();
        }
        dismissed
    }

    fn dismiss_nonmodal_controls_for_tab(&self, tab: TabId) {
        if self
            .active_context_menu
            .borrow()
            .as_ref()
            .is_some_and(|pending| pending.tab == tab)
        {
            self.active_context_menu.borrow_mut().take();
        }
        if self
            .active_select
            .borrow()
            .as_ref()
            .is_some_and(|pending| pending.tab == tab)
        {
            self.active_select.borrow_mut().take();
            self.active_select_value.borrow_mut().clear();
        }
        if self
            .active_color_picker
            .borrow()
            .as_ref()
            .is_some_and(|pending| pending.tab == tab)
        {
            self.active_color_picker.borrow_mut().take();
        }
    }

    fn dismiss_controls_for_tab(&self, tab: TabId) {
        self.dismiss_nonmodal_controls_for_tab(tab);
        if self
            .active_dialog
            .borrow()
            .as_ref()
            .is_some_and(|pending| pending.tab == tab)
        {
            self.active_dialog.borrow_mut().take();
            self.dialog_focus_request.set(None);
        }
    }

    pub(crate) fn has_modal_dialog(&self) -> bool {
        self.active_dialog.borrow().is_some()
    }

    fn page_control_owns_focus(&self) -> bool {
        self.active_context_menu.borrow().is_some()
            || self.active_select.borrow().is_some()
            || self.active_color_picker.borrow().is_some()
            || self.active_dialog.borrow().is_some()
            || self.active_reader_is_visible()
    }

    pub(crate) fn resolve_dialog(&self, confirm: bool) -> bool {
        let Some(pending) = self.active_dialog.borrow_mut().take() else {
            return false;
        };
        if confirm {
            pending.control.confirm();
        } else {
            pending.control.dismiss();
        }
        self.restore_page_focus();
        self.dialog_focus_request.set(None);
        true
    }

    pub(crate) fn take_dialog_focus_request(&self, id: EmbedderControlId) -> bool {
        if self.dialog_focus_request.get() == Some(id) {
            self.dialog_focus_request.set(None);
            true
        } else {
            false
        }
    }

    fn forward_to_gui(&self, event: &WindowEvent) {
        let response = self.gui.borrow_mut().on_window_event(&self.window, event);
        if response.repaint {
            self.window.request_redraw();
        }
    }

    pub(crate) fn active_page_label(&self) -> String {
        self.active_tab
            .get()
            .and_then(|tab| self.core.borrow().tabs.get(tab).map(|tab| tab.url.clone()))
            .map(|url| url_identity(&url))
            .unwrap_or_else(|| "unknown page".to_owned())
    }

    pub(crate) fn reader_button_state(&self) -> ReaderButtonState {
        let Some(tab) = self.active_tab.get() else {
            return ReaderButtonState::Unavailable;
        };
        if let Some(state) = self.reader_states.borrow().get(&tab) {
            return match state {
                ReaderTabState::Extracting(_) => ReaderButtonState::Extracting,
                ReaderTabState::Ready { .. } => ReaderButtonState::Active,
                ReaderTabState::Error { request, .. } => ReaderButtonState::Error {
                    retryable: !self.reader_request_is_inflight(request),
                },
            };
        }
        let webview_id = self
            .webviews
            .borrow()
            .iter()
            .find(|tab_webview| tab_webview.tab == tab)
            .map(|tab_webview| tab_webview.webview.id());
        let inflight = self.reader_inflight.borrow();
        if webview_id.is_some_and(|id| inflight.contains_key(&id))
            || inflight.len() >= MAX_OUTSTANDING_READER_EVALUATIONS
        {
            return ReaderButtonState::Waiting;
        }
        let core = self.core.borrow();
        let Some(tab_state) = core.tabs.get(tab) else {
            return ReaderButtonState::Unavailable;
        };
        if tab_state.load_state == LoadState::Loaded && reader_url_is_eligible(&tab_state.url) {
            ReaderButtonState::Available
        } else {
            ReaderButtonState::Unavailable
        }
    }

    pub(crate) fn active_reader_view(&self) -> Option<ReaderView> {
        let tab = self.active_tab.get()?;
        match self.reader_states.borrow().get(&tab)? {
            ReaderTabState::Extracting(request) => Some(ReaderView::Extracting {
                source_url: request.source_url.clone(),
            }),
            ReaderTabState::Ready { request, article } => Some(ReaderView::Ready {
                source_url: request.source_url.clone(),
                article: article.clone(),
                generation: request.generation,
            }),
            ReaderTabState::Error { request, error } => Some(ReaderView::Error {
                source_url: request.source_url.clone(),
                message: error.message(),
                retryable: !self.reader_request_is_inflight(request),
            }),
        }
    }

    fn active_reader_ready_generation(&self) -> Option<u64> {
        let tab = self.active_tab.get()?;
        match self.reader_states.borrow().get(&tab)? {
            ReaderTabState::Ready { request, .. } => Some(request.generation),
            ReaderTabState::Extracting(_) | ReaderTabState::Error { .. } => None,
        }
    }

    pub(crate) fn reader_ready_generations(&self) -> Vec<u64> {
        self.reader_states
            .borrow()
            .values()
            .filter_map(|state| match state {
                ReaderTabState::Ready { request, .. } => Some(request.generation),
                ReaderTabState::Extracting(_) | ReaderTabState::Error { .. } => None,
            })
            .collect()
    }

    pub(crate) fn toggle_reader_mode(self: &Rc<Self>) {
        match self.reader_button_state() {
            ReaderButtonState::Unavailable | ReaderButtonState::Waiting => {}
            ReaderButtonState::Available => self.start_reader_mode(),
            ReaderButtonState::Error { retryable: true } => {
                if let Some(tab) = self.active_tab.get() {
                    self.leave_reader_for_tab(tab, false);
                }
                self.start_reader_mode();
            }
            ReaderButtonState::Error { retryable: false }
            | ReaderButtonState::Extracting
            | ReaderButtonState::Active => {
                self.close_reader_mode();
            }
        }
    }

    pub(crate) fn close_reader_mode(&self) -> bool {
        let Some(tab) = self.active_tab.get() else {
            return false;
        };
        self.leave_reader_for_tab(tab, true)
    }

    fn start_reader_mode(self: &Rc<Self>) {
        let Some(tab) = self.active_tab.get() else {
            return;
        };
        let Some(tab_state) = self.core.borrow().tabs.get(tab).cloned() else {
            return;
        };
        if tab_state.load_state != LoadState::Loaded || !reader_url_is_eligible(&tab_state.url) {
            return;
        }
        let webview_id = self
            .webviews
            .borrow()
            .iter()
            .find(|tab_webview| tab_webview.tab == tab)
            .and_then(|tab_webview| {
                (matches!(tab_webview.webview.load_status(), LoadStatus::Complete)
                    && tab_webview.webview.url().as_ref() == Some(&tab_state.url))
                .then_some(tab_webview.webview.id())
            });
        let Some(webview_id) = webview_id else {
            return;
        };
        let inflight = self.reader_inflight.borrow();
        if inflight.contains_key(&webview_id)
            || inflight.len() >= MAX_OUTSTANDING_READER_EVALUATIONS
        {
            return;
        }
        drop(inflight);

        let mut generation = self.next_reader_generation.get().wrapping_add(1);
        if generation == 0 {
            generation = 1;
        }
        self.next_reader_generation.set(generation);
        let request = ReaderRequest {
            generation,
            navigation_seq: tab_state.navigation_seq,
            source_url: tab_state.url,
            webview_id,
            deadline: Instant::now() + READER_EXTRACTION_TIMEOUT,
        };
        self.reader_inflight
            .borrow_mut()
            .insert(webview_id, generation);
        self.dismiss_nonmodal_controls_for_tab(tab);
        self.reader_states
            .borrow_mut()
            .insert(tab, ReaderTabState::Extracting(request.clone()));

        let weak_state = Rc::downgrade(self);
        let callback_request = request.clone();
        let mut started = false;
        self.with_webview(tab, |webview| {
            started = true;
            webview.evaluate_javascript(EXTRACTION_SCRIPT, move |result| {
                let result = result
                    .map_err(|_| ReaderError::EvaluationFailed)
                    .and_then(article_from_js);
                if let Some(state) = weak_state.upgrade() {
                    state.queue_ui_event(UiEvent::ReaderResult(tab, callback_request, result));
                }
            });
        });
        if !started {
            self.reader_states.borrow_mut().remove(&tab);
            self.reader_inflight.borrow_mut().remove(&webview_id);
            return;
        }

        self.cancel_page_composition(tab);
        self.set_page_focus(false);
        self.window.request_redraw();
    }

    fn leave_reader_for_tab(&self, tab: TabId, restore_focus: bool) -> bool {
        let removed = self.reader_states.borrow_mut().remove(&tab).is_some();
        if !removed {
            return false;
        }
        self.with_webview(tab, |webview| webview.set_throttled(false));
        if restore_focus && self.active_tab.get() == Some(tab) {
            self.set_page_focus(true);
            self.window.request_redraw();
        }
        true
    }

    fn reader_request_is_inflight(&self, request: &ReaderRequest) -> bool {
        self.reader_inflight.borrow().get(&request.webview_id) == Some(&request.generation)
    }

    /// Expire only the native request state. Servo has no evaluator
    /// cancellation API, so the in-flight tombstone remains until its
    /// exact callback arrives.
    fn expire_reader_extractions(&self, now: Instant) -> Option<Instant> {
        let (expired, next_deadline) = {
            let states = self.reader_states.borrow();
            let mut expired = Vec::new();
            let mut next_deadline: Option<Instant> = None;
            for (tab, state) in states.iter() {
                let ReaderTabState::Extracting(request) = state else {
                    continue;
                };
                if request.deadline <= now {
                    expired.push((*tab, request.clone()));
                } else {
                    next_deadline = Some(
                        next_deadline
                            .map_or(request.deadline, |current| current.min(request.deadline)),
                    );
                }
            }
            (expired, next_deadline)
        };

        for (tab, request) in expired {
            let still_current = self.reader_states.borrow().get(&tab).is_some_and(
                |state| matches!(state, ReaderTabState::Extracting(current) if current == &request),
            );
            if !still_current {
                continue;
            }
            self.with_webview(tab, |webview| webview.set_throttled(true));
            self.reader_states.borrow_mut().insert(
                tab,
                ReaderTabState::Error {
                    request,
                    error: ReaderError::TimedOut,
                },
            );
            if self.active_tab.get() == Some(tab) {
                self.set_page_focus(false);
                self.window.request_redraw();
            }
        }
        next_deadline
    }

    pub(crate) fn active_reader_is_visible(&self) -> bool {
        self.active_tab
            .get()
            .is_some_and(|tab| self.reader_states.borrow().contains_key(&tab))
    }
}

impl ApplicationHandler<AppEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        match AppState::create(
            event_loop,
            self.proxy.clone(),
            self.initial_url.clone(),
            self.initial_tracker_overrides.clone(),
            self.initial_proxy.clone(),
        ) {
            Ok(state) => self.state = Some(state),
            Err(error) => {
                log::error!("Failed to initialize the browser: {error}");
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: AppEvent) {
        let Some(state) = &self.state else {
            return;
        };
        match event {
            AppEvent::Wake => {
                state.servo.spin_event_loop();
                if state.needs_repaint.replace(false) {
                    state.window.request_redraw();
                }
            }
            AppEvent::Repaint { delay, pass } => {
                if pass < state.gui_repaint_pass.get() {
                    return;
                }
                if pass > state.gui_repaint_pass.replace(pass) {
                    state.gui_repaint_at.set(None);
                }
                if delay.is_zero() {
                    state.gui_repaint_at.set(None);
                    state.window.request_redraw();
                } else if let Some(deadline) = Instant::now().checked_add(delay) {
                    let earlier = state
                        .gui_repaint_at
                        .get()
                        .is_none_or(|existing| deadline < existing);
                    if earlier {
                        state.gui_repaint_at.set(Some(deadline));
                    }
                }
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = &self.state else {
            return;
        };
        if state.window.id() != window_id {
            return;
        }

        match &event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                state.forward_to_gui(&event);
                // Servo refuses zero-sized surfaces; skip the resize
                // while the window is minimized or iconified.
                if size.width > 0 && size.height > 0 {
                    state.window_rendering_context.resize(*size);
                }
                state.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                state.forward_to_gui(&event);
                let scale =
                    Scale::<_, DeviceIndependentPixel, DevicePixel>::new(*scale_factor as f32);
                for tab_webview in state.webviews.borrow().iter() {
                    tab_webview.webview.set_hidpi_scale_factor(scale);
                }
                state.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                state.gui_repaint_at.set(None);
                state.process_ui_events();
                state.gui.borrow_mut().update(&state.window, state);
                if let Err(error) = state.gui.borrow_mut().paint(&state.window) {
                    warn!("Could not paint browser UI: {error}");
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                state.last_cursor_position.set(Some(*position));
                // Always let egui track the pointer so that its widgets
                // see every move; egui ignores moves that miss them.
                state.forward_to_gui(&event);
                // Route by geometry (the last frame's popup/dialog/
                // settings rects) instead of egui's pointer memory,
                // which lags one event behind.
                let reader_visible = state.active_reader_is_visible();
                let over_egui = state.has_modal_dialog()
                    || state
                        .gui
                        .borrow()
                        .wants_pointer_at(*position, reader_visible);
                if state.page_pointer_capture.get().is_some()
                    || (state.egui_buttons_down.borrow().is_empty()
                        && state.point_in_webview(*position)
                        && !over_egui)
                {
                    state.forward_cursor_moved(*position);
                } else {
                    state.last_mouse_point.set(None);
                }
            }
            WindowEvent::MouseInput {
                state: button_state,
                button,
                ..
            } => {
                if *button_state == ElementState::Pressed
                    && state.page_pointer_capture.get().is_none()
                    && state.egui_buttons_down.borrow().is_empty()
                {
                    state.refresh_pointer_route(false);
                }
                let captured_release = *button_state == ElementState::Released
                    && state.page_buttons_down.borrow().contains(button);
                let egui_release = *button_state == ElementState::Released
                    && state.egui_buttons_down.borrow().contains(button);
                if captured_release {
                    state.forward_mouse_button(*button_state, *button);
                    if state.page_pointer_capture.get().is_none() {
                        state.refresh_pointer_route(true);
                    }
                } else if egui_release {
                    state.forward_to_gui(&event);
                    state.egui_buttons_down.borrow_mut().remove(button);
                    if state.egui_buttons_down.borrow().is_empty() {
                        state.refresh_pointer_route(true);
                    }
                } else if *button_state == ElementState::Released {
                    // A release without a matching in-window press can
                    // occur after focus changes. Do not synthesize an Up
                    // for either owner.
                    state.refresh_pointer_route(false);
                } else if state.has_modal_dialog() {
                    state.last_mouse_point.set(None);
                    if *button_state == ElementState::Pressed {
                        state.egui_buttons_down.borrow_mut().insert(*button);
                    }
                    state.forward_to_gui(&event);
                } else if state.last_mouse_point.get().is_some() {
                    if *button_state == ElementState::Pressed {
                        state.dismiss_transient_controls();
                        // A click on the page takes the keyboard focus
                        // away from the UI and hands it to Servo.
                        state.gui.borrow_mut().surrender_focus();
                        state.ui_ime_composing.set(false);
                        state.set_page_focus(true);
                        if matches!(
                            button,
                            winit::event::MouseButton::Left | winit::event::MouseButton::Middle
                        ) {
                            state.with_active_webview(|webview| {
                                state.grant_user_action(webview.id(), UserActionGrantKind::General)
                            });
                        }
                    }
                    state.forward_mouse_button(*button_state, *button);
                } else {
                    if *button_state == ElementState::Pressed {
                        let clicked_transient_control =
                            state.last_cursor_position.get().is_some_and(|position| {
                                state.gui.borrow().pointer_in_transient_control(position)
                            });
                        if !clicked_transient_control {
                            state.dismiss_transient_controls();
                        }
                        state.egui_buttons_down.borrow_mut().insert(*button);
                        if let Some(tab) = state.ime_composing_tab.get() {
                            state.cancel_page_composition(tab);
                        }
                        state.set_page_focus(false);
                    }
                    state.forward_to_gui(&event);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                state.refresh_pointer_route(false);
                if state.last_mouse_point.get().is_some() && !state.has_modal_dialog() {
                    state.forward_mouse_wheel(delta);
                } else {
                    state.forward_to_gui(&event);
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                state.modifiers.set(modifiers.state());
                state.forward_to_gui(&event);
            }
            WindowEvent::KeyboardInput {
                event: key_event,
                is_synthetic,
                ..
            } => {
                if *is_synthetic {
                    if key_event.state == ElementState::Released {
                        state.track_modifier_key(key_event);
                        state
                            .consumed_shortcuts
                            .borrow_mut()
                            .remove(&key_event.physical_key);
                    }
                    return;
                }
                state.track_modifier_key(key_event);
                if state.has_modal_dialog() {
                    if state.handle_browser_shortcut(key_event, true) {
                        return;
                    }
                    // Let egui apply all queued text/IME input before
                    // `dialog_ui` resolves Enter or Escape in the frame.
                    state.forward_to_gui(&event);
                    return;
                }
                if state.handle_browser_shortcut(key_event, false) {
                    return;
                }
                // Escape leaves the address bar (or any egui widget)
                // and hands the keyboard back to the page; it also
                // dismisses an open context menu.
                if key_event.state == ElementState::Pressed
                    && key_event.physical_key == PhysicalKey::Code(KeyCode::Escape)
                    && !state.ui_ime_composing.get()
                {
                    if state.dismiss_transient_controls() {
                        return;
                    }
                    if state.gui.borrow().url_bar_has_focus() {
                        let canonical = state.active_tab_url();
                        state.gui.borrow_mut().cancel_url_edit(canonical);
                        state.set_page_focus(!state.active_reader_is_visible());
                        state.window.request_redraw();
                        return;
                    }
                    if state.gui.borrow().has_keyboard_focus() {
                        state.gui.borrow_mut().surrender_focus();
                        state.set_page_focus(!state.active_reader_is_visible());
                        state.window.request_redraw();
                        return;
                    }
                    if state.close_reader_mode() {
                        return;
                    }
                }
                if key_event.state == ElementState::Pressed
                    && !state.gui.borrow().has_keyboard_focus()
                {
                    if let (Some(generation), Some(command)) = (
                        state.active_reader_ready_generation(),
                        reader_scroll_command(key_event),
                    ) {
                        state
                            .gui
                            .borrow_mut()
                            .scroll_reader_with_key(generation, command);
                        state.window.request_redraw();
                        return;
                    }
                }
                if state.page_focus.get() && !state.gui.borrow().has_keyboard_focus() {
                    state.forward_keyboard(key_event);
                } else {
                    state.forward_to_gui(&event);
                    state.set_page_focus(false);
                }
            }
            WindowEvent::Ime(ime_event) => {
                // The system IME is window-global. Route it to exactly
                // one owner, while allowing a started page composition
                // to receive its final Commit/Disabled event.
                let to_page = state.ime_composing_tab.get().is_some()
                    || (state.page_focus.get()
                        && state.active_page_has_ime_target()
                        && !state.gui.borrow().has_keyboard_focus());
                if to_page {
                    state.forward_page_ime(ime_event);
                } else {
                    state.ui_ime_composing.set(crate::ime::is_composing_after(
                        state.ui_ime_composing.get(),
                        ime_event,
                    ));
                    state.forward_to_gui(&event);
                }
            }
            WindowEvent::Focused(false) => {
                state
                    .restore_page_focus_on_window_focus
                    .set(state.page_focus.get());
                if let Some(tab) = state.ime_composing_tab.get() {
                    state.cancel_page_composition(tab);
                }
                state.release_page_pointer_capture();
                state.set_page_focus(false);
                state.ui_ime_composing.set(false);
                state.modifiers.set(ModifiersState::empty());
                state.alt_graph.set(false);
                state.consumed_shortcuts.borrow_mut().clear();
                state.user_action_grants.borrow_mut().clear();
                state.clipboard.revoke_all();
                state.egui_buttons_down.borrow_mut().clear();
                state.last_cursor_position.set(None);
                state.last_mouse_point.set(None);
                state.forward_to_gui(&event);
            }
            WindowEvent::Focused(true) => {
                state.forward_to_gui(&event);
                if state.restore_page_focus_on_window_focus.replace(false)
                    && !state.gui.borrow().has_keyboard_focus()
                    && !state.page_control_owns_focus()
                {
                    state.set_page_focus(true);
                }
            }
            _ => {
                state.forward_to_gui(&event);
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let mut control_flow = ControlFlow::Wait;
        if let Some(state) = &self.state {
            state.servo.spin_event_loop();
            let now = Instant::now();
            let reader_deadline = state.expire_reader_extractions(now);
            if state.needs_repaint.replace(false) {
                state.window.request_redraw();
            }
            let gui_deadline = match state.gui_repaint_at.get() {
                Some(deadline) if deadline <= now => {
                    state.gui_repaint_at.set(None);
                    state.window.request_redraw();
                    None
                }
                deadline => deadline,
            };
            let active_is_animating = !state.active_reader_is_visible()
                && state
                    .active_tab
                    .get()
                    .is_some_and(|tab| state.animating_tabs.borrow().contains(&tab));
            if active_is_animating {
                control_flow = ControlFlow::Poll;
            } else if let Some(deadline) = match (gui_deadline, reader_deadline) {
                (Some(gui), Some(reader)) => Some(gui.min(reader)),
                (gui, reader) => gui.or(reader),
            } {
                control_flow = ControlFlow::WaitUntil(deadline);
            }
        }
        event_loop.set_control_flow(control_flow);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_hosts_are_normalized_and_urls_are_rejected() {
        assert_eq!(
            normalize_site_host(" Example.COM. ").unwrap(),
            "example.com"
        );
        assert_eq!(normalize_site_host("127.0.0.1").unwrap(), "127.0.0.1");
        assert!(normalize_site_host("").is_err());
        assert!(normalize_site_host("https://example.com/path").is_err());
        assert!(normalize_site_host("example.com:8080").is_err());
    }

    #[test]
    fn tracker_override_is_scoped_to_the_top_level_site() {
        let overrides = HashMap::from([
            ("site.example".to_owned(), true),
            ("blocked.example".to_owned(), false),
        ]);
        assert!(trackers_allowed_for_site(&overrides, Some("site.example")));
        assert!(!trackers_allowed_for_site(
            &overrides,
            Some("tracker.example")
        ));
        assert!(!trackers_allowed_for_site(&overrides, None));
    }

    #[test]
    fn reader_results_are_bound_to_the_exact_navigation() {
        let source = Url::parse("https://example.com/article").unwrap();
        let other = Url::parse("https://example.com/other").unwrap();
        assert!(reader_document_matches(7, &source, 7, &source));
        assert!(!reader_document_matches(7, &source, 8, &source));
        assert!(!reader_document_matches(7, &source, 7, &other));
    }

    #[test]
    fn only_the_trusted_webview_url_selects_a_site_override() {
        let page = Url::parse("https://Allowed.Example./old").unwrap();
        assert_eq!(
            site_host_for_request(Some(&page)).as_deref(),
            Some("allowed.example")
        );
        assert_eq!(site_host_for_request(None), None);
    }

    #[test]
    fn alt_graph_is_preserved_for_page_keyboard_events() {
        let modifiers = modifiers_from_winit(ModifiersState::CONTROL | ModifiersState::ALT, true);
        assert!(modifiers.contains(keyboard_types::Modifiers::CONTROL));
        assert!(modifiers.contains(keyboard_types::Modifiers::ALT));
        assert!(modifiers.contains(keyboard_types::Modifiers::ALT_GRAPH));
        assert!(!browser_shortcuts_enabled(ModifiersState::CONTROL, true));
        assert!(browser_shortcuts_enabled(ModifiersState::CONTROL, false));
    }

    #[test]
    fn wheel_units_are_preserved() {
        let line = wheel_delta_from_winit(&MouseScrollDelta::LineDelta(2.0, -3.0));
        assert_eq!(line.mode, WheelMode::DeltaLine);
        assert_eq!((line.x, line.y), (2.0, -3.0));

        let pixel = wheel_delta_from_winit(&MouseScrollDelta::PixelDelta(
            winit::dpi::PhysicalPosition::new(4.5, -8.0),
        ));
        assert_eq!(pixel.mode, WheelMode::DeltaPixel);
        assert_eq!((pixel.x, pixel.y), (4.5, -8.0));
    }

    #[test]
    fn browser_options_keep_pipeline_crashes_recoverable() {
        let options = browser_servo_options();
        assert!(!options.hard_fail);
        assert!(options.temporary_storage);
    }

    #[test]
    fn user_action_grants_expire_at_the_boundary() {
        let now = Instant::now();
        let grant = UserActionGrant {
            expires_at: now + USER_GESTURE_GRANT_LIFETIME,
            kind: UserActionGrantKind::General,
            epoch: 7,
        };
        assert!(user_action_grant_is_valid(grant, grant.expires_at));
        assert!(!user_action_grant_is_valid(
            grant,
            grant.expires_at + Duration::from_nanos(1)
        ));
    }

    #[test]
    fn reserved_file_picker_grant_is_bound_to_focus_epoch() {
        let now = Instant::now();
        let grant = UserActionGrant {
            expires_at: now + USER_GESTURE_GRANT_LIFETIME,
            kind: UserActionGrantKind::General,
            epoch: 7,
        };
        assert!(reserved_user_action_grant_is_valid(
            grant,
            now,
            7,
            UserActionCapability::FilePicker
        ));
        assert!(!reserved_user_action_grant_is_valid(
            grant,
            now,
            8,
            UserActionCapability::FilePicker
        ));
        assert!(!reserved_user_action_grant_is_valid(
            grant,
            grant.expires_at + Duration::from_nanos(1),
            7,
            UserActionCapability::FilePicker
        ));
    }

    #[test]
    fn user_action_grants_queue_without_overwriting() {
        let now = Instant::now();
        let mut queue = VecDeque::from([
            UserActionGrant {
                expires_at: now + USER_GESTURE_GRANT_LIFETIME,
                kind: UserActionGrantKind::General,
                epoch: 7,
            },
            UserActionGrant {
                expires_at: now + USER_GESTURE_GRANT_LIFETIME,
                kind: UserActionGrantKind::General,
                epoch: 7,
            },
        ]);
        assert!(consume_user_action_grant_queue(
            &mut queue,
            now,
            UserActionCapability::Popup
        ));
        assert!(consume_user_action_grant_queue(
            &mut queue,
            now,
            UserActionCapability::FilePicker
        ));
        assert!(!consume_user_action_grant_queue(
            &mut queue,
            now,
            UserActionCapability::Popup
        ));
    }

    #[test]
    fn popup_only_grant_cannot_authorize_a_file_picker() {
        let now = Instant::now();
        let mut queue = VecDeque::from([UserActionGrant {
            expires_at: now + USER_GESTURE_GRANT_LIFETIME,
            kind: UserActionGrantKind::PopupOnly,
            epoch: 7,
        }]);
        assert!(!consume_user_action_grant_queue(
            &mut queue,
            now,
            UserActionCapability::FilePicker
        ));
        assert!(consume_user_action_grant_queue(
            &mut queue,
            now,
            UserActionCapability::Popup
        ));
    }

    #[test]
    fn file_filter_extensions_are_bounded_normalized_and_injection_safe() {
        let extensions = normalized_file_filter_extensions([
            " PNG ",
            "png",
            "tar.gz",
            "*.exe",
            "txt;*.exe",
            "../txt",
            ".hidden",
            "",
        ]);
        assert_eq!(extensions, ["png", "tar.gz"]);

        let many: Vec<_> = (0..MAX_FILE_FILTERS + 5)
            .map(|index| format!("ext{index}"))
            .collect();
        let bounded = normalized_file_filter_extensions(many.iter().map(String::as_str));
        assert_eq!(bounded.len(), MAX_FILE_FILTERS);
    }

    #[test]
    fn log_identity_omits_paths_queries_and_fragments() {
        let url = Url::parse("https://example.com:8443/private?token=secret#value").unwrap();
        assert_eq!(url_identity(&url), "https://example.com:8443");
        assert_eq!(url_identity(&Url::parse("about:blank").unwrap()), "about:");
    }

    #[test]
    fn unknown_or_secure_global_http_fails_closed() {
        let http = Url::parse("http://public.example/resource").unwrap();
        let https = Url::parse("https://public.example/resource").unwrap();
        let secure_page = Url::parse("https://site.example/").unwrap();
        let insecure_page = Url::parse("http://site.example/").unwrap();
        let opaque_page = Url::parse("about:blank").unwrap();
        assert!(global_http_context_is_unsafe(&http, None));
        assert!(global_http_context_is_unsafe(&http, Some(&secure_page)));
        assert!(global_http_context_is_unsafe(&http, Some(&opaque_page)));
        assert!(!global_http_context_is_unsafe(&http, Some(&insecure_page)));
        assert!(!global_http_context_is_unsafe(&https, None));
    }

    #[test]
    fn explicit_proxy_rejects_http_default_port_before_servo_misroutes_it() {
        let default_http = Url::parse("http://example.com:80/path").unwrap();
        let alternate_http = Url::parse("http://example.com:8080/path").unwrap();
        let https = Url::parse("https://example.com/path").unwrap();
        assert!(proxy_default_http_port_is_unsupported(true, &default_http));
        assert!(!proxy_default_http_port_is_unsupported(
            true,
            &alternate_http
        ));
        assert!(!proxy_default_http_port_is_unsupported(true, &https));
        assert!(!proxy_default_http_port_is_unsupported(
            false,
            &default_http
        ));
    }

    #[test]
    fn servo_preferences_use_only_the_explicit_proxy_configuration() {
        let direct = browser_servo_preferences(None);
        assert!(direct.network_http_proxy_uri.is_empty());
        assert!(direct.network_https_proxy_uri.is_empty());
        assert!(direct.network_http_no_proxy.is_empty());

        let proxy =
            StartupProxy::parse("http://proxy.test:8765", Some("localhost,127.0.0.1")).unwrap();
        let configured = browser_servo_preferences(Some(&proxy));
        assert_eq!(configured.network_http_proxy_uri, proxy.uri());
        assert_eq!(configured.network_https_proxy_uri, proxy.uri());
        assert_eq!(configured.network_http_no_proxy, proxy.bypass());
    }

    #[test]
    fn untrusted_ui_text_is_bounded_and_single_line() {
        assert_eq!(text_for_ui("hello\nworld", 20), "hello world");
        assert_eq!(text_for_ui("abcdef", 3), "abc…");
        assert_eq!(text_for_ui("äöü", 3), "äöü");
    }
}
