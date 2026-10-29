"""Transports for the submit tests: the in-process service, and wrappers that lose or refuse calls.

WorldTransport speaks the protocol to the real broker and storage fakes through TestClient. The
wrappers change only what reaches the service or what comes back, so the service stays the real
broker. Nothing here reaches a cloud.
"""

from collections import deque
from collections.abc import Callable, Mapping
from typing import Any
from urllib.parse import parse_qs, urlsplit

from fakes import OWNER_A, owner_headers
from flows import World
from mao_cloud.protocol import ProtocolError, sha256_hex
from mao_cloud.submit import Reply, TransportError


def _reply(response: Any) -> Reply:
    body = response.json() if response.content else None
    return Reply(status=response.status_code, body=body, headers=dict(response.headers))


class WorldTransport:
    """The protocol over the in-process broker. A signed PUT is checked the way storage checks it."""

    def __init__(self, world: World, owner: str = OWNER_A) -> None:
        self.world = world
        self.owner = owner

    def create(self, body: Mapping[str, Any]) -> Reply:
        response = self.world.client.post(
            "/v1/runs", json=dict(body), headers=owner_headers(self.owner)
        )
        return _reply(response)

    def put(self, url: str, headers: Mapping[str, str], data: bytes) -> Reply:
        parts = urlsplit(url)
        expires = int(parse_qs(parts.query)["expires"][0])
        if self.world.clock.now().timestamp() >= expires:
            return Reply(403, {"error": {"code": "not_allowed", "message": "URL expired"}})
        lowered = {key.lower(): value for key, value in headers.items()}
        if sha256_hex(data) != lowered.get("x-goog-meta-sha256"):
            return Reply(400, {"error": {"code": "checksum_mismatch", "message": "checksum"}})
        precondition = int(lowered["x-goog-if-generation-match"])
        try:
            generation = self.world.objects.write(
                parts.path.lstrip("/"), data, if_generation_match=precondition
            )
        except ProtocolError:
            return Reply(412, {"error": {"code": "version_conflict", "message": "exists"}})
        return Reply(200, None, {"x-goog-generation": str(generation)})

    def start(self, run_id: str, body: Mapping[str, Any]) -> Reply:
        response = self.world.client.post(
            f"/v1/runs/{run_id}:start", json=dict(body), headers=owner_headers(self.owner)
        )
        return _reply(response)


class LossyTransport:
    """Forwards to an inner transport, then loses the reply (or the request) for named operations.

    lose_reply: the service handles the call, then the caller sees a TransportError.
    lose_request: the caller sees a TransportError and the service never receives the call.
    Each entry is consumed once, in order.
    """

    def __init__(
        self,
        inner: Any,
        *,
        lose_reply: tuple[str, ...] = (),
        lose_request: tuple[str, ...] = (),
    ) -> None:
        self.inner = inner
        self._lose_reply = deque(lose_reply)
        self._lose_request = deque(lose_request)

    def _call(self, op: str, forward: Callable[[], Reply]) -> Reply:
        if self._lose_request and self._lose_request[0] == op:
            self._lose_request.popleft()
            raise TransportError(f"{op}: request lost before the service received it")
        reply = forward()
        if self._lose_reply and self._lose_reply[0] == op:
            self._lose_reply.popleft()
            raise TransportError(f"{op}: reply lost after the service answered")
        return reply

    def create(self, body: Mapping[str, Any]) -> Reply:
        return self._call("create", lambda: self.inner.create(body))

    def put(self, url: str, headers: Mapping[str, str], data: bytes) -> Reply:
        return self._call("put", lambda: self.inner.put(url, headers, data))

    def start(self, run_id: str, body: Mapping[str, Any]) -> Reply:
        return self._call("start", lambda: self.inner.start(run_id, body))


class SpyTransport:
    """Records each operation and runs an optional hook first. With no inner transport, any call
    raises, so a test that expects no network traffic fails loudly if the code makes a call."""

    def __init__(
        self,
        inner: Any | None,
        *,
        before: Callable[[str], None] | None = None,
    ) -> None:
        self.inner = inner
        self.before = before
        self.calls: list[str] = []

    def _call(self, op: str, forward: Callable[[], Reply]) -> Reply:
        self.calls.append(op)
        if self.before is not None:
            self.before(op)
        if self.inner is None:
            raise AssertionError(f"unexpected service call: {op}")
        return forward()

    def create(self, body: Mapping[str, Any]) -> Reply:
        return self._call("create", lambda: self.inner.create(body))

    def put(self, url: str, headers: Mapping[str, str], data: bytes) -> Reply:
        return self._call("put", lambda: self.inner.put(url, headers, data))

    def start(self, run_id: str, body: Mapping[str, Any]) -> Reply:
        return self._call("start", lambda: self.inner.start(run_id, body))


class ExpiredFirstPut:
    """Refuses the first PUT with 403 without reaching storage, as an expired signed URL would."""

    def __init__(self, inner: Any) -> None:
        self.inner = inner
        self.creates = 0
        self.puts = 0

    def create(self, body: Mapping[str, Any]) -> Reply:
        self.creates += 1
        return self.inner.create(body)

    def put(self, url: str, headers: Mapping[str, str], data: bytes) -> Reply:
        self.puts += 1
        if self.puts == 1:
            return Reply(403, {"error": {"code": "not_allowed", "message": "URL expired"}})
        return self.inner.put(url, headers, data)

    def start(self, run_id: str, body: Mapping[str, Any]) -> Reply:
        return self.inner.start(run_id, body)


class OfflineTransport:
    """Every call fails to reach the service."""

    def create(self, body: Mapping[str, Any]) -> Reply:
        raise TransportError("create: no route to the service")

    def put(self, url: str, headers: Mapping[str, str], data: bytes) -> Reply:
        raise TransportError("put: no route to the service")

    def start(self, run_id: str, body: Mapping[str, Any]) -> Reply:
        raise TransportError("start: no route to the service")
