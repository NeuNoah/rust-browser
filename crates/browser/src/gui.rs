//! The egui-based user interface: toolbar, URL bar and the WebView
//! blit into the egui scene.
//!
//! Layout follows servoshell: a `TopBottomPanel` holds the toolbar;
//! the remaining area is the WebView viewport. Servo paints into an
//! offscreen framebuffer which is copied into the egui scene via a
//! `PaintCallback` on the background layer.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use euclid::{Point2D, Rect, Size2D};
use log::info;
use servo::{
    ContextMenuAction, ContextMenuItem, OffscreenRenderingContext, RenderingContext, RgbColor,
    SelectElementOptionOrOptgroup, SimpleDialog,
};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::window::Window;

use browser_core::SearchEngine;

use crate::app::{AppEvent, AppState, ReaderButtonState, ReaderView};
use crate::reader::{ReaderBlockKind, ReaderDirection};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReaderScrollCommand {
    Home,
    End,
    PageDown,
    PageUp,
    ArrowDown,
    ArrowUp,
}

/// Stable egui id of the address bar, so keyboard shortcuts (Ctrl+L)
/// can focus it.
fn url_bar_id() -> egui::Id {
    egui::Id::new("url_bar")
}

fn url_bar_selection_request_id() -> egui::Id {
    egui::Id::new("url_bar_select_all_request")
}

fn request_url_bar_focus(context: &egui::Context) {
    context.memory_mut(|memory| memory.request_focus(url_bar_id()));
    context.data_mut(|data| data.insert_temp(url_bar_selection_request_id(), true));
    context.request_repaint();
}

fn surrender_egui_focus(context: &egui::Context) {
    context.memory_mut(|memory| {
        if let Some(id) = memory.focused() {
            memory.surrender_focus(id);
        }
        memory.stop_text_input();
    });
}

/// Extend egui's compact built-in Latin font with locally installed
/// Windows fallbacks. Missing files are harmless; no font is fetched or
/// copied, and the default font remains first for existing chrome text.
fn install_system_font_fallbacks(context: &egui::Context) {
    let Some(font_directory) = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .map(|path| path.join("Fonts"))
    else {
        return;
    };
    let candidates = [
        ("reader-arabic", "arial.ttf"),
        ("reader-indic", "Nirmala.ttc"),
        ("reader-japanese", "msgothic.ttc"),
        ("reader-korean", "malgun.ttf"),
    ];
    let mut definitions = egui::FontDefinitions::default();
    let mut loaded = 0;
    for (name, file_name) in candidates {
        let Ok(bytes) = std::fs::read(font_directory.join(file_name)) else {
            continue;
        };
        definitions
            .font_data
            .insert(name.to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            definitions
                .families
                .entry(family)
                .or_default()
                .push(name.to_owned());
        }
        loaded += 1;
    }
    if loaded > 0 {
        context.set_fonts(definitions);
    }
}

#[derive(Default)]
struct SettingsDraft {
    initialized: bool,
    start_page: String,
    search_engine: SearchEngine,
    tracker_host: String,
    message: Option<(bool, String)>,
}

/// All egui state plus the fields the toolbar edits.
pub struct Gui {
    rendering_context: Rc<OffscreenRenderingContext>,
    pub(crate) egui_ctx: egui::Context,
    egui_glow: egui_glow::winit::EguiGlow,
    /// The current URL shown in the address bar.
    pub url: String,
    /// True while the user is editing the address bar; prevents the
    /// engine from overwriting their input.
    pub url_dirty: bool,
    navigation_error: Option<String>,
    /// Tab whose URL is currently represented by `url`.
    shown_tab: Option<browser_core::TabId>,
    /// The WebView viewport in egui points.
    pub webview_rect: egui::Rect,
    /// egui-owned regions (popups, dialogs, the settings window) from
    /// the last frame; used to route pointer events by geometry.
    egui_regions: Vec<egui::Rect>,
    /// The Reader rect is tracked separately so an immediate tab switch
    /// cannot leak one event through before redraw, and its previous rect
    /// cannot steal an event after Reader closes.
    reader_region: Option<egui::Rect>,
    /// Native keyboard-scroll positions keyed by Reader generation.
    reader_scroll_offsets: HashMap<u64, f32>,
    /// Context/select/color controls specifically; clicks elsewhere in
    /// browser chrome dismiss them without confusing settings/dialogs.
    transient_regions: Vec<egui::Rect>,
    settings_draft: SettingsDraft,
}

impl Gui {
    pub fn new(
        event_loop: &ActiveEventLoop,
        window: &Window,
        rendering_context: &Rc<OffscreenRenderingContext>,
        proxy: EventLoopProxy<AppEvent>,
    ) -> Result<Self, String> {
        rendering_context
            .make_current()
            .map_err(|error| format!("Could not make RenderingContext current: {error:?}"))?;
        let egui_glow = egui_glow::winit::EguiGlow::new(
            event_loop,
            rendering_context.glow_gl_api(),
            None,
            None,
            false,
        );
        egui_glow
            .egui_ctx
            .set_request_repaint_callback(move |info| {
                let _ = proxy.send_event(AppEvent::Repaint {
                    delay: info.delay,
                    pass: info.current_cumulative_pass_nr,
                });
            });
        install_system_font_fallbacks(&egui_glow.egui_ctx);
        window.set_visible(true);
        Ok(Self {
            rendering_context: rendering_context.clone(),
            egui_ctx: egui_glow.egui_ctx.clone(),
            egui_glow,
            url: String::new(),
            url_dirty: false,
            navigation_error: None,
            shown_tab: None,
            webview_rect: egui::Rect::NOTHING,
            egui_regions: Vec::new(),
            reader_region: None,
            reader_scroll_offsets: HashMap::new(),
            transient_regions: Vec::new(),
            settings_draft: SettingsDraft::default(),
        })
    }

    /// True when a pointer event at `pos` should go to egui instead of
    /// the page: the toolbar/tab bar area, or any egui window (popup,
    /// dialog, settings) drawn in the last frame.
    pub fn wants_pointer_at(
        &self,
        position: winit::dpi::PhysicalPosition<f64>,
        reader_visible: bool,
    ) -> bool {
        let pixels_per_point = self.egui_ctx.pixels_per_point();
        let pos = egui::pos2(
            position.x as f32 / pixels_per_point,
            position.y as f32 / pixels_per_point,
        );
        if pos.y < self.webview_rect.min.y {
            return true;
        }
        if reader_visible && self.webview_rect.contains(pos) {
            return true;
        }
        self.egui_regions.iter().any(|region| {
            let is_stale_reader = !reader_visible && self.reader_region == Some(*region);
            !is_stale_reader && region.contains(pos)
        })
    }

    pub fn pointer_in_transient_control(
        &self,
        position: winit::dpi::PhysicalPosition<f64>,
    ) -> bool {
        let pixels_per_point = self.egui_ctx.pixels_per_point();
        let pos = egui::pos2(
            position.x as f32 / pixels_per_point,
            position.y as f32 / pixels_per_point,
        );
        self.transient_regions
            .iter()
            .any(|region| region.contains(pos))
    }

    /// Forward a window event to egui-winit (keyboard, focus, …).
    pub fn on_window_event(
        &mut self,
        window: &Window,
        event: &WindowEvent,
    ) -> egui_winit::EventResponse {
        self.egui_glow.egui_winit.on_window_event(window, event)
    }

    /// True while an egui widget (e.g. the address bar) owns the
    /// keyboard focus.
    pub fn has_keyboard_focus(&self) -> bool {
        self.egui_ctx.memory(|memory| memory.focused().is_some())
    }

    pub fn scroll_reader_with_key(&mut self, generation: u64, command: ReaderScrollCommand) {
        let page = (self.webview_rect.height() * 0.85).max(40.0);
        let offset = self.reader_scroll_offsets.entry(generation).or_default();
        match command {
            ReaderScrollCommand::Home => *offset = 0.0,
            ReaderScrollCommand::End => *offset = 1_000_000_000.0,
            ReaderScrollCommand::PageDown => *offset += page,
            ReaderScrollCommand::PageUp => *offset = (*offset - page).max(0.0),
            ReaderScrollCommand::ArrowDown => *offset += 40.0,
            ReaderScrollCommand::ArrowUp => *offset = (*offset - 40.0).max(0.0),
        }
        self.egui_ctx.request_repaint();
    }

    /// Drop the egui keyboard focus so that the next key presses go to
    /// the WebView.
    pub fn surrender_focus(&mut self) {
        surrender_egui_focus(&self.egui_ctx);
    }

    /// Give the address bar the keyboard focus (Ctrl+L).
    pub fn focus_url_bar(&mut self) {
        request_url_bar_focus(&self.egui_ctx);
    }

    pub fn url_bar_has_focus(&self) -> bool {
        self.egui_ctx
            .memory(|memory| memory.focused() == Some(url_bar_id()))
    }

    pub fn cancel_url_edit(&mut self, canonical_url: Option<String>) {
        self.url = canonical_url.unwrap_or_default();
        self.url_dirty = false;
        self.navigation_error = None;
        self.surrender_focus();
    }

    /// Build the frame: tab bar, toolbar UI, then the WebView paint
    /// callback for the active tab.
    pub fn update(&mut self, window: &Window, state: &Rc<AppState>) {
        let active_tab = state.active_tab_id();
        if self.shown_tab != active_tab {
            // A global draft must never migrate into another tab. This
            // synchronization lives here (rather than activate_tab),
            // because tab buttons run while Gui is already borrowed.
            self.shown_tab = active_tab;
            self.url = state.active_tab_url().unwrap_or_default();
            self.url_dirty = false;
            self.navigation_error = None;
            if self.url_bar_has_focus() {
                request_url_bar_focus(&self.egui_ctx);
            }
        }

        // Hit-test regions are frame-local. Keeping stale rectangles
        // would permanently steal pointer events from page content.
        self.egui_regions.clear();
        self.reader_region = None;
        self.transient_regions.clear();
        let ready_generations = state.reader_ready_generations();
        self.reader_scroll_offsets
            .retain(|generation, _| ready_generations.contains(generation));
        let Gui {
            egui_ctx,
            egui_glow,
            url,
            url_dirty,
            navigation_error,
            shown_tab,
            webview_rect,
            egui_regions,
            reader_region,
            reader_scroll_offsets,
            transient_regions,
            settings_draft,
            ..
        } = self;

        egui_glow.run(window, |ui| {
            Self::tab_bar_ui(ui, state);
            Self::toolbar_ui(
                url,
                url_dirty,
                navigation_error,
                *shown_tab == state.active_tab_id(),
                ui,
                state,
            );

            let available_rect = ui.available_rect_before_wrap();
            *webview_rect = available_rect;

            let reader_visible = state.active_reader_is_visible();
            if let Some(rect) =
                Self::reader_view_ui(ui, state, *webview_rect, reader_scroll_offsets)
            {
                *reader_region = Some(rect);
                egui_regions.push(rect);
            }
            if let Some(rect) = Self::context_menu_ui(ui, state, *webview_rect) {
                egui_regions.push(rect);
                transient_regions.push(rect);
            }
            if let Some(rect) = Self::select_ui(ui, state, *webview_rect) {
                egui_regions.push(rect);
                transient_regions.push(rect);
            }
            if let Some(rect) = Self::color_picker_ui(ui, state, *webview_rect) {
                egui_regions.push(rect);
                transient_regions.push(rect);
            }
            if let Some(rect) = Self::settings_window_ui(ui, state, settings_draft) {
                egui_regions.push(rect);
            }
            // Draw the modal last so its backdrop covers and blocks all
            // browser chrome and auxiliary windows.
            if let Some(rect) = Self::dialog_ui(ui, state) {
                egui_regions.push(rect);
            }

            // Keep every tab's WebView and the shared rendering
            // surface sized to the viewport below the toolbar.
            let pixels_per_point = ui.pixels_per_point();
            let surface_size = winit::dpi::PhysicalSize::new(
                (available_rect.width() * pixels_per_point).round().max(1.0) as u32,
                (available_rect.height() * pixels_per_point)
                    .round()
                    .max(1.0) as u32,
            );
            for tab_webview in state.webviews.borrow_mut().iter_mut() {
                if surface_size != tab_webview.viewport_size {
                    tab_webview.webview.resize(surface_size);
                    tab_webview.viewport_size = surface_size;
                }
            }
            if surface_size != state.rendering_context.size() {
                state.rendering_context.resize(surface_size);
            }

            if !reader_visible {
                // Servo renders the active tab into the shared offscreen
                // framebuffer first…
                state.repaint_webviews();

                // …then the result is blitted into the egui scene.
                if let Some(render_to_parent) = state.rendering_context.render_to_parent_callback()
                {
                    ui.ctx()
                        .layer_painter(egui::LayerId::background())
                        .add(egui::PaintCallback {
                            rect: available_rect,
                            callback: Arc::new(egui_glow::painter::CallbackFn::new(
                                move |info, painter| {
                                    let clip = info.viewport_in_pixels();
                                    let rect_in_parent = Rect::new(
                                        Point2D::new(clip.left_px, clip.from_bottom_px),
                                        Size2D::new(clip.width_px, clip.height_px),
                                    );
                                    render_to_parent(painter.gl(), rect_in_parent)
                                },
                            )),
                        });
                }
            }
        });

        let egui_owns_keyboard = egui_ctx.memory(|memory| memory.focused().is_some());
        if state.has_modal_dialog() && !egui_owns_keyboard {
            window.set_ime_allowed(false);
        } else if !egui_owns_keyboard {
            state.sync_page_ime(*webview_rect, egui_ctx.pixels_per_point());
        }
    }

    /// Paint the egui frame to the window surface.
    pub fn paint(&mut self, window: &Window) -> Result<(), String> {
        self.rendering_context
            .make_current()
            .map_err(|error| format!("Could not make RenderingContext current: {error:?}"))?;
        self.rendering_context
            .parent_context()
            .prepare_for_rendering();
        self.egui_glow.paint(window);
        self.rendering_context.parent_context().present();
        Ok(())
    }

    /// The tab bar: one selectable chip per tab, plus a new-tab button.
    fn tab_bar_ui(ui: &mut egui::Ui, state: &Rc<AppState>) {
        egui::Panel::top("tabbar").show(ui, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let active = state.active_tab_id();
                for tab in state.tab_states() {
                    let label = tab.title.clone().unwrap_or_else(|| {
                        if tab.url.as_str() == "about:blank" {
                            "New Tab".to_owned()
                        } else {
                            crate::app::url_identity(&tab.url)
                        }
                    });
                    if ui.selectable_label(active == Some(tab.id), label).clicked() {
                        state.activate_tab(tab.id);
                        surrender_egui_focus(ui.ctx());
                        state.restore_page_focus();
                    }
                }
                if ui.button("+").on_hover_text("New tab (Ctrl+T)").clicked() {
                    state.create_tab(state.start_page_url());
                    request_url_bar_focus(ui.ctx());
                }
            });
            ui.add_space(2.0);
        });
    }

    fn toolbar_ui(
        url: &mut String,
        url_dirty: &mut bool,
        navigation_error: &mut Option<String>,
        url_matches_active_tab: bool,
        ui: &mut egui::Ui,
        state: &Rc<AppState>,
    ) {
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(state.can_go_back.get(), egui::Button::new("←"))
                    .on_hover_text("Back")
                    .clicked()
                {
                    state.navigate_back();
                    surrender_egui_focus(ui.ctx());
                    state.restore_page_focus();
                }
                if ui
                    .add_enabled(state.can_go_forward.get(), egui::Button::new("→"))
                    .on_hover_text("Forward")
                    .clicked()
                {
                    state.navigate_forward();
                    surrender_egui_focus(ui.ctx());
                    state.restore_page_focus();
                }
                if ui.button("⟳").on_hover_text("Reload").clicked() {
                    state.navigate_reload();
                    surrender_egui_focus(ui.ctx());
                    state.restore_page_focus();
                }

                let response = ui.add_sized(
                    [(ui.available_width() - 158.0).max(80.0), 24.0],
                    egui::TextEdit::singleline(url).id(url_bar_id()).hint_text(
                        if state.search_engine() == SearchEngine::Disabled {
                            "Enter address"
                        } else {
                            "Enter address or search (?word)"
                        },
                    ),
                );
                let select_all = url_matches_active_tab
                    && ui.ctx().data_mut(|data| {
                        data.remove_temp::<bool>(url_bar_selection_request_id())
                            .unwrap_or(false)
                    });
                if select_all {
                    response.request_focus();
                    let mut edit_state =
                        egui::TextEdit::load_state(ui.ctx(), url_bar_id()).unwrap_or_default();
                    let selection = egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(url.chars().count()),
                    );
                    edit_state.cursor.set_char_range(Some(selection));
                    egui::TextEdit::store_state(ui.ctx(), url_bar_id(), edit_state);
                }
                if response.changed() {
                    *url_dirty = true;
                    *navigation_error = None;
                }
                let submitted = response.lost_focus()
                    && !state.ui_ime_composing()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter));
                if submitted {
                    match state.navigate(url) {
                        Ok(()) => {
                            *url_dirty = false;
                            *navigation_error = None;
                            surrender_egui_focus(ui.ctx());
                            state.restore_page_focus();
                        }
                        Err(error) => {
                            *url_dirty = true;
                            *navigation_error = Some(error.to_string());
                            response.request_focus();
                        }
                    }
                }
                let (blocking_label, blocking_hint) =
                    if let Some((host, stats)) = state.active_blocking_stats() {
                        let total = stats.total();
                        let label = if total > 9_999 {
                            "Blocked 9999+".to_owned()
                        } else {
                            format!("Blocked {total}")
                        };
                        let hint = format!(
                            "Blocked for {host} this session\nTrackers: {}\nAds: {}",
                            stats.trackers(),
                            stats.advertisements()
                        );
                        (label, hint)
                    } else {
                        (
                            "Blocked —".to_owned(),
                            "Blocking statistics are available on web pages".to_owned(),
                        )
                    };
                ui.label(egui::RichText::new(blocking_label).small())
                    .on_hover_text(blocking_hint);
                let reader_state = state.reader_button_state();
                let (reader_enabled, reader_selected, reader_hint) = match reader_state {
                    ReaderButtonState::Unavailable => (
                        false,
                        false,
                        "Reader is available after an HTTP(S) page finishes loading",
                    ),
                    ReaderButtonState::Waiting => (
                        false,
                        false,
                        "A previous Reader extraction is still stopping",
                    ),
                    ReaderButtonState::Available => (true, false, "Open local Reader view"),
                    ReaderButtonState::Extracting => (true, true, "Cancel Reader extraction"),
                    ReaderButtonState::Active => (true, true, "Exit Reader view"),
                    ReaderButtonState::Error { retryable: true } => {
                        (true, true, "Retry Reader extraction")
                    }
                    ReaderButtonState::Error { retryable: false } => {
                        (true, true, "Close timed-out Reader extraction")
                    }
                };
                if ui
                    .add_enabled(
                        reader_enabled,
                        egui::Button::new("Aa").selected(reader_selected),
                    )
                    .on_hover_text(reader_hint)
                    .clicked()
                {
                    state.toggle_reader_mode();
                    surrender_egui_focus(ui.ctx());
                }
                let settings_button = ui.button("⚙").on_hover_text("Settings");
                if settings_button.clicked() {
                    state.settings_open.set(!state.settings_open.get());
                }
            });
            if let Some(error) = navigation_error.as_deref() {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
            ui.add_space(2.0);
        });
    }

    /// Opaque native Reader surface over the WebView. The page remains
    /// alive underneath, but this rect owns all pointer/scroll input.
    fn reader_view_ui(
        context: &egui::Context,
        state: &Rc<AppState>,
        webview_rect: egui::Rect,
        reader_scroll_offsets: &mut HashMap<u64, f32>,
    ) -> Option<egui::Rect> {
        let view = state.active_reader_view()?;
        egui::Area::new(egui::Id::new("reader_view"))
            .order(egui::Order::Middle)
            .fixed_pos(webview_rect.min)
            .show(context, |ui| {
                ui.set_min_size(webview_rect.size());
                ui.set_max_size(webview_rect.size());
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(32, 24))
                    .show(ui, |ui| {
                        ui.set_min_size(egui::vec2(
                            (webview_rect.width() - 64.0).max(1.0),
                            (webview_rect.height() - 48.0).max(1.0),
                        ));
                        match view {
                            ReaderView::Extracting { source_url } => {
                                ui.vertical_centered(|ui| {
                                    ui.add_space(80.0);
                                    ui.spinner();
                                    ui.heading("Building Reader view…");
                                    ui.label(crate::app::url_identity(&source_url));
                                    ui.add_space(8.0);
                                    ui.weak("Extraction is local; the original page is not fetched again.");
                                });
                            }
                            ReaderView::Error {
                                source_url,
                                message,
                                retryable,
                            } => {
                                ui.vertical_centered(|ui| {
                                    ui.add_space(80.0);
                                    ui.heading("Reader view unavailable");
                                    ui.label(message);
                                    ui.weak(crate::app::url_identity(&source_url));
                                    ui.add_space(8.0);
                                    if retryable {
                                        ui.label("Use Aa to retry or Escape to return to the page.");
                                    } else {
                                        ui.label(
                                            "The prior evaluator is still stopping. Use Aa or Escape to return to the page.",
                                        );
                                    }
                                });
                            }
                            ReaderView::Ready {
                                source_url,
                                article,
                                generation,
                            } => {
                                let scroll_offset = reader_scroll_offsets
                                    .get(&generation)
                                    .copied()
                                    .unwrap_or_default();
                                let output = egui::ScrollArea::vertical()
                                    .id_salt("reader_scroll")
                                    .vertical_scroll_offset(scroll_offset)
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        ui.vertical_centered(|ui| {
                                            ui.set_max_width(780.0);
                                            ui.add_space(12.0);
                                            ui.heading(&article.title);
                                            if let Some(byline) = article.byline.as_deref() {
                                                ui.strong(byline);
                                            }
                                            ui.weak(crate::app::url_identity(&source_url));
                                            ui.add_space(4.0);
                                            ui.small(
                                                "Local text view — the original page remains active but throttled.",
                                            );
                                            ui.separator();
                                        });

                                        let alignment = match article.direction {
                                            ReaderDirection::RightToLeft => egui::Align::RIGHT,
                                            ReaderDirection::Auto
                                            | ReaderDirection::LeftToRight => egui::Align::LEFT,
                                        };
                                        ui.with_layout(
                                            egui::Layout::top_down(alignment),
                                            |ui| {
                                                ui.set_max_width(780.0);
                                                for block in &article.blocks {
                                                    match block.kind {
                                                        ReaderBlockKind::Heading => {
                                                            ui.add_space(12.0);
                                                            ui.heading(&block.text);
                                                        }
                                                        ReaderBlockKind::Paragraph => {
                                                            ui.add(
                                                                egui::Label::new(
                                                                    egui::RichText::new(&block.text)
                                                                        .size(18.0),
                                                                )
                                                                .wrap(),
                                                            );
                                                            ui.add_space(8.0);
                                                        }
                                                        ReaderBlockKind::Quote => {
                                                            egui::Frame::new()
                                                                .fill(ui.visuals().faint_bg_color)
                                                                .inner_margin(12)
                                                                .show(ui, |ui| {
                                                                    ui.add(
                                                                        egui::Label::new(
                                                                            egui::RichText::new(
                                                                                &block.text,
                                                                            )
                                                                            .italics(),
                                                                        )
                                                                        .wrap(),
                                                                    );
                                                                });
                                                            ui.add_space(8.0);
                                                        }
                                                        ReaderBlockKind::Code => {
                                                            ui.add(
                                                                egui::Label::new(
                                                                    egui::RichText::new(&block.text)
                                                                        .monospace(),
                                                                )
                                                                .wrap(),
                                                            );
                                                            ui.add_space(8.0);
                                                        }
                                                    }
                                                }
                                                ui.add_space(32.0);
                                            },
                                        );
                                    });
                                reader_scroll_offsets.insert(generation, output.state.offset.y);
                            }
                        }
                    });
            });
        Some(webview_rect)
    }

    /// The settings window: start page and per-site overrides. Proxy
    /// controls stay startup-only until Servo can rebuild its network
    /// connector at runtime.
    /// Returns the drawn window rect for pointer routing.
    fn settings_window_ui(
        ui: &mut egui::Ui,
        state: &Rc<AppState>,
        draft: &mut SettingsDraft,
    ) -> Option<egui::Rect> {
        let mut open = state.settings_open.get();
        if open && !draft.initialized {
            draft.start_page = state.start_page_url().to_string();
            draft.search_engine = state.search_engine();
            draft.initialized = true;
            draft.message = None;
        }
        let mut drawn_rect: Option<egui::Rect> = None;
        if let Some(window_response) = egui::Window::new("Settings")
            .open(&mut open)
            .default_width(360.0)
            .show(ui.ctx(), |ui| {
                ui.heading("Start page");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.start_page)
                            .hint_text("about:blank")
                            .desired_width(240.0),
                    );
                    if ui.button("Apply").clicked() {
                        draft.message = Some(match state.apply_start_page(&draft.start_page) {
                            Ok(()) => (true, "Start page updated".to_owned()),
                            Err(error) => (false, error),
                        });
                    }
                });

                ui.separator();
                ui.heading("Search");
                ui.label(
                    "Disabled by default. Queries are sent only after Enter; search suggestions \
                     and keystroke requests stay off.",
                );
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("settings_search_engine")
                        .selected_text(draft.search_engine.label())
                        .show_ui(ui, |ui| {
                            for engine in SearchEngine::CHOICES {
                                ui.selectable_value(
                                    &mut draft.search_engine,
                                    engine,
                                    engine.label(),
                                );
                            }
                        });
                    if ui.button("Apply").clicked() {
                        state.set_search_engine(draft.search_engine);
                        draft.message = Some((
                            true,
                            format!("Search provider: {}", draft.search_engine.label()),
                        ));
                    }
                });
                ui.label("Use words with spaces, or prefix a one-word query with ?.");

                ui.separator();
                ui.heading("Proxy");
                ui.label(
                    "Startup only: --proxy http://host:port, optionally with \
                     --proxy-bypass host1,host2. Runtime changes are not supported by Servo 0.5.",
                );

                ui.separator();
                ui.heading("Per-site tracker override");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.tracker_host)
                            .hint_text("example.com")
                            .desired_width(180.0),
                    );
                    if ui.button("Allow trackers").clicked() {
                        draft.message = Some(
                            match state.set_tracker_override(&draft.tracker_host, true) {
                                Ok(()) => {
                                    draft.tracker_host.clear();
                                    (true, "Site override added".to_owned())
                                }
                                Err(error) => (false, error),
                            },
                        );
                    }
                });
                for (host, allow) in state.tracker_overrides() {
                    let mut allow = allow;
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut allow, &host).changed() {
                            draft.message = Some(match state.set_tracker_override(&host, allow) {
                                Ok(()) => (true, format!("Tracker override updated for {host}")),
                                Err(error) => (false, error),
                            });
                        }
                        if ui.button("Remove").clicked() {
                            state.remove_tracker_override(&host);
                            draft.message =
                                Some((true, format!("Tracker override removed for {host}")));
                        }
                    });
                }
                if let Some((success, message)) = &draft.message {
                    let color = if *success {
                        egui::Color32::LIGHT_GREEN
                    } else {
                        egui::Color32::LIGHT_RED
                    };
                    ui.colored_label(color, message);
                }
            })
        {
            drawn_rect = Some(window_response.response.rect);
        }
        state.settings_open.set(open);
        if !open {
            if draft.initialized {
                surrender_egui_focus(ui.ctx());
                state.restore_page_focus();
            }
            draft.initialized = false;
        }
        drawn_rect
    }

    /// The context menu requested by page content (right click). A
    /// click on an item resolves the request with that action. Returns
    /// the drawn window rect for pointer routing.
    fn context_menu_ui(
        ui: &mut egui::Ui,
        state: &Rc<AppState>,
        webview_rect: egui::Rect,
    ) -> Option<egui::Rect> {
        let mut clicked: Option<ContextMenuAction> = None;
        let mut drawn_rect: Option<egui::Rect> = None;
        let mut request_initial_focus = ui.ctx().memory(|memory| memory.focused().is_none());
        {
            let menu_borrow = state.active_context_menu.borrow();
            let menu = &menu_borrow.as_ref()?.control;
            let position = menu.position();
            let ppp = ui.pixels_per_point();
            let origin = egui::pos2(
                webview_rect.min.x + position.min.x as f32 / ppp,
                webview_rect.min.y + position.min.y as f32 / ppp,
            );
            if let Some(response) = egui::Window::new("Context menu")
                .title_bar(false)
                .resizable(false)
                .fixed_pos(origin)
                .show(ui.ctx(), |ui| {
                    for item in menu.items() {
                        match item {
                            ContextMenuItem::Separator => {
                                ui.separator();
                            }
                            ContextMenuItem::Item {
                                label,
                                action,
                                enabled,
                            } => {
                                let enabled = *enabled;
                                let action = *action;
                                let response = ui.add_enabled(
                                    enabled,
                                    egui::Button::new(crate::app::text_for_ui(label, 200)),
                                );
                                if enabled && request_initial_focus {
                                    response.request_focus();
                                    request_initial_focus = false;
                                }
                                if response.clicked() {
                                    clicked = Some(action);
                                }
                            }
                        }
                    }
                })
            {
                drawn_rect = Some(response.response.rect);
            }
        }
        if let Some(action) = clicked {
            let Some(pending) = state.active_context_menu.borrow_mut().take() else {
                return drawn_rect;
            };
            info!("Context menu action: {action:?}");
            state.authorize_context_menu_action(pending.tab, action);
            pending.control.select(action);
            surrender_egui_focus(ui.ctx());
            state.restore_page_focus();
        }
        drawn_rect
    }

    /// The `<select>` popup requested by page content. Returns the
    /// drawn window rect for pointer routing.
    fn select_ui(
        ui: &mut egui::Ui,
        state: &Rc<AppState>,
        webview_rect: egui::Rect,
    ) -> Option<egui::Rect> {
        enum SelectRow<'a> {
            Group(&'a str),
            Option(&'a servo::SelectElementOption, bool),
        }

        let mut submit = false;
        let mut selection_changed = false;
        let mut drawn_rect: Option<egui::Rect> = None;
        let mut selection = state.active_select_value.borrow().clone();
        let request_initial_focus = ui.ctx().memory(|memory| memory.focused().is_none());
        {
            let select_borrow = state.active_select.borrow();
            let select = &select_borrow.as_ref()?.control;
            let position = select.position();
            let ppp = ui.pixels_per_point();
            let origin = egui::pos2(
                webview_rect.min.x + position.min.x as f32 / ppp,
                webview_rect.min.y + position.min.y as f32 / ppp,
            );
            let multiple = select.allow_select_multiple();
            let mut rows = Vec::new();
            for item in select.options() {
                match item {
                    SelectElementOptionOrOptgroup::Option(option) => {
                        rows.push(SelectRow::Option(option, false));
                    }
                    SelectElementOptionOrOptgroup::Optgroup { label, options } => {
                        rows.push(SelectRow::Group(label));
                        rows.extend(options.iter().map(|option| SelectRow::Option(option, true)));
                    }
                }
            }
            let focus_row = if request_initial_focus {
                rows.iter()
                    .position(|row| {
                        matches!(row, SelectRow::Option(option, _) if !option.is_disabled && selection.contains(&option.id))
                    })
                    .or_else(|| {
                        rows.iter().position(|row| {
                            matches!(row, SelectRow::Option(option, _) if !option.is_disabled)
                        })
                    })
            } else {
                None
            };
            if let Some(response) = egui::Window::new("Select element")
                .title_bar(false)
                .resizable(false)
                .fixed_pos(origin)
                .show(ui.ctx(), |ui| {
                    let row_height = ui.spacing().interact_size.y;
                    let list_height = (webview_rect.height() * 0.6).clamp(48.0, 360.0);
                    let mut scroll = egui::ScrollArea::vertical().max_height(list_height);
                    if let Some(row) = focus_row {
                        scroll = scroll.vertical_scroll_offset(row as f32 * row_height);
                    }
                    scroll.show_rows(ui, row_height, rows.len(), |ui, visible_rows| {
                        for row_index in visible_rows {
                            match &rows[row_index] {
                                SelectRow::Group(label) => {
                                    ui.label(
                                        egui::RichText::new(crate::app::text_for_ui(label, 200))
                                            .strong(),
                                    );
                                }
                                SelectRow::Option(option, indented) => {
                                    ui.horizontal(|ui| {
                                        if *indented {
                                            ui.add_space(12.0);
                                        }
                                        let id = option.id;
                                        if multiple {
                                            let mut selected = selection.contains(&id);
                                            let response = ui.add_enabled(
                                                !option.is_disabled,
                                                egui::Checkbox::new(
                                                    &mut selected,
                                                    crate::app::text_for_ui(&option.label, 200),
                                                ),
                                            );
                                            if focus_row == Some(row_index) {
                                                response.request_focus();
                                            }
                                            if response.changed() {
                                                selection_changed = true;
                                                if selected {
                                                    selection.push(id);
                                                    selection.sort_unstable();
                                                    selection.dedup();
                                                } else {
                                                    selection
                                                        .retain(|selected_id| *selected_id != id);
                                                }
                                            }
                                        } else {
                                            let response = ui.add_enabled(
                                                !option.is_disabled,
                                                egui::Button::selectable(
                                                    selection.contains(&id),
                                                    crate::app::text_for_ui(&option.label, 200),
                                                ),
                                            );
                                            if focus_row == Some(row_index) {
                                                response.request_focus();
                                            }
                                            if response.clicked() {
                                                selection.clear();
                                                selection.push(id);
                                                selection_changed = true;
                                                submit = true;
                                            }
                                        }
                                    });
                                }
                            }
                        }
                    });
                    if multiple {
                        ui.separator();
                        submit = ui.button("Done").clicked();
                    }
                })
            {
                drawn_rect = Some(response.response.rect);
            }
        }
        if submit {
            let Some(pending) = state.active_select.borrow_mut().take() else {
                return drawn_rect;
            };
            info!("Select element chose options {selection:?}");
            let mut select = pending.control;
            select.select(selection);
            select.submit();
            state.active_select_value.borrow_mut().clear();
            surrender_egui_focus(ui.ctx());
            state.restore_page_focus();
        } else if selection_changed {
            // Keep a GUI-only draft. Servo's SelectElement commits its
            // current value even when it is dropped, so mutating it here
            // would make Escape/outside-click act like a partial submit.
            *state.active_select_value.borrow_mut() = selection;
        }
        drawn_rect
    }

    fn color_picker_ui(
        ui: &mut egui::Ui,
        state: &Rc<AppState>,
        webview_rect: egui::Rect,
    ) -> Option<egui::Rect> {
        enum Action {
            Apply,
            Cancel,
        }

        let mut action = None;
        let mut color = state.active_color_value.get();
        let mut drawn_rect = None;
        let request_initial_focus = ui.ctx().memory(|memory| memory.focused().is_none());
        {
            let picker_borrow = state.active_color_picker.borrow();
            let picker = &picker_borrow.as_ref()?.control;
            let position = picker.position();
            let pixels_per_point = ui.pixels_per_point();
            let origin = egui::pos2(
                webview_rect.min.x + position.min.x as f32 / pixels_per_point,
                webview_rect.min.y + position.min.y as f32 / pixels_per_point,
            );
            if let Some(response) = egui::Window::new("Choose color")
                .auto_sized()
                .resizable(false)
                .collapsible(false)
                .fixed_pos(origin)
                .show(ui.ctx(), |ui| {
                    // Match the picker canvas to the numeric RGB row so the
                    // transient window does not contain a large empty column.
                    ui.spacing_mut().slider_width = 200.0;
                    let mut selected = egui::Color32::from_rgb(color[0], color[1], color[2]);
                    if egui::color_picker::color_picker_color32(
                        ui,
                        &mut selected,
                        egui::color_picker::Alpha::Opaque,
                    ) {
                        color = [selected.r(), selected.g(), selected.b()];
                    }
                    ui.horizontal(|ui| {
                        let apply = ui.button("Apply");
                        if request_initial_focus {
                            apply.request_focus();
                        }
                        if apply.clicked() {
                            action = Some(Action::Apply);
                        }
                        if ui.button("Cancel").clicked() {
                            action = Some(Action::Cancel);
                        }
                    });
                })
            {
                drawn_rect = Some(response.response.rect);
            }
        }
        state.active_color_value.set(color);

        if let Some(action) = action {
            let Some(pending) = state.active_color_picker.borrow_mut().take() else {
                return drawn_rect;
            };
            if matches!(action, Action::Apply) {
                let mut picker = pending.control;
                picker.select(Some(RgbColor {
                    red: color[0],
                    green: color[1],
                    blue: color[2],
                }));
                picker.submit();
            }
            // Dropping without `select` keeps the original color.
            surrender_egui_focus(ui.ctx());
            state.restore_page_focus();
        }
        drawn_rect
    }

    /// alert() / confirm() / prompt() dialogs from page content.
    /// Returns the drawn window rect for pointer routing.
    fn dialog_ui(ui: &mut egui::Ui, state: &Rc<AppState>) -> Option<egui::Rect> {
        enum DialogResponse {
            Confirm,
            Dismiss,
        }
        if !state.active_dialog.borrow().is_some() {
            return None;
        }
        let drawn_rect: egui::Rect;
        let mut response: Option<DialogResponse> = None;
        let mut value_edit = match state.active_dialog.borrow().as_ref() {
            Some(pending) => match &pending.control {
                SimpleDialog::Prompt(prompt) => prompt.current_value().to_owned(),
                _ => String::new(),
            },
            _ => String::new(),
        };
        {
            let dialog_borrow = state.active_dialog.borrow();
            let dialog = &dialog_borrow.as_ref()?.control;
            let dialog_id = dialog_borrow.as_ref()?.id;
            let message = crate::app::text_for_ui(dialog.message(), 4096);
            let is_prompt = matches!(dialog, SimpleDialog::Prompt(_));
            let is_confirm_or_prompt =
                matches!(dialog, SimpleDialog::Confirm(_) | SimpleDialog::Prompt(_));
            let focus_prompt = state.take_dialog_focus_request(dialog_id) && is_prompt;
            let title = format!("Page dialog — {}", state.active_page_label());
            let modal_response =
                egui::Modal::new(egui::Id::new("page-dialog")).show(ui.ctx(), |ui| {
                    ui.heading(title);
                    ui.label(&message);
                    if is_prompt {
                        let edit = ui.text_edit_singleline(&mut value_edit);
                        if focus_prompt {
                            edit.request_focus();
                        }
                    }
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() {
                            response = Some(DialogResponse::Confirm);
                        }
                        if is_confirm_or_prompt && ui.button("Cancel").clicked() {
                            response = Some(DialogResponse::Dismiss);
                        }
                    });
                    let (enter, escape) = ui.input_mut(|input| {
                        let composing = state.ui_ime_composing();
                        let enter = !composing
                            && input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
                        let escape = !composing
                            && input.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
                        (enter, escape)
                    });
                    if response.is_none() {
                        if escape {
                            response = Some(DialogResponse::Dismiss);
                        } else if enter {
                            response = Some(DialogResponse::Confirm);
                        }
                    }
                });
            drawn_rect = modal_response.response.rect;
        }
        // Persist the frame-local edit before resolving a keyboard or
        // button response. This preserves a text/IME commit and Enter
        // that arrived in the same raw-input batch.
        if let Some(pending) = state.active_dialog.borrow_mut().as_mut() {
            if let SimpleDialog::Prompt(prompt) = &mut pending.control {
                prompt.set_current_value(&value_edit);
            }
        }
        if let Some(response) = response {
            // `dialog_ui` runs while AppState already holds a mutable
            // borrow of Gui, so release egui focus through this frame's
            // context instead of re-borrowing AppState::gui.
            surrender_egui_focus(ui.ctx());
            match response {
                DialogResponse::Confirm => {
                    info!("Page dialog: OK");
                    state.resolve_dialog(true);
                }
                DialogResponse::Dismiss => {
                    info!("Page dialog: Cancel");
                    state.resolve_dialog(false);
                }
            }
        }
        Some(drawn_rect)
    }
}
