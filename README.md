# Rust Browser

[![Policy CI](https://github.com/NeuNoah/rust-browser/actions/workflows/policy-ci.yml/badge.svg)](https://github.com/NeuNoah/rust-browser/actions/workflows/policy-ci.yml)
[![CodeQL](https://github.com/NeuNoah/rust-browser/actions/workflows/codeql.yml/badge.svg)](https://github.com/NeuNoah/rust-browser/actions/workflows/codeql.yml)

An experimental, privacy-oriented web browser written in Rust. It uses
[Servo](https://servo.org/) as the rendering engine and winit+egui for
the interface. Performance and memory use have not been benchmarked yet,
so no lightweight claim is made at this stage.

> **Experimental software:** the embedded renderer is not sandboxed and
> the browser is not suitable for sensitive or everyday browsing yet.
> See [SECURITY.md](SECURITY.md) for implemented defenses and known gaps.

## Status

Phase 1 (foundation), Phase 2 (session basics) and Phase 3 (tabs) are
complete. Phase 4 (input and UX polish) is in progress: context menus,
select/color controls, native file selection, page dialogs, zoom,
Reader view and session settings are implemented and smoke-tested at
100% Windows scale. Per-site tracker overrides have also passed a live,
fail-closed local network test. System-IME and high-DPI coverage are
still open. The download filesystem boundary is implemented and tested,
but Servo/UI integration is not. Phase 6 search is complete with no
provider selected by default. Phase 7 now has Brave's network adblock
engine in the live request pipeline with a small embedded seed list;
the toolbar reports bounded per-site blocking counts, while subscription
updates are still open. The browser
is not yet suitable as a daily browser (no usable downloads or persistent
history yet).

## Features today

- Multiple tabs with a tab bar; each tab is a Servo `WebView`
  (all share one offscreen rendering context); closing the last tab
  opens the configured start page (`about:blank` by default)
- Tab shortcuts: Ctrl+T (new tab), Ctrl+W (close), Ctrl+Tab /
  Ctrl+Shift+Tab (cycle); back/forward state is tracked per tab
- Crash resilience: a dead WebView closes its tab, the app survives
- URL bar with HTTPS-first completion (`example.com` → `https://example.com/`)
- Back / forward / reload, page title in the window title
- Mouse input (move, click, wheel) and keyboard input to the page,
  with focus handover between URL bar and page
- Copy / paste in page text fields via a system clipboard delegate
  (arboard); each read or write needs a recent matching keyboard or
  context-menu action and the one-shot grant is scoped to one WebView
- Page context menus; grouped single-select, multiple-select and color
  controls; native single/multiple file selection; modal
  alert/confirm/prompt dialogs
- A bounded, text-only Reader view per tab. Extraction runs locally in
  the loaded document, performs no second fetch, accepts no page HTML,
  rejects stale navigation results and restores page focus/throttling
  when closed
- Per-tab page zoom with Ctrl+Plus/Minus/0; page-created WebViews open
  as normal browser tabs only after a recent user action, using a
  short-lived one-shot popup grant. Unlike opener-free WebViews created
  by browser UI, they never receive first-document top-level trust
- Session-only settings for a validated start page and normalized
  per-site tracker overrides. A repeatable startup-only override can be
  supplied with `--allow-trackers-for <host>`
- Session-only search selection: disabled by default, with DuckDuckGo and
  Brave Search available only after explicit selection. Queries are sent
  only on Enter, limited to 512 characters and percent-encoded. Search
  suggestions and keystroke requests are not implemented. Prefix `?` to
  deliberately search for a single word
- Validated startup-only HTTP proxy selection with `--proxy <http-url>`
  and an optional bounded `--proxy-bypass <host-list>`. Ambient proxy
  variables are ignored and an unavailable configured proxy never falls
  back to a direct connection. Default-port HTTP and WebSocket proxying
  remain unsupported; runtime proxy changes are not available
- A Windows download writer that keeps the destination directory pinned,
  writes incomplete data through an exclusive temporary file and
  publishes it without overwriting an existing file or following a
  reparse point. It is a tested safety foundation; no download UI is
  exposed yet
- WebView HTTP(S) requests exposed by Servo's resource-interception
  callbacks, plus global loads not rejected up front, pass through a
  privacy pipeline. A global plain-HTTP load without an explicit HTTP
  initiator (absent, opaque or secure) is rejected fail-closed first:
  - HTTP(S) URL validation, including rejection of embedded credentials;
    a separate navigation boundary denies `javascript:`, `data:`,
    `blob:` and `file:` navigations and allows only `about:blank` from
    the `about:` family
  - URL-level blocking of explicit loopback/private-network targets
    requested by a public page (DNS rebinding is not covered)
  - Mixed-content blocking (no plain-HTTP subresources on HTTPS pages)
  - Built-in tracker host list for third-party requests (blocked as empty
    responses; exact same-host first-party requests remain usable)
  - Brave's ABP-compatible network-filter engine with a small, local seed
    list. It performs no list download or background update yet; a tracker
    override does not bypass this later pipeline layer. The toolbar shows
    session-only per-site tracker/ad counts without retaining request URLs
- No telemetry, accounts, sync or remote configuration

## Building

Prerequisites: a recent stable Rust (1.97+), a C/C++ toolchain
(Visual Studio Build Tools on Windows), LLVM (libclang for bindgen) and
Python 3 on the PATH.

```sh
cargo test --workspace   # unit tests across the workspace
cargo run -p browser     # the browser itself
```

An explicit startup proxy can be selected without modifying system or
shell settings:

```sh
cargo run -p browser -- --proxy http://127.0.0.1:8765 \
  --proxy-bypass localhost,127.0.0.1 https://example.com/
```

Only credential-free `http://` proxy endpoints are accepted. Servo 0.5
currently misroutes a proxied default-port `http://` destination to port
443, so the embedder blocks that case before networking. Use of this
experimental option is therefore limited to HTTP URLs with an explicit
non-default port and HTTPS destinations whose trusted proxy path has
been verified by the operator. The local test proves HTTPS CONNECT/TLS
routing, not a successful trusted HTTPS session. WebSockets are not
proxied.

On Windows, run the one-command quality gate instead — it prepares the
LLVM/Python environment and runs fmt, build, tests and clippy:

```sh
powershell -ExecutionPolicy Bypass -File tools/check.ps1
```

The first build takes a long time (Servo is ~900 crates) — expect
30–60 minutes on an average machine. Use `-j` to limit parallelism if
your machine has little RAM.

## Repository layout

```
crates/
  browser/          binary: winit event loop, egui GUI, Servo glue
  browser-core/     tab model and navigation orchestration (engine-free)
  browser-network/  the request pipeline (layers, decisions)
  browser-privacy/  tracker engine, cookie policy, fingerprinting config
  browser-security/ URL/scheme policy, download policy
resources/
  filterlists/      built-in tracker and ad-protection seed lists
```

The policy crates (`browser-*` except `browser`) are pure Rust with no
dependencies on the engine, so they build and test in seconds.

## Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md) — how the pieces fit together
- [SECURITY.md](SECURITY.md) — threat model and security decisions
- [PRIVACY.md](PRIVACY.md) — privacy features and data flow
- [PERFORMANCE.md](PERFORMANCE.md) — performance goals and benchmarks
- [ROADMAP.md](ROADMAP.md) — the phase plan
- [DEPENDENCIES.md](DEPENDENCIES.md) — every dependency, why and its license
- [CONTRIBUTING.md](CONTRIBUTING.md) — how to work on this project
- [tools/smoke/README.md](tools/smoke/README.md) — manual GUI checks

## Security reports

Please do not disclose vulnerabilities in a public issue. Use GitHub's
[private vulnerability reporting](https://github.com/NeuNoah/rust-browser/security/advisories/new)
so details can be assessed before public disclosure.

## License

MPL-2.0, matching Servo. See [LICENSE](LICENSE).
