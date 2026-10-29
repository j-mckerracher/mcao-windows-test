"""Render, export, and check the protocol-v1 JSON Schemas. The committed files must equal render_all()."""

import json
from pathlib import Path
from typing import Any

from pydantic import BaseModel

from mao_cloud.protocol import (
    CLIENT_CONTROLLED_FIELDS,
    DOCUMENT_FORBIDDEN_FIELDS,
    FIELD_ORIGINS,
    SCHEMA_MODELS,
    AcceptedReceipt,
    ActionRequest,
    ClaimDocument,
    CreateRunRequest,
    LogEntry,
    RetryRequest,
    RunResultDocument,
    StartRunRequest,
    StatusReport,
    WorkerBootstrap,
)

# Names each model refuses from a client (requests) or from a stored document. These are the same
# sets the parsers screen with: parse_request uses CLIENT_CONTROLLED_FIELDS, parse_document uses
# DOCUMENT_FORBIDDEN_FIELDS. Identity fields are accepted on documents, so they are not listed there.
FORBIDDEN_INPUTS: dict[type[BaseModel], frozenset[str]] = {
    CreateRunRequest: CLIENT_CONTROLLED_FIELDS,
    StartRunRequest: CLIENT_CONTROLLED_FIELDS,
    RetryRequest: CLIENT_CONTROLLED_FIELDS,
    ActionRequest: CLIENT_CONTROLLED_FIELDS,
    AcceptedReceipt: DOCUMENT_FORBIDDEN_FIELDS,
    WorkerBootstrap: DOCUMENT_FORBIDDEN_FIELDS,
    ClaimDocument: DOCUMENT_FORBIDDEN_FIELDS,
    StatusReport: DOCUMENT_FORBIDDEN_FIELDS,
    RunResultDocument: DOCUMENT_FORBIDDEN_FIELDS,
    LogEntry: DOCUMENT_FORBIDDEN_FIELDS,
}


def _annotate(model: type[BaseModel], schema: dict[str, Any]) -> dict[str, Any]:
    """Add the authority metadata the Python classification declares.

    x-origin sits on each property that FIELD_ORIGINS classifies. x-forbidden-inputs sits on models
    whose parser screens client- or document-supplied names. Embedded definitions use the same
    annotations. x-blocking is already emitted by each BLOCKING field's own json_schema_extra.
    """
    origins = FIELD_ORIGINS.get(model.__name__, {})
    properties: dict[str, Any] = schema.get("properties", {})
    for name, origin in origins.items():
        properties[name]["x-origin"] = origin.value
    forbidden = FORBIDDEN_INPUTS.get(model)
    if forbidden is not None:
        schema["x-forbidden-inputs"] = sorted(forbidden)
    definitions = schema.get("$defs", {})
    for nested_model in SCHEMA_MODELS.values():
        definition = definitions.get(nested_model.__name__)
        if definition is not None:
            _annotate(nested_model, definition)
    return schema


def render_all() -> dict[str, str]:
    """File name to exact file text, for every model in SCHEMA_MODELS."""
    rendered: dict[str, str] = {}
    for name in sorted(SCHEMA_MODELS):
        model = SCHEMA_MODELS[name]
        schema = _annotate(model, model.model_json_schema())
        rendered[f"{name}.json"] = json.dumps(schema, indent=2, sort_keys=True) + "\n"
    return rendered


def export(directory: Path) -> list[Path]:
    """Write every schema into directory, creating it if needed. Returns the written paths."""
    directory.mkdir(parents=True, exist_ok=True)
    written: list[Path] = []
    for filename, text in render_all().items():
        path = directory / filename
        path.write_text(text, encoding="utf-8")
        written.append(path)
    return written


def check(directory: Path) -> list[str]:
    """Problems between directory and the rendered schemas. An empty list means they match."""
    if not directory.is_dir():
        return [f"{directory} is not a directory"]
    expected = render_all()
    problems: list[str] = []
    for filename, text in expected.items():
        path = directory / filename
        if not path.is_file():
            problems.append(f"missing {filename}")
        elif path.read_text(encoding="utf-8") != text:
            problems.append(f"stale {filename}; regenerate with `mao-cloud schema export`")
    for path in sorted(directory.glob("*.json")):
        if path.name not in expected:
            problems.append(f"unexpected {path.name}")
    return problems
