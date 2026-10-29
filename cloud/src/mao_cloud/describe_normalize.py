"""Rewrite one `gcloud artifacts docker images describe` response as strict JSON (DPES-819).

gcloud can print raw control characters inside JSON strings. `release_evidence` reads them (strict=False);
jq rejects them. The digest-recording workflow runs this once per describe response before any jq read,
so every later reader sees the same strict document. Unparseable output still fails here.
"""

import json
import sys
from pathlib import Path


def normalize_file(path: Path) -> None:
    document = json.loads(path.read_text(encoding="utf-8"), strict=False)
    path.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    if len(args) != 1:
        print("usage: python -m mao_cloud.describe_normalize <describe.json>", file=sys.stderr)
        return 2
    normalize_file(Path(args[0]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
