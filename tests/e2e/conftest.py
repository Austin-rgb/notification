import shutil
import subprocess

import pytest
import requests

import settings
from helpers import Api, MockBrevo, NatsBus, WsClient, new_user, wait_until


def _ready():
    try:
        return requests.get(f"{settings.BASE_URL}/readyz", timeout=3).status_code == 200
    except requests.RequestException:
        return False


def wait_for_service(timeout=120):
    return wait_until(_ready, timeout=timeout, interval=1.0)


@pytest.fixture(scope="session", autouse=True)
def service_ready():
    if not wait_for_service():
        pytest.exit(
            f"notification service not ready at {settings.BASE_URL}/readyz "
            "(start the stack: python tests/e2e/run_e2e.py)",
            returncode=2,
        )


@pytest.fixture(scope="session")
def bus():
    b = NatsBus()
    yield b
    b.close()


@pytest.fixture(scope="session")
def api():
    return Api()


@pytest.fixture(scope="session")
def brevo():
    return MockBrevo()


@pytest.fixture
def user():
    return new_user()


@pytest.fixture
def connect():
    """Open authenticated websocket connections; all closed after the test."""
    clients = []

    def _connect(u):
        client = WsClient(u)
        clients.append(client)
        return client

    yield _connect
    for c in clients:
        c.close()


@pytest.fixture
def restart_service():
    if shutil.which("docker") is None or not settings.COMPOSE_FILE.exists():
        pytest.skip("docker CLI / compose file not available")

    def _restart():
        subprocess.run(
            ["docker", "compose", "-f", str(settings.COMPOSE_FILE), "restart", "notification"],
            check=True,
            capture_output=True,
        )
        assert wait_for_service(), "service did not become ready after restart"

    return _restart
