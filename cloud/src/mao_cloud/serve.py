"""Controller server: the broker app over production adapters, served on Cloud Run's PORT.

The controller image's entrypoint is this module. It reads the server policy from the file named by
--policy, or by MAO_CLOUD_POLICY_FILE when --policy is absent, builds the broker app, and serves it
with uvicorn on 0.0.0.0 at PORT (Cloud Run's contract; 8080 when PORT is unset). Each step refuses
in order, and nothing is bound until every step passes:

1. PORT is a TCP port number, a policy file is named, and the policy file loads. Exit 2 otherwise.
2. The policy declares api_audience, the audience caller tokens must carry. Exit 2 otherwise.
3. The production configuration is complete and valid (mao_cloud.production). Exit 2 otherwise,
   naming each missing setting.
4. Every production client opens (mao_cloud.production). Exit 3 otherwise.

The reconcile Scheduler job is deployment configuration outside this image.
"""

import argparse
import os
import re
import sys
from collections.abc import Callable, Mapping, Sequence
from pathlib import Path
from typing import Any

import uvicorn
from fastapi import FastAPI
from google.auth.transport import requests as google_requests

from mao_cloud.broker import BearerIdentity, PolicyIdentities, Ports, create_app
from mao_cloud.config import Policy, PolicyError, load_policy
from mao_cloud.production import (
    Adapters,
    ProductionAdaptersUnavailable,
    ProductionConfigError,
    ProductionSdk,
    build_adapters,
    load_config,
)

HOST = "0.0.0.0"
DEFAULT_PORT = 8080
ENV_POLICY_FILE = "MAO_CLOUD_POLICY_FILE"
EXIT_CONFIG = 2
EXIT_UNAVAILABLE = 3
_PORT_PATTERN = re.compile(r"[0-9]{1,5}")


class PortError(ValueError):
    """PORT is not a TCP port number. Startup refuses."""


def port_from(environ: Mapping[str, str]) -> int:
    raw = environ.get("PORT", str(DEFAULT_PORT))
    if _PORT_PATTERN.fullmatch(raw) is None or not 1 <= int(raw) <= 65535:
        raise PortError(f"PORT must be a TCP port number from 1 to 65535 (got {raw!r})")
    return int(raw)


def policy_path_from(argument: Path | None, environ: Mapping[str, str]) -> Path:
    """The policy file: --policy when given, else the file MAO_CLOUD_POLICY_FILE names.

    Neither refuses. An empty MAO_CLOUD_POLICY_FILE counts as unset, so it refuses too.
    """
    if argument is not None:
        return argument
    named = environ.get(ENV_POLICY_FILE, "")
    if not named:
        raise PolicyError(
            f"no policy file: pass --policy or set {ENV_POLICY_FILE} to the policy file's path"
        )
    return Path(named)


def production_adapters(
    _policy: Policy,
    environ: Mapping[str, str] | None = None,
    *,
    sdk: ProductionSdk | None = None,
) -> Adapters:
    """Read production configuration from environ (os.environ by default), then open the clients.

    Raises ProductionConfigError when configuration is missing or invalid, and
    ProductionAdaptersUnavailable when a client cannot be opened. build_app has already checked the
    policy's audience before this runs.
    """
    config = load_config(os.environ if environ is None else environ)
    return build_adapters(config, sdk)


def build_app(
    policy: Policy,
    make_adapters: Callable[[Policy], Adapters],
    *,
    transport: Callable[..., Any] | None = None,
) -> FastAPI:
    """Refuse without an audience, build the adapters, then wire the app. Each step can refuse."""
    if policy.api_audience is None:
        raise PolicyError(
            "api_audience is required: without it no caller token can be verified"
        )
    adapters = make_adapters(policy)
    bearer = BearerIdentity(
        audience=policy.api_audience,
        directory=PolicyIdentities(policy),
        transport=google_requests.Request() if transport is None else transport,
    )
    ports = Ports(
        store=adapters.store,
        objects=adapters.objects,
        signer=adapters.signer,
        launcher=adapters.launcher,
        clock=adapters.clock,
        owner_of=bearer.owner_of,
        is_controller=bearer.is_controller,
        is_operator=bearer.is_operator,
        admissions=adapters.admissions,
    )
    return create_app(policy=policy, ports=ports)


def _serve(app: FastAPI, *, host: str, port: int) -> None:
    uvicorn.run(app, host=host, port=port, log_level="info")


def main(
    argv: Sequence[str] | None = None,
    *,
    environ: Mapping[str, str] | None = None,
    make_adapters: Callable[[Policy], Adapters] | None = None,
    serve: Callable[..., None] = _serve,
) -> int:
    parser = argparse.ArgumentParser(
        prog="mao-cloud-broker", description=__doc__.splitlines()[0]
    )
    parser.add_argument(
        "--policy",
        type=Path,
        default=None,
        help=f"server policy JSON file (default: the file named by {ENV_POLICY_FILE})",
    )
    args = parser.parse_args(argv)
    env = os.environ if environ is None else environ
    # The same environment supplies PORT, the policy path, and the production configuration.
    build = make_adapters or (lambda policy: production_adapters(policy, env))
    try:
        port = port_from(env)
        policy = load_policy(policy_path_from(args.policy, env))
        app = build_app(policy, build)
    except (OSError, PolicyError, PortError, ProductionConfigError) as exc:
        print(f"mao-cloud-broker: {exc}", file=sys.stderr)
        return EXIT_CONFIG
    except ProductionAdaptersUnavailable as exc:
        print(f"mao-cloud-broker: refusing to start. {exc}", file=sys.stderr)
        return EXIT_UNAVAILABLE
    serve(app, host=HOST, port=port)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
