#!/usr/bin/env python3
"""fm-audit-sanitize.py - sanitize one audit event before it is serialized, spooled, logged, or shipped.

docs/audit-sanitization.md owns the sanitization contract and its engineering defaults, and
docs/audit-envelope.md owns the envelope this boundary feeds.

Usage:
  fm-audit-sanitize.py sanitize --config PATH PATH|-

Reads one closed config file from PATH and one JSON event from PATH, or from stdin for "-".
Prints one JSON result line:
  {"result": "sanitized", "event": {...}, "field_dropped": [...]}
  {"result": "refused", "reason": "config_invalid" | "event_invalid", "paths": [...]}

Exit status: 0 sanitized, 1 refused, 2 usage or input error.
The boundary writes nothing: no file, spool, or log. A refusal carries only fixed reasons and
JSON Pointers, never a config value or an event value. field_dropped lists the JSON Pointer of
every member that was dropped, so a consumer can see what was removed without seeing its value.
"""

import hashlib
import hmac
import json
import posixpath
import re
import sys
import unicodedata
from pathlib import Path
from urllib.parse import urlsplit

USAGE = "usage: fm-audit-sanitize.py sanitize --config PATH PATH|-"
CONFIG_KEYS = frozenset({"home_key"})
HOME_KEY_PATTERN = re.compile(r"[0-9a-f]{64}")
TRUNCATE_CHARS = 256
# Empty by default: nothing is kept from a shell command or a tool call until a reviewed
# entry is added here. Nothing in the config file can extend either allowlist.
EXECUTABLE_ALLOWLIST = frozenset()
TOOL_SCALAR_ALLOWLIST = {}
# Member names whose values are never kept, whatever their type or content.
DROPPED_MEMBERS = frozenset({
    "prompt", "model_output", "retry_body", "exception_body", "env", "environment", "file_contents",
})
# Typed objects are marked by a "$kind" member; any member outside the kind's set drops the object.
TYPED_MEMBERS = {
    "shell_command": frozenset({"argv"}),
    "tool_call": frozenset({"tool", "args"}),
    "path": frozenset({"path", "root"}),
    "destination": frozenset({"url"}),
}
# Applied in order: token and key shapes first, then the broad credential assignment, then PII/PHI.
# The MRN pattern runs before the date-of-birth pattern: the date-of-birth pattern reaches up to 20
# characters past its keyword, so running it first would consume digits that belong to a record number.
REDACTIONS = (
    ("private_key", re.compile(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|\Z)")),
    ("jwt", re.compile(r"eyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]*")),
    ("github_token", re.compile(r"gh[pousr]_[A-Za-z0-9]{20,}")),
    ("github_token", re.compile(r"github_pat_[A-Za-z0-9_]{20,}")),
    ("aws_access_key", re.compile(r"AKIA[0-9A-Z]{16}")),
    ("google_api_key", re.compile(r"AIza[0-9A-Za-z_-]{35}")),
    ("slack_token", re.compile(r"xox[abprs]-[A-Za-z0-9-]{10,}")),
    ("bearer_token", re.compile(r"Bearer\s+[A-Za-z0-9._~+/=-]{16,}")),
    ("credential_assignment", re.compile(r"(?i)(password|passwd|secret|token|api[_-]?key|x-awopt-key)\s*[:=]\s*\S+")),
    ("ssn", re.compile(r"\b\d{3}-\d{2}-\d{4}\b")),
    ("email", re.compile(r"(?<![A-Za-z0-9._%+-])[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")),
    ("phone", re.compile(r"\(?\b\d{3}\)?[-. ]\d{3}[-. ]\d{4}\b")),
    ("medical_record_number", re.compile(r"(?i)\bmrn[:#\s]*\d{5,}")),
    ("date_of_birth", re.compile(r"(?i)\b(dob|date of birth)\b.{0,20}\d")),
)
DROP = object()


class UsageError(Exception):
    """The command line or the event input cannot be processed."""


class ConfigRefused(Exception):
    """The config file is invalid; paths names the offending members, never their values."""

    def __init__(self, paths):
        super().__init__("config refused")
        self.paths = paths


class EventRefused(Exception):
    """The event is not a JSON object that this boundary can sanitize."""


class FieldDrop(Exception):
    """A member or typed object must be dropped and recorded in field_dropped."""


def sanitize_text(text):
    """Return the text redacted and cut to TRUNCATE_CHARS, or None when it cannot be carried as UTF-8."""
    try:
        text.encode("utf-8")
    except UnicodeEncodeError:
        return None
    for label, pattern in REDACTIONS:
        text = pattern.sub("[REDACTED:" + label + "]", text)
    return text[:TRUNCATE_CHARS]


def clean_text(text):
    cleaned = sanitize_text(text)
    if cleaned is None:
        raise FieldDrop()
    return cleaned


def pointer_token(name):
    return name.replace("~", "~0").replace("/", "~1")


def repo_relative(raw, root):
    """Return the task-worktree-relative path for raw, or None when it lies outside the worktree."""
    if root is not None:
        base = root.rstrip("/")
        if raw.rstrip("/") == base:
            return "."
        if raw.startswith(base + "/"):
            raw = raw[len(base) + 1:]
    if raw.startswith("/"):
        return None
    normalized = posixpath.normpath(raw)
    if normalized == ".." or normalized.startswith("../") or normalized.startswith("/"):
        return None
    return normalized


def is_typed_scalar(value):
    return isinstance(value, (bool, int, float))


class Sanitizer:
    """Sanitizes one event; self.dropped collects the JSON Pointer of every dropped member."""

    def __init__(self, home_key):
        self.home_key = home_key
        self.dropped = []

    def event(self, event):
        if not isinstance(event, dict) or "$kind" in event:
            raise EventRefused()
        return self.mapping(event, "")

    def value(self, value, pointer):
        try:
            if isinstance(value, dict):
                return self.typed(value) if "$kind" in value else self.mapping(value, pointer)
            if isinstance(value, list):
                return self.sequence(value, pointer)
            if isinstance(value, str):
                return clean_text(value)
            return value
        except Exception:
            # No raw fallback: any sanitizer error drops this member and records it.
            self.dropped.append(pointer)
            return DROP

    def mapping(self, node, pointer):
        out = {}
        for key, member in node.items():
            # A key that cannot be named safely, or that collides after redaction, drops like a value.
            name = sanitize_text(key)
            if name is None or name in out or key in DROPPED_MEMBERS:
                self.dropped.append(pointer + "/" + pointer_token(name or "[unparseable-key]"))
                continue
            kept = self.value(member, pointer + "/" + pointer_token(name))
            if kept is not DROP:
                out[name] = kept
        return out

    def sequence(self, node, pointer):
        out = []
        for index, item in enumerate(node):
            kept = self.value(item, pointer + "/" + str(index))
            if kept is not DROP:
                out.append(kept)
        return out

    def typed(self, node):
        kind = node["$kind"]
        if not isinstance(kind, str) or kind not in TYPED_MEMBERS:
            raise FieldDrop()
        if set(node) - TYPED_MEMBERS[kind] - {"$kind"}:
            raise FieldDrop()
        if kind == "shell_command":
            return self.shell_command(node)
        if kind == "tool_call":
            return self.tool_call(node)
        if kind == "path":
            return self.path(node)
        return self.destination(node)

    def shell_command(self, node):
        argv = node["argv"]
        if not isinstance(argv, list) or not argv or not all(isinstance(arg, str) for arg in argv):
            raise FieldDrop()
        normalized = [unicodedata.normalize("NFC", arg) for arg in argv]
        out = {"argv_digest": self.digest(normalized)}
        executable = posixpath.basename(normalized[0])
        if executable in EXECUTABLE_ALLOWLIST:
            out["executable"] = clean_text(executable)
        return out

    def tool_call(self, node):
        tool, args = node["tool"], node["args"]
        if not isinstance(tool, str) or not isinstance(args, dict):
            raise FieldDrop()
        out = {"tool": clean_text(tool), "args_digest": self.digest(args)}
        scalars = TOOL_SCALAR_ALLOWLIST.get(tool, frozenset())
        kept = {name: args[name] for name in sorted(scalars) if name in args and is_typed_scalar(args[name])}
        if kept:
            out["args"] = kept
        return out

    def path(self, node):
        raw, root = node["path"], node.get("root")
        if not isinstance(raw, str) or (root is not None and (not isinstance(root, str) or not root.startswith("/"))):
            raise FieldDrop()
        relative = repo_relative(raw, root)
        if relative is not None:
            return {"repo_path": clean_text(relative)}
        return {"basename": "~/" + clean_text(posixpath.basename(raw.rstrip("/"))), "path_digest": self.digest(raw)}

    def destination(self, node):
        url = node["url"]
        if not isinstance(url, str):
            raise FieldDrop()
        parts = urlsplit(url)
        if not parts.scheme or not parts.hostname:
            raise FieldDrop()
        return {"scheme": clean_text(parts.scheme), "host": clean_text(parts.hostname)}

    def digest(self, value):
        message = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode("ascii")
        return "hmac-sha256:" + hmac.new(self.home_key, message, hashlib.sha256).hexdigest()


def no_duplicate_keys(pairs):
    obj = {}
    for key, value in pairs:
        if key in obj:
            raise ValueError("duplicate object key")
        obj[key] = value
    return obj


def reject_constant(name):
    raise ValueError("non-finite number")


def load_home_key(path):
    """Return the home key bytes from the closed config, or raise ConfigRefused with pointers only."""
    try:
        text = Path(path).read_text(encoding="utf-8")
        doc = json.loads(text, object_pairs_hook=no_duplicate_keys, parse_constant=reject_constant)
    except (OSError, UnicodeDecodeError, ValueError):
        raise ConfigRefused([""]) from None
    if not isinstance(doc, dict) or set(doc) - CONFIG_KEYS:
        raise ConfigRefused([""])
    home_key = doc.get("home_key")
    if not isinstance(home_key, str) or not HOME_KEY_PATTERN.fullmatch(home_key):
        raise ConfigRefused(["/home_key"])
    return bytes.fromhex(home_key)


def load_event(path):
    try:
        text = sys.stdin.read() if path == "-" else Path(path).read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        raise UsageError("cannot read event: " + type(exc).__name__) from None
    try:
        return json.loads(text, object_pairs_hook=no_duplicate_keys, parse_constant=reject_constant)
    except json.JSONDecodeError:
        raise UsageError("event is not valid JSON") from None
    except ValueError:
        raise UsageError("event contains a duplicate key or a non-finite number") from None


def parse_args(argv):
    if argv in (["-h"], ["--help"]):
        return None
    if len(argv) != 4 or argv[0] != "sanitize" or argv[1] != "--config":
        raise UsageError(USAGE)
    if argv[2] == "-":
        raise UsageError("the config must be a file path, not stdin")
    return argv[2], argv[3]


def refuse(reason, paths):
    print(json.dumps({"result": "refused", "reason": reason, "paths": paths}, sort_keys=True))
    return 1


def main(argv):
    try:
        parsed = parse_args(argv)
        if parsed is None:
            print(USAGE)
            return 0
        config_path, event_path = parsed
        try:
            home_key = load_home_key(config_path)
        except ConfigRefused as refusal:
            return refuse("config_invalid", refusal.paths)
        event = load_event(event_path)
        sanitizer = Sanitizer(home_key)
        try:
            sanitized = sanitizer.event(event)
        except EventRefused:
            return refuse("event_invalid", [""])
    except UsageError as exc:
        print(f"fm-audit-sanitize: {exc}", file=sys.stderr)
        print(USAGE, file=sys.stderr)
        return 2
    print(json.dumps({"result": "sanitized", "event": sanitized, "field_dropped": sanitizer.dropped}, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
