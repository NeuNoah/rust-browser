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

Every navigation from the address bar and every subresource request is
checked by `NavigationPolicy`:

- **Scheme allow-lists per source.** The address bar allows `https`,
  `http`, `about`, `file`; web content may additionally navigate to
  `http`/`https`/`about` but never to `file:`, `data:`, `blob:`,
  `javascript:` — the classic drive-by vectors.
- **No embedded credentials.** `https://user:pass@host/` is rejected
  outright: credentials in URLs are a phishing vector and the `url`
  crate would otherwise normalize them away silently.
- **Host requirement.** Network schemes without a host are denied
  (defensive: current `url` versions already refuse to parse them).

### Request pipeline (`browser-network`)

Every request passes `SchemeValidationLayer` → `TrackerLayer` →
`MixedContentLayer` before Servo's network stack sees it:

- **Mixed content.** A plain-HTTP subresource on an HTTPS page is
  blocked. (Upgrade-in-place comes with the networking phase.)
- **Tracker blocking** answered with an empty 200 response rather than
  an error, so pages render without breakage.

### Download policy (`browser-security`)

- `sanitize_filename`: replaces path separators and Windows-forbidden
  characters, rejects empty/dot-only names, rewrites Windows reserved
  device names (`CON`, `NUL`, …), normalizes trailing dots/spaces.
- `target_path`: lexical containment check — `..` escaping the download
  directory is refused.
- `is_dangerous_extension`: double-extension heuristic
  (`report.pdf.exe`) and an executable/script extension set, to be used
  by the download UI for warnings.

### TLS

All transport security is handled by Servo's network stack via rustls
(`aws-lc-rs` provider, installed once before the event loop starts).
No plaintext fallbacks are configured.

## Explicitly not implemented yet (honesty)

- **No sandboxing.** A compromised renderer can do whatever Servo can
  do. This is a known limitation of embedding Servo today.
- **No certificate pinning, no HSTS preload handling** at the embedder
  level (Servo's network stack may still apply its own rules).
- **No content security policy enforcement** by the embedder (Servo
  handles CSP for its pages).
- **No download UI yet** — the policy functions exist and are tested;
  the download flow lands in a later phase.
- **No phishing/social-engineering classification.**

## Rules of engagement

1. Policy crates are pure and testable; security decisions live there,
   not in the GUI.
2. Never `unwrap()`/`expect()` in production paths with user-controlled
   input (panic = denial of service).
3. Every deny path must have a test.
4. When in doubt, deny and log.