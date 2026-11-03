"""Python results for the string, float, time and int helpers the no-mistakes readers mirror.

Reads one JSON object on stdin with the lists `floats` (decimal text, so inf survives JSON),
`strings`, `epochs` (integers) and `ints` (text), and prints one JSON object with the same
results Python's own functions give for them.
"""

import json
import sys
import time

request = json.load(sys.stdin)


def gmtime_text(value):
    try:
        return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(value))
    except (OverflowError, OSError) as err:
        return {"error": type(err).__name__}


def parse_int(text):
    try:
        value = int(text)
    except ValueError:
        return None
    return value if -(2 ** 63) <= value < 2 ** 63 else None


result = {
    "json_float": [json.dumps(float(text)) for text in request["floats"]],
    "json_string_ascii": [json.dumps(text) for text in request["strings"]],
    "json_string_utf8": [json.dumps(text, ensure_ascii=False) for text in request["strings"]],
    "splitlines": [text.splitlines() for text in request["strings"]],
    "strip": [text.strip() for text in request["strings"]],
    "utc": [gmtime_text(value) for value in request["epochs"]],
    "utc_float": [gmtime_text(float(text)) for text in request["floats"]],
    "parse_int": [parse_int(text) for text in request["ints"]],
    "isspace": [code for code in range(0x110000) if chr(code).isspace()],
}
print(json.dumps(result))
