# Dependencies

Direct dependencies of the workspace, with license and why they are
here. The transitive set is ~900 crates (almost entirely Servo's own
dependencies); the audit focus is on direct, non-Servo additions.
`cargo deny` (license + advisory checking) is still planned before
release readiness; it is not part of the current quality gate.

## Workspace root (`Cargo.toml`, `[workspace.dependencies]`)

| Crate | Version | License | Why |
|---|---|---|---|
| servo | 0.5.0 | MPL-2.0 | The rendering engine. The only engine considered: Chromium/Firefox embedding is too heavy, WebKitGTK is not Rust and not viable on Windows. |
| winit | 0.30 | Apache-2.0 | Windowing, event loop. The foundation servoshell uses. |
| egui | 0.36 | MIT/Apache-2.0 | Immediate-mode GUI for the toolbar/UI. |
| egui_glow | 0.36 | MIT/Apache-2.0 | GL backend for egui; `winit` feature enables `EguiGlow` integration. |
| egui-winit | 0.36 | MIT/Apache-2.0 | egui↔winit event plumbing. |
| rustls | 0.23 | Apache-2.0/ISC | TLS (with `aws-lc-rs` provider; installed once at startup). |
| url | 2.5 | MIT/Apache-2.0 | URL parsing everywhere; also Servo's own URL type. |
| ureq | 3.4 | MIT/Apache-2.0 | Blocking HTTPS client used only on a worker thread for explicit, bounded filter-subscription updates. Default features are disabled; it uses the process rustls provider and bundled WebPKI roots without ambient proxy discovery or redirects. |
| thiserror | 2 | MIT/Apache-2.0 | Error enums in the policy crates. |
| log | 0.4 | MIT/Apache-2.0 | Logging facade used by the embedder. Servo supplies the logger implementation. |
| adblock | 0.13.3 | MPL-2.0 | Brave's ABP-compatible network-filter engine. Default features are disabled to exclude its non-`Sync` single-thread mode; only embedded domain resolution and full regex handling are enabled. |

## Crate-local additions

| Crate | License | Why |
|---|---|---|
| browser (bin) | — | adds the workspace crates plus the native integrations below. |
| arboard 3 | MIT/Apache-2.0 | Native system clipboard access; the embedder adds per-WebView user-action grants and stores no fallback text. |
| euclid 0.22 | MPL-2.0 | Geometry types matching Servo's (`Scale`, `Size2D`, `Rect`) for the blit. |
| keyboard-types 0.8.3 | MIT/Apache-2.0 | W3C-compatible keyboard and IME event values forwarded to Servo. |
| rfd 0.17 | MIT | Window-parented native single/multiple file-open dialogs. Page-provided filters are sanitized and bounded before use. |
| ureq 3.4 | MIT/Apache-2.0 | Fixed-catalog EasyList/EasyPrivacy fetches; HTTPS and proxy handling plus zero redirects, timeout, header and body limits are configured explicitly. |
| browser-core | — | adds `browser-security` (path dep). |
| browser-network | — | adds `browser-security`, `browser-privacy` (path deps) and Brave `adblock`. |
| browser-privacy | — | none beyond `url`/`thiserror`. |
| browser-security | — | `url`, `thiserror`; `windows-sys` 0.61 (MIT/Apache-2.0) on Windows for reparse/open/share flag constants used by the safe download writer. |

## Not direct dependencies

`surfman`, `dpi`, `image` and `env_logger` occur in the locked
transitive graph through Servo/winit and related crates, but are not
declared as direct workspace dependencies. They must not be presented as
embedder features merely because they are present transitively.

## License notes

- Servo + its Mozilla crates: MPL-2.0. Our policy crates and docs are
  MPL-2.0 to match.
- egui family: MIT/Apache-2.0 (permissive).
- rustls + aws-lc: Apache-2.0/ISC (aws-lc-rs is Apache-2.0/ISC with
  BoringSSL-derived code; BoringSSL is OpenSSL-derived, all permissive).

## Deferred dependencies (not added yet — deliberately)

| Crate | Why deferred |
|---|---|
| rusqlite | Phase 8 (history store); decide on `sqlite` vs `redb` then. |
| dirs / directories | Phase 8 (session restore paths). |
| winreg | Phase 14 (Windows packaging/installer). |
