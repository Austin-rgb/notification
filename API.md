# Notification Service API

## 1. Overview

The Notification Service exposes authenticated HTTP endpoints for email
and push-notification preferences, in-app notification retrieval,
notification tags, and a WebSocket connection for real-time delivery.

The module is mounted under a configurable namespace. Unless configured
otherwise, the namespace is `/notifications`.

All routes mounted by the notification module are protected by session
middleware. Requests must include the session cookie configured by
`SESSION_COOKIE` (default: `session`). The cookie refers to a session
created by the upstream authentication service; this service reads the
session and uses its authenticated subject as the user ID.

Base URL examples:

-   Local default: `http://localhost:8080/notifications`
-   Email preferences: `/notifications/email/...`
-   Push preferences: `/notifications/push/...`
-   In-app notifications: `/notifications/in-app`
-   WebSocket: `/notifications/ws/`
-   Tags: `/notifications/tags` (CRUD paths depend on the `viewset`
    service conventions)

The API uses JSON for request and response bodies where applicable.
Exact host, TLS, and deployment prefix depend on the deployment.

## 2. Authentication

Send the session cookie on each HTTP request and on the WebSocket
handshake:

``` http
Cookie: session=<session-id>
```

Replace `session` with the value configured by `SESSION_COOKIE` if
different. The cookie must identify a valid server-side session. The
authenticated user ID is taken from that session; preference and in-app
endpoints do not accept a user ID from the client to select another
user.

Unauthenticated or invalid-session behavior is enforced by middleware
and may depend on the shared authentication/session implementation.

## 3. Email and push preferences

The same preference workflow is exposed for both delivery channels:

-   Email: `/email/preferences/...`
-   Push: `/push/preferences/...`

A preference maps an event-stream `subject` to a delivery `address`. For
email, `address` is an email address. For push, it is the target
identifier understood by the push delivery implementation.

A preference is not active until its address is confirmed with a
one-time password (OTP).

### 3.1 Set preferences

`POST /{channel}/preferences/set`

Starts a pending preference update and sends an OTP to the address
supplied in the request. `{channel}` is `email` or `push`.

Request:

``` http
POST /notifications/email/preferences/set
Content-Type: application/json
Cookie: session=<session-id>
```

``` json
{
  "preferences": [
    {
      "subject": "blogs.comment.created",
      "address": "person@example.com"
    },
    {
      "subject": "blogs.post.published",
      "address": "person@example.com"
    }
  ]
}
```

Fields:

  ----------------------------------------------------------------------------------
  Field                     Type                          Required Description
  ------------------------- ---------------- --------------------- -----------------
  `preferences`             array                              Yes One or more
                                                                   preference
                                                                   entries to set
                                                                   together.

  `preferences[].subject`   string                             Yes Event subject to
                                                                   subscribe this
                                                                   user to. Length:
                                                                   1--64 characters.
                                                                   Must be allowed
                                                                   by server
                                                                   configuration for
                                                                   the selected
                                                                   channel.

  `preferences[].address`   string                             Yes Delivery
                                                                   address/target.
                                                                   Length: 1--64
                                                                   characters. Every
                                                                   entry in one
                                                                   batch must use
                                                                   the same address.
  ----------------------------------------------------------------------------------

Success --- `200 OK`:

``` json
{
  "nonce": "0123456789abcdef"
}
```

The `nonce` is an opaque server-side handle. Store it temporarily and
send it with the OTP to the confirmation endpoint. The OTP is delivered
out-of-band to the supplied address; it is not returned in the HTTP
response.

Errors: - `400 Bad Request` --- the preferences array is empty. -
`403 Forbidden` --- invalid preference data, inconsistent addresses
within the batch, or a subject not allowed for this channel. The
response body contains an error string. - A delivery-provider failure
while sending the OTP is logged. The endpoint can still return `200 OK`
because the pending confirmation has already been stored; the user may
retry confirmation while the pending entry remains valid.

### 3.2 Confirm preferences

`POST /{channel}/preferences/confirm`

Confirms the pending batch using the nonce returned by the set endpoint
and the OTP delivered to the address.

Request:

``` http
POST /notifications/email/preferences/confirm
Content-Type: application/json
Cookie: session=<session-id>
```

``` json
{
  "nonce": "0123456789abcdef",
  "token": 123456
}
```

Fields:

  ----------------------------------------------------------------------------
  Field            Type                          Required Description
  ---------------- ---------------- --------------------- --------------------
  `nonce`          string                             Yes Nonce returned by
                                                          `preferences/set`.

  `token`          integer                            Yes Six-digit OTP
                                                          delivered
                                                          out-of-band. Valid
                                                          range:
                                                          100000--999999.
  ----------------------------------------------------------------------------

Success --- `200 OK`, empty response body.

On success, all entries in the batch are persisted. Setting an existing
`(user, subject)` preference updates its address rather than creating a
duplicate. The pending confirmation is consumed after the OTP is
verified.

Errors: - `500 Internal Server Error` --- invalid or expired nonce/OTP,
missing pending entry, or a persistence/confirmation error. The response
body contains an error string.

The pending confirmation entry is stored with a 30-minute cache TTL in
the current implementation. The configured `OTP_TTL_SECS` setting is
present in configuration, but the preference implementation currently
creates the pending-entry cache with a fixed 30-minute TTL.

### 3.3 Get a preference

`GET /{channel}/preferences/get?subject={subject}`

Returns the authenticated user's configured address for one event
subject.

Example:

``` http
GET /notifications/email/preferences/get?subject=blogs.comment.created
Cookie: session=<session-id>
```

Success --- `200 OK`:

``` json
"person@example.com"
```

The response is a JSON string, not an object.

Errors: - `404 Not Found` --- no preference exists for the authenticated
user and subject. - `500 Internal Server Error` --- the preference could
not be read.

### 3.4 Subject restrictions and delivery behavior

Allowed subjects are configured separately for email and push using
`EMAIL_SUBJECTS` and `PUSH_SUBJECTS`. These are comma-separated lists of
event subjects. A subject not configured for the selected channel cannot
be added through the preference API.

When an event arrives, the service resolves each audience identifier to
a user ID, looks up that user's preference for the event subject, and
sends the event only when a preference exists. Missing preferences skip
that audience member; they do not prevent delivery to other audience
members.

## 4. In-app notifications

### List the authenticated user's notifications

`GET /in-app`

Returns notifications stored for the authenticated user.

Example:

``` http
GET /notifications/in-app
Cookie: session=<session-id>
```

Success --- `200 OK`:

``` json
{
  "items": [
    {
      "id": "4a8a4b76-9d0b-4b4d-8f28-5f6a2d88a6c2",
      "user_id": "f87c0b4b-7c83-4cc5-9f6d-7b9f3a6d2d10",
      "subject": "blogs.comment.created",
      "payload": {
        "metadata": {},
        "data": {}
      },
      "created_at": "2026-01-15T10:30:00Z"
    }
  ],
  "count": 1
}
```

The values above are illustrative. `payload` contains the event as
published, represented as JSON; if the incoming message is not valid
JSON, it is stored as a JSON string. `created_at` is the stored creation
timestamp.

The endpoint accepts query parameters interpreted as `viewset` filters.
The implementation always overrides the `user_id` filter with the
authenticated user's ID, so clients cannot use this endpoint to list
another user's notifications. Consult the `viewset` conventions used by
this service for supported filter syntax and pagination behavior.

Errors: - `500 Internal Server Error` --- the notification list could
not be read.

In-app event subjects are configured by `IN_APP_SUBJECTS`. If unset, the
service uses the configured push subject list.

## 5. WebSocket real-time notifications

### Connect

`GET /ws/` --- WebSocket upgrade

Connect to the WebSocket endpoint using the authenticated session
cookie.

Example URL:

``` text
ws://localhost:8080/notifications/ws/
```

Use `wss://` in deployments using TLS. The endpoint is mounted at `/ws/`
with a trailing slash.

The server sends notification messages as text frames. A delivered
notification has this JSON shape:

``` json
{
  "id": "9e4b1f5d-1b73-4b82-8ac2-bb6c9f0a91b1",
  "source": "push",
  "payload": "{\"metadata\":{},\"data\":{}}"
}
```

Fields:

  -----------------------------------------------------------------------
  Field                   Type                    Description
  ----------------------- ----------------------- -----------------------
  `id`                    string                  Server-generated UUID
                                                  for this WebSocket
                                                  message.

  `source`                string                  Source label. The
                                                  current push-preference
                                                  delivery path uses
                                                  `push`.

  `payload`               string                  Message content as a
                                                  string. For
                                                  event-driven push
                                                  delivery this is the
                                                  serialized event JSON.
  -----------------------------------------------------------------------

The `payload` field is a string even when it contains serialized JSON.
Clients that need to inspect the event should parse the string
separately.

### Client-to-server private messages

The WebSocket implementation also accepts text frames in this shape:

``` json
{
  "type": "private",
  "to": "recipient-user-id",
  "content": "Hello"
}
```

When `type` is `private`, the server forwards the message to the
connected recipient as a JSON text frame:

``` json
{
  "from": "sender-user-id",
  "content": "Hello"
}
```

Other message types or malformed JSON are ignored. This is a
connected-user relay, not a durable messaging API: the recipient must be
connected at delivery time.

### Heartbeat and connection lifecycle

The server sends WebSocket pings every five seconds. A connection is
stopped when no heartbeat response has been observed for more than ten
seconds. Clients should respond to ping frames according to the
WebSocket protocol and reconnect after disconnection.

## 6. Notification tags

The tag resource is mounted under `/tags` (for example,
`/notifications/tags`) and is implemented using the project's generated
`DefaultViewSet` CRUD service.

The underlying resource fields are:

  -----------------------------------------------------------------------
  Field                   Type                    Description
  ----------------------- ----------------------- -----------------------
  `id`                    UUID                    Server-generated
                                                  identifier.

  `tag`                   string                  Tag identifier used to
                                                  resolve an event
                                                  audience identifier to
                                                  a user.

  `user_id`               UUID                    User associated with
                                                  the tag.
  -----------------------------------------------------------------------

The create model accepts `tag` and `user_id`. The update model permits
`user_id`; `tag` and `id` are not updateable. Exact HTTP methods, item
paths, list filters, and response envelopes are provided by the
`viewset::web::DefaultService` conventions and should be confirmed
against the version of that dependency used by the deployment; they are
not explicitly declared in this repository's route file.

Tags are used when event metadata audience entries contain a tag
identifier. The service resolves the tag through the `notification_tags`
table to obtain the associated user UUID.

## 7. Health check

The standalone HTTP server exposes:

`GET /healthz`

This route is configured by the infrastructure implementation, not by
the notification module itself. It reports infrastructure health,
including Postgres, Redis, and NATS status/latency; the documented
implementation returns `503` when an infrastructure dependency is down.
The response schema is defined by the configured `Infra` implementation.

## 8. Configuration reference

The following environment variables affect API behavior and deployment:

  -------------------------------------------------------------------------------
  Variable                                    Required Default / notes
  ----------------------- ---------------------------- --------------------------
  `BIND_ADDR`                                       No `0.0.0.0:8080`

  `WORKERS`                                         No Framework default if
                                                       omitted; if supplied, must
                                                       be greater than zero.

  `API_PREFIX`                                      No `notifications`;
                                                       surrounding slashes are
                                                       removed.

  `SESSION_COOKIE`                                  No `session`

  `SESSION_TTL_SECS`                                No `86400` seconds; session
                                                       storage is managed by the
                                                       shared cache/auth
                                                       integration.

  `EMAIL_PROVIDER`                                 Yes `brevo`, `resend`,
                                                       `gmail`, or `console`.

  `BREVO_API_KEY`                          Conditional Required when
                                                       `EMAIL_PROVIDER=brevo`.

  `RESEND_API_KEY`                         Conditional Required when
                                                       `EMAIL_PROVIDER=resend`.

  `GMAIL_CLIENT_ID`                        Conditional Required when
                                                       `EMAIL_PROVIDER=gmail`.

  `GMAIL_CLIENT_SECRET`                    Conditional Required when
                                                       `EMAIL_PROVIDER=gmail`.

  `GMAIL_REFRESH_TOKEN`                    Conditional Required when
                                                       `EMAIL_PROVIDER=gmail`.

  `EMAIL_FROM_ADDRESS`                         Usually Required except for
                                                       `console`; console
                                                       defaults to
                                                       `noreply@localhost`.

  `EMAIL_FROM_NAME`                                 No `Notifications`

  `EMAIL_SUBJECTS`                                 Yes Comma-separated event
                                                       subjects enabled for
                                                       email.

  `PUSH_SUBJECTS`                                  Yes Comma-separated event
                                                       subjects enabled for push.

  `IN_APP_SUBJECTS`                                 No Comma-separated in-app
                                                       subjects; defaults to
                                                       `PUSH_SUBJECTS`.

  `CACHE_TTL_SECS`                                  No `3600` seconds for
                                                       resolved address cache
                                                       entries.

  `OTP_TTL_SECS`                                    No Configuration default is
                                                       `300` seconds; see the
                                                       pending-entry TTL note in
                                                       section 3.2.

  `DATABASE_URL`                        Infrastructure Postgres connection string
                                                       read by the remote
                                                       infrastructure
                                                       implementation.

  `REDIS_URL`                           Infrastructure Redis connection string
                                                       read by the remote
                                                       infrastructure
                                                       implementation.

  `NATS_URL`                            Infrastructure NATS connection string
                                                       read by the remote
                                                       infrastructure
                                                       implementation.
  -------------------------------------------------------------------------------

`EMAIL_PROVIDER=console` is intended for development and writes email
content to stdout rather than delivering it through a provider.

## 9. Error-handling notes

The endpoints do not share one uniform error envelope:

-   Some failures return an empty response body (for example, `404` from
    preference lookup).
-   Preference set/confirm failures may return a plain-text error
    string.
-   In-app list failures return an empty `500` response.
-   Event-delivery and subscription failures are logged and are not
    necessarily reflected in the original HTTP response.

Clients should rely on HTTP status codes first and should not assume
every error body is JSON.

## 10. Operational and security considerations

-   Keep session cookies secure and use TLS in production.
-   Do not expose OTPs or session identifiers in client logs.
-   A successful preference-set response means the pending request was
    recorded; it does not mean the address has been confirmed.
-   Treat the preference `nonce` as temporary sensitive state and pair
    it with the OTP only for confirmation.
-   WebSocket push delivery is transient; clients requiring durable
    history should use the in-app notification endpoint.
-   The current WebSocket server stores one active connection per user
    ID; a new connection for the same user replaces the prior connection
    in the in-memory map.
-   The `GET /in-app` response contains user-specific data and is scoped
    to the authenticated session.

## 11. Endpoint summary

  --------------------------------------------------------------------------------------
  Method                  Path (relative to namespace)           Purpose
  ----------------------- -------------------------------------- -----------------------
  `POST`                  `/email/preferences/set`               Start email preference
                                                                 update and send OTP.

  `POST`                  `/email/preferences/confirm`           Confirm email
                                                                 preference update.

  `GET`                   `/email/preferences/get?subject=...`   Get an email preference
                                                                 address.

  `POST`                  `/push/preferences/set`                Start push preference
                                                                 update and send OTP.

  `POST`                  `/push/preferences/confirm`            Confirm push preference
                                                                 update.

  `GET`                   `/push/preferences/get?subject=...`    Get a push preference
                                                                 target.

  `GET`                   `/in-app`                              List the authenticated
                                                                 user's in-app
                                                                 notifications.

  `GET` + WebSocket       `/ws/`                                 Connect for real-time
  upgrade                                                        notifications and
                                                                 private relay messages.

  Generated CRUD          `/tags`                                Manage notification
                                                                 tags; see section 6 for
                                                                 schema and dependency
                                                                 note.

  `GET`                   `/healthz`                             Infrastructure health
                                                                 check (standalone
                                                                 server).
  --------------------------------------------------------------------------------------
