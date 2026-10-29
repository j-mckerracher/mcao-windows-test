"""Deployed caller, owner-scope, and ingress smoke for the cloud control API (DPES-896).

Two kinds of test live here.

- Local guards run on every suite pass against the in-memory broker. They pin the reviewed route
  table, prove that every reviewed route refuses an anonymous caller, and prove the layer
  classification helper, so a new unauthenticated route or a misread refusal fails before any
  deployment exists.
- Live checks run only when MAO_SMOKE_BASE_URL names a deployed control API. They are read-only:
  they never create, start, cancel, or retry a run. Owner, scheduler, and fixture values come from
  the environment of the operator who runs them. Tokens are never printed and never appear in an
  assertion message.

Live checks skip, and do not pass, when their environment is absent. docs/deployed-smoke.md owns the
run procedure and the evidence record.
"""

import os
from collections.abc import Callable, Iterator
from typing import cast

import httpx
import pytest
from fastapi import FastAPI
from fastapi.routing import APIRoute

from auth_support import NO_AUTH, UNKNOWN_RUN_ID, AuthWorld, make_auth_world

BASE_URL_ENV = "MAO_SMOKE_BASE_URL"
OWNER_A_ENV = "MAO_SMOKE_OWNER_A_TOKEN"
OWNER_B_ENV = "MAO_SMOKE_OWNER_B_TOKEN"
SCHEDULER_ENV = "MAO_SMOKE_SCHEDULER_TOKEN"
FIXTURE_RUN_ENV = "MAO_SMOKE_FIXTURE_RUN_ID"

# Every (method, path template) the broker serves. A new route fails until it is reviewed here.
REVIEWED_ROUTES = frozenset(
    {
        ("POST", "/v1/runs"),
        ("POST", "/v1/runs/{run_id}:start"),
        ("POST", "/v1/runs/{run_id}:cancel"),
        ("POST", "/v1/runs/{run_id}:retry"),
        ("GET", "/v1/runs/{run_id}"),
        ("GET", "/v1/runs/{run_id}/status"),
        ("GET", "/v1/runs/{run_id}/logs"),
        ("GET", "/v1/runs/{run_id}/result"),
        ("GET", "/v1/runs/{run_id}/teardown"),
        ("POST", "/internal/v1/runs/{run_id}:reconcile"),
        ("GET", "/internal/v1/admissions"),
        ("POST", "/internal/v1/admissions:drain"),
        ("POST", "/internal/v1/admissions:resume"),
        ("POST", "/internal/v1/runs:bulk-cancel"),
        ("POST", "/internal/v1/runs:reconcile"),
    }
)

ACCEPTED = "accepted"
EDGE_REFUSAL = "edge-refusal"
APP_NOT_ALLOWED = "app-refusal:not_allowed"
APP_NOT_FOUND = "app-refusal:not_found"


def classify(response: httpx.Response) -> str:
    """Name the layer that answered: the app's typed error body, or anything else at the edge.

    Cloud Run's invoker check and the load balancer answer with a non-JSON body, so a refusal
    without a typed body is an edge refusal. The body is never echoed into a message.
    """
    if response.status_code < 400:
        return ACCEPTED
    try:
        code = response.json()["error"]["code"]
    except (ValueError, KeyError, TypeError):
        return EDGE_REFUSAL
    return f"app-refusal:{code}" if isinstance(code, str) else EDGE_REFUSAL


# -- Local guards -----------------------------------------------------------


@pytest.fixture
def world() -> AuthWorld:
    return make_auth_world()


def _served_routes(world: AuthWorld) -> set[tuple[str, str]]:
    app = cast(FastAPI, world.client.app)
    return {
        (method, route.path)
        for route in app.routes
        if isinstance(route, APIRoute)
        for method in route.methods - {"HEAD"}
    }


def test_route_table_is_the_reviewed_set(world: AuthWorld) -> None:
    assert _served_routes(world) == set(REVIEWED_ROUTES)


@pytest.mark.parametrize(("method", "template"), sorted(REVIEWED_ROUTES))
def test_every_reviewed_route_refuses_an_anonymous_caller(
    world: AuthWorld, method: str, template: str
) -> None:
    path = template.replace("{run_id}", UNKNOWN_RUN_ID)
    response = world.client.request(
        method, path, headers=NO_AUTH, json={} if method == "POST" else None
    )
    assert classify(response) == APP_NOT_ALLOWED, (
        f"{method} {template} -> {response.status_code}"
    )


def test_classify_names_the_app_refusal_from_its_typed_body() -> None:
    response = httpx.Response(
        403, json={"error": {"code": "not_allowed", "message": "refused"}}
    )
    assert classify(response) == APP_NOT_ALLOWED


def test_classify_names_an_edge_refusal_from_a_non_json_body() -> None:
    response = httpx.Response(
        403, text="<html>Forbidden</html>", headers={"content-type": "text/html"}
    )
    assert classify(response) == EDGE_REFUSAL


def test_classify_accepts_a_success() -> None:
    assert classify(httpx.Response(200, json={"run": {}})) == ACCEPTED


# -- Live checks (read-only; skip without the environment) -----------------


def _required(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        pytest.skip(f"{name} is not set; the live deployed smoke did not run")
    return value


def _invoker_headers(token: str) -> dict[str, str]:
    """The invoker-service-account flow: the same ID token in both headers.

    Cloud Run validates X-Serverless-Authorization; the application verifies Authorization.
    """
    return {
        "Authorization": f"Bearer {token}",
        "X-Serverless-Authorization": f"Bearer {token}",
    }


@pytest.fixture
def live() -> Iterator[httpx.Client]:
    base_url = _required(BASE_URL_ENV)
    if not base_url.startswith("https://"):
        pytest.fail(f"{BASE_URL_ENV} must be an https:// URL")
    with httpx.Client(
        base_url=base_url, timeout=30.0, follow_redirects=False
    ) as client:
        yield client


def test_live_anonymous_owner_read_is_refused_at_the_edge_or_the_app(
    live: httpx.Client,
) -> None:
    """AC4: no anonymous path to an owner route. Either layer may refuse; neither may accept."""
    response = live.get(f"/v1/runs/{UNKNOWN_RUN_ID}")
    assert response.status_code in (401, 403), (
        f"anonymous read returned {response.status_code}"
    )
    assert classify(response) in (EDGE_REFUSAL, APP_NOT_ALLOWED)


def assert_owner_reads_the_fixture_run(
    client: httpx.Client, token: str, run_id: str
) -> None:
    """AC1 own-read and AC2 header preservation: the owner's read returns the fixture run itself.

    Status alone is not enough: a 200 for some other run fails the check.
    """
    response = client.get(f"/v1/runs/{run_id}", headers=_invoker_headers(token))
    assert response.status_code == 200, (
        f"owner read returned {response.status_code} ({classify(response)})"
    )
    try:
        body = response.json()
    except ValueError:
        pytest.fail("owner read returned 200 with a body that is not JSON")
    identity = body.get("run_id") if isinstance(body, dict) else None
    assert identity == run_id, "owner read returned a run other than the fixture run"


def assert_other_owner_gets_the_missing_run_refusal(
    client: httpx.Client, token: str, run_id: str
) -> None:
    """AC1 cross-owner: another owner gets the typed not-found that a missing run gets.

    A generic edge 404 is not the application's refusal, so it fails even when both reads match.
    """
    headers = _invoker_headers(token)
    foreign = client.get(f"/v1/runs/{run_id}", headers=headers)
    missing = client.get(f"/v1/runs/{UNKNOWN_RUN_ID}", headers=headers)
    assert classify(foreign) == APP_NOT_FOUND, (
        f"cross-owner read returned {foreign.status_code} ({classify(foreign)})"
    )
    assert classify(missing) == APP_NOT_FOUND, (
        f"missing-run read returned {missing.status_code} ({classify(missing)})"
    )
    assert foreign.status_code == missing.status_code
    assert foreign.json() == missing.json()


def assert_scheduler_is_refused_as_an_app_not_allowed(
    client: httpx.Client, token: str, run_id: str
) -> None:
    """The Scheduler identity may invoke the API but is never an owner.

    Only the application's typed not_allowed proves the owner route refused this caller. An edge
    refusal leaves that unproven, so it fails here as an unmet prerequisite, not as a pass.
    """
    refused = client.get(f"/v1/runs/{run_id}", headers=_invoker_headers(token))
    kind = classify(refused)
    if kind == EDGE_REFUSAL:
        pytest.fail(
            "unmet prerequisite: the scheduler request was refused at the edge, "
            "so the owner route was not reached"
        )
    assert refused.status_code == 403, (
        f"scheduler read returned {refused.status_code} ({kind})"
    )
    assert kind == APP_NOT_ALLOWED, (
        f"scheduler read was not the typed not_allowed refusal ({kind})"
    )


def test_live_owner_reads_its_own_run_through_the_invoker_flow(
    live: httpx.Client,
) -> None:
    assert_owner_reads_the_fixture_run(
        live, _required(OWNER_A_ENV), _required(FIXTURE_RUN_ENV)
    )


def test_live_other_owner_gets_the_same_refusal_as_a_missing_run(
    live: httpx.Client,
) -> None:
    """AC1 cross-owner: another owner sees the same typed not-found as a run that does not exist."""
    assert_other_owner_gets_the_missing_run_refusal(
        live, _required(OWNER_B_ENV), _required(FIXTURE_RUN_ENV)
    )


def test_live_scheduler_is_refused_on_an_owner_route(live: httpx.Client) -> None:
    """The Scheduler identity may invoke the API but is never an owner."""
    assert_scheduler_is_refused_as_an_app_not_allowed(
        live, _required(SCHEDULER_ENV), _required(FIXTURE_RUN_ENV)
    )


# -- Local regressions for the live assertions (fake responses; run on every suite pass) --

Handler = Callable[[httpx.Request], httpx.Response]
FIXTURE_RUN = "fixture-run"
TOKEN = "token-under-test"
TYPED_NOT_ALLOWED = {
    "error": {
        "code": "not_allowed",
        "message": "caller identity is not authorized for this API",
        "detail": {},
    }
}
TYPED_NOT_FOUND = {
    "error": {"code": "not_found", "message": "run not found", "detail": {}}
}


def _reply(status: int, *, body: object | None = None, text: str = "") -> Handler:
    def handle(_request: httpx.Request) -> httpx.Response:
        if body is not None:
            return httpx.Response(status, json=body)
        return httpx.Response(status, text=text, headers={"content-type": "text/html"})

    return handle


def _fake_live(handler: Handler) -> httpx.Client:
    return httpx.Client(
        base_url="https://lb.example.test",
        transport=httpx.MockTransport(handler),
        follow_redirects=False,
    )


def _routed(foreign: Handler, missing: Handler) -> Handler:
    """Answer the fixture run with one reply and the unknown run with another."""

    def handle(request: httpx.Request) -> httpx.Response:
        if request.url.path == f"/v1/runs/{FIXTURE_RUN}":
            return foreign(request)
        return missing(request)

    return handle


SCHEDULER_EDGE_REFUSALS = [
    pytest.param(_reply(403, text="<html>Forbidden</html>"), id="html-403"),
    pytest.param(
        _reply(403, body={"message": "Forbidden"}), id="json-without-typed-code-403"
    ),
]


@pytest.mark.parametrize("handler", SCHEDULER_EDGE_REFUSALS)
def test_scheduler_edge_refusal_is_an_unmet_prerequisite_not_a_pass(
    handler: Handler,
) -> None:
    with _fake_live(handler) as client:
        with pytest.raises(pytest.fail.Exception, match="unmet prerequisite"):
            assert_scheduler_is_refused_as_an_app_not_allowed(
                client, TOKEN, FIXTURE_RUN
            )


SCHEDULER_NOT_REFUSED = [
    pytest.param(_reply(200, body={"run_id": FIXTURE_RUN}), id="accepted"),
    pytest.param(_reply(404, body=TYPED_NOT_FOUND), id="typed-not-found"),
]


@pytest.mark.parametrize("handler", SCHEDULER_NOT_REFUSED)
def test_scheduler_reaching_the_run_or_a_wrong_typed_refusal_is_not_a_pass(
    handler: Handler,
) -> None:
    with _fake_live(handler) as client:
        with pytest.raises(AssertionError):
            assert_scheduler_is_refused_as_an_app_not_allowed(
                client, TOKEN, FIXTURE_RUN
            )


def test_scheduler_typed_not_allowed_refusal_passes() -> None:
    with _fake_live(_reply(403, body=TYPED_NOT_ALLOWED)) as client:
        assert_scheduler_is_refused_as_an_app_not_allowed(client, TOKEN, FIXTURE_RUN)


GENERIC_HTML_404 = _reply(404, text="<html>Not Found</html>")
GENERIC_JSON_404 = _reply(404, body={"message": "Not Found"})
TYPED_NOT_FOUND_REPLY = _reply(404, body=TYPED_NOT_FOUND)
DIFFERENT_TYPED_NOT_FOUND = _reply(
    404, body={"error": {"code": "not_found", "message": "no such run", "detail": {}}}
)

CROSS_OWNER_REFUSALS = [
    pytest.param(
        _routed(GENERIC_HTML_404, GENERIC_HTML_404), id="both-generic-html-404"
    ),
    pytest.param(
        _routed(GENERIC_JSON_404, GENERIC_JSON_404), id="both-generic-json-404"
    ),
    pytest.param(
        _routed(TYPED_NOT_FOUND_REPLY, GENERIC_HTML_404),
        id="typed-foreign-generic-missing",
    ),
    pytest.param(
        _routed(TYPED_NOT_FOUND_REPLY, DIFFERENT_TYPED_NOT_FOUND),
        id="typed-but-different-body",
    ),
]


@pytest.mark.parametrize("handler", CROSS_OWNER_REFUSALS)
def test_cross_owner_is_not_a_pass_unless_it_matches_the_typed_missing_run(
    handler: Handler,
) -> None:
    with _fake_live(handler) as client:
        with pytest.raises(AssertionError):
            assert_other_owner_gets_the_missing_run_refusal(client, TOKEN, FIXTURE_RUN)


def test_cross_owner_typed_not_found_matching_the_missing_run_passes() -> None:
    with _fake_live(TYPED_NOT_FOUND_REPLY) as client:
        assert_other_owner_gets_the_missing_run_refusal(client, TOKEN, FIXTURE_RUN)


WRONG_RUN_READS = [
    pytest.param(_reply(200, body={"run_id": "some-other-run"}), id="200-other-run"),
    pytest.param(_reply(200, body={}), id="200-without-run-id"),
    pytest.param(_reply(200, body=[]), id="200-not-an-object"),
    pytest.param(_reply(200, text="ok"), id="200-not-json"),
    pytest.param(_reply(404, body=TYPED_NOT_FOUND), id="typed-not-found"),
]


@pytest.mark.parametrize("handler", WRONG_RUN_READS)
def test_owner_read_that_is_not_the_fixture_run_is_not_a_pass(handler: Handler) -> None:
    with _fake_live(handler) as client:
        with pytest.raises((AssertionError, pytest.fail.Exception)):
            assert_owner_reads_the_fixture_run(client, TOKEN, FIXTURE_RUN)


def test_owner_read_of_the_fixture_run_passes() -> None:
    with _fake_live(_reply(200, body={"run_id": FIXTURE_RUN})) as client:
        assert_owner_reads_the_fixture_run(client, TOKEN, FIXTURE_RUN)


def test_invoker_headers_send_one_token_as_both_authorization_headers() -> None:
    seen: list[httpx.Headers] = []

    def capture(request: httpx.Request) -> httpx.Response:
        seen.append(request.headers)
        return httpx.Response(200, json={"run_id": FIXTURE_RUN})

    with _fake_live(capture) as client:
        assert_owner_reads_the_fixture_run(client, "token-123", FIXTURE_RUN)

    assert seen[0]["authorization"] == "Bearer token-123"
    assert seen[0]["x-serverless-authorization"] == "Bearer token-123"
    assert _invoker_headers("token-123") == {
        "Authorization": "Bearer token-123",
        "X-Serverless-Authorization": "Bearer token-123",
    }
