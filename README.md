# Rust Browser

A privacy-first, ultra-lightweight web browser written in 100% Rust. It
uses [Servo](https://servo.org/) as the rendering engine and winit+egui
for the interface.

## Status

Phase 1 (foundation) and Phase 2 (session basics) are complete: the
first window exists — a Servo `WebView` blitted into an egui scene,
with a toolbar, URL bar, navigation buttons, mouse and keyboard
interaction, tracker blocking at the request layer, and copy/paste via
a system clipboard delegate. Not yet usable as a daily browser (no
tabs, downloads or history yet).

## Features today

- Single window, single tab, Servo engine (`servo` crate 0.5.0)
- URL bar with HTTPS-first completion (`example.com` → `https://example.com/`)
- Back / forward / reload, page title in the window title
- Mouse input (move, click, wheel) and keyboard input to the page,
  with focus handover between URL bar and page
- Copy / paste in page text fields via a system clipboard delegate
  (arboard, in-process fallback when the clipboard is unavailable)
- Every network request passes through a privacy pipeline:
  - URL validation and scheme policy (no `javascript:`, `data:`,
    `blob:`, `file:` subresources, no embedded credentials)
  - Built-in tracker host list (blocked as empty responses)
  - Mixed-content blocking (no plain-HTTP subresources on HTTPS pages)
- No telemetry, no accounts, no remote anything

## Building

Prerequisites: a recent stable Rust (1.97+), a C/C++ toolchain
(Visual Studio Build Tools on Windows), LLVM (libclang for bindgen) and
Python 3 on the PATH.

```sh
cargo test --workspace   # unit tests of the policy crates
cargo run -p browser     # the browser itself
```

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
  filterlists/      the built-in tracker host list
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

## License

MPL-2.0, matching Servo. The license text is added in Phase 3.