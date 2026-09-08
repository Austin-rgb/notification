# notification

A Rust/Actix Web microservice for the Ferrumec ecosystem that manages per-user
notification preferences and delivers notifications over three channels —
**email**, **push** (WebSocket), and **console** — driven by events on a
shared NATS-based event bus (`typed-eventbus`).

## How it works

1. A producer service publishes an event to the bus (via `typed-eventbus`),
   addressed to an *audience* of `Identifier`s (a `Uuid` or a user-defined
   `Tag`).
2. This service subscribes to the configured subjects for each channel
   (email/push/console). When a matching event arrives, it:
   - resolves each audience identifier to a `user_id`,
   - looks up that user's preference (has the user opted into this
     subject on this channel, and what address/target should receive it),
   - hands the message off to the channel's `Sender` for delivery.
3. Users opt in to a channel/subject via a two-step, OTP-confirmed flow:
   `POST /preferences/set` → OTP delivered to the target address →
   `POST /preferences/confirm` with the OTP → preference is written and a
   `contact.channel.confirmed` event is published.

## Channels

| Channel | Delivery | Backend |
|---|---|---|
| `email` | Brevo or Resend, HTML rendered with Tera templates | `emailgrid` |
| `push`  | Actix WebSocket session per connected user | `push` |
| `console` | Logs to stdout (useful for local dev/testing) | `Console` sender in `lib.rs` |

Each channel gets its own preference table (`EmailPreference`,
`PushPreference`, `ConsolePreference`) and its own allow-list of subjects it
will react to, configured independently.

## Tagging

In addition to addressing events directly by `user_id`, producers can
address an audience by an opaque `Tag` string. Tags are registered via the
`/tags` CRUD endpoints (backed by `viewset`) and resolved to a `user_id` by
`MyIdResolver` at delivery time.

## API

All routes below are mounted under whatever namespace the host application
passes to `Module::config`.

### Preferences (per channel: `/email`, `/push`, `/console`)

| Method | Path | Description |
|---|---|---|
| `POST` | `/{channel}/preferences/set` | Submit a batch of `{subject, address}` preferences (all sharing one address). Returns a `nonce`; sends an OTP to the address. |
| `POST` | `/{channel}/preferences/confirm` | Confirm with `{nonce, token}`. Writes the preference rows and publishes `contact.channel.confirmed`. |
| `GET` | `/{channel}/preferences/get?subject=...` | Look up the confirmed address for the caller + subject. |

All three require an authenticated `Identity` (via `actixutils::Auth`).

### Push / WebSocket

| Method | Path | Description |
|---|---|---|
| `GET` | `/push/ws/` | Upgrade to a WebSocket connection for the authenticated user. Supports heartbeat ping/pong and `{"type": "private", "to": ..., "content": ...}` client messages for direct peer messaging. |

### Tags

| Method | Path | Description |
|---|---|---|
| `GET/POST/PUT/DELETE` | `/tags` | Standard CRUD viewset over `{tag, user_id}`. |

## Configuration

Set via environment variables before startup:

| Variable | Purpose |
|---|---|
| `email.subjects` | Comma-separated list of event subjects the email channel subscribes to |
| `push.subjects` | Comma-separated list of event subjects the push channel subscribes to |
| `console.subjects` | Comma-separated list of event subjects the console channel subscribes to |
| `BREVO_API_KEY` | API key for the Brevo email backend (if used) |
| `RESEND_API_KEY` | API key for the Resend email backend (if used) |

> All three `*.subjects` variables are required at startup — the service
> panics on boot if any is unset.

Email HTML bodies are rendered from Tera templates under `./templates/`,
one file per subject (`{subject}.html`), loaded once at startup.

## Integration

```rust
let module = notification::Module::new(
    pg_pool,
    emailing_context,   // EmailingContext (Brevo/Resend + templates)
    identity_validator, // Arc<dyn Validate<Identity>>
    event_stream,        // Arc<dyn EventStream>
).await?;

// in your actix_web App::configure:
module.config(&mut cfg, "/notifications");
```

## Dependencies

Built on the Ferrumec Rust stack: `actix-web`, `actix` / `actix-web-actors`
(WebSocket actors), `sqlx` (Postgres), `moka` (in-memory caching for
preferences and pending OTPs), `typed-eventbus` (event bus pub/sub),
`viewset` (CRUD scaffolding), `actixutils` (auth/identity), `validator`,
`tera` (email templates), `reqwest` (email API calls).

## Known limitations

- Each user_id supports a single active WebSocket connection at a time;
  connecting from a second device will replace the first.
- Email delivery failures from the provider API are not currently
  surfaced as errors back to the caller of `/preferences/set` — check
  service logs.
