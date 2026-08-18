//! The application state: window, Servo instance, WebView and the
//! glue between them.
//!
//! `AppState` is shared as `Rc<AppState>`: it is the `WebViewDelegate`
//! (Servo calls back on background threads), so every mutable piece of
//! state lives behind `RefCell`/`Cell`. Delegate callbacks only queue
//! events; the main thread drains them on redraw.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::str::FromStr;

use euclid::Scale;
use log::{debug, info, warn};
use servo::{
    Cursor, DeviceIndependentPixel, DevicePixel, DevicePoint, InputEvent, LoadStatus, MouseButton,
    MouseButtonAction, MouseButtonEvent, MouseMoveEvent, OffscreenRenderingContext,
    RenderingContext, Servo, ServoBuilder, WebResourceLoad, WebResourceResponse, WebView,
    WebViewBuilder, WebViewDelegate, WheelDelta, WheelEvent, WheelMode, WindowRenderingContext,
};
use url::Url;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::raw_window_handle::{HasDisplayHandle as _, HasWindowHandle as _};
use winit::window::{CursorIcon, Window, WindowId};

use browser_core::{BrowserCore, NavigationCommand, TabId};
use browser_network::{default_pipeline, RequestContext, RequestPipeline, ResourceType};
use browser_privacy::trackers::TrackerEngine;

use crate::gui::Gui;
use crate::waker::EventLoopWaker;

/// Events winit delivers to our application.
pub enum AppEvent {
    /// Servo woke the event loop up; drain its message queues.
    Wake,
}

/// The root of all browser state. See the module docs for the sharing
/// model.
pub struct AppState {
    pub(crate) window: Rc<Window>,
    servo: Servo,
    window_rendering_context: Rc<WindowRenderingContext>,
    pub(crate) rendering_context: Rc<OffscreenRenderingContext>,
    pub(crate) webviews: RefCell<Vec<WebView>>,
    gui: RefCell<Gui>,
    core: RefCell<BrowserCore>,
    pipeline: RequestPipeline,
    active_tab: Cell<Option<TabId>>,
    pub(crate) can_go_back: Cell<bool>,
    pub(crate) can_go_forward: Cell<bool>,
    load_status: Cell<LoadStatus>,
    pub(crate) needs_repaint: Cell<bool>,
    last_mouse_point: Cell<Option<DevicePoint>>,
    /// True while the WebView owns the keyboard focus; false while an
    /// egui widget (e.g. the address bar) does.
    page_focus: Cell<bool>,
    /// The currently pressed modifier keys, tracked via
    /// `WindowEvent::ModifiersChanged`.
    modifiers: Cell<winit::keyboard::ModifiersState>,
    ui_events: RefCell<Vec<UiEvent>>,
}

/// Delegate notifications queued for the main thread.
enum UiEvent {
    Url(Url),
    PageTitle(Option<String>),
    LoadStatus(LoadStatus),
}

/// The winit application.
pub struct App {
    proxy: EventLoopProxy<AppEvent>,
    initial_url: Option<String>,
    state: Option<Rc<AppState>>,
}

impl App {
    pub fn new(event_loop: &EventLoop<AppEvent>, initial_url: Option<String>) -> Self {
        Self {
            proxy: event_loop.create_proxy(),
            initial_url,
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
    ) -> Result<Rc<Self>, String> {
        let waker: Box<dyn servo::EventLoopWaker> = Box::new(EventLoopWaker::new(proxy));

        let servo = ServoBuilder::default().event_loop_waker(waker).build();
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

        let state = Rc::new(Self {
            window: window.clone(),
            servo,
            window_rendering_context,
            rendering_context: rendering_context.clone(),
            webviews: RefCell::new(Vec::new()),
            gui: RefCell::new(Gui::new(event_loop, &window, &rendering_context)),
            core: RefCell::new(BrowserCore::new()),
            pipeline: default_pipeline(TrackerEngine::builtin()),
            active_tab: Cell::new(None),
            can_go_back: Cell::new(false),
            can_go_forward: Cell::new(false),
            load_status: Cell::new(LoadStatus::Complete),
            needs_repaint: Cell::new(false),
            last_mouse_point: Cell::new(None),
            page_focus: Cell::new(false),
            modifiers: Cell::new(winit::keyboard::ModifiersState::empty()),
            ui_events: RefCell::new(Vec::new()),
        });

        let initial_url = initial_url
            .map(|input| Url::parse(&input))
            .transpose()
            .map_err(|error| format!("Could not parse initial URL: {error}"))?
            .unwrap_or_else(|| Url::parse("about:blank").expect("static URL"));

        let webview = WebViewBuilder::new(&state.servo, state.rendering_context.clone())
            .url(initial_url)
            .hidpi_scale_factor(Scale::new(window.scale_factor() as f32))
            .delegate(state.clone())
            .clipboard_delegate(Rc::new(crate::clipboard::SystemClipboard::new()))
            .build();
        state.webviews.borrow_mut().push(webview);

        state
            .active_tab
            .set(Some(state.core.borrow_mut().start_session()));

        window.request_redraw();
        Ok(state)
    }

    /// Navigate according to address-bar input.
    pub fn navigate(&self, input: &str) {
        match self.core.borrow().command_from_input(input, false) {
            Ok(NavigationCommand::Load(url)) => self.load_url(url),
            Ok(NavigationCommand::Reload) => self.navigate_reload(),
            Ok(NavigationCommand::Back) => self.navigate_back(),
            Ok(NavigationCommand::Forward) => self.navigate_forward(),
            Ok(NavigationCommand::NewTab(_)) => warn!("new tabs are a Phase 5 feature"),
            Err(error) => warn!("navigation rejected: {error}"),
        }
    }

    fn load_url(&self, url: Url) {
        info!("Loading {url}");
        if let Some(tab) = self.active_tab.get() {
            let _ = self.core.borrow_mut().load_started(tab);
        }
        if let Some(webview) = self.webviews.borrow().first() {
            webview.load(url);
        }
    }

    pub fn navigate_back(&self) {
        if self.can_go_back.get() {
            if let Some(webview) = self.webviews.borrow().first() {
                webview.go_back(1);
            }
        }
    }

    pub fn navigate_forward(&self) {
        if self.can_go_forward.get() {
            if let Some(webview) = self.webviews.borrow().first() {
                webview.go_forward(1);
            }
        }
    }

    pub fn navigate_reload(&self) {
        if let Some(webview) = self.webviews.borrow().first() {
            webview.reload();
        }
    }

    /// Have Servo paint all WebViews into their rendering context.
    pub(crate) fn repaint_webviews(&self) {
        self.window_rendering_context
            .make_current()
            .expect("Could not make window RenderingContext current");
        for webview in self.webviews.borrow().iter() {
            webview.paint();
        }
        self.window_rendering_context.present();
    }

    /// Apply queued delegate notifications to the core model and GUI.
    fn process_ui_events(&self) {
        let events = std::mem::take(&mut *self.ui_events.borrow_mut());
        for event in events {
            match event {
                UiEvent::Url(url) => {
                    if let Some(tab) = self.active_tab.get() {
                        let _ = self.core.borrow_mut().location_changed(tab, url.clone());
                    }
                    let mut gui = self.gui.borrow_mut();
                    if !gui.url_dirty {
                        gui.url = url.to_string();
                    }
                }
                UiEvent::PageTitle(title) => {
                    if let Some(title) = title {
                        self.window.set_title(&title);
                    }
                }
                UiEvent::LoadStatus(status) => {
                    self.load_status.set(status);
                    if let Some(tab) = self.active_tab.get() {
                        let mut core = self.core.borrow_mut();
                        match status {
                            LoadStatus::Complete => {
                                let _ = core.load_finished(tab);
                            }
                            _ => {
                                let _ = core.load_started(tab);
                            }
                        }
                    }
                }
            }
        }
    }

    fn point_in_webview(&self, position: PhysicalPosition<f64>) -> bool {
        let gui = self.gui.borrow();
        let scale = gui.egui_ctx.pixels_per_point();
        let origin = self.window.inner_position().unwrap_or_default();
        let point = egui::pos2(
            (position.x - origin.x as f64) as f32 / scale,
            (position.y - origin.y as f64) as f32 / scale,
        );
        gui.webview_rect.contains(point)
    }

    /// The cursor position relative to the top-left corner of the
    /// WebView, in physical pixels. Servo expects viewport-relative
    /// coordinates, not window or screen coordinates.
    fn webview_relative_point(&self, position: PhysicalPosition<f64>) -> DevicePoint {
        let gui = self.gui.borrow();
        let scale = gui.egui_ctx.pixels_per_point();
        let origin = self.window.inner_position().unwrap_or_default();
        DevicePoint::new(
            (position.x - origin.x as f64 - (gui.webview_rect.min.x as f64) * (scale as f64))
                as f32,
            (position.y - origin.y as f64 - (gui.webview_rect.min.y as f64) * (scale as f64))
                as f32,
        )
    }

    fn forward_cursor_moved(&self, position: PhysicalPosition<f64>) {
        let point = self.webview_relative_point(position);
        self.last_mouse_point.set(Some(point));
        if let Some(webview) = self.webviews.borrow().first() {
            webview.notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(point.into())));
        }
    }

    fn forward_mouse_button(&self, state: ElementState, button: winit::event::MouseButton) {
        let Some(point) = self.last_mouse_point.get() else {
            return;
        };
        let action = match state {
            ElementState::Pressed => MouseButtonAction::Down,
            ElementState::Released => MouseButtonAction::Up,
        };
        let button = match button {
            winit::event::MouseButton::Left => MouseButton::Left,
            winit::event::MouseButton::Middle => MouseButton::Middle,
            winit::event::MouseButton::Right => MouseButton::Right,
            winit::event::MouseButton::Back => MouseButton::Back,
            winit::event::MouseButton::Forward => MouseButton::Forward,
            winit::event::MouseButton::Other(id) => MouseButton::Other(id),
        };
        if let Some(webview) = self.webviews.borrow().first() {
            webview.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                action,
                button,
                point.into(),
            )));
        }
    }

    fn forward_mouse_wheel(&self, delta: &MouseScrollDelta) {
        let Some(point) = self.last_mouse_point.get() else {
            return;
        };
        let (x, y) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (*x as f64, *y as f64),
            MouseScrollDelta::PixelDelta(position) => (position.x, position.y),
        };
        let event = InputEvent::Wheel(WheelEvent::new(
            WheelDelta {
                x,
                y,
                z: 0.0,
                mode: WheelMode::DeltaPixel,
            },
            point.into(),
        ));
        if let Some(webview) = self.webviews.borrow().first() {
            webview.notify_input_event(event);
        }
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
            Key::Named(NamedKey::Alt) | Key::Named(NamedKey::AltGraph) => {
                if pressed {
                    modifiers.insert(ModifiersState::ALT);
                } else {
                    modifiers.remove(ModifiersState::ALT);
                }
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
            modifiers: modifiers_from_winit(self.modifiers.get()),
            repeat: key_event.repeat,
            is_composing: false,
        };
        if let Some(webview) = self.webviews.borrow().first() {
            webview.notify_input_event(InputEvent::Keyboard(
                servo::input_events::KeyboardEvent::new(event),
            ));
        }
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
fn modifiers_from_winit(modifiers: winit::keyboard::ModifiersState) -> keyboard_types::Modifiers {
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
    result
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
        debug!("new frame ready");
        self.window.request_redraw();
    }

    fn notify_url_changed(&self, _webview: WebView, url: Url) {
        info!("URL changed: {url}");
        self.ui_events.borrow_mut().push(UiEvent::Url(url));
    }

    fn notify_page_title_changed(&self, _webview: WebView, title: Option<String>) {
        self.ui_events.borrow_mut().push(UiEvent::PageTitle(title));
    }

    fn notify_load_status_changed(&self, _webview: WebView, status: LoadStatus) {
        info!("Load status: {status:?}");
        self.ui_events
            .borrow_mut()
            .push(UiEvent::LoadStatus(status));
    }

    fn notify_history_changed(&self, _webview: WebView, entries: Vec<Url>, current: usize) {
        self.can_go_back.set(current > 0);
        self.can_go_forward.set(current + 1 < entries.len());
    }

    fn notify_cursor_changed(&self, _webview: WebView, cursor: Cursor) {
        self.window
            .set_cursor(winit::window::Cursor::Icon(cursor_icon_for(cursor)));
    }

    /// Every subresource passes through the privacy pipeline here. A
    /// blocked request is answered with an empty 200 response so the
    /// page keeps rendering normally.
    fn load_web_resource(&self, _webview: WebView, load: WebResourceLoad) {
        let request = load.request();
        let url = request.url.clone();
        let initiator = request.referrer_url.clone();
        let resource_type = ResourceType::from_engine_string(request.destination.as_str());
        let context = RequestContext::new(
            url.clone(),
            initiator,
            resource_type,
            request.is_for_main_frame,
        );

        match self.pipeline.evaluate(&context) {
            browser_network::PipelineDecision::Allow => {}
            browser_network::PipelineDecision::Block { layer, reason } => {
                info!("Blocked {url} ({layer}: {reason})");
                load.intercept(WebResourceResponse::new(url)).finish();
            }
        }
    }
}

impl ApplicationHandler<AppEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        match AppState::create(event_loop, self.proxy.clone(), self.initial_url.clone()) {
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
                let _ = state
                    .gui
                    .borrow_mut()
                    .on_window_event(&state.window, &event);
                // Servo refuses zero-sized surfaces; skip the resize
                // while the window is minimized or iconified.
                if size.width > 0 && size.height > 0 {
                    state.window_rendering_context.resize(*size);
                    state.rendering_context.resize(*size);
                    for webview in state.webviews.borrow().iter() {
                        webview.resize(*size);
                    }
                }
                state.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let _ = state
                    .gui
                    .borrow_mut()
                    .on_window_event(&state.window, &event);
                let scale =
                    Scale::<_, DeviceIndependentPixel, DevicePixel>::new(*scale_factor as f32);
                for webview in state.webviews.borrow().iter() {
                    webview.set_hidpi_scale_factor(scale);
                }
                state.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                state.process_ui_events();
                state.gui.borrow_mut().update(&state.window, state);
                state.gui.borrow_mut().paint(&state.window);
            }
            WindowEvent::CursorMoved { position, .. } => {
                if state.point_in_webview(*position) {
                    state.forward_cursor_moved(*position);
                } else {
                    state.last_mouse_point.set(None);
                    let _ = state
                        .gui
                        .borrow_mut()
                        .on_window_event(&state.window, &event);
                }
            }
            WindowEvent::MouseInput {
                state: button_state,
                button,
                ..
            } => {
                if state.last_mouse_point.get().is_some() {
                    if *button_state == ElementState::Pressed {
                        // A click on the page takes the keyboard focus
                        // away from the UI and hands it to Servo.
                        state.gui.borrow_mut().surrender_focus();
                        state.page_focus.set(true);
                    }
                    state.forward_mouse_button(*button_state, *button);
                } else {
                    let _ = state
                        .gui
                        .borrow_mut()
                        .on_window_event(&state.window, &event);
                }
            }
            WindowEvent::MouseWheel { delta, .. } if state.last_mouse_point.get().is_some() => {
                state.forward_mouse_wheel(delta);
            }
            WindowEvent::MouseWheel { .. } => {
                let _ = state
                    .gui
                    .borrow_mut()
                    .on_window_event(&state.window, &event);
            }
            WindowEvent::ModifiersChanged(_) => {
                let _ = state
                    .gui
                    .borrow_mut()
                    .on_window_event(&state.window, &event);
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                state.track_modifier_key(key_event);
                if state.page_focus.get() && !state.gui.borrow().has_keyboard_focus() {
                    state.forward_keyboard(key_event);
                } else {
                    let _ = state
                        .gui
                        .borrow_mut()
                        .on_window_event(&state.window, &event);
                    state.page_focus.set(false);
                }
            }
            _ => {
                let _ = state
                    .gui
                    .borrow_mut()
                    .on_window_event(&state.window, &event);
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(state) = &self.state {
            state.servo.spin_event_loop();
            if state.needs_repaint.replace(false) {
                state.window.request_redraw();
            }
        }
        event_loop.set_control_flow(ControlFlow::Wait);
    }
}
