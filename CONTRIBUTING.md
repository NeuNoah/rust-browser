# Contributing

This is a local, single-developer project — but written as if others
will read every line. Rules:

## Workflow

1. Check the roadmap. Phase boundaries are real: don't start Phase 3
   code before Phase 2 is done.
2. Run the quality gate before finishing any change:

   ```sh
   powershell -ExecutionPolicy Bypass -File tools/check.ps1
   ```

   (fmt, `cargo check`, all tests, clippy — no warnings tolerated).

3. Commit small, message like the existing history
   (`phase: what changed`). Never commit secrets or build artifacts.

## Code rules

- **Policy crates are pure.** Security/privacy decisions live in
  `browser-security`/`browser-privacy`/`browser-network`/`browser-core`,
  never in the GUI. They must not depend on Servo.
- **No `unwrap()`/`expect()` in production paths** with user-controlled
  input. `expect` is allowed only for invariants with a clear message
  (e.g. "GL context must be current").
- **No fake security.** A feature is documented as implemented only if
  it is implemented and tested. Unimplemented things are listed as
  unimplemented (see SECURITY.md, PRIVACY.md).
- **Threading.** Delegate callbacks run on Servo threads. They may only
  set cells, push to the UI queue, or request redraws — never borrow
  `RefCell`s that the main thread also uses.
- **No invented APIs.** Every Servo API used must be verified against
  the vendored source or docs; note the source in the commit message
  when it wasn't obvious.
- **No comments that restate the code.** Comments explain *why*.

## Testing

- Every deny path in the policy crates needs a test.
- `cargo test --workspace` must stay green and fast (currently < 5 s
  for the policy crates; keep it that way).
- The binary itself is manually smoke-tested (it needs a GPU/window).

## Build notes (Windows)

- Rust 1.97+ with the MSVC toolchain.
- Visual Studio 2026 Community provides the C toolchain and CMake; the
  CMake binary lives at `...\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin`
  (needed by `aws-lc-rs` if not on PATH).
- The first Servo build takes 30–60 minutes on ~8 GB machines; prefer
  `cargo check -p browser -j 6` for iteration.