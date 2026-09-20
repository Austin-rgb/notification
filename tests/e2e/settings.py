"""Endpoints and credentials of the docker-compose.e2e.yml stack.

Every value can be overridden through the environment so the same suite can
be pointed at a staging deployment (set E2E_* and matching JWT settings).
"""

import os
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
COMPOSE_FILE = ROOT / "docker-compose.e2e.yml"
SCHEMA_DIR = ROOT / "docs" / "schemas"

BASE_URL = os.environ.get("E2E_BASE_URL", "http://localhost:18080").rstrip("/")
API_PREFIX = os.environ.get("E2E_API_PREFIX", "/notifications").rstrip("/")
NATS_URL = os.environ.get("E2E_NATS_URL", "nats://localhost:14222")
MOCK_BREVO_URL = os.environ.get("E2E_MOCK_BREVO_URL", "http://localhost:18025").rstrip("/")

# Must equal JWT_SECRET / JWT_ISSUER in docker-compose.e2e.yml.
JWT_SECRET = os.environ.get("E2E_JWT_SECRET", "e2e-secret-e2e-secret-e2e-secret-0123456789")
JWT_ISSUER = os.environ.get("E2E_JWT_ISSUER", "notification-e2e")
# The service validates `aud`. Tokens carry several plausible audiences; if every
# authenticated call returns 401, set E2E_JWT_AUDIENCES to what HS256Signer expects.
JWT_AUDIENCES = os.environ.get("E2E_JWT_AUDIENCES", f"{JWT_ISSUER},notification").split(",")

# Subjects configured in docker-compose.e2e.yml
EMAIL_SUBJECT = "order.shipped"
PUSH_SUBJECT = "chat.message"
PUSH_AND_EMAIL_SUBJECT = "order.shipped"
CONSOLE_SUBJECT = "debug.event"

NOTIFY_TIMEOUT = float(os.environ.get("E2E_NOTIFY_TIMEOUT", "10"))
NEGATIVE_WAIT = float(os.environ.get("E2E_NEGATIVE_WAIT", "2"))

WS_URL = BASE_URL.replace("http://", "ws://").replace("https://", "wss://") + API_PREFIX + "/ws/ws/"
