# Architecture

## Overview

```
winit event loop (main thread)
   │  ┌─────────────────────────────────────┐
   │  │ AppState (Rc, WebViewDelegate)      │
   │  │  ├─ window: Rc<Window>              │
   │  │  ├─ servo: Servo                    │
   │  │  ├─ gui: RefCell<Gui> (egui)        │
   │  │  ├─ core: RefCell<BrowserCore>      │
   │  │  ├─ pipeline: RequestPipeline       │
   │  │  └─ webviews: RefCell<Vec<WebView>> │
   │  └─────────────────────────────────────┘
   │        │                       ▲
   │  Servo threads ────────────────┘ (delegate callbacks)
   └─ render: Servo → offscreen FBO → egui PaintCallback → window
```

## Threading model

- The main thread owns the window, egui and the `BrowserCore`.
- Servo runs its own threads. It calls `WebViewDelegate` methods
  (implemented by `AppState`) from those threads.
- Delegate callbacks therefore only:
  - `request_redraw()` (thread-safe),
  - set `Cell<bool>` flags,
  - push into `ui_events: RefCell<Vec<UiEvent>>`.
- The main thread drains `ui_events` at the start of every redraw
  (`AppState::process_ui_events`) and applies them to the core model
  and the GUI. This avoids cross-thread `RefCell` collisions.

## Wake-ups

Servo needs the main thread to drain its message queues:

```
Servo ──EventLoopWaker::wake──▶ EventLoopProxy::send_event(AppEvent::Wake)
                                     ▼
                    main loop: Servo::spin_event_loop()
```

`spin_event_loop` is also called from `about_to_wait`; `ControlFlow`
switches to `Poll` while Servo reports pending events, `Wait` otherwise.

## Frame loop

On `RedrawRequested`:

1. `process_ui_events()` — apply queued delegate notifications.
2. `Gui::update()`:
   a. egui input is taken, the toolbar UI runs (URL bar, buttons);
   b. the WebView is resized to the area below the toolbar;
   c. `AppState::repaint_webviews()` makes the GL context current and
      calls `WebView::paint()`, which makes Servo render into the
      offscreen framebuffer;
   d. `render_to_parent_callback()` provides the blit closure; it is
      registered as an egui `PaintCallback` on the background layer.
3. `Gui::paint()` — makes the context current, prepares the parent
   surface, paints the egui frame (executing the blit), presents.

The blit is a GL `glBlitFramebuffer` from Servo's offscreen FBO into
the window's draw buffer, scissored to the WebView rectangle.

## Request pipeline

Every network request is evaluated by `RequestPipeline` (crates/
browser-network) before Servo sees it:

```
RequestContext { url, initiator, resource_type, is_top_level }
   → SchemeValidationLayer   (browser-security policy)
   → TrackerLayer            (browser-privacy tracker engine)
   → MixedContentLayer       (no HTTP subresources on HTTPS)
   → PipelineDecision
```

- `Allow` — the request proceeds untouched.
- `Block` — `WebViewDelegate::load_web_resource` intercepts the load and
  answers with an empty 200 response, so the page renders normally
  around the missing resource.

Top-level navigations are exempt from tracker blocking (visiting a
tracker's own site is legitimate) but still pass scheme validation.

## Data flow of a navigation

```
URL bar input
  → BrowserCore::command_from_input (normalize + NavigationPolicy)
  → NavigationCommand::Load(url)
  → WebView::load(url)
  → Servo navigates
  → notify_url_changed → UiEvent::UrlChanged → core.location_changed,
    URL bar updated (unless the user is editing)
```

## Crates

| Crate            | Responsibility                          | Depends on          |
|------------------|-----------------------------------------|---------------------|
| browser          | window, GUI, Servo glue                 | core, network, servo, winit, egui |
| browser-core     | tabs, navigation commands, core state   | security            |
| browser-network  | request pipeline, layers                | security, privacy   |
| browser-privacy  | tracker engine, cookies, fingerprinting | —                   |
| browser-security | scheme/navigation/download policy       | —                   |

The four policy crates are engine-free: they are pure functions over
URLs and state, unit-tested without Servo, and compiled in seconds.
The binary is the only place where Servo's types appear.

## Why these decisions

- **Servo crate, not the servo repo**: the published `servo` crate is
  the supported embeddable API; building the repo directly means
  maintaining a custom build.
- **offscreen rendering**: Servo renders into an FBO and egui blits it.
  This is the servoshell approach and keeps all compositing in one GL
  context.
- **RefCell/queue bridge**: servoshell uses a command queue; we keep the
  same idea minimal: a small `UiEvent` enum.