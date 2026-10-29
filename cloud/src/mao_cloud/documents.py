"""File loading shared by the command-line entry points. Protocol refusals come from protocol.py."""

import json
from datetime import datetime
from pathlib import Path
from typing import Any, TypeVar

from pydantic import BaseModel

from mao_cloud.protocol import parse_document

M = TypeVar("M", bound=BaseModel)


class InputError(ValueError):
    """A file cannot be read or is not JSON, or an argument is not a usable value."""


def read_json(path: Path) -> Any:
    try:
        text = path.read_text(encoding="utf-8")
    except UnicodeDecodeError as exc:
        raise InputError(f"{path} is not valid UTF-8") from exc
    except OSError as exc:
        raise InputError(f"cannot read {path}: {exc.strerror or exc}") from exc
    try:
        return json.loads(text)
    except ValueError as exc:
        raise InputError(f"{path} is not valid JSON") from exc


def load_model(path: Path, model: type[M]) -> M:
    """Parse a protocol document. Raises InputError for unreadable files, ProtocolError otherwise."""
    return parse_document(model, read_json(path))


def parse_instant(text: str) -> datetime:
    try:
        value = datetime.fromisoformat(text)
    except ValueError as exc:
        raise InputError(f"{text!r} is not an ISO 8601 timestamp") from exc
    if value.tzinfo is None:
        raise InputError("timestamp must include a timezone offset")
    return value
