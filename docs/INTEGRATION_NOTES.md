# Integration notes

Read this before the first build. It records what was changed in the existing code, what has and has not been
verified, and the problems found that need a decision from you.

## What is verified and what is not

The environment this was written in had **no Rust toolchain, no Docker and no network**. So:

| Verified | How |
|---|---|
| API shapes of `typed-eventbus` 0.2.1, `actixutils` 0.6.7, `viewset` 0.4.2 / `viewset-macros` 0.2, sqlx 0.9 feature names | Read from their published sources/docs |
| Delimiter balance of every Rust file | Script |
| Python: stub email server behaves as specified | Executed |
| Python: helpers (JWT minting, event envelope, inbox), test discovery (24 tests), compose YAML, JSON Schemas parse | Executed / parsed |

| **Not** verified | Consequence |
|---|---|
| The Rust code compiles | Expect to fix a few small errors on the first `cargo check` (most likely spots: `redis` 1.x API details in `src/kv.rs`, trait-object bounds on `dyn Validate<Identity>` / `dyn EventStream`, the exact `Filters` conversion in `Preferences::get`). |
| The service starts and serves | Untested end to end |
| The e2e suite passes | It has never run against a live stack. Failures may be test bugs as well as service bugs |
| `HS256Signer`'s expectations for `aud`/`iss` | Its source was not readable; tests send several plausible audiences. See README "If every authenticated call returns 401" |
| Status codes of `viewset` error responses | Marked *(inferred)* in API.md |

Suggested order: `cargo check` → `docker build .` → `python tests/e2e/run_e2e.py`.

## Changes made to your existing code

Every change is needed for the service to boot or for the described flows to work; none adds features. In the
order you would review them:

| File | Change | Why |
|---|---|---|
| `Cargo.toml` | Added `[lib]`/`[[bin]]`, `actixutils` feature `jwt`, `redis`, `tokio`, `tracing-subscriber`; serde `derive`; sqlx `runtime-tokio`, `tls-rustls-ring-webpki`, `migrate`, `macros`; removed `moka`, unused now | `Jwt`/`HS256Signer` only exist with `jwt`; sqlx needs a runtime feature and `migrate` for embedded migrations |
| `src/lib.rs` | `mod read_session;` commented out | The file does not compile (`'_` in a struct, `from_request` returns `Self`, missing imports) and `actixutils::ReadSession` now exists. File left untouched — delete it when convenient |
| `src/lib.rs`, `handlers.rs`, `push/ws.rs` | `use actixutils::Auth` → `Jwt as Auth` | `Auth` was renamed `Jwt` in actixutils 0.2 and is not exported at the root |
| `src/lib.rs` | Entities: explicit `table = "email_preferences"` etc.; field `user` → `user_id`; `subject` and `user_id` marked `filterable` | See "Why the entities changed" below |
| `src/tagging.rs` | `table = "notification_tags"` | The ID resolver reads `notification_tags`, but the derive defaults to `tags`, so tags created over the API were never found |
| `src/mgk/mod.rs` | `CreatePreference.user` → `user_id`; `Module::new` takes `kv` + `settings`; **`Ok(None) => return` → `continue`** | Rename as above. The `return` aborted the whole audience loop as soon as one member had no preference, silently dropping everyone after them |
| `src/mgk/prefs/db.rs` | Pending OTPs and the address cache moved from in-process `moka` to Redis (`KvStore`); confirm writes with an upsert; cache errors fall back to the DB | Multi-replica correctness (an OTP requested on one replica could not be confirmed on another; a confirmed address stayed stale on the others). `Preferences::confirm` now uses `ON CONFLICT (user_id, subject) DO UPDATE` |
| `src/lib.rs` (`Email::send`) | Returns provider/template/HTTP-status errors instead of always `Ok(())` | The `Sender` trait says implementations must return errors; failures were invisible |
| `src/emailgrid/utils.rs` | Non-object messages (the OTP `"123456"`) are exposed to templates as `{{ value }}` | Tera cannot build a context from a bare number, so OTP emails could never render |
| `src/emailgrid/emailing.rs` | `Brevo`/`Resend` are named-field structs with `with_config(key, url)`; endpoint overridable | So the e2e stack can point the real sender at a stub. `Brevo(key)` tuple construction no longer exists |
| `migrations/` | The two old files replaced by `0001_notification_tags.sql`, `0002_channel_preferences.sql` | Old `0002` used a column named `user`, which PostgreSQL rejects, and neither file matched the entities |
| env vars | `email.subjects` etc. → `EMAIL_SUBJECTS` etc. | Dotted names can't be set from a shell; `get_list` also panicked on absence |

New files: `src/{main,config,infra,kv,app,telemetry}.rs`, `build.rs`, `Dockerfile`, `.dockerignore`,
`docker-compose.e2e.yml`, `templates/confirm address.html`, `tests/e2e/*`, `docs/*`.

### Why the entities changed

Three separate defects combined:

1. **Wrong tables.** `viewset-macros` defaults the table to `lowercase(struct name) + "s"` (`emailpreferences`,
   `tags`). The migrations created different tables (`preferences`, `notification_tags`) with different columns.
2. **`user` is a reserved word in PostgreSQL** and the generated INSERT/SELECT does not quote identifiers, so
   `INSERT INTO … (subject, address, user)` is a syntax error. (The commented-out `? ` placeholders in the
   original suggest this code was ported from SQLite.)
3. **Unfiltered lookups leaked other users' data.** `viewset`'s `list` silently ignores filters on columns not
   declared `#[entity(filterable)]`. `Preferences::get` filtered on `user` and `subject`, neither of which was
   declared, so it returned the **first row in the table** — for any user — and cached it. Notifications would be
   routed to someone else's address. The e2e test `test_preferences_are_isolated_per_user` guards against a
   regression.

## Known issues — decisions needed

Not fixed here because each needs a product/architecture decision. The first three are the ones to resolve
before production.

### 1. The tags API is unauthenticated

`viewset`'s handlers take no identity, and the service does not add auth. Anyone who can reach
`{P}/tags` can map any tag to any user id — including creating a tag such as `support-team` pointing at
themselves, then receiving every notification addressed to it. Authentication alone would not be enough
(there is no admin role in `Identity`).

Options: expose it only on an internal listener / block it at the gateway; require a service token with an
admin role (`actixutils::Authority` has a `role` bitmask); or drop the HTTP API and manage tags via migrations
or an internal event. The e2e test `test_tags_api_requires_authentication` is an `xfail` that will start
passing once this is fixed.

### 2. More than one replica duplicates emails

`typed-eventbus` 0.2.1's `NatsEventStream::subscribe` creates a plain subscription with no queue group, so
every replica receives every event and every replica's email/console channel sends. Push is unaffected.
Run one replica until you decide between: queue-group support in `typed-eventbus`, or a Redis
`SET NX` de-duplication keyed by `(channel, event_id, user)` for the email/console channels only. The same
NATS mode is also at-most-once: events published while the service is down are lost (JetStream would fix that).

### 3. OTP codes can be guessed repeatedly

Six digits, valid for 5 minutes, no attempt limit. An attacker with any valid token can attach an arbitrary
address to their account, i.e. make the service send notifications to a victim. Add an attempt counter in
Redis (e.g. delete the pending entry after 5 wrong codes) and/or rate-limit `confirm` with
`actixutils::middleware::RateLimiter`.

### 4. Smaller items

* **Wrong or expired code → HTTP 500; every `set` failure → 403.** Status mapping in `handlers.rs` uses
  `InternalServerError`/`Forbidden` for all errors, including infrastructure failures.
* **`set` returns 200 even if the OTP could not be delivered** (logged only).
* **WebSocket path is `…/ws/ws/`** (scope `/ws` + route `/ws/`). Probably unintended; kept so existing clients
  keep working.
* **Push registry is per process, one connection per user id.** A second connection replaces the first, and
  closing the first then unregisters the second. Multi-device users need a `Vec` of recipients.
* **Any authenticated user can WebSocket-message any user id** (`{"type":"private",…}`).
* **The `console` channel prints message bodies — including OTPs — to stdout.** Development only; give it a
  subject nobody publishes to in production.
* **RS256 is not wired.** Only the shared-secret `HS256Signer` is used. For an auth service that signs with a
  private key, construct `RS256Validator` in `NotificationService::build` (its constructor was not verified).
* **No TLS for Redis.** `REDIS_URL` must be `redis://`. For `rediss://` enable the appropriate `redis` TLS
  feature in `Cargo.toml`. PostgreSQL TLS is supported (`sslmode=require` in `DATABASE_URL`).
* **`Brevo`/`Resend` build a new `reqwest::Client` per email** (no connection reuse), and Brevo requests are
  sent with an empty recipient `name` and an empty `attachments` array; check both against your provider.
* **`/readyz` does not probe NATS.** The client reconnects on its own but exposes no cheap health check.
* **`ChannelConfirmed` carries no subject**, so consumers cannot tell which subject was confirmed. Adding a
  `subject` field would be backwards compatible.
* **`WORKERS` and `startup_timeout`** are conservative defaults; tune for your deployment.
