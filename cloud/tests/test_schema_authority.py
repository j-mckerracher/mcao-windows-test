"""Authority metadata in the committed protocol-v1 schemas: field origins, forbidden inputs, BLOCKING tickets.

The JSON must carry the classification the Python tables declare and the names the parsers screen,
so a reviewer reading a schema sees the same authority rules the code enforces.
"""

import json
from pathlib import Path
from typing import Any

import pytest

from mao_cloud.protocol import (
    CLIENT_CONTROLLED_FIELDS,
    DOCUMENT_FORBIDDEN_FIELDS,
    FIELD_ORIGINS,
    SCHEMA_MODELS,
    ActionRequest,
    CreateRunRequest,
    Origin,
    RetryRequest,
    StartRunRequest,
)
from mao_cloud.schemas import FORBIDDEN_INPUTS

COMMITTED = Path(__file__).resolve().parents[1] / "schemas" / "protocol-v1"
STEM_BY_CLASS = {model.__name__: stem for stem, model in SCHEMA_MODELS.items()}
REQUEST_MODELS = [CreateRunRequest, StartRunRequest, RetryRequest, ActionRequest]
DOCUMENT_CLASSES = sorted(model.__name__ for model in FORBIDDEN_INPUTS if model not in REQUEST_MODELS)


def _load(class_name: str) -> dict[str, Any]:
    text = (COMMITTED / f"{STEM_BY_CLASS[class_name]}.json").read_text(encoding="utf-8")
    loaded: dict[str, Any] = json.loads(text)
    return loaded


# --- x-origin: every classified field carries exactly its declared origin ------------------------


@pytest.mark.parametrize("class_name", sorted(FIELD_ORIGINS))
def test_every_classified_property_carries_its_origin(class_name: str) -> None:
    properties = _load(class_name)["properties"]
    emitted = {name: prop["x-origin"] for name, prop in properties.items() if "x-origin" in prop}
    assert emitted == {name: origin.value for name, origin in FIELD_ORIGINS[class_name].items()}


def test_no_unclassified_property_is_emitted_with_an_origin() -> None:
    for class_name in FIELD_ORIGINS:
        properties = _load(class_name)["properties"]
        assert set(properties) == set(FIELD_ORIGINS[class_name]), class_name


@pytest.mark.parametrize("response_class", ["CreateRunResponse", "StatusView"])
def test_embedded_run_view_origins_match_the_standalone_schema(response_class: str) -> None:
    embedded = _load(response_class)["$defs"]["RunView"]["properties"]
    standalone = _load("RunView")["properties"]

    assert {name: prop["x-origin"] for name, prop in embedded.items()} == {
        name: prop["x-origin"] for name, prop in standalone.items()
    }


def test_client_manifest_origin_appears_only_on_request_models() -> None:
    request_names = {model.__name__ for model in REQUEST_MODELS}
    for class_name in FIELD_ORIGINS:
        origins = set(FIELD_ORIGINS[class_name].values())
        if class_name in request_names:
            assert origins == {Origin.CLIENT_MANIFEST}, class_name
        else:
            assert Origin.CLIENT_MANIFEST not in origins, class_name


# --- x-forbidden-inputs: the emitted names are the names the parsers screen ----------------------


@pytest.mark.parametrize("model", REQUEST_MODELS, ids=lambda model: model.__name__)
def test_request_schema_lists_exactly_the_client_controlled_names(model: type) -> None:
    schema = _load(model.__name__)
    assert schema["x-forbidden-inputs"] == sorted(CLIENT_CONTROLLED_FIELDS)
    assert not set(schema["x-forbidden-inputs"]) & set(schema["properties"])


@pytest.mark.parametrize("class_name", DOCUMENT_CLASSES)
def test_document_schema_lists_exactly_the_document_forbidden_names(class_name: str) -> None:
    schema = _load(class_name)
    assert schema["x-forbidden-inputs"] == sorted(DOCUMENT_FORBIDDEN_FIELDS)
    assert not set(schema["x-forbidden-inputs"]) & set(schema["properties"])


def test_only_request_and_document_models_declare_forbidden_inputs() -> None:
    declared = {
        stem
        for stem in SCHEMA_MODELS
        if "x-forbidden-inputs" in json.loads((COMMITTED / f"{stem}.json").read_text(encoding="utf-8"))
    }
    expected = {STEM_BY_CLASS[model.__name__] for model in FORBIDDEN_INPUTS}
    assert declared == expected


# --- x-blocking: BLOCKING fields name the ticket that owns their value ---------------------------


def test_blocking_fields_carry_the_ticket_that_owns_them() -> None:
    properties = _load("WorkerBootstrap")["properties"]
    assert properties["durable_root"]["x-blocking"] == ["DPES-869"]
    assert properties["checkpoint_ref"]["x-blocking"] == ["DPES-869"]
    assert properties["continuation_budget"]["x-blocking"] == ["DPES-870"]
