"""One-shot admission-layout provisioning for the Firestore database (DPES-809).

The run store refuses every operation until admission_layout/current holds the shard count the
service expects. Run this once per database, before the first start of a controller that uses it:

  python3 -m mao_cloud.provision

Environment:
  MAO_CLOUD_PROJECT             required  GCP project that owns the database
  MAO_CLOUD_FIRESTORE_DATABASE  optional  database name; default mcao-cloud

Exit status:
  0  the layout is provisioned for ADMISSION_SHARDS shards, now or from an earlier run
  2  MAO_CLOUD_PROJECT is missing or blank; nothing was opened
  3  the write was not confirmed (client or transaction failure, reported by exception type only);
     run the command again, because a repeat with the same shard count is idempotent
  4  the database already holds a layout for a different shard count; nothing was changed

Why this is a command and not a startup step: a Cloud Run service may start many instances at once.
Provisioning from every instance would make each one write the layout, which needs write rights
that the serving identity does not otherwise need, and would make a shard-count change refuse every
instance at once. Run here by one operator identity, it is a single transaction that the serving
instances never run. The service then only reads the layout and refuses if it is missing.

Nothing here has been run against a live Firestore database. Its behaviour is unverified until a
deployed database proves it.
"""

import os
import sys
from collections.abc import Callable, Mapping

from mao_cloud.firestore_store import (
    ADMISSION_SHARDS,
    FirestoreClient,
    FirestoreStoreError,
    production_firestore_client,
    provision_admission_layout,
)
from mao_cloud.production import (
    DEFAULT_FIRESTORE_DATABASE,
    ENV_FIRESTORE_DATABASE,
    ENV_PROJECT,
)

EXIT_PROVISIONED = 0
EXIT_CONFIG = 2
EXIT_NOT_CONFIRMED = 3
EXIT_LAYOUT_CONFLICT = 4

_PREFIX = "mao-cloud-provision"


def main(
    environ: Mapping[str, str] | None = None,
    *,
    client_factory: Callable[..., FirestoreClient] = production_firestore_client,
) -> int:
    values = os.environ if environ is None else environ
    project = values.get(ENV_PROJECT, "").strip()
    if not project:
        print(f"{_PREFIX}: {ENV_PROJECT} is required", file=sys.stderr)
        return EXIT_CONFIG
    database = (
        values.get(ENV_FIRESTORE_DATABASE, "").strip() or DEFAULT_FIRESTORE_DATABASE
    )
    try:
        client = client_factory(project=project, database=database)
        provision_admission_layout(client, shards=ADMISSION_SHARDS)
    except FirestoreStoreError as exc:
        print(f"{_PREFIX}: {exc}; nothing was changed", file=sys.stderr)
        return EXIT_LAYOUT_CONFLICT
    except Exception as exc:
        print(
            f"{_PREFIX}: not confirmed in database {database} ({type(exc).__name__}); "
            "run the command again",
            file=sys.stderr,
        )
        return EXIT_NOT_CONFIRMED
    print(
        f"{_PREFIX}: admission layout provisioned for {ADMISSION_SHARDS} shards "
        f"in database {database}"
    )
    return EXIT_PROVISIONED


if __name__ == "__main__":
    raise SystemExit(main())
