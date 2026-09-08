# Manual GUI smoke tests

These checks cover the Servo/window boundary that unit tests cannot
exercise without a real window and system input method.

## Phase 4 — input and controls

Serve the fixture over local HTTP so Servo exercises a normal page
origin. From the repository root, start this in one PowerShell:

```powershell
python -m http.server 8765 --directory .\tools\smoke
```

Then start the browser in a second PowerShell:

```powershell
cargo run -p browser -- http://127.0.0.1:8765/phase4.html
```

Verify:

1. Normal text, dead keys and AltGr work in the input, textarea and
   contenteditable area. Backspace/Delete, arrows, Home/End, Enter, Tab
   and key repeat behave normally. Confirm in the event log that the
   DOM `key`, `code`, `repeat`, `isComposing` and AltGraph modifier
   values match the physical action.
2. With a system IME, preedit is visible, commit appears exactly once,
   and the event log shows Start/Update/End in order. Repeat in the URL
   bar and confirm that candidate-selection Enter does not navigate.
   Confirm that the candidate window follows the page and URL-bar caret.
3. Ctrl+L focuses and selects the URL; Ctrl+T/W/Tab still manage tabs.
   Ctrl+V/Shift+Insert and copy/cut shortcuts still work, while a page
   script without a matching recent action cannot read, clear or write
   the system clipboard. Each grant must be consumed once. Switch away,
   switch back, and confirm that the old tab's unused grant was revoked
   rather than carried through the tab switch.
4. The context menu, grouped single-select, multiple-select, color
   picker and alert/confirm/prompt controls respond. Newly opened page
   controls must take keyboard focus; verify Arrow/Tab navigation,
   Enter/Space activation and Escape focus restoration. A long select
   list must remain height-bounded and scrollable. Prompt text must
   survive redraws; page dialogs must show the page identity and block
   interaction with the page behind them.
5. The single-file input opens a native dialog parented to the browser,
   displays only its `.txt`/`.md` filter, accepts
   `tools/smoke/sample-upload.txt`, and reports only its name and size on
   the page. Cancel must preserve the prior selection. The multiple-file
   input accepts both `sample-upload.txt` and `sample-upload.md` in one
   choice. Clicking “Delayed picker” must not open a dialog after the
   two-second user-action grant expires. Clicking “Picker after chrome
   focus” and immediately focusing the URL bar must likewise keep the
   dialog closed even though the grant has not yet expired.
6. Ctrl+Plus/Minus/0 changes and resets page zoom on the active tab.
7. The `target=_blank` link opens one active browser tab after the link
   click and Ctrl+W returns to the fixture without losing its state.
   Repeated scripted `window.open()` calls without a fresh action must
   not create further tabs, and a hidden tab must not steal focus.
8. Settings reject an unsafe start page, accept a valid one for the
   session, normalize a mixed-case/trailing-dot tracker host, and allow
   that override to be toggled and removed. Separately verify with
   network logging that only a WebView whose committed main-page host
   has the override is allowed through the tracker layer. An iframe
   referrer and global/service-worker load must not acquire that
   exception.
9. Repeat pointer/control and IME candidate-position checks at 100% and
   a non-integer/high-DPI scale (the reference scales are 1.0125x and
   2.25x). The latest run below covered only 100%; the other scales
   remain unverified for the current Phase 4 implementation.

## Reader view

Open `http://127.0.0.1:8765/reader.html` and verify:

1. Aa opens a native, text-only view with the fixture title, author,
   source origin, Unicode samples and preserved preformatted lines. The
   navigation text, button and output from the source page are absent.
2. Wheel, Arrow, Page Up/Down, Home and End scroll only Reader. Clicking
   where the covered source button lives must not update its result.
   Escape restores the source page at its prior scroll position and with
   working keyboard focus.
3. Open another tab while Reader is ready, then switch back. Reader state
   must remain attached to its original tab without focus theft or state
   mixing. Navigating that tab must discard the old Reader result.
4. `reader-empty.html` produces the native unavailable/error surface.
   `reader-slow.html` produces the native timeout after five seconds;
   once its first evaluator returns, Aa retry must succeed without an
   accumulated second evaluator.

## Per-site tracker override

This test uses a local fail-closed HTTP proxy. It serves synthetic
`site.test`, `other.test` and `tracker.site.test` responses from memory and
never resolves or forwards those names to the external network.

Start the fixture:

```powershell
python .\tools\smoke\tracker-proxy.py --port 8765
```

In a second PowerShell, launch the browser with the explicit startup
proxy (ambient proxy variables are deliberately ignored):

```powershell
cargo run -p browser -- --proxy http://127.0.0.1:8765 http://site.test:8765/tracker-override.html
```

For a reproducible startup-only override check, add
`--allow-trackers-for site.test` before the URL. This initializes the same
in-memory override state as Settings and does not persist anything:

```powershell
cargo run -p browser -- --proxy http://127.0.0.1:8765 --allow-trackers-for site.test http://site.test:8765/tracker-override.html
```

Verify:

1. The page settles on `Tracker request: BLOCKED`, and the proxy prints
   no `TRACKER` request.
2. Relaunch with the startup-only override shown above, or add an
   `Allow trackers` override for `site.test` in Settings and reload. The
   page settles on `Tracker request: ALLOWED` and the proxy prints one or
   more `TRACKER` requests. Servo may repeat a resource request; each one
   must target only the in-memory `tracker.site.test` fixture.
3. Open `http://other.test:8765/tracker-override.html` in another tab. It
   must remain `BLOCKED`, proving that the `site.test` exception is not global.
4. Remove the `site.test` override, return to its tab and reload. The
   result must return to `BLOCKED`.

## Startup proxy matrix

The explicit proxy uses a credential-free `http://` endpoint and an
optional comma-separated bypass list. Verify the following with the
tracker proxy above and a direct fixture on port 8766:

```powershell
python -m http.server 8766 --bind 127.0.0.1 --directory .\tools\smoke
cargo run -p browser -- --proxy http://127.0.0.1:8765 --proxy-bypass 127.0.0.1 http://127.0.0.1:8766/phase4.html
```

1. Invalid schemes, credentials, paths, duplicate flags, an empty bypass
   entry and bypass `*` must exit before a window is created.
2. A non-default-port HTTP fixture goes through the proxy unless its host
   matches `--proxy-bypass`; the bypassed request appears only at the
   direct fixture.
3. Stop the proxy while leaving the direct fixture reachable. The browser
   must show a connection error and the direct fixture must receive no
   request.
4. `http://site.test:80/` must be cancelled before the proxy receives a
   request. Servo 0.5 otherwise defaults this HTTP tunnel to port 443.
5. `https://site.test/` must reach the proxy as CONNECT plus a TLS
   ClientHello. This fail-closed fixture intentionally rejects TLS, so it
   proves routing but not a successful trusted HTTPS session.
6. Open `websocket-proxy.html` through a `127.0.0.1` bypass. The current
   Servo path reports `WebSocket request: ERROR` and sends no request to
   the proxy; keep browser-wide WebSocket proxy support unclaimed.

Record the date, OS/input method, tested scales and any skipped item in
the Roadmap entry before marking an engine-dependent feature complete.

### Latest run

2026-09-01 — Windows 11 25H2 build 26200.9168, 100% scale (96 DPI),
local HTTP fixtures. Uncommitted working tree; manual-smoke snapshot
source/test fingerprint
`718145525c103dbe69a2bc0d9534c0694a9c74f9026ed0902b91a63ca1492e52`.

The fingerprint covers 43 files: `.gitattributes`, the workspace
manifests and lockfile, every file under `crates/` and `resources/`,
`tools/check.ps1`, and every direct fixture/sample file under
`tools/smoke/` except this README. Root documentation is excluded so
recording the fingerprint does not make it recursive.
For each file, decode text, normalize CRLF/CR to LF, encode as UTF-8
without a BOM and calculate lowercase SHA-256. Sort by repository-
relative path, form `relative-path:sha256` lines, join them with LF and
no trailing newline, then calculate SHA-256 over that UTF-8 manifest.

The later Phase 7 Brave-adblock integration passed the complete automated
quality gate on 2026-09-02 but has not replaced the manual GUI snapshot
above. Its 44-file source/test fingerprint is
`1acca381658144f2daf3a59a2e1cabc5c9bc4e1321c5c85f497ff655a06ed0fc`.

Passed fresh browser-created-tab navigation to the local HTTP fixture,
direct Unicode insertion, Ctrl+L and page focus handoff, modal Ctrl+T
blocking, Ctrl+T/Ctrl+W with full URL selection on the restored tab,
URL-draft isolation, context menu and its initial keyboard focus,
keyboard single-select, a bounded/scrollable 200-item select, color
cancel plus initial focus and filled layout, alert, zoom/reset,
`target=_blank`, visible rejection and recovery for `javascript:`, and
opening/closing Settings. Later targeted passes in the rebuilt executable
covered native single/multiple file selection, cancel, filters and
expired/delayed actions. Reader passed
bounded text/Unicode/preformatted rendering, local fonts, wheel and
keyboard scrolling, covered-page input blocking, focus and scroll
restoration, per-tab persistence, timeout and successful retry. The
fail-closed tracker proxy then passed default blocking, a normalized
`SITE.TEST.` startup exception and continued blocking on `other.test`.
The startup proxy passed strict validation, ambient-variable isolation,
non-default HTTP routing, bypass and outage/no-fallback checks. CONNECT
reached the TLS boundary. Default-port HTTP was confirmed unsafe in the
upstream tunnel implementation and is now blocked before networking;
the WebSocket fixture confirmed that no proxy request is made.
The rebuilt search UI then passed its privacy-neutral default: submitting
`rust privacy` with no provider selected showed a local error and made no
navigation. Settings displayed Disabled, DuckDuckGo and Brave Search and
allowed a draft provider selection. The Apply action was not automated
because the Windows UI safety policy forbids mutating an in-app privacy
setting; provider state and exact encoded query forwarding are covered by
the `browser-core` tests instead.

Not exercised in this run: dead keys, AltGr, key repeat, an actual
system IME/preedit/candidate window, non-integer/high DPI, a live system-
clipboard flow, multiple-select and confirm/prompt in the final pass,
Settings mutation, a successful trusted HTTPS proxy session, and the
sub-two-second chrome-focus picker race. The latter remains covered by
focus-epoch unit tests;
the safe UI automation loop cannot complete and inspect both inputs
inside that timing window. File picker, Reader and tracker policy were
exercised in their targeted passes described above.
