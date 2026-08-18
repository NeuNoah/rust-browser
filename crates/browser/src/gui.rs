//! The egui-based user interface: toolbar, URL bar and the WebView
//! blit into the egui scene.
//!
//! Layout follows servoshell: a `TopBottomPanel` holds the toolbar;
//! the remaining area is the WebView viewport. Servo paints into an
//! offscreen framebuffer which is copied into the egui scene via a
//! `PaintCallback` on the background layer.

use std::rc::Rc;
use std::sync::Arc;

use euclid::{Point2D, Rect, Scale, Size2D};
use servo::{DeviceIndependentPixel, DevicePixel, OffscreenRenderingContext, RenderingContext};
use url::Url;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use crate::app::AppState;

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
    /// The WebView viewport in egui points.
    pub webview_rect: egui::Rect,
}

impl Gui {
    pub fn new(
        event_loop: &ActiveEventLoop,
        window: &Window,
        rendering_context: &Rc<OffscreenRenderingContext>,
    ) -> Self {
        rendering_context
            .make_current()
            .expect("Could not make RenderingContext current");
        let egui_glow = egui_glow::winit::EguiGlow::new(
            event_loop,
            rendering_context.glow_gl_api(),
            None,
            None,
            false,
        );
        window.set_visible(true);
        Self {
            rendering_context: rendering_context.clone(),
            egui_ctx: egui_glow.egui_ctx.clone(),
            egui_glow,
            url: String::new(),
            url_dirty: false,
            webview_rect: egui::Rect::NOTHING,
        }
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

    /// Drop the egui keyboard focus so that the next key presses go to
    /// the WebView.
    pub fn surrender_focus(&mut self) {
        self.egui_ctx.memory_mut(|memory| {
            if let Some(id) = memory.focused() {
                memory.surrender_focus(id);
            }
            memory.stop_text_input();
        });
    }

    /// Build the frame: tab bar, toolbar UI, then the WebView paint
    /// callback for the active tab.
    pub fn update(&mut self, window: &Window, state: &Rc<AppState>) {
        let Gui {
            egui_ctx,
            egui_glow,
            url,
            url_dirty,
            webview_rect,
            ..
        } = self;

        egui_glow.run(window, |ui| {
            Self::tab_bar_ui(ui, state);
            Self::toolbar_ui(url, url_dirty, ui, state);

            let available_rect = ui.available_rect_before_wrap();
            *webview_rect = available_rect;

            // Keep every tab's WebView and the shared rendering
            // surface sized to the viewport below the toolbar.
            let scale = Scale::<_, DeviceIndependentPixel, DevicePixel>::new(ui.pixels_per_point());
            let size = Size2D::new(available_rect.width(), available_rect.height()) * scale;
            let surface_size = winit::dpi::PhysicalSize::new(size.width as u32, size.height as u32);
            for tab_webview in state.webviews.borrow().iter() {
                if size != tab_webview.webview.size() {
                    log::debug!(
                        "WebView resize {}x{} -> {}x{}",
                        tab_webview.webview.size().width,
                        tab_webview.webview.size().height,
                        size.width,
                        size.height
                    );
                    tab_webview.webview.resize(surface_size);
                }
            }
            if surface_size != state.rendering_context.size() {
                state.rendering_context.resize(surface_size);
            }

            // Servo renders the active tab into the shared offscreen
            // framebuffer first…
            state.repaint_webviews();

            // …then the result is blitted into the egui scene.
            if let Some(render_to_parent) = state.rendering_context.render_to_parent_callback() {
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
        });

        // Widgets may request repaints (e.g. the focused URL bar caret).
        if egui_ctx.has_requested_repaint() {
            state.needs_repaint.set(true);
        }
    }

    /// Paint the egui frame to the window surface.
    pub fn paint(&mut self, window: &Window) {
        self.rendering_context
            .make_current()
            .expect("Could not make RenderingContext current");
        self.rendering_context
            .parent_context()
            .prepare_for_rendering();
        self.egui_glow.paint(window);
        self.rendering_context.parent_context().present();
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
                            tab.url.to_string()
                        }
                    });
                    if ui.selectable_label(active == Some(tab.id), label).clicked() {
                        state.activate_tab(tab.id);
                    }
                }
                if ui.button("+").clicked() {
                    state.create_tab(Url::parse("about:blank").expect("static URL"));
                }
            });
            ui.add_space(2.0);
        });
    }

    fn toolbar_ui(url: &mut String, url_dirty: &mut bool, ui: &mut egui::Ui, state: &Rc<AppState>) {
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(state.can_go_back.get(), egui::Button::new("←"))
                    .clicked()
                {
                    state.navigate_back();
                }
                if ui
                    .add_enabled(state.can_go_forward.get(), egui::Button::new("→"))
                    .clicked()
                {
                    state.navigate_forward();
                }
                if ui.button("⟳").clicked() {
                    state.navigate_reload();
                }

                let response = ui.add_sized(
                    [ui.available_width(), 24.0],
                    egui::TextEdit::singleline(url).hint_text("Search or enter address"),
                );
                if response.changed() {
                    *url_dirty = true;
                }
                let submitted =
                    response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                if submitted {
                    let input = std::mem::take(url);
                    *url_dirty = false;
                    state.navigate(&input);
                }
            });
            ui.add_space(2.0);
        });
    }
}
