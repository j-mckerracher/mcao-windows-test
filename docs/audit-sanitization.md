# Audit sanitization

This page owns the sanitization boundary that every audit event passes through before it is serialized, spooled, logged, or shipped.
`bin/fm-audit-sanitize.py`'s header owns the command mechanics, and its constants and patterns are the authoritative form of the defaults described here.
[docs/audit-envelope.md](audit-envelope.md) owns the envelope this boundary feeds, including the home key lifecycle.

The boundary is the single place where raw free text, commands, arguments, paths, destinations, and diagnostic payloads are reduced to the approved metadata that may reach local storage, error logs, or a SIEM request.
Body export stays disabled: no prompt, model output, retry body, exception body, environment, or file content is kept, and no opt-in exists in this version.

## Scope

This version defines the boundary and nothing that feeds or consumes it.
Producer instrumentation and SIEM transport are later work, and each must route its output through this boundary rather than around it.
The per-home store in [audit-writer.md](audit-writer.md) does not sanitize, so its callers must route events through this boundary first.

## Invocation

`bin/fm-audit-sanitize.py sanitize --config PATH PATH|-` reads one closed config file and one JSON event, from a file or from stdin for `-`.
The config must be a file path, never stdin.
It prints one JSON result line and writes nothing: no file, spool, or log.

| Result | Shape | Exit |
| --- | --- | --- |
| Sanitized | `{"result":"sanitized","event":{...},"field_dropped":[...]}` | `0` |
| Refused | `{"result":"refused","reason":"config_invalid" or "event_invalid","paths":[...]}` | `1` |
| Usage or input error | Message on stderr, naming no value | `2` |

A refusal carries only fixed reasons and JSON Pointers.
A config refusal uses the pointer `""` for the document as a whole or `/home_key` for the key, and an event refusal uses `""`.
A usage or input error covers bad arguments, an unreadable event, invalid JSON, duplicate keys, and non-finite numbers in the event.
Nothing in any refusal or error echoes a config value or an event value.

## Config

The config is a closed JSON object whose only member is `home_key`.
`home_key` is 64 lowercase hex characters, the 32-byte per-home key that the audit envelope's key lifecycle defines.
Any other member, a missing or malformed key, or unparseable JSON refuses with `config_invalid`.
The config cannot extend the allowlists below, and no other config option exists in this version.

## Default policy

These are engineering defaults from the baseline for this boundary, not new product guarantees.

1. **Bodies are dropped.**
   Any member named `prompt`, `model_output`, `retry_body`, `exception_body`, `env`, `environment`, or `file_contents` is dropped at any depth, whatever its type, and its JSON Pointer is recorded in `field_dropped`.
   Dropping is by member name only, so a producer must use these names for bodies; a body stored under any other name is sanitized as text rather than dropped.
2. **Every string leaf is sanitized.**
   Each string value and each object key is redacted through the ordered redaction classes below, then cut to 256 characters.
   Redaction runs before the cut, so a secret that straddles the 256-character boundary is still redacted.
3. **Typed objects keep digests, not values.**
   An object with a `$kind` member is handled as one of the four typed kinds below.
   Any member outside that kind's set drops the whole object and records its pointer.
4. **Allowlists are empty.**
   Nothing is kept from a shell command's executable or from a tool call's scalar arguments until a reviewed entry is added to the code.
   The config file cannot add entries.
5. **A sanitizer error drops the member.**
   Any exception while sanitizing a member drops that member and records its pointer.
   There is no raw fallback.
6. **Text that cannot be carried as UTF-8 is dropped.**
   Lone surrogates and other unencodable text drop as a member, and an object key that cannot be named safely drops under the pointer `[unparseable-key]`.
7. **Only the approved metadata stays.**
   The boundary keeps every member that is not dropped, sanitized; it adds no field of its own beyond the typed-object digests.
8. **A top-level `$kind` is refused.**
   An event whose top level is not a JSON object, or that carries a top-level `$kind`, refuses with `event_invalid`.

### Typed kinds

| Kind | Members | Kept |
| --- | --- | --- |
| `shell_command` | `argv` (a non-empty array of strings) | `argv_digest` over the NFC-normalized argv; `executable` only when its basename is in the empty executable allowlist. |
| `tool_call` | `tool` (string), `args` (object) | `tool` as sanitized text, `args_digest` over the args object; scalar `args` only for names in the empty tool allowlist. |
| `path` | `path` (string), `root` (optional absolute path) | `repo_path` when the path is inside `root` or is a relative path that does not escape; `root` itself becomes `.`. Otherwise `basename` as `~/` plus the sanitized basename, and `path_digest` over the raw path. |
| `destination` | `url` (string) | `scheme` and `host` only; a URL without a scheme or host drops. |

Digests are `hmac-sha256:` followed by the lowercase hex of HMAC-SHA256 over the value's JSON encoding, with sorted keys, compact separators, and ASCII escapes, keyed by the home key.

## Redaction classes

Each match is replaced with `[REDACTED:<class>]`.
The classes apply in the order below, and the exact patterns live in `bin/fm-audit-sanitize.py` and are pinned by `tests/fm-audit-sanitize.test.sh`.

| Class | Matches |
| --- | --- |
| `private_key` | A PEM private key block, through its END line or to the end of the text when no END line is present. |
| `jwt` | A three-segment token beginning `eyJ`. |
| `github_token` | `gh` plus one of `p`, `o`, `u`, `s`, `r`, and an underscore followed by 20 or more alphanumerics, and fine-grained `github_pat_` tokens. |
| `aws_access_key` | An `AKIA` access key ID. |
| `google_api_key` | An `AIza` key of 35 characters after the prefix. |
| `slack_token` | An `xox` token with a `a`, `b`, `p`, `r`, or `s` type letter. |
| `bearer_token` | A `Bearer` credential of 16 or more characters. |
| `credential_assignment` | A `password`, `passwd`, `secret`, `token`, `api` key, or `x-awopt-key` name, matched case-insensitively as Python's case-insensitive match does, so U+0130 and U+0131 match `i`, U+212A matches `k`, and U+017F matches `s` in addition to ASCII case, followed by `:` or `=` and a value. |
| `ssn` | A three-two-four digit pattern with hyphens. |
| `email` | An address with a dotted domain. |
| `phone` | A ten-digit North American pattern whose three groups are separated by a `-`, `.`, or space. The separators are required and only the parentheses around the area code are optional, so an unseparated run of ten digits is not matched. |
| `medical_record_number` | `MRN`, case-insensitively, followed by optional `:`, `#`, or whitespace and five or more digits. It runs before `date_of_birth` because that pattern reaches up to 20 characters past its keyword and would otherwise consume the record number's digits. |
| `date_of_birth` | `DOB` or `date of birth` followed within 20 characters by a digit. |

## Gap-fills

The baseline defined the patterns, the empty allowlists, the every-string-leaf rule, and the failure result, but not the following.
Each was filled with the choice recorded here, and each is a candidate for review.

- The JWT pattern shape and length bounds.
- The private-key extent: BEGIN to END, or to the end of the text when END is missing.
- The replacement class labels.
- The home key's config location and shape: a closed `home_key` config, rather than a key source the envelope contract names.
- What "unparseable" means: text that cannot be encoded as UTF-8.
- The `~/<basename>` format for paths outside `root`, and the raw-path input to `path_digest`.
- The `field_dropped` pointer format, including `[unparseable-key]`.
- The exit code for a refusal, which is `1`.

## Not in scope

- The body-export opt-in, which stays unimplemented.
- Producer instrumentation and SIEM transport, which consume this boundary in later work.
- The rollout of any of the above.
