# API

Base URL: `http://<host>:8080`. All routes below except health are under the **API prefix**
(`API_PREFIX`, default `/notifications`, written `{P}` here).

Items marked *(inferred)* come from reading the `viewset`/`actixutils` crate sources rather than from
running the service.

## Authentication

Except `/healthz`, `/readyz` and the tags API (see [known gap](INTEGRATION_NOTES.md#1-the-tags-api-is-unauthenticated)),
requests need a JWT:

```
Authorization: Bearer <jwt>
```

(An `access_token` cookie is accepted as a fallback, which is what browsers use for WebSockets.)

* Algorithm: HS256, signed with the service's `JWT_SECRET`.
* Claims (`actixutils::Identity`): `sub` (user UUID), `aud` (array of strings), `iat`, `exp` (Unix **seconds**).
* Missing/invalid/expired token → `401` *(inferred)*.

The user id used everywhere below is the token's `sub`.

## Health

| Route | Purpose | Response |
|---|---|---|
| `GET /healthz` | Liveness | `200 {"status":"ok"}` |
| `GET /readyz` | Readiness (Postgres + Redis) | `200 {"status":"ok","postgres":true,"redis":true}` or `503` with the failing check `false` |

## Preferences

A **channel** is `email`, `push` or `console`. A **subject** is an event-stream subject (see [EVENTS.md](EVENTS.md)).
Opting in is two steps: `set` sends a one-time code to the address, `confirm` proves ownership.

### `POST {P}/{channel}/preferences/set`

Start opt-in for one or more subjects that share a single address.

```json
{ "preferences": [ { "subject": "order.shipped", "address": "me@example.com" } ] }
```

* `subject`, `address`: 1–64 characters each.
* Every entry in one request must have the **same address**.
* Every subject must be in that channel's configured list (`EMAIL_SUBJECTS` / `PUSH_SUBJECTS` / `CONSOLE_SUBJECTS`).
* A 6-digit code is delivered to `address` through the channel itself (email → OTP email, push → pushed to the
  WebSocket whose user id equals `address`, console → printed to the service log).

| Status | Meaning |
|---|---|
| `200` | `{"nonce":"<16 chars>"}` — pass it to `confirm` together with the code. Valid for `OTP_TTL_SECS`. |
| `400` | `preferences must not be empty` |
| `403` | Any validation failure (length, mixed addresses, subject not allowed); body is a plain-text reason. Also returned if Redis is unavailable. |
| `401` | No/invalid token |

Note: the response is `200` even if delivering the code failed (the failure is logged). Retry `set` for a new code.

**Push addresses:** for the push channel the address is the *user id of the WebSocket that should receive
notifications* (normally your own `sub`). The code is delivered to that socket, so only that user can confirm.

### `POST {P}/{channel}/preferences/confirm`

```json
{ "nonce": "k3j2h4g5f6d7s8a9", "token": 123456 }
```

`token` is the 6-digit code as a number (100000–999999).

| Status | Meaning |
|---|---|
| `200` | Preferences stored; a [`contact.channel.confirmed`](EVENTS.md#published-event-contactchannelconfirmed) event is published per subject. Confirming again for the same subject **replaces** the address. |
| `500` | Wrong code, unknown/expired/already-used nonce, or an internal error (body: reason). |
| `401` | No/invalid token |

A wrong code does not consume the pending confirmation; a correct one does (single use). There is currently no
attempt limit — see [notes](INTEGRATION_NOTES.md#3-otp-codes-can-be-guessed-repeatedly).

### `GET {P}/{channel}/preferences/get?subject=<subject>`

Returns the confirmed address for the caller and subject.

| Status | Body |
|---|---|
| `200` | JSON string, e.g. `"me@example.com"` |
| `404` | No confirmed preference |
| `500` | Internal error |

Results are cached in Redis for `CACHE_TTL_SECS` and refreshed on `confirm`.

## Tags

A **tag** maps a name to a user id so events can address `{"Tag": "<name>"}` instead of a user id.
Standard CRUD provided by `viewset`. **These routes have no authentication** (see notes).

| Route | Description |
|---|---|
| `POST {P}/tags` | Body `{"tag":"team-a","user_id":"<uuid>"}` → `201` with `{"id","tag","user_id"}`. `tag` is unique. |
| `GET {P}/tags` | Paginated list *(response shape is the `viewset` page object; not verified)* |
| `GET {P}/tags/{id}` | One tag by its `id` (UUID) |
| `DELETE {P}/tags/{id}` | `204` |
| `PUT` / `PATCH {P}/tags/{id}` | Always rejected — the entity declares no update DTO |

Error status codes for these routes come from `viewset`'s `ApiError` mapping *(not verified; a malformed id is a validation error)*.

## Push WebSocket

```
GET {P}/ws/ws/          (Upgrade: websocket, Authorization: Bearer <jwt>)
```

The path really is `…/ws/ws/` (scope `/ws` + route `/ws/`) and the trailing slash is required.
Unauthenticated upgrades are rejected (`401`).

* One live connection per user id; a second connection replaces the first in the registry.
* The server pings every 5 s and closes connections that have not answered within 10 s. Standard WebSocket
  clients answer pings automatically.

**Server → client (notifications and OTPs)** — text frames:

```json
{ "id": "5e3c…", "source": "push", "payload": "<string>" }
```

`payload` is always a **string**: for an OTP it is the code (`"123456"`); for an event it is the whole event
envelope serialised as JSON (`JSON.parse(msg.payload).payload` is the event body).

**Client → server** — user-to-user relay (existing behaviour, not tied to the event stream):

```json
{ "type": "private", "to": "<user id>", "content": "hi" }
```

The recipient receives `{"from": "<sender id>", "content": "hi"}` as a text frame (note: a different shape
from notifications). Any authenticated user can message any connected user id.

## Examples

```bash
TOKEN=...   # HS256 JWT: {"sub":"<uuid>","aud":["notification"],"iat":…,"exp":…}

# 1. ask for a code (for the email channel)
curl -sX POST localhost:8080/notifications/email/preferences/set \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"preferences":[{"subject":"order.shipped","address":"me@example.com"}]}'
# {"nonce":"k3j2h4g5f6d7s8a9"}

# 2. confirm with the code from the email
curl -sX POST localhost:8080/notifications/email/preferences/confirm \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"nonce":"k3j2h4g5f6d7s8a9","token":123456}'

# 3. check
curl -s "localhost:8080/notifications/email/preferences/get?subject=order.shipped" \
  -H "Authorization: Bearer $TOKEN"
# "me@example.com"
```
