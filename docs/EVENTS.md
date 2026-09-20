# Events

The service consumes events from, and publishes events to, **NATS** using the wire format of
`typed-eventbus` 0.2.1. Machine-readable schemas: [`schemas/event-envelope.schema.json`](schemas/event-envelope.schema.json)
and [`schemas/channel-confirmed.schema.json`](schemas/channel-confirmed.schema.json). The e2e suite validates
published events against them.

## Envelope

Every event is one JSON document published on a NATS subject:

```json
{
  "metadata": {
    "event_id": "6f1c1c7e-3f0e-4c47-9a0e-2f6d1d3f9b11",
    "event_version": "v1",
    "occurred_at": "2026-09-19T06:00:00.123456Z",
    "producer": "orders",
    "correlation_id": null,
    "trace_id": null,
    "user_id": null,
    "audience": [
      { "Uuid": "a3b8f0c2-1d4e-4a57-8f2b-9c1e6d7a5b30" },
      { "Tag": "support-team" }
    ],
    "session_id": null
  },
  "payload": { "order_id": "ORD-1001" }
}
```

| Field | Required | Type | Notes |
|---|---|---|---|
| `metadata.event_id` | **yes** | UUID string | |
| `metadata.event_version` | **yes** | string | `"v1"` by default |
| `metadata.occurred_at` | **yes** | RFC 3339 timestamp | Use UTC with `Z` |
| `metadata.audience` | **yes** | array | Who to notify. May be empty (nobody is notified) |
| `metadata.producer` | no | string \| null | |
| `metadata.correlation_id`, `trace_id`, `user_id`, `session_id` | no | UUID string \| null | Not used by the notification service |
| `payload` | **yes** | any JSON | Opaque; forwarded to channels |

The service parses only `metadata`. An event whose `metadata` is missing or malformed is logged
(`Could not deserialize EventMetaData`) and dropped; it never stops other events.

### Audience identifiers

Externally tagged JSON, one of:

* `{"Uuid": "<user id>"}` — the user id (the JWT `sub`).
* `{"Tag": "<name>"}` — resolved through the tags table (`POST {P}/tags`) to a user id. Unknown tags are logged
  and skipped; the rest of the audience is still processed.

## Subjects

The **NATS subject** the event is published on selects the preferences and the email template:

* Each channel subscribes to the subjects in its env var (`EMAIL_SUBJECTS`, `PUSH_SUBJECTS`, `CONSOLE_SUBJECTS`).
  A subject may be configured for several channels.
* For each audience member the service looks up that user's confirmed address for `(channel, subject)`.
  Members without one are skipped silently (that is the normal opt-out case).
* Publishing on a subject nobody configured does nothing.
* Wildcards: the configured strings are passed to NATS as-is, but preferences are matched by exact subject
  string, so use concrete subjects.

## What each channel does with an event

| Channel | Delivery |
|---|---|
| `email` | Renders `templates/<subject>.html` with the full event as context (`payload.*`, `metadata.*`) and sends it via the provider API to the confirmed address. |
| `push` | Pushes `{"id","source":"push","payload":"<event JSON string>"}` to the WebSocket of the user whose id equals the confirmed address. Dropped if that user is not connected (no offline queue). |
| `console` | Prints the event to stdout. Development only. |

## Delivery semantics (important)

`typed-eventbus` 0.2.1 uses **core NATS**, not JetStream:

* **At-most-once.** Events published while the service is down or reconnecting are lost. No replay.
* **No queue group.** Every replica receives every event, so with N replicas an email is sent N times
  (push is unaffected because only the replica holding the socket delivers). Run one replica until this is
  addressed — see [notes](INTEGRATION_NOTES.md#2-more-than-one-replica-duplicates-emails).
* Handlers run inline per subscription; there is no retry on delivery failure (errors are logged).

## Published event: `contact.channel.confirmed`

Published (best effort) once per subject when a user confirms an address.

```json
{
  "metadata": {
    "event_id": "…", "event_version": "v1", "occurred_at": "2026-09-19T06:01:12.004211Z",
    "producer": "mgk", "correlation_id": null, "trace_id": null, "user_id": null,
    "audience": [], "session_id": null
  },
  "payload": { "user": "a3b8f0c2-1d4e-4a57-8f2b-9c1e6d7a5b30", "channel": "email", "address": "me@example.com" }
}
```

| Field | Description |
|---|---|
| `payload.user` | User id (UUID string) |
| `payload.channel` | `email`, `push` or `console` |
| `payload.address` | The confirmed address |

It does not say *which subject* was confirmed (one event per subject, identical payloads).

## Publishing events

**Rust** (`typed-eventbus`):

```rust
#[derive(serde::Serialize)]
struct OrderShipped { order_id: String }
impl typed_eventbus::Publishable for OrderShipped { const SUBJECT: &'static str = "order.shipped"; }

typed_eventbus::Event::new(OrderShipped { order_id: "ORD-1001".into() })
    .with_producer("orders")
    .with_audience(vec![user_id])            // Uuid, or Identifier::Tag(..)
    .publish(bus.clone()).await?;
```

**NATS CLI**:

```bash
nats pub order.shipped '{
  "metadata": {"event_id":"6f1c1c7e-3f0e-4c47-9a0e-2f6d1d3f9b11","event_version":"v1",
               "occurred_at":"2026-09-19T06:00:00Z","audience":[{"Uuid":"a3b8f0c2-1d4e-4a57-8f2b-9c1e6d7a5b30"}]},
  "payload": {"order_id":"ORD-1001"}
}'
```

**Python** (`nats-py`): see `make_event` and `NatsBus.publish` in `tests/e2e/helpers.py`.
