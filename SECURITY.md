# Security

This document states the current security posture honestly: what is
implemented, what is planned, and what is *not* yet a defense.

## Threat model

We defend the user against:

1. **Malicious web content** — drive-by downloads, credential
   harvesting, script-based attacks against the browser.
2. **Trackers and data brokers** — third-party observation of browsing.
3. **Network attackers** — passive sniffing and active downgrade
   attacks on plaintext connections.
4. **Malicious downloads** — files that arrive as executables.

We do not (yet) defend against: compromised Servo (no sandboxing
story exists for the embedded engine), malicious extensions (none
exist), side-channel attacks on the machine.

## Implemented defenses

### URL and navigation policy (`browser-security`)

Address-bar, command-line start-page and settings start-page input is
checked by `NavigationPolicy` before a load command is sent to Servo.
Every request at Servo's navigation boundary is checked again with
`NavigationPolicy::check_content`. HTTP(S) resource requests delivered
through Servo's WebView and global interception callbacks also pass
through `SchemeValidationLayer` as the first request-pipeline check:

- **Scheme allow-lists per source.** The address bar and web content are
  limited to `http:`, `https:` and only `about:blank` (a fragment is
  allowed; a query or another `about:` page is not). They may never
  navigate to `file:`, `data:`, `blob:` or `javascript:` — the classic
  drive-by vectors.
  Servo 0.5 reports top-level and inner-frame navigation through the
  same callback without public frame identity, so local `file:` loads
  stay disabled instead of relying on a forgeable URL-only grant.
- **No embedded credentials.** `https://user:pass@host/` is rejected
  outright because credentials can obscure URL authority and create
  phishing or secret-leak risks.
- **Host requirement.** Network schemes without a host are denied
  (defensive: current `url` versions already refuse to parse them).

### Request pipeline (`browser-network`)

Every WebView HTTP(S) request, and every global request not rejected by
the fail-closed pre-check below, passes `SchemeValidationLayer` →
`PrivateNetworkLayer` → `MixedContentLayer` → `TrackerLayer` →
`AdblockLayer` before it is allowed to continue through Servo's network
stack:

- **Mixed content.** A plain-HTTP subresource on an HTTPS page is
  blocked. (Upgrade-in-place comes with the networking phase.)
- **Private-network requests.** Public or opaque/unattributed contexts
  cannot request explicit `localhost`, `.local`, single-label, loopback,
  link-local or numeric private-network targets. Explicitly local
  sources can still reach local targets. The authenticated first
  `Document` of an opener-free, browser-created WebView is also allowed
  as a genuine top-level navigation; other public, opaque or
  unattributed requests remain blocked.
  This is a URL-level guard only: a public hostname that resolves to a
  private address (DNS rebinding) is not detected yet.
- **Tracker blocking** answered with an empty 200 response rather than
  an error, so pages render without breakage.
- **Ad blocking.** Brave's `adblock` engine evaluates ABP-compatible
  network rules from the embedded list. Authenticated top-level
  navigations are exempt so a rule cannot make its publisher's site
  unvisitable; subframes remain filterable. Unsupported adblock request
  forms pass back to the earlier scheme/security policy instead of
  creating a second URL policy.
- **Global loads.** A shared `ServoDelegate` applies the same pipeline
  to intercepted HTTP(S) loads without a WebView association, such as
  service-worker traffic. A global plain-HTTP load without an explicit
  HTTP initiator (including absent, opaque or secure initiators) is
  rejected fail-closed before the pipeline. Global loads receive no per-
  site tracker exception because there is no trusted WebView main-page
  identity.
- **Scoped exceptions.** An embedder-authenticated initial top-level
  `Document` bypasses tracker blocking. For later loads, a WebView's
  tracker override is selected only from its committed main-page URL,
  never from suppressible referrer metadata. Host and pattern matches
  are allowed when the request host exactly equals that trusted main-
  page host, keeping listed sites and first-party routes usable. An
  explicit site override can additionally allow third-party tracker
  hosts; global loads get neither exception. The override skips only the
  concrete tracker layer and cannot bypass the later adblock layer.

### Clipboard, file selection and page-created WebViews

- Clipboard reads and writes are denied unless a recent matching paste,
  copy or cut action produced a short-lived, one-shot grant for that
  WebView. There is no in-process fallback retaining clipboard text.
- A page-created WebView is accepted only after an eligible recent click,
  Enter/Space key press or popup-opening context-menu action in its
  parent WebView. The popup grant is short-lived and consumed by one
  creation request, limiting script-driven tab floods and hidden-tab
  focus theft.
- A file-input request is accepted only for the active tab after a
  recent eligible page click or Enter/Space action. The same general
  one-shot grant cannot authorize both a popup and a file picker, while
  popup-only context-menu grants cannot authorize file selection. The
  parented native dialog is the final disclosure boundary: cancel sends
  no selection, paths are neither logged nor persisted, and untrusted
  extension filters are syntax-checked, deduplicated and bounded before
  reaching the operating system. Navigation, tab switch/close, browser-
  chrome focus and window focus loss revoke unused grants. A show
  immediately superseded by
  hide, navigation, close or another control in the same Servo event
  batch is dismissed before the blocking native dialog is opened.

### Reader view

- Reader extraction uses one fixed embedder-owned script and never
  evaluates page-supplied code, fetches a second copy or renders returned
  HTML. Because the script runs in the page's main world, its result is
  treated as fully untrusted.
- Both JavaScript and Rust enforce independent traversal, block and text
  limits. The native UI displays only bounded text and a separately
  retained source origin; page markup, styles and links cannot become
  browser chrome.
- A result is accepted only for the exact tab, WebView, committed URL and
  navigation sequence that requested it. Navigation and tab closure make
  old callbacks harmless.
- The five-second UI timeout cannot cancel Servo's evaluator. One
  outstanding request per WebView and a global cap of four remain in
  force until callbacks arrive, preventing retry-driven accumulation.
  Reader mode is a presentation feature, not a script blocker or an
  engine sandbox.

### Download policy (`browser-security`)

- `sanitize_filename`: replaces path separators and Windows-forbidden
  characters, rejects empty/dot-only names, rewrites Windows reserved
  device names (`CON`, `NUL`, …), normalizes trailing dots/spaces.
- `target_path`: lexical containment check — `..` escaping the download
  directory is refused.
- `is_dangerous_extension`: double-extension heuristic
  (`report.pdf.exe`) and an executable/script extension set, to be used
  by the download UI for warnings.
- `SafeDownloadWriter`: rejects redirected/reparse download directories
  on Windows, holds the ordinary directory open without delete sharing,
  and gives every active target its own cloned directory handle.
  Incomplete bytes use an exclusive temporary file. `finish` syncs them
  and publishes with a same-directory hard link while the exclusive
  source handle remains live; it cannot overwrite an existing regular
  file, symlink, junction or other reparse point.
- Regression tests cover abort cleanup, existing-target preservation,
  long and dangerous names, exclusive temporary-file replacement, a
  real Windows junction and directory-swap attempts both before and
  after the parent writer is dropped.

### Search boundary (`browser-core`)

Search is disabled by default and is session-only. The address bar
classifies whitespace, or an explicit `?word`, as a query only after
normal URL parsing fails. Malformed input containing an explicit `://`
is never forwarded as a query. Search text is control-free, limited to
512 Unicode scalar values and encoded through `url` query-pair APIs.
Startup and start-page fields use a separate URL-only path. There are no
suggestion, autocomplete or per-keystroke provider requests.

### TLS

HTTPS transport security is handled by Servo's network stack via rustls
(`aws-lc-rs` provider, installed once before the event loop starts).
Explicit `http://` navigation remains allowed by the current navigation
policy, but the embedder does not automatically fall back from HTTPS to
HTTP after a failure.

### Startup proxy

The browser ignores ambient `HTTP_PROXY`, `HTTPS_PROXY` and `NO_PROXY`
values. `--proxy` accepts one validated, credential-free `http://`
endpoint; `--proxy-bypass` accepts a bounded host/IP/CIDR list and rejects
the all-bypass wildcard. Servo receives the endpoint for both HTTP and
HTTPS before its connector is built. A proxy outage produces a network
error without a direct fallback. Default-port HTTP is rejected before
networking because Servo 0.5 would otherwise tunnel it to port 443.
Successful HTTPS proxying and WebSocket proxy routing are not claimed.

## Explicitly not implemented yet (honesty)

- **No sandboxing.** A compromised renderer can do whatever Servo can
  do. This is a known limitation of embedding Servo today.
- **No certificate pinning, no HSTS preload handling** at the embedder
  level (Servo's network stack may still apply its own rules).
- **No content security policy enforcement** by the embedder (Servo
  handles CSP for its pages).
- **No download UI or Servo download callback yet.** The safe filesystem
  writer is implemented and tested but deliberately remains unwired;
  issuing a separate network request would not preserve Servo's request
  context, cookies or response semantics. Non-Windows builds also do not
  yet claim the Windows directory-pinning guarantee.
- **No post-resolution private-network enforcement.** Literal URL hosts
  are checked, but DNS rebinding remains open until the resolved address
  is validated at the connector boundary.
- **No general trustworthy frame identity at resource interception.**
  Servo 0.5 labels iframe and top-level documents alike. The embedder
  deliberately grants neither a generic document exemption nor a
  URL/timeout token that a hidden iframe could steal. It authenticates
  only the first `Document` in a fresh, browser-created WebView without
  an opener; page-created WebViews and all later documents receive no
  exemption. A later legitimate HTTP/local top-level navigation may
  therefore fail closed until the engine exposes an unforgeable frame or
  navigation identity.
- **No phishing/social-engineering classification.**

## Rules of engagement

1. Policy crates are pure and testable; security decisions live there,
   not in the GUI.
2. Never `unwrap()`/`expect()` in production paths with user-controlled
   input (panic = denial of service).
3. Every deny path must have a test.
4. When in doubt, deny and log.
