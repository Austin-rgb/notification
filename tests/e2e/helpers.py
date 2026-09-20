"""Building blocks for the e2e suite: JWTs, HTTP client, WebSocket client,
NATS client and event envelopes. Everything talks to the real service over
its public interfaces (HTTP, WebSocket, NATS) exactly as a production
client or upstream service would."""

import asyncio
import json
import queue
import re
import threading
import time
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone

import jwt
import nats
import requests
import websocket

import settings


# --------------------------------------------------------------------- utils
def wait_until(fn, timeout=settings.NOTIFY_TIMEOUT, interval=0.2):
    """Poll `fn` until it returns a truthy value; return it (or None on timeout)."""
    deadline = time.time() + timeout
    while True:
        value = fn()
        if value:
            return value
        if time.time() >= deadline:
            return None
        time.sleep(interval)


class Inbox:
    """Thread-safe list of JSON messages with a blocking `wait_for`."""

    def __init__(self):
        self._q = queue.Queue()
        self.items = []

    def put(self, raw):
        try:
            self._q.put(json.loads(raw))
        except (TypeError, ValueError):
            self._q.put({"_unparsed": raw})

    def wait_for(self, predicate, timeout=settings.NOTIFY_TIMEOUT):
        deadline = time.time() + timeout
        while True:
            for item in self.items:
                if predicate(item):
                    return item
            remaining = deadline - time.time()
            if remaining <= 0:
                return None
            try:
                self.items.append(self._q.get(timeout=min(remaining, 0.5)))
            except queue.Empty:
                pass


# --------------------------------------------------------------------- auth
@dataclass
class User:
    id: uuid.UUID
    token: str

    @property
    def headers(self):
        return {"Authorization": f"Bearer {self.token}"}


def make_token(user_id, expires_in=300, secret=None, audiences=None):
    now = int(time.time())
    claims = {
        "sub": str(user_id),
        "aud": audiences or settings.JWT_AUDIENCES,
        "iss": settings.JWT_ISSUER,
        "iat": now,
        "exp": now + expires_in,
    }
    return jwt.encode(claims, secret or settings.JWT_SECRET, algorithm="HS256")


def new_user():
    user_id = uuid.uuid4()
    return User(id=user_id, token=make_token(user_id))


# --------------------------------------------------------------------- HTTP
class Api:
    def __init__(self):
        self.base = settings.BASE_URL + settings.API_PREFIX
        self.http = requests.Session()

    def set_pref(self, channel, user, preferences, token=None):
        headers = {"Authorization": f"Bearer {token}"} if token else (user.headers if user else {})
        return self.http.post(
            f"{self.base}/{channel}/preferences/set",
            json={"preferences": preferences},
            headers=headers,
            timeout=10,
        )

    def confirm(self, channel, user, nonce, token_code):
        return self.http.post(
            f"{self.base}/{channel}/preferences/confirm",
            json={"nonce": nonce, "token": token_code},
            headers=user.headers,
            timeout=10,
        )

    def get_pref(self, channel, user, subject):
        return self.http.get(
            f"{self.base}/{channel}/preferences/get",
            params={"subject": subject},
            headers=user.headers if user else {},
            timeout=10,
        )

    def create_tag(self, tag, user_id, headers=None):
        return self.http.post(
            f"{self.base}/tags",
            json={"tag": tag, "user_id": str(user_id)},
            headers=headers or {},
            timeout=10,
        )


# ---------------------------------------------------------------- WebSocket
class WsClient:
    """Authenticated push connection. A reader thread keeps answering the
    server's pings (it drops silent clients after ~10s) and queues messages."""

    def __init__(self, user, settle=0.5):
        self.inbox = Inbox()
        self.ws = websocket.create_connection(
            settings.WS_URL, header=[f"Authorization: Bearer {user.token}"], timeout=2
        )
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._read, daemon=True)
        self._thread.start()
        time.sleep(settle)  # let the server register the connection before we trigger sends

    def _read(self):
        while not self._stop.is_set():
            try:
                message = self.ws.recv()
            except websocket.WebSocketTimeoutException:
                continue
            except Exception:
                return
            if message:
                self.inbox.put(message)

    def wait_for(self, predicate, timeout=settings.NOTIFY_TIMEOUT):
        return self.inbox.wait_for(predicate, timeout)

    def wait_for_otp(self, timeout=settings.NOTIFY_TIMEOUT):
        msg = self.wait_for(lambda m: re.fullmatch(r"\d{6}", str(m.get("payload", ""))), timeout)
        return int(msg["payload"]) if msg else None

    def wait_for_event(self, event_id, timeout=settings.NOTIFY_TIMEOUT):
        """Wait for a pushed notification carrying the given event id.
        Returns the decoded event envelope (`{"metadata":…, "payload":…}`)."""

        def matches(m):
            try:
                return json.loads(m["payload"])["metadata"]["event_id"] == event_id
            except (KeyError, TypeError, ValueError):
                return False

        msg = self.wait_for(matches, timeout)
        return json.loads(msg["payload"]) if msg else None

    def close(self):
        self._stop.set()
        try:
            self.ws.close()
        except Exception:
            pass


# --------------------------------------------------------------------- NATS
class NatsBus:
    """Publishes/subscribes on the same NATS server the service uses."""

    def __init__(self, url=settings.NATS_URL):
        self._loop = asyncio.new_event_loop()
        self._thread = threading.Thread(target=self._loop.run_forever, daemon=True)
        self._thread.start()
        self._nc = self._run(nats.connect(url, connect_timeout=5))

    def _run(self, coro, timeout=15):
        return asyncio.run_coroutine_threadsafe(coro, self._loop).result(timeout)

    def publish(self, subject, data):
        if isinstance(data, (dict, list)):
            data = json.dumps(data)
        if isinstance(data, str):
            data = data.encode()

        async def go():
            await self._nc.publish(subject, data)
            await self._nc.flush()

        self._run(go())

    def subscribe(self, subject):
        inbox = Inbox()

        async def on_message(msg):
            inbox.put(msg.data)

        async def go():
            await self._nc.subscribe(subject, cb=on_message)
            await self._nc.flush()  # server has registered the subscription

        self._run(go())
        return inbox

    def close(self):
        try:
            self._run(self._nc.close())
        finally:
            self._loop.call_soon_threadsafe(self._loop.stop)


# ------------------------------------------------------------- event helpers
def _now_iso():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ")


def uid(user_id):
    """Audience entry addressing a user by id."""
    return {"Uuid": str(user_id)}


def tag(name):
    """Audience entry addressing a user through a tag."""
    return {"Tag": name}


def make_event(payload, audience, producer="e2e-test"):
    """A wire-format event envelope (see docs/EVENTS.md)."""
    return {
        "metadata": {
            "event_id": str(uuid.uuid4()),
            "event_version": "v1",
            "occurred_at": _now_iso(),
            "producer": producer,
            "correlation_id": None,
            "trace_id": None,
            "user_id": None,
            "audience": audience,
            "session_id": None,
        },
        "payload": payload,
    }


# ------------------------------------------------------------ mock email API
class MockBrevo:
    def __init__(self):
        self.base = settings.MOCK_BREVO_URL

    def captured(self, to=None):
        params = {"to": to} if to else None
        return requests.get(f"{self.base}/_captured", params=params, timeout=5).json()

    def wait_for_mail(self, to, count=1, timeout=settings.NOTIFY_TIMEOUT):
        return wait_until(lambda: (m := self.captured(to)) and len(m) >= count and m, timeout)


# ------------------------------------------------------------------- flows
def register_push(api, ws, user, subject=settings.PUSH_SUBJECT):
    """Full opt-in for the push channel: set -> OTP arrives on the socket -> confirm."""
    resp = api.set_pref("push", user, [{"subject": subject, "address": str(user.id)}])
    assert resp.status_code == 200, resp.text
    nonce = resp.json()["nonce"]
    code = ws.wait_for_otp()
    assert code is not None, "OTP was not pushed to the websocket"
    resp = api.confirm("push", user, nonce, code)
    assert resp.status_code == 200, resp.text
