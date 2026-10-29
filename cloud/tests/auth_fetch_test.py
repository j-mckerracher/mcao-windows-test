"""Certificate fetches: bounded in time, and never holding the event loop.

Identity resolution fetches Google's certificates synchronously. These tests pin two properties of
the HTTP layer around that fetch: every fetch carries the bounded timeout, and a fetch that stalls
does not stop another request on the same worker from being served.
"""

import asyncio
import threading
from typing import Any

import httpx

from auth_support import (
    AUDIENCE,
    GOOGLE_SIGNER,
    NO_AUTH,
    UNKNOWN_RUN_ID,
    CertTransport,
    SigningKey,
    as_owner_a,
    make_auth_world,
)
from mao_cloud.broker import CERT_FETCH_TIMEOUT_SECONDS


class RecordingTransport(CertTransport):
    """Records the timeout each certificate fetch is given."""

    def __init__(self, *signers: SigningKey) -> None:
        super().__init__(*signers)
        self.timeouts: list[Any] = []

    def __call__(self, url: str, method: str = "GET", **kwargs: Any) -> Any:
        self.timeouts.append(kwargs.get("timeout"))
        return super().__call__(url, method, **kwargs)


class TimedOutTransport(CertTransport):
    """A certificate fetch that exceeds its timeout."""

    def __call__(self, url: str, method: str = "GET", **_: Any) -> Any:
        raise TimeoutError("certificate fetch timed out")


class GatedTransport(CertTransport):
    """Holds each certificate fetch until the test releases it, as a stalled endpoint would."""

    def __init__(self, *signers: SigningKey) -> None:
        super().__init__(*signers)
        self.entered = threading.Event()
        self.release = threading.Event()
        self.held_until_released = False

    def __call__(self, url: str, method: str = "GET", **kwargs: Any) -> Any:
        self.entered.set()
        self.held_until_released = self.release.wait(timeout=5)
        return super().__call__(url, method, **kwargs)


def test_every_certificate_fetch_is_given_the_bounded_timeout() -> None:
    transport = RecordingTransport(GOOGLE_SIGNER)
    world = make_auth_world(transport=transport)

    world.client.get(f"/v1/runs/{UNKNOWN_RUN_ID}", headers=as_owner_a())

    assert transport.fetches == 1
    assert transport.timeouts == [CERT_FETCH_TIMEOUT_SECONDS]
    assert 0 < CERT_FETCH_TIMEOUT_SECONDS <= 30


def test_a_certificate_fetch_that_times_out_refuses_the_caller() -> None:
    world = make_auth_world(transport=TimedOutTransport())

    refused = world.client.get(f"/v1/runs/{UNKNOWN_RUN_ID}", headers=as_owner_a())

    assert refused.status_code == 403
    assert refused.json() == world.client.get(f"/v1/runs/{UNKNOWN_RUN_ID}", headers=NO_AUTH).json()


def test_a_stalled_certificate_fetch_does_not_block_another_request_on_the_worker() -> None:
    transport = GatedTransport(GOOGLE_SIGNER)
    world = make_auth_world(transport=transport)

    async def scenario() -> None:
        async with httpx.AsyncClient(
            transport=httpx.ASGITransport(app=world.client.app), base_url=AUDIENCE
        ) as client:
            stalled = asyncio.ensure_future(
                client.get(f"/v1/runs/{UNKNOWN_RUN_ID}", headers=as_owner_a())
            )
            try:
                assert await asyncio.wait_for(
                    asyncio.to_thread(transport.entered.wait, 5), timeout=5
                )
                # Served while the owner's certificate fetch is still held open.
                other = await asyncio.wait_for(
                    client.get(f"/v1/runs/{UNKNOWN_RUN_ID}", headers=NO_AUTH), timeout=2
                )
                assert other.status_code == 403
            finally:
                transport.release.set()
            resumed = await stalled
            assert resumed.status_code == 404

    asyncio.run(scenario())
    assert transport.held_until_released, "the other request waited on the stalled fetch"
