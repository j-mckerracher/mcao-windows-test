"""Worker log redaction maps pi JSON events into bounded, sanitized records (DPES-815).

Child stdout is untrusted. Only the four pi event types named by the protocol are forwarded, and
only through a fixed field whitelist: no event payload text, arguments, or results are copied, so a
credential inside model or tool output never reaches a log record.
"""

import json

from mao_cloud.pi_events import PiMapping, map_pi_lines
from mao_cloud.protocol import MAX_LOG_LINE_CHARS

SECRET = "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJ3b3JrZXIifQ.c2lnbmF0dXJl"  # gitleaks:allow - synthetic JWT-shaped redaction fixture, not a credential
SIGNED_URL = "https://storage.googleapis.com/bucket/obj?X-Goog-Signature=abcdef0123"


def _event(**fields: object) -> str:
    return json.dumps(fields)


def test_each_named_pi_event_maps_to_one_bounded_record() -> None:
    mapping = map_pi_lines(
        [
            _event(type="message_end", message={"role": "assistant", "content": "hello"}),
            _event(type="tool_execution_start", toolName="bash", args={"command": "ls"}),
            _event(type="tool_execution_end", toolName="bash", isError=False, result="ok"),
            _event(type="agent_end", messages=[]),
        ],
        max_records=10,
    )

    assert mapping == PiMapping(
        records=(
            "pi message_end",
            "pi tool_execution_start tool=bash",
            "pi tool_execution_end tool=bash error=false",
            "pi agent_end",
        ),
        dropped=0,
    )


def test_payload_text_and_credentials_inside_events_are_never_forwarded() -> None:
    mapping = map_pi_lines(
        [
            _event(type="message_end", message={"content": f"token {SECRET}"}),
            _event(type="tool_execution_end", toolName="curl", isError=True, result=SIGNED_URL),
            _event(type="tool_execution_start", toolName="bash", args={"env": SECRET}),
        ],
        max_records=10,
    )

    rendered = "\n".join(mapping.records)
    assert SECRET not in rendered
    assert SIGNED_URL not in rendered
    assert "error=true" in rendered


def test_arbitrary_output_and_unknown_event_types_are_dropped_and_counted() -> None:
    mapping = map_pi_lines(
        [
            f"Authorization: Bearer {SECRET}",
            _event(type="session_start", id="x"),
            json.dumps(["not", "an", "event"]),
            "",
            _event(type="agent_end"),
        ],
        max_records=10,
    )

    assert mapping.records == ("pi agent_end",)
    assert mapping.dropped == 4
    assert SECRET not in "\n".join(mapping.records)


def test_tool_name_outside_the_safe_token_form_is_omitted() -> None:
    mapping = map_pi_lines(
        [_event(type="tool_execution_start", toolName=f"bash {SECRET}")],
        max_records=10,
    )

    assert mapping.records == ("pi tool_execution_start",)
    assert SECRET not in "\n".join(mapping.records)


def test_non_string_tool_name_and_non_boolean_error_flag_are_omitted() -> None:
    mapping = map_pi_lines(
        [_event(type="tool_execution_end", toolName=["bash"], isError="yes")],
        max_records=10,
    )

    assert mapping.records == ("pi tool_execution_end",)


def test_records_are_capped_and_the_overflow_is_counted_not_silently_lost() -> None:
    lines = [_event(type="agent_end") for _ in range(5)]

    mapping = map_pi_lines(lines, max_records=2)

    assert mapping.records == ("pi agent_end", "pi agent_end")
    assert mapping.dropped == 3


def test_an_oversized_output_line_is_dropped_without_being_parsed() -> None:
    oversized = _event(type="agent_end", padding="x" * (2 * 1024 * 1024))

    mapping = map_pi_lines([oversized], max_records=10)

    assert mapping.records == ()
    assert mapping.dropped == 1


def test_every_record_fits_the_protocol_line_limit() -> None:
    long_tool = "a" * 10_000
    mapping = map_pi_lines([_event(type="tool_execution_start", toolName=long_tool)], max_records=1)

    assert all(len(record) <= MAX_LOG_LINE_CHARS for record in mapping.records)
    # An over-long name is not a safe token, so it is omitted rather than truncated into one.
    assert mapping.records == ("pi tool_execution_start",)


def _nested_payload(levels: int) -> str:
    """A pi-shaped object whose total nesting, the outer object included, is exactly levels."""
    arrays = levels - 1
    return '{"type":"agent_end","payload":' + "[" * arrays + "0" + "]" * arrays + "}"


def test_unterminated_string_with_many_escaped_quotes_is_dropped_and_mapping_continues() -> None:
    malformed = '{"type":"message_end","payload":"' + r'\"' * 100_000

    mapping = map_pi_lines([malformed, _event(type="agent_end")], max_records=10)

    assert mapping == PiMapping(records=("pi agent_end",), dropped=1)


def test_deeply_nested_json_is_counted_as_dropped_and_later_records_still_map() -> None:
    # The mapper bounds nesting itself (MAX_PI_EVENT_DEPTH = 32), so the outcome does not depend on
    # the interpreter's recursion limit: 33 levels is dropped, 32 levels still maps. Brackets inside
    # a string value are not nesting and must not count toward the bound.
    bracket_text = 'say \\"hi\\" ' + "[" * 100
    mapping = map_pi_lines(
        [
            _nested_payload(33),
            _event(type="agent_end"),
            _nested_payload(32),
            _event(type="message_end", message={"content": bracket_text}),
        ],
        max_records=10,
    )
    assert mapping == PiMapping(
        records=("pi agent_end", "pi agent_end", "pi message_end"),
        dropped=1,
    )
