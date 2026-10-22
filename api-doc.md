API documentation for the **notification** service (Rust / Actix-web library).

---

# Notification Service API

## Overview

The notification service delivers messages over **email** and **push (WebSocket)** channels. Clients:

1. Register delivery addresses via an OTP-confirmed preference flow  
2. Optionally map human-readable **tags** to users  
3. Connect over WebSocket to receive push notifications  
4. Rely on an upstream event stream (NATS-style) that the service subscribes to for outbound delivery  

The service is implemented as a `ferrumec::Module` and is typically mounted under a configurable namespace (default: `notifications`).

**Base URL (example):** `https://api.example.com/notifications`

All routes under the module namespace require an authenticated **session cookie** (see Authentication).

---

## Authentication

Every route under the module scope is wrapped with session middleware (`RequiredSession` / `SessionMiddleware`).

| Mechanism | Description |
|-----------|-------------|
| Session cookie | UUID cookie that references a server-side session stored in the shared cache under a `session:` key. The session payload contains `user_id` (UUID). |
| Identity | Resolved into `viewset::web::Identity` with `sub` = authenticated user id (string form of the UUID). |

Sessions are **created by an upstream auth service**. This service only reads them.

Requests without a valid session receive the middleware’s unauthorized response (typically 401).

---

## Global Health

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `GET` | `/healthz` | None | Infrastructure health (Postgres, Redis, NATS latencies). Returns 503 if a dependency is down. |

Mounted by the infra layer, not under the module namespace.

---

## Module Routes

Assuming namespace = `notifications` (override with env `API_PREFIX`).

### 1. Preference endpoints (email & push)

Identical handlers are mounted twice:

| Channel | Base path |
|---------|-----------|
| Email | `/notifications/email` |
| Push | `/notifications/push` |

#### 1.1 Set preferences (start OTP flow)

```
POST /notifications/{email|push}/preferences/set
```

**Request body** (`application/json`):

```json
{
  "preferences": [
    {
      "subject": "order.shipped",
      "address": "user@example.com"
    },
    {
      "subject": "order.cancelled",
      "address": "user@example.com"
    }
  ]
}
```

| Field | Type | Constraints |
|-------|------|-------------|
| `preferences` | array | min length 1 |
| `preferences[].subject` | string | 1–64 chars; must be in the channel’s allowed subjects |
| `preferences[].address` | string | 1–64 chars; **all entries in a batch must share the same address** |

**Behavior:**

- Validates the batch and allowed subjects.
- Generates a 6-digit OTP and a 16-character alphanumeric **nonce**.
- Stores a pending entry in Redis (TTL = `otp_ttl`, default 300 s).
- Sends the OTP to the address via the channel sender (email provider or internal push).
- Returns the nonce so the client can confirm.

**Success response** `200 OK`:

```json
{
  "nonce": "a1b2c3d4e5f6g7h8"
}
```

**Error responses:**

| Status | When |
|--------|------|
| `400` | Empty preferences array |
| `403` | Validation failure, disallowed subject, mixed addresses, etc. |

> Note: If OTP delivery fails, the pending entry is still kept and the HTTP response remains `200`. The client may still call `/confirm` within the TTL.

---

#### 1.2 Confirm preferences

```
POST /notifications/{email|push}/preferences/confirm
```

**Request body:**

```json
{
  "nonce": "a1b2c3d4e5f6g7h8",
  "token": 123456
}
```

| Field | Type | Constraints |
|-------|------|-------------|
| `nonce` | string | Value returned by `/set` |
| `token` | u32 | 6-digit OTP (`100000`–`999999`) |

**Behavior:**

- Looks up the pending entry by `(user_id, nonce)`.
- Verifies the OTP.
- Writes each `(subject, address)` row into the channel’s preference table (`email_preferences` or `push_preferences`).
- Deletes the pending entry.

**Success:** `200 OK` (empty body)  
**Failure:** `500` with error message (invalid/expired nonce, wrong OTP, DB error, etc.)

---

#### 1.3 Get preference

```
GET /notifications/{email|push}/preferences/get?subject={subject}
```

**Query parameters:**

| Name | Required | Description |
|------|----------|-------------|
| `subject` | Yes | Event subject to look up |

**Success** `200 OK` — returns the stored preference entity (shape depends on channel):

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "subject": "order.shipped",
  "address": "user@example.com",
  "user_id": "user-uuid-string"
}
```

**Not found:** `404`  
**Error:** `500`

---

### 2. Notification tags

CRUD for mapping short tags to users. Used by the ID resolver so events can target a tag instead of a raw user UUID.

**Base path:** `/notifications/tags`

Implemented via `viewset` (`DefaultViewSet`). Typical REST shape:

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/notifications/tags` | List tags (supports viewset filters / pagination) |
| `POST` | `/notifications/tags` | Create a tag |
| `GET` | `/notifications/tags/{id}` | Retrieve one tag |
| `PUT` / `PATCH` | `/notifications/tags/{id}` | Update |
| `DELETE` | `/notifications/tags/{id}` | Delete |

**Entity:**

```json
{
  "id": "uuid",
  "tag": "ops-oncall",
  "user_id": "uuid"
}
```

**Create body:**

```json
{
  "tag": "ops-oncall",
  "user_id": "550e8400-e29b-41d4-a716-446655440000"
}
```

Table: `notification_tags`.

---

### 3. WebSocket push

```
GET /notifications/ws/ws/
```

Upgrade to WebSocket. Requires a valid session; the connection is bound to `auth.sub` (user id).

#### Server → client messages

Outbound notifications are delivered as JSON text frames:

```json
{
  "id": "uuid-v4",
  "source": "push",
  "payload": "the notification message string"
}
```

#### Client → server messages

Optional private messaging between connected users:

```json
{
  "msg_type": "private",
  "to": "target-user-id",
  "content": "hello"
}
```

Server forwards to the target (if online) as:

```json
{
  "from": "sender-user-id",
  "content": "hello"
}
```

Heartbeat / disconnect handling is managed by the Actix WebSocket actor.

---

## Event-driven delivery (internal)

The service does **not** expose a public “send notification” HTTP API for arbitrary clients. Delivery is driven by the **event stream**:

1. An upstream producer publishes an event with a subject (e.g. `order.shipped`).
2. The email and/or push modules are subscribed to configured subjects.
3. On event receipt the module:
   - Resolves the target via `Identifier` (UUID or tag → user UUID).
   - Looks up the user’s preferred address for that subject.
   - Calls the channel `Sender` (email provider or WebSocket bus).

**Email sender** wraps the message in a template context (`{{ value }}` if the payload is not already a JSON object).

**Push sender** pushes a `NotificationRequest` onto the in-process WebSocket bus.

---

## Data model summary

| Table | Purpose |
|-------|---------|
| `email_preferences` | Confirmed email addresses per user + subject |
| `push_preferences` | Confirmed push addresses (user ids) per user + subject |
| `notification_tags` | Tag → user_id mapping |

**Preference entity fields:** `id` (UUID), `subject`, `address`, `user_id` (string).

---

## Configuration (environment)

Relevant variables (see `ModuleConfig` / `AppConfig`):

| Variable | Purpose | Default / notes |
|----------|---------|-----------------|
| `API_PREFIX` | Module namespace | `notifications` |
| `BIND_ADDR` | HTTP listen address | (required for standalone) |
| `WORKERS` | Actix workers | optional |
| Email subjects | Comma-separated list | required for email channel |
| Push subjects | Comma-separated list | required for push channel |
| `otp_ttl` / cache TTL | Preference confirmation & cache lifetime | 300 s / 3600 s |
| Email provider | `Brevo`, `Resend`, `Gmail`, `Console` | + corresponding API keys / OAuth secrets |
| From address / name | Envelope sender | required for email |

---

## Error conventions

| HTTP status | Typical cause |
|-------------|---------------|
| `400` | Malformed body (empty batch, etc.) |
| `401` | Missing / invalid session |
| `403` | Preference validation / authorization failure |
| `404` | Preference not found for subject |
| `500` | Internal (DB, cache, OTP mismatch, provider error) |

Error bodies are plain text (`body(e.to_string())`) unless the handler returns a typed JSON response.

---

## Example flows

### Register an email address for a subject

```http
POST /notifications/email/preferences/set
Cookie: session=...
Content-Type: application/json

{
  "preferences": [
    { "subject": "order.shipped", "address": "alice@example.com" }
  ]
}
```

→ `{"nonce":"x7k9m2..."}`  
User receives OTP by email.

```http
POST /notifications/email/preferences/confirm
Cookie: session=...
Content-Type: application/json

{
  "nonce": "x7k9m2...",
  "token": 482913
}
```

→ `200 OK`

### Receive push notifications

1. Open WebSocket: `wss://api.example.com/notifications/ws/ws/` (with session cookie).  
2. Keep the connection alive.  
3. When an event arrives for a subject the user has a push preference for, a frame with `id` / `source` / `payload` is sent.

---

## Implementation notes

- Package: `notification` v0.1.0 (Rust 2024 edition).  
- Framework: Actix-web 4 + Actix actors for WebSocket.  
- Persistence: Postgres via `sqlx` + `viewset` entities.  
- Cache / pending OTPs: Redis (via `ferrumec` cache/store).  
- Events: `ferrumec::event::EventStream`.  
- Standalone server entry: `app::serve` mounts modules and `/healthz`.

This document describes the HTTP/WebSocket surface derived from the source in `notification.zip`.
