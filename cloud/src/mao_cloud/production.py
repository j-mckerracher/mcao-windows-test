"""Production adapters for the controller image (DPES-809): configuration and SDK wiring.

serve.production_adapters calls build_adapters with the process environment. Configuration is read
first: a missing or invalid required name raises ProductionConfigError (startup exits 2). The
clients are then opened: a failure to open any of them raises ProductionAdaptersUnavailable
(startup exits 3). Nothing is bound until both steps pass.

Environment configuration:
  MAO_CLOUD_PROJECT             required  GCP project for Firestore, Cloud Storage and Cloud Run
  MAO_CLOUD_FIRESTORE_DATABASE  optional  Firestore database; default mcao-cloud
  MAO_CLOUD_OBJECT_BUCKET       required  the one bucket that holds every run object
  MAO_CLOUD_RUN_LOCATION        required  region of the worker job
  MAO_CLOUD_RUN_JOB             required  name of the worker Cloud Run job
  MAO_CLOUD_ADMISSIONS          required  "open" or "closed": the admissions switch at start
  MAO_CLOUD_SIGNER_URL          required  https origin of the signer service, with no path

What is wired and what is not:
- Run store: FirestoreRunStore over the SDK client. The admission layout must already be
  provisioned, or every store operation refuses. `python3 -m mao_cloud.provision` does that once
  per database, outside the service.
- Object store: GcsObjectStore over one Cloud Storage bucket.
- Launcher: refuses every launch while the production launch gate is closed (protocol-v1 gap 72),
  so jobs.run is never called here. Stop and confirmation are refused on the same gate; see
  cloud_run_jobs.
- Admissions switch: set from MAO_CLOUD_ADMISSIONS at start. Operator drain and resume change it in
  this process only, so a restart reads the environment again (protocol-v1 gap 55).
- Signer client: HttpSignerClient at MAO_CLOUD_SIGNER_URL. It asks the signer service for each
  capability with the controller's Google ID token and signs nothing itself (DPES-899). The live
  call and the signer image are unverified until a deployed run proves them.
- Clock: SystemClock.

Nothing here has been run against live Firestore, Cloud Storage or Cloud Run. Every live behaviour
is unverified until a deployed run proves it.
"""

import re
from collections.abc import Callable, Mapping
from dataclasses import dataclass

from mao_cloud.broker import (
    AdmissionSwitch,
    Clock,
    Launcher,
    ObjectStore,
    RunStore,
    SystemClock,
)
from mao_cloud.cloud_run_jobs import CloudRunJobLauncher, JobsSession, open_session
from mao_cloud.firestore_store import (
    FirestoreClient,
    FirestoreRunStore,
    production_firestore_client,
)
from mao_cloud.gcs_objects import GcsBucket, GcsObjectStore
from mao_cloud.signer import SignerClient
from mao_cloud.signer_client import HttpSignerClient
from mao_cloud.signer_wire import SIGNER_ORIGIN

ENV_PROJECT = "MAO_CLOUD_PROJECT"
ENV_FIRESTORE_DATABASE = "MAO_CLOUD_FIRESTORE_DATABASE"
ENV_OBJECT_BUCKET = "MAO_CLOUD_OBJECT_BUCKET"
ENV_RUN_LOCATION = "MAO_CLOUD_RUN_LOCATION"
ENV_RUN_JOB = "MAO_CLOUD_RUN_JOB"
ENV_ADMISSIONS = "MAO_CLOUD_ADMISSIONS"
ENV_SIGNER_URL = "MAO_CLOUD_SIGNER_URL"
DEFAULT_FIRESTORE_DATABASE = "mcao-cloud"
_REQUIRED = (
    ENV_PROJECT,
    ENV_OBJECT_BUCKET,
    ENV_RUN_LOCATION,
    ENV_RUN_JOB,
    ENV_ADMISSIONS,
    ENV_SIGNER_URL,
)
_ADMISSION_VALUES = {"open": True, "closed": False}

# Identifiers that name a Google Cloud resource. Each is interpolated into a URL path or a client
# call, so a value with a separator or a traversal segment must be refused before any client opens.
# Project: a domain-scoped form such as example.com:project, or a 6-30 character project ID.
# Run job: Cloud Run's start-execution-token rule says the job name plus the token must be fewer
# than 63 characters (Cloud Run execute-jobs page; REST reference for projects.locations.jobs).
# With no token that caps the name at 62 characters. The character rule is not stated there.
PROJECT_ID = re.compile(
    r"(?:[a-z0-9-]+(?:\.[a-z0-9-]+)*:)?[a-z][a-z0-9-]{4,28}[a-z0-9]"
)
_RUN_LOCATION = re.compile(r"[a-z]+(?:-[a-z]+)*\d+")
_RUN_JOB = re.compile(r"[a-z](?:[a-z0-9-]{0,60}[a-z0-9])?")
_RESOURCE_NAME_RULES = (
    (ENV_PROJECT, PROJECT_ID, "a Google Cloud project ID"),
    (ENV_RUN_LOCATION, _RUN_LOCATION, "a Cloud Run region such as us-central1"),
    (ENV_RUN_JOB, _RUN_JOB, "a Cloud Run job name of at most 62 characters"),
    (
        ENV_SIGNER_URL,
        SIGNER_ORIGIN,
        "an https origin with no path, such as https://signer.example.run.app",
    ),
)


class ProductionConfigError(Exception):
    """Required production configuration is missing or invalid. Startup refuses."""


class ProductionAdaptersUnavailable(Exception):
    """A production client could not be opened. Startup refuses and binds nothing."""


@dataclass(frozen=True)
class Adapters:
    """What the broker reaches outside its own logic. Production builds all six.

    The signer must read the same clock as the broker: a capability's expiry and deadline checks
    compare against the signer's now, so two clocks would disagree about the same run.
    """

    store: RunStore
    objects: ObjectStore
    signer: SignerClient
    launcher: Launcher
    admissions: AdmissionSwitch
    clock: Clock


@dataclass(frozen=True)
class ProductionConfig:
    project: str
    firestore_database: str
    object_bucket: str
    run_location: str
    run_job: str
    admissions_open: bool
    signer_url: str


def load_config(environ: Mapping[str, str]) -> ProductionConfig:
    """Read every production setting from environ. Each required name must be non-empty."""
    values = {
        name: environ.get(name, "").strip()
        for name in (*_REQUIRED, ENV_FIRESTORE_DATABASE)
    }
    missing = [name for name in _REQUIRED if not values[name]]
    if missing:
        raise ProductionConfigError(
            "missing required configuration: " + ", ".join(missing)
        )
    # Every malformed identifier is named together, and no value is echoed into the refusal.
    invalid = [
        f"{name} must be {kind}"
        for name, pattern, kind in _RESOURCE_NAME_RULES
        if pattern.fullmatch(values[name]) is None
    ]
    if invalid:
        raise ProductionConfigError("; ".join(invalid))
    admissions = values[ENV_ADMISSIONS]
    if admissions not in _ADMISSION_VALUES:
        raise ProductionConfigError(
            f"{ENV_ADMISSIONS} must be 'open' or 'closed' (got {admissions!r})"
        )
    return ProductionConfig(
        project=values[ENV_PROJECT],
        firestore_database=values[ENV_FIRESTORE_DATABASE] or DEFAULT_FIRESTORE_DATABASE,
        object_bucket=values[ENV_OBJECT_BUCKET],
        run_location=values[ENV_RUN_LOCATION],
        run_job=values[ENV_RUN_JOB],
        admissions_open=_ADMISSION_VALUES[admissions],
        signer_url=values[ENV_SIGNER_URL],
    )


class EnvironmentAdmissionSwitch:
    """The admissions switch, set at start from MAO_CLOUD_ADMISSIONS.

    Operator drain and resume change this value in the current process only. A restart, including
    a replaced Cloud Run instance, reads the environment again, so a drain does not survive it.
    """

    def __init__(self, *, open_: bool) -> None:
        self._open = open_

    def is_open(self) -> bool:
        return self._open

    def set_open(self, open_: bool) -> None:
        self._open = open_


def _open_bucket(*, project: str, name: str) -> GcsBucket:
    # Imported here so that importing this module does not load google-cloud-storage.
    from mao_cloud.gcs_sdk import open_bucket

    return open_bucket(project=project, name=name)


@dataclass(frozen=True)
class ProductionSdk:
    """The factories that open SDK clients. The defaults build real clients; tests inject fakes."""

    firestore_client: Callable[..., FirestoreClient] = production_firestore_client
    bucket: Callable[..., GcsBucket] = _open_bucket
    jobs_session: Callable[[], JobsSession] = open_session


def build_adapters(
    config: ProductionConfig, sdk: ProductionSdk | None = None
) -> Adapters:
    """Open the SDK clients and assemble the six adapters. Building opens clients; it calls no API."""
    factories = ProductionSdk() if sdk is None else sdk
    try:
        firestore = factories.firestore_client(
            project=config.project, database=config.firestore_database
        )
        bucket = factories.bucket(project=config.project, name=config.object_bucket)
        session = factories.jobs_session()
    except Exception as exc:
        # Only the exception type is reported: client construction can carry credential detail.
        raise ProductionAdaptersUnavailable(
            f"could not open a production client ({type(exc).__name__})"
        ) from exc
    return Adapters(
        store=FirestoreRunStore(firestore),
        objects=GcsObjectStore(bucket),
        signer=HttpSignerClient(origin=config.signer_url),
        launcher=CloudRunJobLauncher(
            session=session,
            project=config.project,
            location=config.run_location,
            job=config.run_job,
        ),
        admissions=EnvironmentAdmissionSwitch(open_=config.admissions_open),
        clock=SystemClock(),
    )
