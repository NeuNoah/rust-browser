# Performance

Goals, current state, and the benchmark plan.

## Goal (not yet measured)

The intended direction is fast startup, modest memory use, smooth
scrolling and low idle CPU use. No benchmark results exist yet, so the
project does not currently claim to be lightweight or comparable to
Servo's `servoshell`.

## Current state (through the Phase 4 work in progress)

- The policy crates perform no filesystem or network I/O during pipeline
  evaluation. They do perform normal URL/string work; throughput and
  allocation cost have not been measured.
- The pipeline short-circuits on the first block; typical evaluation is
  a handful of string comparisons per request.
- The built-in tracker list is embedded at compile time
  (`include_str!`) and loaded once; matching is exact/prefix-based,
  no regex at runtime.
- GUI cost is egui's: one `PaintCallback` (GL blit) per frame.
- The event loop polls continuously only while the active tab is
  animating. Animating hidden tabs do not force busy polling; scheduled
  egui repaints use `WaitUntil`, otherwise the loop waits.
- Servo's temporary client/cache storage can touch disk even though the
  embedder has no persistent-history feature; its startup and I/O cost
  has not been measured.

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
   evaluation; establish a target only after a baseline measurement.

Results are recorded in `PERFORMANCE.md` with the date, machine, and
build profile so trends are visible.

## Known cost centers

- Servo's first paint after navigation (engine-side; we inherit it).
- egui repaints requested by the URL-bar caret: the whole frame
  repaints, including the WebView blit. A dirty-rect optimization is
  possible later.
- The `-j` build setting on 8 GB machines (documented in the README).
