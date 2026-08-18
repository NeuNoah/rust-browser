# Dependencies

Direct dependencies of the workspace, with license and why they are
here. The transitive set is ~900 crates (almost entirely Servo's own
dependencies); the audit focus is on direct, non-Servo additions.
`cargo deny` (license + advisory checking) is added in Phase 2.

## Workspace root (`Cargo.toml`, `[workspace.dependencies]`)

| Crate | Version | License | Why |
|---|---|---|---|
| servo | 0.5.0 | MPL-2.0 | The rendering engine. The only engine considered: Chromium/Firefox embedding is too heavy, WebKitGTK is not Rust and not viable on Windows. |
| winit | 0.30 | Apache-2.0 | Windowing, event loop. The foundation servoshell uses. |
| egui | 0.31 | MIT/Apache-2.0 | Immediate-mode GUI for the toolbar/UI. |
| egui_glow | 0.31 | MIT/Apache-2.0 | GL backend for egui; `winit` feature enables `EguiGlow` integration. |
| egui-winit | 0.31 | MIT/Apache-2.0 | egui↔winit event plumbing. |
| surfman | 0.13 | MPL-2.0 | Surfman provides the GL context Servo renders into (feature `sm-raw-window-handle-06` for winit 0.30 compatibility). |
| rustls | 0.23 | Apache-2.0/ISC | TLS (with `aws-lc-rs` provider; installed once at startup). |
| url | 2.5 | MIT/Apache-2.0 | URL parsing everywhere; also Servo's own URL type. |
| dpi | 0.1.2 | MIT/Apache-2.0 | Physical/logical size types (re-exported by winit). |
| euclid | 0.22 | MPL-2.0 | Geometry types matching Servo's (Scale, Size2D, Rect for the blit). |
| thiserror | 2 | MIT/Apache-2.0 | Error enums in the policy crates. |
| log + env_logger | 0.4 / 0.11 | MIT/Apache-2.0 | Logging (env_logger also a Servo dev-dependency). |
| image | 0.25 | MIT/Apache-2.0 | Favicon/screenshot work (future phases). |

## Crate-local additions

| Crate | License | Why |
|---|---|---|
| browser (bin) | — | adds only workspace crates plus the above. |
| browser-core | — | adds `browser-security` (path dep). |
| browser-network | — | adds `browser-security`, `browser-privacy` (path deps). |
| browser-privacy | — | none beyond `url`/`thiserror`. |
| browser-security | — | `url`, `thiserror`. |

## License notes

- Servo + its Mozilla crates: MPL-2.0. Our policy crates and docs are
  MPL-2.0 to match.
- egui family: MIT/Apache-2.0 (permissive).
- rustls + aws-lc: Apache-2.0/ISC (aws-lc-rs is Apache-2.0/ISC with
  BoringSSL-derived code; BoringSSL is OpenSSL-derived, all permissive).

## Deferred dependencies (not added yet — deliberately)

| Crate | Why deferred |
|---|---|
| adblock (Brave) | Phase 7; its dependency tree (CSS engine re-mapping) is large. |
| rusqlite | Phase 8 (history store); decide on `sqlite` vs `redb` then. |
| dirs / directories | Phase 8 (session restore paths). |
| winreg | Phase 14 (Windows packaging/installer). |