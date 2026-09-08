"""Fail-closed local HTTP proxy for tracker and proxy-boundary smoke tests.

The proxy serves two synthetic public-looking hosts entirely from memory:

* site.test / other.test: the test page
* tracker.site.test: a reserved test-only host in the built-in tracker list

It never resolves or forwards a hostname, so a typo cannot escape to the
network. This is a manual Servo boundary fixture, not a production proxy.
"""

from __future__ import annotations

import argparse
import socket
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit


LISTEN_HOST = "127.0.0.1"
SITE_HOSTS = frozenset({"site.test", "other.test"})
TRACKER_HOST = "tracker.site.test"
PAGE_PATH = "/tracker-override.html"
TRACKER_PATH = "/tracker-smoke.js"

PAGE = b"""<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>Tracker override smoke test</title>
  <style>
    body { font: 18px/1.5 system-ui, sans-serif; margin: 3rem auto; max-width: 760px; padding: 0 1rem; }
    output { display: block; margin: 1.5rem 0; padding: 1rem; border: 2px solid currentColor; font-weight: 700; }
  </style>
</head>
<body>
  <h1>Tracker override smoke test</h1>
  <p>The external script uses <code>tracker.site.test</code>, a reserved test-only host in the built-in tracker list.</p>
  <output id="result">Tracker request: WAITING</output>
  <button id="reload" type="button">Reload test</button>
  <script>window.trackerSmokeLoaded = false;</script>
  <script src="http://tracker.site.test:8765/tracker-smoke.js"></script>
  <script>
    setTimeout(() => {
      const allowed = window.trackerSmokeLoaded === true;
      const result = document.querySelector('#result');
      result.value = allowed ? 'Tracker request: ALLOWED' : 'Tracker request: BLOCKED';
      result.dataset.result = allowed ? 'allowed' : 'blocked';
    }, 250);
    document.querySelector('#reload').onclick = () => location.reload();
  </script>
</body>
</html>
"""

TRACKER_SCRIPT = b"""window.trackerSmokeLoaded = true;
document.documentElement.dataset.trackerSmokeExecuted = 'true';
"""


def request_target(raw_target: str, host_header: str | None) -> tuple[str, str]:
    """Return a normalized (host, path) for proxy or origin-form requests."""

    parsed = urlsplit(raw_target)
    if parsed.scheme:
        host = (parsed.hostname or "").rstrip(".").lower()
        path = parsed.path or "/"
    else:
        host = (host_header or "").split(":", 1)[0].rstrip(".").lower()
        path = parsed.path or "/"
    return host, path


class TrackerSmokeProxy(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, format: str, *args: object) -> None:
        print(f"[{self.log_date_time_string()}] {format % args}", flush=True)

    def send_body(self, status: int, body: bytes, content_type: str) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)
        self.close_connection = True

    @staticmethod
    def route(host: str, path: str) -> tuple[int, bytes, str, str]:
        if host in SITE_HOSTS and path in {"/", PAGE_PATH}:
            return 200, PAGE, "text/html; charset=utf-8", "SITE"
        if host == TRACKER_HOST and path == TRACKER_PATH:
            return 200, TRACKER_SCRIPT, "text/javascript; charset=utf-8", "TRACKER"
        return 404, b"fixture route not found\n", "text/plain; charset=utf-8", "MISS"

    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        host, path = request_target(self.path, self.headers.get("Host"))
        status, body, content_type, label = self.route(host, path)
        print(f"{label} {host}{path}", flush=True)
        self.send_body(status, body, content_type)

    def do_CONNECT(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        target_host = self.path.rsplit(":", 1)[0].rstrip(".").lower()
        if target_host not in SITE_HOSTS | {TRACKER_HOST}:
            self.send_body(502, b"CONNECT target is not a fixture host\n", "text/plain")
            return

        # Servo 0.5 uses CONNECT even for an HTTP target. Terminate that
        # local tunnel in memory instead of opening a socket or resolving
        # the requested hostname. TLS ClientHello bytes are rejected.
        self.send_response_only(200, "Connection Established")
        self.end_headers()
        self.connection.settimeout(5)
        try:
            request_line = self.rfile.readline(65_537)
            if not request_line.startswith(b"GET "):
                print(f"TUNNEL-REJECT {target_host} non-HTTP payload", flush=True)
                self.close_connection = True
                try:
                    self.connection.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
                return
            parts = request_line.decode("iso-8859-1").rstrip("\r\n").split(" ")
            if len(parts) != 3:
                print(f"TUNNEL-REJECT {target_host} malformed request", flush=True)
                return
            while True:
                header_line = self.rfile.readline(65_537)
                if header_line in {b"\r\n", b"\n", b""}:
                    break
            parsed = urlsplit(parts[1])
            path = parsed.path or "/"
            status, body, content_type, label = self.route(target_host, path)
            print(f"{label} {target_host}{path} via CONNECT", flush=True)
            self.send_body(status, body, content_type)
        except (OSError, UnicodeDecodeError) as error:
            print(f"TUNNEL-REJECT {target_host} {error}", flush=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=8765)
    args = parser.parse_args()
    server = ThreadingHTTPServer((LISTEN_HOST, args.port), TrackerSmokeProxy)
    print(f"Tracker smoke proxy listening on http://{LISTEN_HOST}:{args.port}", flush=True)
    print(f"Open through this proxy: http://site.test:{args.port}{PAGE_PATH}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
