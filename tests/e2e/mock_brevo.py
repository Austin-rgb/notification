"""Minimal stand-in for the Brevo transactional-email API (stdlib only).

The notification service is pointed here via BREVO_API_URL, so the real
`Brevo` sender code path (HTTP POST, `api-key` header, JSON payload) runs
unchanged. Emails are captured so tests can read OTPs and rendered templates.

  POST   /v3/smtp/email        capture an email (201, like Brevo). 401 on a wrong api-key.
  GET    /_captured[?to=addr]  list captured emails (optionally by recipient)
  DELETE /_captured            forget everything
  GET    /_health              liveness
"""

import json
import os
import threading
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

EXPECTED_API_KEY = os.environ.get("EXPECTED_API_KEY", "")
CAPTURED = []
LOCK = threading.Lock()


class Handler(BaseHTTPRequestHandler):
    def _send(self, status, body=None):
        raw = json.dumps(body if body is not None else {}).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_POST(self):
        if urlparse(self.path).path != "/v3/smtp/email":
            return self._send(404, {"message": "not found"})
        if EXPECTED_API_KEY and self.headers.get("api-key") != EXPECTED_API_KEY:
            return self._send(401, {"message": "invalid api-key"})
        length = int(self.headers.get("Content-Length", 0))
        try:
            payload = json.loads(self.rfile.read(length) or b"{}")
        except json.JSONDecodeError:
            return self._send(400, {"message": "invalid json"})
        with LOCK:
            CAPTURED.append(payload)
        return self._send(201, {"messageId": f"<{uuid.uuid4()}@mock-brevo>"})

    def do_GET(self):
        url = urlparse(self.path)
        if url.path == "/_health":
            return self._send(200, {"status": "ok"})
        if url.path == "/_captured":
            to = parse_qs(url.query).get("to", [None])[0]
            with LOCK:
                items = [
                    m
                    for m in CAPTURED
                    if to is None or any(r.get("email") == to for r in m.get("to", []))
                ]
            return self._send(200, items)
        return self._send(404, {"message": "not found"})

    def do_DELETE(self):
        if urlparse(self.path).path == "/_captured":
            with LOCK:
                CAPTURED.clear()
            return self._send(204)
        return self._send(404, {"message": "not found"})

    def log_message(self, fmt, *args):  # keep container logs quiet
        pass


if __name__ == "__main__":
    port = int(os.environ.get("PORT", "8025"))
    ThreadingHTTPServer(("0.0.0.0", port), Handler).serve_forever()
