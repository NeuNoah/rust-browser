# Roadmap

Phases are ordered by dependency, not by importance. Each phase ends
with something visible or verifiable. "Done" means the feature works
and is covered by tests.

## Phase 1 — Foundation (current)

- [x] Workspace with five crates; policy crates engine-free and tested
- [x] Window with Servo `WebView` blitted into egui (servoshell pattern)
- [x] Toolbar: URL bar, back/forward/reload, title in window title
- [x] Mouse input (move, click, wheel) to the WebView
- [x] Request pipeline wired into `load_web_resource` (tracker blocking
      live in the running browser)
- [x] First successful end-to-end run: page loads, click focuses an
      input field, typing inserts text (smoke-tested against a local
      test page at 2.25x and 1.0125x device scale)
- [x] Docs (this repository's markdown set), `tools/check.ps1` (quality
      gate: fmt, build, tests, clippy, verified green)
- [ ] First commit to the local repository

## Phase 2 — Session basics

- [x] Window resize correctness across monitors and DPI changes
      (window-relative coordinate fix verified on both monitor scales)
- [x] Keyboard input to the page (Servo `InputEvent::Keyboard`), focus
      management between URL bar and page (focus surrender + page focus
      verified end to end in the smoke test)
- [x] Copy/paste basics; system clipboard delegate (own
      `SystemClipboard` delegate via arboard with in-process fallback;
      Ctrl+C writes to the system clipboard and Ctrl+V requests it,
      verified end to end; modifier state is tracked from the key
      events themselves because the winit Windows backend reads
      `GetKeyState`, which synthesized input never updates)
- [x] Scrollbar and cursor feedback from the page (scrollbars are
      rendered by Servo; cursor-icon callbacks wired to the window)

## Phase 3 — Tabs

- Tab bar in egui; multiple `WebView`s, one active
- Tab lifecycle: open, close, switch; history per tab
- Keyboard shortcuts (Ctrl+T/W/Tab)
- Crash resilience: a dead WebView doesn't take down the app

## Phase 4 — Input & UX polish

- Full keyboard handling incl. IME (URL bar and page)
- Context menus, form element UI (Servo dialogs)
- Page zoom (Ctrl+Plus/Minus/0), reader mode candidate
- Settings window (start page, proxy, per-site toggles)

## Phase 5 — Downloads

- Download flow end to end: intercept, sanitize, save with
  `DownloadPolicy`, progress UI, dangerous-type warning

## Phase 6 — Search

- Search engine selection (user-chosen, default none or user choice),
  query forwarding; suggestions off by default

## Phase 7 — Adblock

- Brave `adblock` crate integration behind the existing pipeline;
  subscription lists, update UX
- Per-site blocking stats in the UI

## Phase 8 — Session & history

- Session restore, history store (local, encrypted at rest)
- Address-bar suggestions from history (on-device only)

## Phase 9 — Site data

- Cookie enforcement at the network layer via `CookiePolicy`;
  site-data management UI (Servo `SiteDataManager`)

## Phase 10 — Privacy hardening

- Referrer trimming, canvas-readback blocking, permission prompts
  (camera/mic/geolocation deny-by-default)
- Fingerprinting configuration UI

## Phase 11 — Performance work

- Profiling, dirty-rect rendering, egui optimizations

## Phase 12 — Multi-process hardening

- Investigate Servo's process model for the embedder; crash isolation

## Phase 13 — Extensions

- Minimal extension model (no APIs that leak data by default)

## Phase 14 — Localization & accessibility

- i18n of the UI; accesskit integration for the toolbar

## Phase 15 — Benchmarks & release readiness

- The benchmark plan in PERFORMANCE.md; release profile tuning
- Packaging (installer on Windows), update mechanism (signed)

### Not planned (explicitly)

- Chromium/Firefox engines, cloud sync, built-in AI, advertising APIs,
  telemetry, remote feature flags.