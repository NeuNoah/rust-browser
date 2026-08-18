# Performance

Goals, current state, and the benchmark plan.

## Goal

An ultra-lightweight browser: fast to start, low memory, smooth
scrolling, minimal battery drain. The reference point is Servo's own
`servoshell`; we aim to be comparable or better for simple browsing
workloads.

## Current state (Phase 1)

- The policy crates are pure functions: zero allocation concerns, no
  I/O in the request path except the pipeline evaluation itself.
- The pipeline short-circuits on the first block; typical evaluation is
  a handful of string comparisons per request.
- The built-in tracker list is embedded at compile time
  (`include_str!`) and loaded once; matching is exact/prefix-based,
  no regex at runtime.
- GUI cost is egui's: one `PaintCallback` (GL blit) per frame.

## Deliberate non-goals for now

- `profile.release` stays conservative (`opt-level = 3` only). LTO,
  `codegen-units` and `panic=abort` are benchmarked in Phase 15 before
  being enabled; nothing is tuned blind.
- No parallelism beyond what Servo already does.

## Benchmark plan (Phase 15)

Each benchmark is a committed, runnable script under `tools/bench/`
plus a documented result in this file. Runs happen on the reference
machine (Windows 10 Pro, Ryzen 5 PRO 5650GE, 6C/12T, 8 GB RAM).

1. **Startup** — wall-clock from process start to first painted frame.
2. **Memory** — peak working set after loading a set of representative
   pages (Wikipedia, a news site, a JS-heavy site).
3. **Scroll smoothness** — frame-time distribution while scrolling a
   long page.
4. **Pipeline throughput** — requests/sec through `RequestPipeline`
   with a realistic URL mix (hit/miss tracker ratio).
5. **Tracker block latency** — added latency per request from pipeline
   evaluation (should be sub-microsecond).

Results are recorded in `PERFORMANCE.md` with the date, machine, and
build profile so trends are visible.

## Known cost centers

- Servo's first paint after navigation (engine-side; we inherit it).
- egui repaints requested by the URL-bar caret: the whole frame
  repaints, including the WebView blit. A dirty-rect optimization is
  possible later.
- The `-j` build setting on 8 GB machines (documented in the README).