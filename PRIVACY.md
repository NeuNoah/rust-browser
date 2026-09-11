# Privacy

The browser's reason for existing. This document is written from the
principle: **state in the code must match reality**, so features that
are planned but not implemented are listed as such.

## Privacy properties (implemented)

- **No telemetry.** The binary performs no network communication except
  what the user's pages request and filter-list downloads the user
  explicitly starts in Settings.
- **No accounts, no sync, no automatic remote configuration.**
- **Third-party tracker blocking by default.** `browser-privacy` ships a
  curated host list (`resources/filterlists/trackers.txt`), compiled
  into the binary. Matching third-party requests are blocked as empty
  responses.
  Rules:
  - `host:example.com` blocks the host and its subdomains;
  - `pattern:...` matches a URL substring;
  - an explicit, in-memory per-site override can allow tracker requests
    for one WebView main-page host. The host comes from the embedder's
    committed WebView URL, not referrer metadata. Overrides never apply
    to global/unassociated loads. Host scoping is covered by unit tests
    and by a fail-closed local Servo-network smoke test;
  - both host and pattern matches are allowed when the request host
    exactly equals the trusted WebView main-page host. This keeps listed
    sites and first-party routes usable without trusting Servo's
    ambiguous document/main-frame flag. An explicit override can also
    allow matching third-party hosts; global loads get neither exception.
- **Local ad-filter engine with explicit subscriptions.** Brave's
  `adblock` crate evaluates ABP-compatible network rules from a small seed
  list compiled into the binary. Settings offers a fixed EasyList and
  EasyPrivacy catalog; neither is selected or downloaded by default. A
  user-started update is HTTPS-only, bounded and validated, and becomes
  active only after every selected list compiles. There is no automatic
  refresh, redirect following, persistence, custom URL input or partial
  replacement. Optional rules cannot relax the independently evaluated
  embedded seed, and a per-site tracker exception cannot skip this separate
  layer. The toolbar reports
  tracker and ad blocks for the committed top-level host. Counts are
  session-only, retain no request URLs, exclude unrelated security blocks
  and global unattributed traffic, and are capped at 256 sites.
- **HTTPS-first input.** Typing `example.com` never produces a plaintext
  first load; bare hosts always complete to `https://`.
- **Mixed-content blocking** (HTTP subresources on HTTPS pages).
- **User-action clipboard access.** System clipboard reads and writes
  require a recent matching action in the same WebView and consume a
  short-lived grant; clipboard contents are not retained in an
  in-process fallback.
- **URL-level private-network guard.** Public and opaque/unattributed
  contexts are blocked from explicit local-name/loopback/private targets;
  explicit local-to-local requests remain possible. DNS rebinding is
  outside this URL-only check and remains open.
- **Explicit startup proxy boundary.** Ambient `HTTP_PROXY`,
  `HTTPS_PROXY` and `NO_PROXY` values are cleared before Servo starts.
  A credential-free `http://` endpoint and bounded bypass list are used
  only when explicitly supplied on the command line. Connection failure
  does not fall back to a direct request. Because Servo 0.5 sends a
  default-port HTTP destination to port 443 through its tunnel connector,
  the embedder blocks that case before networking; WebSocket traffic is
  not claimed to use the proxy. Explicit filter-list updates also use the
  configured endpoint with no direct fallback. This separate client does
  not honor bypass entries, preferring a failed update over an accidental
  direct catalog request.
- **Local Reader view.** Reader extraction uses the document already
  loaded in the active WebView and makes no second network request. Only
  bounded text is copied into the native view. The original page remains
  alive but is visually covered and throttled while Reader is visible;
  this does not stop its scripts or provide additional tracking defense.
- **Opt-in search only.** No provider is selected by default. DuckDuckGo
  or Brave Search can be chosen for the current session; only a query
  explicitly submitted with Enter becomes one bounded, percent-encoded
  HTTPS URL. No provider receives partial text, suggestions or keystroke
  requests. URL-only startup and start-page fields never invoke search.

## Policy models (implemented, not enforced)

- **Cookie policy model.** `CookiePolicy` is implemented and tested. Its
  defaults reject third-party cookies, require `Secure` on HTTPS origins
  and apply `SameSite=Lax` when the attribute is absent. The running
  browser does not enforce these decisions yet because the policy is not
  connected to Servo's cookie handling.
- **Fingerprinting configuration model.** `FingerprintingConfig` is
  implemented and tested, with configuration defaults for a fixed user
  agent and blocked client hints. It is not connected to Servo, so these
  settings are not protections provided by the running browser yet. The
  model deliberately does not use randomization because random values can
  make a browser more distinctive.

## Data the browser itself stores

- The embedder does not create a durable history, settings, tracker-
  override or subscription file. Filter-list selections and downloaded
  contents, like bounded per-site counts, exist only for the session.
- Servo is started with `temporary_storage = true`, but its client and
  cache-storage implementations can still create temporary directories
  and SQLite/files on disk. Normal shutdown should remove temporary
  directories; a crash or forced termination can leave remnants in the
  operating system's temporary directory. This is temporary disk-backed
  state, not a memory-only guarantee.
- Copy/paste uses the operating system clipboard. The browser keeps no
  fallback copy of clipboard text, but other applications can observe
  normal OS-clipboard state.
- File inputs use a window-parented operating-system picker. Only paths
  the user confirms are handed to Servo for the requesting page; the
  embedder does not log or persist those paths. The page can then read
  the selected files through the normal web File API, exactly as the
  dialog authorizes.

## Data flow: what a page load touches

1. URL bar input → validated locally (no network). Query-like text stays
   local unless a provider was explicitly selected and Enter submits it;
   only then is one bounded search URL constructed.
2. Intercepted WebView HTTP(S) request → `RequestPipeline` (scheme,
   private network, mixed content, tracker, adblock) → Servo network stack
   (rustls/TLS) → the site. A global plain-HTTP load without an explicit
   HTTP initiator (absent, opaque or secure) is rejected fail-closed
   before the otherwise shared pipeline. Other schemes stay at the
   navigation/Servo boundary.
3. Tracker/ad-filter matches are intercepted and answered empty *before*
   any connection is attempted: the blocked host is never contacted.
4. The page's scripts run locally in Servo.
5. If Reader view is requested, a fixed local evaluator derives bounded
   text from that existing document; no additional page fetch occurs.
6. If the user explicitly applies filter subscriptions, the fixed catalog
   URLs are fetched directly over HTTPS without following redirects, or
   through the explicit startup proxy when present. Checked lists replace the
   current in-memory pipeline as one complete set; failure keeps the old set.

## Planned (not yet implemented — do not claim otherwise)

- **Cookie enforcement at the network layer.** `CookiePolicy` exists
  with tests; wiring it into Servo's cookie handling is a later phase.
- **Fingerprinting enforcement.** Wire the existing configuration model
  into the engine before claiming its user-agent, client-hint or other
  protection levels as active.
- **Cookie store / site data management** (Servo exposes
  `SiteDataManager` for this).
- **Automatic subscription refresh.** Current external filter-list updates
  are explicit, session-only actions; no timer or persistent cache exists.
- **Canvas/WebGL fingerprinting defense** and canvas-read-back
  blocking.
- **Referrer trimming** (default referrer policy) at the embedder level.
- **NoScript-style per-site script toggles.**
- **Container-style site isolation** of storage (per-site profiles).
- **Post-DNS private-network enforcement** to cover public hostnames
  that resolve or rebind to loopback/private addresses.

## Design rules

1. Privacy models live in `browser-privacy`; their request enforcement
   and the ad-filter layer are composed in `browser-network`. Both crates
   remain independent of Servo and are unit-tested.
2. Defaults are privacy-preserving; anything that reduces privacy must
   be user-visible and opt-in.
3. If a privacy feature cannot be verified to work, it is not claimed
   as working.
