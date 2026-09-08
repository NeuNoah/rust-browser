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
   │  │  ├─ pipeline: Rc<RequestPipeline>   │
   │  │  └─ webviews: RefCell<Vec<TabWebView>> │
   │  └─────────────────────────────────────┘
   │        │                       ▲
   │  Servo workers ──channels──▶ spin_event_loop (delegate callbacks)
   └─ render: Servo → offscreen FBO → egui PaintCallback → window
```

## Threading model

- The main thread owns the window, egui and the `BrowserCore`.
- Servo runs worker threads, but its public delegate is an
  `Rc<dyn WebViewDelegate>` and callbacks are dispatched while the main
  thread runs `Servo::spin_event_loop()`.
- Callbacks that would otherwise re-enter GUI/core borrows push small
  values into `ui_events`; lightweight engine state such as history and
  cursor feedback can be updated directly on the main thread.
- The main thread drains `ui_events` at the start of every redraw
  (`AppState::process_ui_events`) and applies them to the core model
  and the GUI. This avoids re-entrant `RefCell` borrows; it is not a
  cross-thread synchronization mechanism.

## Wake-ups

Servo needs the main thread to drain its message queues:

```
Servo ──EventLoopWaker::wake──▶ EventLoopProxy::send_event(AppEvent::Wake)
                                     ▼
                    main loop: Servo::spin_event_loop()
```

`spin_event_loop` is also called from `about_to_wait`. `ControlFlow`
uses `Poll` only while the active tab is reported as animating,
`WaitUntil` for a scheduled egui repaint, and `Wait` otherwise. An
animating hidden tab is tracked but does not by itself keep the window
in a busy polling loop.

## Frame loop

On `RedrawRequested`:

1. `process_ui_events()` — apply queued delegate notifications.
2. `Gui::update()`:
   a. egui input is taken, the toolbar UI runs (URL bar, buttons);
   b. all WebViews are resized to the area below the toolbar so tab
      switches preserve the correct viewport size;
   c. `AppState::repaint_webviews()` makes the GL context current and
      calls `WebView::paint()` for the active tab, which makes Servo
      render into the offscreen framebuffer;
   d. `render_to_parent_callback()` provides the blit closure; it is
      registered as an egui `PaintCallback` on the background layer.
3. `Gui::paint()` — makes the context current, prepares the parent
   surface, paints the egui frame (executing the blit), presents.

The blit is a GL `glBlitFramebuffer` from Servo's offscreen FBO into
the window's draw buffer, scissored to the WebView rectangle.

## Request pipeline

Every WebView HTTP(S) request delivered through Servo's resource-
interception callbacks is evaluated by `RequestPipeline`
(crates/browser-network) before it is allowed to continue. Global
plain-HTTP loads without an explicit HTTP initiator are rejected fail-
closed first; the remaining global HTTP(S) loads use the pipeline too:

```
RequestContext { url, initiator, top_level_url, resource_type, is_top_level }
   → SchemeValidationLayer   (browser-security policy)
   → PrivateNetworkLayer     (URL-level loopback/private-address guard)
   → MixedContentLayer       (no HTTP subresources on HTTPS)
   → TrackerLayer            (browser-privacy tracker engine)
   → AdblockLayer            (Brave ABP-compatible network filtering)
   → PipelineDecision
```

- `Allow` — the request proceeds untouched.
- `Block` — `WebViewDelegate::load_web_resource` intercepts the load and
  answers with an empty 200 response, so the page renders normally
  around the missing resource.

WebView-associated loads use `WebViewDelegate::load_web_resource` and
attach the WebView's committed main-page URL as trusted
`top_level_url`. Referrer metadata remains untrusted and may be absent.
A per-site tracker override is selected only from that trusted main-page
URL. It can skip only the structurally registered `TrackerLayer`; the
later `AdblockLayer` is still evaluated. The adblock engine is compiled
once from complete local lists and is never mutated during request
handling. Loads without a WebView (including service-worker traffic) use a
`ServoDelegate` backed by the same pipeline, but have neither a trusted
top-level URL nor a per-site tracker exception. The private-network
layer recognizes literal/local URL hosts; post-DNS enforcement is still
needed to cover DNS rebinding.

Tracker- and ad-layer blocks associated with a WebView increment a
bounded `BlockingStatsStore` in `browser-core`, keyed only by the
normalized trusted top-level host. Global loads and security-policy
blocks are not counted, request URLs are not retained, and the toolbar
reads the active site's session totals without persistent storage.

Proxy selection is startup-only. `StartupProxy` validates one
credential-free HTTP endpoint and a bounded bypass list before winit
creates a window. `AppState::create` clears proxy values inherited by
`Preferences::default`, applies only that explicit configuration and
passes it to `ServoBuilder`. Servo's connector does not fall back after
a proxy connection error. Its HTTP tunnel currently defaults a
destination with no explicit non-default port to 443 even for `http://`;
both request delegates therefore cancel default-port HTTP before the
connector can contact the wrong port. WebSockets do not currently reach
the configured proxy in the local integration fixture.

Download bytes do not reach disk yet because Servo 0.5 exposes no
embedder download callback. The filesystem boundary is nevertheless
ready: `SafeDownloadWriter` rejects redirected/reparse destination paths
on Windows, pins the directory without delete sharing, creates an
exclusive temporary regular file and publishes it with a non-overwriting
same-directory hard link only after flush and sync. Each active target
clones the directory handle so the protection survives parent-writer
drop. Abort, collision, junction and directory-swap paths are tested.
The exclusive temporary handle is also tested against replacement before
publication.

Navigation callbacks are a separate boundary and apply
`NavigationPolicy` to top-level and iframe navigations, including URLs
whose schemes do not reach the HTTP(S) resource callback.

Servo 0.5 marks every `Document` resource, including an iframe, as a
main-frame request at this interception boundary. The embedder therefore
does not copy that ambiguous flag into trusted `is_top_level` state and
does not issue a forgeable URL/timeout exemption. There is one narrowly
authenticated bootstrap case: `AppState` gives a WebView created
directly by the browser without an opener an identity-bound one-shot
marker. Its first intercepted HTTP(S) `Document` consumes that marker
and is treated as top-level. No running page exists in that fresh
WebView from which an iframe could steal the marker. Page-created
WebViews and every later document get no such
exception. A later genuine main navigation can still be blocked
conservatively until Servo exposes frame/navigation identity, while an
iframe cannot acquire a PNA or mixed-content exemption.

## User-action grants

Servo 0.5 does not enforce all clipboard, popup and file-input
activation rules at the embedding boundary. `AppState` therefore issues
short-lived, WebView-scoped grants from eligible keyboard, pointer or
context-menu actions.
Clipboard grants authorize one matching read or write (a write grant may
also cover the clear/write pair Servo emits). A general page-action grant
can authorize one popup or native file picker and is removed whichever
capability consumes it first. The context-menu action for opening a new
view creates a popup-only grant, so page script cannot repurpose it for a
file picker. Grants are revoked on navigation, tab switch, tab close,
window focus loss or any transfer of input focus into browser chrome.
File-picker requests are held until the current Servo
event batch has processed matching hide/navigation/close and competing-
control events, then presented as a window-parented system dialog with
bounded, sanitized extension filters. The native dialog is synchronous:
once it is visible, the embedder event loop resumes only after the user
closes it, so a later Servo hide message cannot dismiss an already-
visible operating-system dialog.

## Reader view

Reader mode is a native egui presentation surface layered over exactly
one tab. The already-loaded page evaluates a fixed extraction script in
its main JavaScript world; the embedder does not fetch the URL again and
does not rewrite the DOM. The script returns only structured text. Rust
then independently validates and caps the title, byline, block count,
block length and total text before any value reaches the UI. Raw HTML,
page-supplied styles and executable URLs are never rendered.

Each extraction request records the tab, WebView identity, committed
URL, core navigation sequence and a monotonically increasing generation.
Results are accepted only when all identities still match. Reader state
is per tab (`Extracting`, `Ready` or `Error`), so switching tabs neither
mixes results nor steals focus. A visible ready/error Reader surface
throttles its covered WebView and consumes page pointer, wheel and Reader
scroll-key input; closing it restores normal scheduling and page focus.

Servo's evaluator cannot currently be canceled. The UI therefore times
out after five seconds while an in-flight tombstone remains until the
callback arrives. Only one evaluation per WebView and four globally may
be outstanding, preventing repeated timeout/retry actions from stacking
unbounded work.

## Data flow of a navigation

```
URL bar input
  → BrowserCore::command_from_input
      ├─ URL → normalize + NavigationPolicy
      └─ query → explicitly selected SearchEngine → bounded HTTPS URL
  → NavigationCommand::Load(url)
  → WebView::load(url)
  → WebViewDelegate::request_navigation (content-navigation policy)
  → Servo navigates
  → notify_url_changed → UiEvent::Url → core.location_changed,
    URL bar updated (unless the user is editing)
```

## Crates

| Crate            | Responsibility                          | Depends on          |
|------------------|-----------------------------------------|---------------------|
| browser          | window, GUI, Servo glue                 | core, network, servo, winit, egui |
| browser-core     | tabs, navigation/search commands, core state | security        |
| browser-network  | request pipeline, layers                | security, privacy, Brave adblock |
| browser-privacy  | tracker engine, cookies, fingerprinting | —                   |
| browser-security | scheme/navigation/download filesystem boundary | windows-sys (Windows) |

The four policy crates are rendering-engine-free and unit-tested without Servo.
Most policy is pure URL/state logic; `browser-security` additionally owns
the small platform filesystem boundary for safe download creation. The
binary is the only place where Servo's types appear.

## Why these decisions

- **Servo crate, not the servo repo**: the published `servo` crate is
  the supported embeddable API; building the repo directly means
  maintaining a custom build.
- **offscreen rendering**: Servo renders into an FBO and egui blits it.
  This is the servoshell approach and keeps all compositing in one GL
  context.
- **RefCell/queue bridge**: servoshell uses a command queue; we keep the
  same idea minimal: a small `UiEvent` enum.
