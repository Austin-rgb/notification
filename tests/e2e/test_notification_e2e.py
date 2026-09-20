"""End-to-end tests for the notification service.

They run against the docker-compose.e2e.yml stack (release image + real
PostgreSQL, Redis, NATS) and use only public interfaces: HTTP, WebSocket and
NATS. Each test uses fresh user ids, so tests are independent and the stack
does not need resetting between them.
"""

import json
import re
import uuid

import pytest
import requests
from jsonschema import validate

import settings
from helpers import (
    make_event,
    make_token,
    new_user,
    register_push,
    tag,
    uid,
    wait_until,
)

AUTH_FAILURE = (401, 403)


def _schema(name):
    return json.loads((settings.SCHEMA_DIR / f"{name}.schema.json").read_text())


# ------------------------------------------------------------------ health
def test_liveness_and_readiness():
    assert requests.get(f"{settings.BASE_URL}/healthz", timeout=5).status_code == 200
    body = requests.get(f"{settings.BASE_URL}/readyz", timeout=5)
    assert body.status_code == 200
    assert body.json()["postgres"] is True and body.json()["redis"] is True


# -------------------------------------------------------------------- auth
def test_preferences_require_a_token(api):
    assert api.get_pref("push", None, settings.PUSH_SUBJECT).status_code in AUTH_FAILURE
    resp = api.set_pref("push", None, [{"subject": settings.PUSH_SUBJECT, "address": "x"}])
    assert resp.status_code in AUTH_FAILURE


def test_expired_and_forged_tokens_are_rejected(api, user):
    expired = make_token(user.id, expires_in=-120)
    forged = make_token(user.id, secret="not-the-real-secret-not-the-real-secret")
    for token in (expired, forged):
        resp = api.set_pref("push", None, [{"subject": settings.PUSH_SUBJECT, "address": "x"}], token=token)
        assert resp.status_code in AUTH_FAILURE


def test_websocket_requires_a_token():
    import websocket

    with pytest.raises(websocket.WebSocketBadStatusException) as err:
        websocket.create_connection(settings.WS_URL, timeout=5)
    assert err.value.status_code in AUTH_FAILURE


# ------------------------------------------------- preference validation
def test_subject_must_be_allowed_for_the_channel(api, user):
    # push subjects: chat.message, order.shipped — console's subject is not allowed on push
    resp = api.set_pref("push", user, [{"subject": settings.CONSOLE_SUBJECT, "address": "a"}])
    assert resp.status_code == 403
    assert "not allowed" in resp.text.lower()
    # ...but is allowed on the console channel
    resp = api.set_pref("console", user, [{"subject": settings.CONSOLE_SUBJECT, "address": "a"}])
    assert resp.status_code == 200 and resp.json()["nonce"]


def test_batch_must_share_one_address(api, user):
    resp = api.set_pref(
        "push",
        user,
        [
            {"subject": "chat.message", "address": "one"},
            {"subject": "order.shipped", "address": "two"},
        ],
    )
    assert resp.status_code == 403
    assert "same address" in resp.text


def test_empty_batch_is_a_bad_request(api, user):
    assert api.set_pref("push", user, []).status_code == 400


def test_get_unknown_preference_is_404(api, user):
    assert api.get_pref("push", user, settings.PUSH_SUBJECT).status_code == 404


def test_confirm_with_unknown_nonce_fails(api, user):
    assert api.confirm("push", user, "0000000000000000", 123456).status_code >= 400


# ----------------------------------------------- push: full user journey
def test_push_opt_in_and_delivery(api, bus, user, connect):
    ws = connect(user)
    resp = api.set_pref("push", user, [{"subject": "chat.message", "address": str(user.id)}])
    assert resp.status_code == 200
    nonce = resp.json()["nonce"]
    code = ws.wait_for_otp()
    assert code is not None, "OTP must arrive over the websocket (the push address)"

    # A wrong code is refused and does not consume the pending confirmation.
    wrong = 100000 if code != 100000 else 100001
    assert api.confirm("push", user, nonce, wrong).status_code >= 400
    assert api.get_pref("push", user, "chat.message").status_code == 404

    assert api.confirm("push", user, nonce, code).status_code == 200
    got = api.get_pref("push", user, "chat.message")
    assert got.status_code == 200 and got.json() == str(user.id)

    # An event on the subject reaches the socket, wrapped as {id, source, payload}.
    event = make_event({"text": "hello"}, [uid(user.id)])
    bus.publish("chat.message", event)
    delivered = ws.wait_for_event(event["metadata"]["event_id"])
    assert delivered is not None, "event was not pushed to the user"
    assert delivered["payload"] == {"text": "hello"}


def test_otp_cannot_be_reused(api, user, connect):
    ws = connect(user)
    resp = api.set_pref("push", user, [{"subject": "chat.message", "address": str(user.id)}])
    nonce, code = resp.json()["nonce"], ws.wait_for_otp()
    assert api.confirm("push", user, nonce, code).status_code == 200
    assert api.confirm("push", user, nonce, code).status_code >= 400


def test_confirming_again_replaces_the_address(api, connect):
    """(user, subject) is unique: setting an address again replaces the old one."""
    user, other = new_user(), new_user()
    ws_user, ws_other = connect(user), connect(other)
    register_push(api, ws_user, user)

    # Re-register the same subject; the new address is `other`'s socket, so that is where the OTP goes.
    resp = api.set_pref("push", user, [{"subject": "chat.message", "address": str(other.id)}])
    assert resp.status_code == 200
    code = ws_other.wait_for_otp()
    assert code is not None
    assert api.confirm("push", user, resp.json()["nonce"], code).status_code == 200
    assert api.get_pref("push", user, "chat.message").json() == str(other.id)


def test_preferences_are_isolated_per_user(api, connect):
    """Regression: an unfiltered lookup used to return the first row of the table
    (i.e. another user's address) for everyone."""
    a, b = new_user(), new_user()
    ws_a, ws_b = connect(a), connect(b)
    register_push(api, ws_a, a)
    register_push(api, ws_b, b)
    assert api.get_pref("push", a, "chat.message").json() == str(a.id)
    assert api.get_pref("push", b, "chat.message").json() == str(b.id)


def test_users_do_not_receive_each_others_events(api, bus, connect):
    a, b = new_user(), new_user()
    ws_a, ws_b = connect(a), connect(b)
    register_push(api, ws_a, a)
    register_push(api, ws_b, b)

    event = make_event({"for": "a"}, [uid(a.id)])
    bus.publish("chat.message", event)
    assert ws_a.wait_for_event(event["metadata"]["event_id"]) is not None
    assert ws_b.wait_for_event(event["metadata"]["event_id"], timeout=settings.NEGATIVE_WAIT) is None


# ------------------------------------------------------ audience handling
def test_tag_audience_is_resolved_to_the_user(api, bus, user, connect):
    ws = connect(user)
    register_push(api, ws, user)
    name = f"team-{uuid.uuid4().hex[:10]}"
    resp = api.create_tag(name, user.id, headers=user.headers)
    assert resp.status_code == 201, resp.text

    event = make_event({"via": "tag"}, [tag(name)])
    bus.publish("chat.message", event)
    assert ws.wait_for_event(event["metadata"]["event_id"]) is not None


def test_one_bad_audience_entry_does_not_block_the_others(api, bus, user, connect):
    """Unknown tag and a user without preferences come first; the opted-in user must still be notified."""
    ws = connect(user)
    register_push(api, ws, user)
    no_prefs = new_user()

    event = make_event(
        {"x": 1},
        [tag(f"missing-{uuid.uuid4().hex[:8]}"), uid(no_prefs.id), uid(user.id)],
    )
    bus.publish("chat.message", event)
    assert ws.wait_for_event(event["metadata"]["event_id"]) is not None


def test_events_on_unconfigured_subjects_are_ignored(api, bus, user, connect):
    ws = connect(user)
    register_push(api, ws, user)
    event = make_event({"x": 1}, [uid(user.id)])
    bus.publish("chat.unconfigured", event)
    assert ws.wait_for_event(event["metadata"]["event_id"], timeout=settings.NEGATIVE_WAIT) is None


def test_service_survives_malformed_events(api, bus, user, connect):
    ws = connect(user)
    register_push(api, ws, user)
    bus.publish("chat.message", b"this is not json")
    bus.publish("chat.message", b'{"metadata": 5, "payload": {}}')
    bus.publish("chat.message", b'{"payload": {}}')  # no metadata

    event = make_event({"after": "garbage"}, [uid(user.id)])
    bus.publish("chat.message", event)
    assert ws.wait_for_event(event["metadata"]["event_id"]) is not None


# --------------------------------------------------------- email channel
def test_email_opt_in_and_delivery(api, bus, brevo, user):
    address = f"{uuid.uuid4().hex[:16]}@example.test"

    resp = api.set_pref("email", user, [{"subject": settings.EMAIL_SUBJECT, "address": address}])
    assert resp.status_code == 200
    nonce = resp.json()["nonce"]

    # The OTP is delivered as a real email through the provider API.
    mails = brevo.wait_for_mail(address, count=1)
    assert mails, "no OTP email reached the (stub) email provider"
    otp_mail = mails[0]
    assert otp_mail["subject"] == "confirm address"
    assert otp_mail["sender"]["email"] == "no-reply@notifications.test"
    match = re.search(r"\b(\d{6})\b", otp_mail["htmlContent"])
    assert match, otp_mail["htmlContent"]

    assert api.confirm("email", user, nonce, int(match.group(1))).status_code == 200
    assert api.get_pref("email", user, settings.EMAIL_SUBJECT).json() == address

    # An event on the subject is rendered through the template and emailed.
    order_id = f"ORD-{uuid.uuid4().hex[:8]}"
    bus.publish(settings.EMAIL_SUBJECT, make_event({"order_id": order_id}, [uid(user.id)]))
    mails = brevo.wait_for_mail(address, count=2)
    assert mails and len(mails) == 2, "event email was not sent"
    assert order_id in mails[1]["htmlContent"]
    assert mails[1]["to"][0]["email"] == address


def test_same_event_fans_out_to_every_channel_the_user_enabled(api, bus, brevo, user, connect):
    subject = settings.PUSH_AND_EMAIL_SUBJECT
    address = f"{uuid.uuid4().hex[:16]}@example.test"
    ws = connect(user)
    register_push(api, ws, user, subject=subject)

    resp = api.set_pref("email", user, [{"subject": subject, "address": address}])
    mails = brevo.wait_for_mail(address, count=1)
    code = int(re.search(r"\b(\d{6})\b", mails[0]["htmlContent"]).group(1))
    assert api.confirm("email", user, resp.json()["nonce"], code).status_code == 200

    event = make_event({"order_id": "ORD-FANOUT"}, [uid(user.id)])
    bus.publish(subject, event)
    assert ws.wait_for_event(event["metadata"]["event_id"]) is not None
    assert brevo.wait_for_mail(address, count=2)


# ------------------------------------------------- events we publish
def test_channel_confirmed_event_is_published(api, bus, user, connect):
    inbox = bus.subscribe("contact.channel.confirmed")
    ws = connect(user)
    register_push(api, ws, user)

    event = inbox.wait_for(lambda e: e.get("payload", {}).get("user") == str(user.id))
    assert event is not None, "contact.channel.confirmed was not published"
    validate(event, _schema("channel-confirmed"))
    validate(event, _schema("event-envelope"))
    assert event["payload"] == {"user": str(user.id), "channel": "push", "address": str(user.id)}


def test_events_conform_to_the_envelope_schema_we_document():
    validate(make_event({"any": "thing"}, [uid(uuid.uuid4()), tag("t")]), _schema("event-envelope"))


# -------------------------------------------- state lives outside the process
@pytest.mark.docker
def test_pending_otp_survives_a_service_restart(api, user, connect, restart_service):
    """Pending confirmations live in Redis, not process memory, so a restart
    (or a different replica) can complete a confirmation started earlier."""
    ws = connect(user)
    resp = api.set_pref("push", user, [{"subject": "chat.message", "address": str(user.id)}])
    nonce, code = resp.json()["nonce"], ws.wait_for_otp()
    assert code is not None

    restart_service()

    assert api.confirm("push", user, nonce, code).status_code == 200
    assert wait_until(lambda: api.get_pref("push", user, "chat.message").status_code == 200)


# --------------------------------------------------------- known gaps
@pytest.mark.xfail(
    reason="KNOWN GAP: the tags CRUD API is unauthenticated (viewset leaves auth to the app). "
    "Anyone can map a tag to any user. See docs/INTEGRATION_NOTES.md.",
    strict=False,
)
def test_tags_api_requires_authentication(api, user):
    resp = api.create_tag(f"anon-{uuid.uuid4().hex[:8]}", user.id, headers={})
    assert resp.status_code in AUTH_FAILURE
