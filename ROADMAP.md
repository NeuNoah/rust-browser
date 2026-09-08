# Roadmap

Phases are ordered by dependency, not by importance. Each phase ends
with something visible or verifiable. "Done" means the feature works
and is covered by automated tests where it is engine-independent, or
by a documented manual smoke test when it requires Servo and a window.

## Phase 1 — Foundation

- [x] Workspace with five crates; policy crates engine-free and tested
- [x] Window with Servo `WebView` blitted into egui (servoshell pattern)
- [x] Toolbar: URL bar, back/forward/reload, title in window title
- [x] Mouse input (move, click, wheel) to the WebView
- [x] Request pipeline wired into the HTTP(S) `load_web_resource`
      interception boundary (tracker blocking live in the running browser)
- [x] First successful end-to-end run: page loads, click focuses an
      input field, typing inserts text (smoke-tested against a local
      test page at 2.25x and 1.0125x device scale)
- [x] Docs (this repository's markdown set), `tools/check.ps1` (quality
      gate: fmt, build, tests, clippy, verified green)
- [x] First commit to the local repository (`2e545a6`)

## Phase 2 — Session basics

- [x] Window resize correctness across monitors and DPI changes
      (window-relative coordinate fix verified on both monitor scales)
- [x] Keyboard input to the page (Servo `InputEvent::Keyboard`), focus
      management between URL bar and page (focus surrender + page focus
      verified end to end in the smoke test)
- [x] Copy/paste basics; system clipboard delegate (own
      `SystemClipboard` delegate via arboard, without an in-process
      fallback; matching keyboard/context-menu user actions create a
      short-lived one-shot read or write grant scoped to one WebView)
- [x] Scrollbar and cursor feedback from the page (scrollbars are
      rendered by Servo; cursor-icon callbacks wired to the window)

## Phase 3 — Tabs

- [x] Tab bar in egui; multiple `WebView`s, one active
      (all WebViews share one offscreen `RenderingContext`, the
      servoshell model; the active tab is painted and blitted into
      the egui scene, inactive tabs are hidden via `show`/`hide`)
- [x] Tab lifecycle: open, close, switch; history per tab
      (Ctrl+T/W/Tab/Shift+Tab; closing the last tab opens a fresh
      tab at the configured start page, `about:blank` by default;
      per-tab `(can_go_back, can_go_forward)` tracked)
- [x] Keyboard shortcuts (Ctrl+T/W/Tab)
- [x] Crash resilience: a dead WebView doesn't take down the app
      (`notify_closed`/`notify_crashed` events close the tab; the
      window survives, `WebViewDelegate` never panics)

## Phase 4 — Input & UX polish (current)

- [ ] Complete everyday keyboard coverage in the URL bar and page
      (direct Unicode input, focus arbitration and shortcuts are wired;
      dead-key, AltGr, repeat and composition flags now have isolated
      translation regression tests and visible smoke-page diagnostics;
      the real keyboard-layout paths still need manual coverage)
- [ ] System IME in the URL bar and page (composition translation and
      commit lifecycle have unit tests; page-relative candidate geometry
      is regression-tested at 1x, 1.0125x and 2.25x; real preedit,
      candidate placement and commit at those scales remain to be tested)
- [x] Page context menus
- [x] Servo form-control UI for grouped single-select, multiple-select
      and color input, including cancel/apply behavior
- [x] Modal page dialogs for alert, confirm and prompt, with page
      identity, persistent prompt text and blocked background input
- [x] Page zoom with Ctrl+Plus/Minus/0 on the active tab
- [x] Page-created WebViews (`target=_blank`) open as browser tabs only
      after a recent user action; one short-lived parent-WebView grant
      authorizes one popup
- [x] Native single/multiple file-picker UI with active-tab user-action
      gating, parent-window modality and sanitized/bounded accept filters
- [x] Reader-mode feasibility decision — GO for a local, text-first
      native egui view using Servo's asynchronous JavaScript evaluator;
      no original-DOM rewrite and no privacy/script-blocking claim
- [x] Implement the bounded per-tab Reader view, including stale-result
      rejection after navigation and focus/throttle restoration
- [x] Settings window with a validated, session-only start page
- [x] Per-site tracker override end to end (normalization, shared state,
      trusted WebView-main-page selection and UI are covered; the live
      Servo smoke test confirms default blocking, a normalized
      site-only exception and continued blocking on another site)
- [x] Proxy lifecycle decision — Servo 0.5 fixes HTTP(S) proxy and
      bypass settings at startup; runtime changes cannot rebuild its
      connector reliably
- [x] Add explicit fail-closed startup proxy configuration: validated
      `--proxy`/`--proxy-bypass`, no ambient environment inheritance,
      tested non-default HTTP routing and bypass, and no direct fallback
      when the proxy is unavailable
- [ ] Complete browser-wide proxy support: Servo 0.5 tunnels default-port
      HTTP to port 443, so the embedder now blocks that case before the
      network; a successful HTTPS CONNECT path and WebSocket proxying
      still need engine support and integration coverage

Manual verification through 2026-09-01: Windows 11 25H2 build
26200.9168, 100% scale (96 DPI), using the local HTTP fixtures in
`tools/smoke/`. The final 43-file source/test fingerprint is
`718145525c103dbe69a2bc0d9534c0694a9c74f9026ed0902b91a63ca1492e52`.
The earlier broad Phase 4 pass covered navigation, Unicode input,
URL/page focus, tab and modal shortcuts, context/select/color controls,
dialogs, zoom, `target=_blank`, unsafe-URL rejection and Settings.
Targeted rebuilt-executable passes then covered native single/multiple
file selection and cancel, bounded filters, delayed-script rejection,
and the complete Reader path: bounded text extraction, Unicode/font
fallback, preformatted text, wheel and
Home/End keyboard scrolling, covered-page input blocking, focus and
throttle restoration, per-tab state, stale-result handling and the
five-second timeout followed by a successful retry. A fail-closed local
proxy then verified that tracker traffic is blocked by default, allowed
for the normalized `SITE.TEST.` exception and still blocked on
`other.test`. The explicit startup proxy then passed validation, ambient-
environment isolation, non-default HTTP routing, host bypass and an
outage test with no direct fallback. HTTPS reached the CONNECT/TLS
boundary; default-port HTTP was found to be misrouted by Servo and is
now blocked locally, while WebSocket traffic did not reach the proxy.
Skipped: dead keys, AltGr, key repeat, an actual system IME and candidate
UI, non-integer/high DPI, a live system-clipboard flow, successful local
HTTPS proxy termination and the sub-two-second chrome-focus picker race
(the focus-epoch revocation remains covered by unit tests). The
fingerprint scope and reproducible algorithm are recorded in
`tools/smoke/README.md`.

## Phase 5 — Downloads

- [ ] Download flow end to end: intercept, sanitize, save with
  `DownloadPolicy`, progress UI, dangerous-type warning
- [x] Safe Windows final-file creation: the ordinary download directory
  is resolved and pinned without delete sharing, redirected/reparse
  paths are rejected, incomplete files stay private, final publication
  never overwrites, and junction/directory-swap regressions are tested
  (28 `browser-security` tests pass)

## Phase 6 — Search

- [x] Session-only search engine selection with no provider by default;
  DuckDuckGo or Brave Search must be chosen explicitly. Only submitted,
  bounded queries are forwarded, malformed explicit URLs stay local,
  URL-only settings never become searches, and suggestions remain off
  (live UI verified through provider selection; applying the privacy
  setting is covered by tests because Windows UI automation may not
  mutate in-app privacy settings)

## Phase 7 — Adblock

- [x] Brave `adblock` 0.13.3 network-rule engine behind the existing
      pipeline, compiled once from a deterministic embedded seed list;
      top-level navigation stays usable and tracker overrides cannot
      bypass the adblock layer. Full quality gate passed on 2026-09-02:
      122 tests, workspace build, formatting and all-target Clippy. The
      44-file automated source/test fingerprint is
      `1acca381658144f2daf3a59a2e1cabc5c9bc4e1321c5c85f497ff655a06ed0fc`
- [ ] User-selected subscription list catalog and bounded, atomic update UX
- [x] Per-site blocking stats in the toolbar: session-only tracker/ad
      counts are keyed by the trusted committed host, exclude unrelated
      security-policy blocks and global unattributed requests, and retain
      at most 256 sites

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
- Post-resolution private-network checks to cover DNS rebinding beyond
  the current URL-level literal-host guard

## Phase 11 — Performance work

- Profiling, dirty-rect rendering, egui optimizations

## Phase 12 — Multi-process hardening

- Investigate Servo's process model for the embedder; crash isolation

## Phase 13 — Extensions

- Minimal extension model (no APIs that leak data by default)

## Phase 14 — Localization & accessibility

- i18n of the UI; AccessKit integration for both browser chrome and the
  embedded WebView accessibility tree

## Phase 15 — Benchmarks & release readiness

- The benchmark plan in PERFORMANCE.md; release profile tuning
- Packaging (installer on Windows), update mechanism (signed)

### Not planned (explicitly)

- Chromium/Firefox engines, cloud sync, built-in AI, advertising APIs,
  telemetry, remote feature flags.
