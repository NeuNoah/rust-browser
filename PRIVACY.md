# Privacy

The browser's reason for existing. This document is written from the
principle: **state in the code must match reality**, so features that
are planned but not implemented are listed as such.

## Privacy properties (implemented)

- **No telemetry.** The binary performs no network communication except
  what the user's pages request.
- **No accounts, no sync, no remote configuration.**
- **Tracker blocking by default.** `browser-privacy` ships a curated
  host list (`resources/filterlists/trackers.txt`), compiled into the
  binary. Requests to those hosts are blocked as empty responses.
  Rules:
  - `host:example.com` blocks the host and its subdomains;
  - `pattern:...` matches a URL substring;
  - top-level navigations to a tracker's own site are *allowed*
    (the user may visit the site directly).
- **No third-party cookies.** `CookiePolicy` defaults to rejecting
  third-party cookies, requiring `Secure` on HTTPS origins and applying
  `SameSite=Lax` when the attribute is absent. (Enforcement at the
  network layer is pending — see below.)
- **Fingerprint reduction defaults.** `FingerprintingConfig` fixes the
  user agent string to a stable value and blocks client hints. No
  randomization: randomizing the fingerprint would break sites and is
  easily detected; the goal is a consistent, unremarkable profile.
- **HTTPS-first input.** Typing `example.com` never produces a plaintext
  first load; bare hosts always complete to `https://`.
- **Mixed-content blocking** (HTTP subresources on HTTPS pages).

## Data the browser itself stores

- Nothing persistent. No history file, no cookie store, no cache
  directory is created by the embedder (Servo keeps its memory-based
  stores; persistence is opt-in in a later phase).

## Data flow: what a page load touches

1. URL bar input → validated locally (no network).
2. Request → `RequestPipeline` (scheme, tracker, mixed content) → Servo
   network stack (rustls/TLS) → the site.
3. Tracker requests are intercepted and answered empty *before* any
   connection is attempted: the tracker host is never contacted.
4. The page's scripts run locally in Servo.

## Planned (not yet implemented — do not claim otherwise)

- **Cookie enforcement at the network layer.** `CookiePolicy` exists
  with tests; wiring it into Servo's cookie handling is a later phase.
- **Cookie store / site data management** (Servo exposes
  `SiteDataManager` for this).
- **Full adblock lists** (Brave's `adblock` crate, Phase 7), beyond the
  built-in tracker list.
- **Canvas/WebGL fingerprinting defense** and canvas-read-back
  blocking.
- **Referrer trimming** (default referrer policy) at the embedder level.
- **NoScript-style per-site script toggles.**
- **Container-style site isolation** of storage (per-site profiles).
- **Local search integration** (Phase 6) that forwards queries only to a
  user-chosen engine, never by default.

## Design rules

1. Privacy decisions live in `browser-privacy`, are pure and tested.
2. Defaults are privacy-preserving; anything that reduces privacy must
   be user-visible and opt-in.
3. If a privacy feature cannot be verified to work, it is not claimed
   as working.