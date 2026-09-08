# Notification Preferences Client

A lightweight Python client for interacting with a Notification Preferences Service. The client provides both synchronous and asynchronous APIs to manage default notification channels and user-specific preferences.

This repository contains a single-file client implementation (client.py) that implements common operations such as setting/getting defaults, setting/getting user preferences, batch operations, health checks, and retry configuration.

Features

- Synchronous and asynchronous interfaces (requests + aiohttp)
- Support for common delivery channels: email, push, sms, in_app
- Convenience dataclasses for responses and batch requests
- Configurable retry behavior and request timeouts
- Context-manager support for both sync and async usage
- Basic error handling via typed exceptions

Quickstart

Requirements

- Python 3.8+
- requests
- aiohttp

Install

You can install the client directly from this repository (no PyPI package published):

pip install git+https://github.com/Austin-rgb/notification.git

Or copy `client.py` into your project and import NotificationPrefsClient directly.

Basic usage (synchronous)

```python
from client import NotificationPrefsClient, Channel

client = NotificationPrefsClient("http://localhost:8080", api_version="v1")

# Set a default channel
client.set_default("marketing", Channel.EMAIL)

# Get the default channel
channel = client.get_default("marketing")
print(channel)

# Set a user preference
client.set_preference("user123", "marketing", Channel.PUSH)

# Get effective preference (falls back to default if no user preference)
pref = client.get_preference("user123", "marketing")
print(pref)

# Use context manager
with NotificationPrefsClient("http://localhost:8080", api_version="v1") as client:
    client.set_default("security", Channel.PUSH)
```

Basic usage (asynchronous)

```python
import asyncio
from client import NotificationPrefsClient, Channel

async def main():
    async with NotificationPrefsClient("http://localhost:8080") as client:
        await client.async_set_preference("user789", "marketing", Channel.EMAIL)
        pref = await client.async_get_preference("user789", "marketing")
        print(pref)

# asyncio.run(main())
```

API reference (high level)

Classes

- NotificationPrefsClient
  - __init__(base_url, api_version='v1', timeout=30.0, retry_config=None, headers=None, verify_ssl=True)
  - set_default(subject, channel)
  - get_default(subject)
  - get_default_or_none(subject)
  - set_preference(user, subject, channel)
  - get_preference(user, subject) -> PreferenceResponse
  - get_preference_or_none(user, subject)
  - batch_set_defaults(defaults: List[dict])
  - batch_set_preferences(preferences: List[BatchPreferenceRequest])
  - health_check()
  - async equivalents: async_set_default, async_get_default, async_set_preference, async_get_preference, async_batch_set_preferences
  - close(), async_close(), context manager support (__enter__/__exit__, __aenter__/__aexit__)

- Channel (Enum): EMAIL, PUSH, SMS, IN_APP

- PreferenceResponse (dataclass): user, subject, channel, is_default

- BatchPreferenceRequest (dataclass): user, subject, channel

Exceptions

- APIError: Base exception for API-related errors
- NotFoundError: Raised when a resource is not found (404)
- ServerError: Raised for server-side errors (5xx)
- ValidationError: Raised for client-side validation errors (4xx)

Configuration

- Retry behavior can be customized via the RetryConfig class passed to the client constructor. The default retry strategy retries on 429 and 5xx statuses and performs exponential backoff.
- Additional headers can be provided via the headers parameter.
- SSL verification is configurable via verify_ssl.

Repository layout

- client.py - Primary client implementation and usage examples
- migrations/ - (empty directory placeholder for database migrations)
- src/ - (empty directory placeholder)
- database.db - example/local SQLite DB file (committed in this repo)
- Cargo.toml, Cargo.lock - appear to be present but are unrelated to the Python client; treat this repository primarily as a Python client implementation.

Notes and caveats

- The client expects the server API to expose endpoints under the configured base URL and api_version prefix. Endpoints referenced by the client include:
  - /{api_version}/defaults/set
  - /{api_version}/defaults/get
  - /{api_version}/preferences/set
  - /{api_version}/preferences/get

- Some helper methods use heuristics (for example, _has_explicit_preference) and may require server-side support for fully accurate behavior.
- The repository currently includes a committed SQLite database file (database.db). Consider removing or replacing it with migrations and fixtures if sensitive data is present.

Contributing

Contributions welcome. Open an issue or create a pull request with changes. Please include tests and update the README where appropriate.

License

This project is provided under the repository LICENSE file.
