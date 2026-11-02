# Verification: hook-source differential against the shell and jq

Active empirical record for the session-start source port in `crates/mcao-core/src/hook_source.rs`.
[`hook-source.md`](../hook-source.md) owns the contract; this record owns how the evidence was produced and what it measured.

## Subject

| Field | Value |
|---|---|
| Verified | 2026-10-28 |
| Code under test | `crates/mcao-core/src/hook_source.rs` at commit `25cc691e` |
| Host | Darwin 25.6.0 arm64 |
| Shell | GNU bash 5.3.20 (`bash --version`) |
| awk | BWK awk 20200816 (`awk --version`), run under `LC_ALL=C` |
| jq | jq-1.8.1 (`jq --version`) |
| xxd | 2025-08-24 build (`xxd -v`) |
| Rust | rustc 1.95.0, cargo 1.95.0 |
| Python | 3.14.6 |

## What this record establishes

The awk source extraction matches the wrapper's awk program on every payload in the corpus under the C locale.
The foreign-host predicate does not match `jq -e` on the payload classes listed in the directed table.
Every predicate divergence in the corpus reads as not foreign in the port where jq reads it as foreign, so the port runs where jq would skip.
No payload in the corpus reads as foreign in the port and not foreign in jq.

## Directed cases

The first 19 corpus lines are directed cases, so each row below comes from line 1 to 19 of `shell.txt` and `rust.txt`.
Each awk source column shows the extracted value, and an empty cell means no source.

| # | Case | Shell awk source | Port awk source | jq 1.8.1 | Port |
|---|---|---|---|---|---|
| 1 | awk control: `{"source":"clear"}` | `clear` | `clear` | not foreign | not foreign |
| 2 | foreign control: `{"cursor_version":"x"}` | | | foreign | foreign |
| 3 | BOM before the object | | | foreign | not foreign |
| 4 | `NaN` literal | | | foreign | not foreign |
| 5 | `Infinity` literal | | | foreign | not foreign |
| 6 | leading zero `01` | | | foreign | not foreign |
| 7 | leading plus `+1` | | | foreign | not foreign |
| 8 | leading dot `.5` | | | foreign | not foreign |
| 9 | out-of-range `1e999` | | | foreign | not foreign |
| 10 | out-of-range `-1e999` | | | foreign | not foreign |
| 11 | out-of-range `1.8e308` | | | foreign | not foreign |
| 12 | lone low surrogate escape `"\uDC00"` | | | foreign | not foreign |
| 13 | nesting depth 200 | | | foreign | not foreign |
| 14 | stream `{}` then a foreign object | | | foreign | foreign |
| 15 | stream a foreign object then `{}` | | | not foreign | not foreign |
| 16 | two foreign objects | | | foreign | foreign |
| 17 | foreign object then trailing garbage | | | not foreign | not foreign |
| 18 | NBSP around the colon, under `LC_ALL=C` | | | not foreign | not foreign |
| 19 | trailing quote `{"source":"x"` | `x` | `x` | not foreign | not foreign |

## Reproduction

Work in an empty scratch directory outside the repository.
Save each block below under the file name in its first line comment.

1. Generate the corpus.

   ```sh
   python3 gen-corpus.py 2000 951 > corpus.hex
   ```

2. Run the shell oracle.

   ```sh
   ./shell-oracle.sh corpus.hex > shell.txt 2> shell-stderr.txt
   ```

3. Run the Rust side from the repository root, then delete the helper.
   Set `SCRATCH` to the scratch directory that holds steps 1 and 2.

   ```sh
   SCRATCH=/path/to/scratch
   cp "$SCRATCH/rust-harness.rs" crates/mcao-core/tests/hook_source_differential.rs
   DPES951_CORPUS="$SCRATCH/corpus.hex" DPES951_OUT="$SCRATCH/rust.txt" \
     cargo test -p mcao-core --test hook_source_differential differential_dump -- --nocapture
   rm crates/mcao-core/tests/hook_source_differential.rs
   ```

4. Compare and classify.

   ```sh
   python3 compare.py shell.txt rust.txt
   python3 classify.py shell.txt rust.txt
   ```

### gen-corpus.py

```python
# Generates hook-payload corpora as hex lines. Usage: python3 gen-corpus.py <count> <seed> > corpus.hex
import random, sys
count, seed = int(sys.argv[1]), int(sys.argv[2])
random.seed(seed)
# Byte tokens: ASCII shell space classes, quotes, keys, JSON punctuation, and the
# divergence classes found in review (BOM, NaN, Infinity, leading zero, leading plus,
# leading dot, out-of-range numbers, lone low surrogate escape).
TOKENS = [
    b'"', b'"', b'"', b'source', b'source', b'source', b':', b':',
    b' ', b'\t', b'\n', b'\x0b', b'\x0c', b'\r', b'\xc2\xa0', b'\xe2\x80\x83',
    b'x', b'clear', b'resume', b'{', b'}', b'[', b']', b',', b'\\',
    b'\xff', b'\x00', b'cursor_version', b'\xef\xbb\xbf',
    b'NaN', b'Infinity', b'01', b'+1', b'.5', b'1e999', b'-1e999', b'1.8e308',
    b'\\uDC00', b'null', b'true', b'1',
]
DIRECTED = [
    # awk control and foreign-host control
    b'{"source":"clear"}', b'{"cursor_version":"x"}',
    # jq-accepted classes that the port fails to parse (see docs/hook-source.md)
    b'\xef\xbb\xbf{"cursor_version":"x"}',
    b'{"cursor_version":"x","n":NaN}', b'{"cursor_version":"x","n":Infinity}',
    b'{"cursor_version":"x","n":01}', b'{"cursor_version":"x","n":+1}',
    b'{"cursor_version":"x","n":.5}', b'{"cursor_version":"x","n":1e999}',
    b'{"cursor_version":"x","n":-1e999}', b'{"cursor_version":"x","n":1.8e308}',
    b'{"cursor_version":"x","n":"\\uDC00"}',
    b'{"cursor_version":"x","a":' + b'[' * 200 + b']' * 200 + b'}',
    # stream rules
    b'{} {"cursor_version":"x"}', b'{"cursor_version":"x"} {}',
    b'{"cursor_version":"x"}{"cursor_version":"y"}', b'{"cursor_version":"x"} garbage',
    # awk separator and trailing-quote rules
    b'{"source"\xc2\xa0:\xc2\xa0"x"}', b'{"source":"x"',
]
out = []
for i in range(count):
    if i < len(DIRECTED):
        out.append(DIRECTED[i].hex()); continue
    n = random.randint(0, 16)
    b = b''.join(random.choice(TOKENS) for _ in range(n))
    if i % 4 == 0:  # structured shape around the key
        sep = random.choice([b' ', b' : ', b':', b'\t:\n', b'\x0b:\x0c', b'::', b' x:', b'', b'\xc2\xa0:\xc2\xa0'])
        val = random.choice([b'x', b'clear', b'a b', b'\n'])
        b = b'{' + random.choice([b'', b'junk ', b'"a":"source",']) + b'"source"' + sep + b'"' + val + random.choice([b'"', b'', b'"}']) + random.choice([b'', b'\n', b'}'])
    if i % 9 == 0:  # cursor-shaped objects, sometimes with divergence tokens
        b = b'{"cursor_version":' + random.choice([b'"x"', b'1', b'null']) + b',"n":' + random.choice([b'1', b'NaN', b'01', b'1e999', b'"\\uDC00"']) + b'}' + random.choice([b'', b' ', b' {}', b' garbage'])
    out.append(b.hex())
print('\n'.join(out))
```

### shell-oracle.sh

```sh
#!/usr/bin/env bash
# Shell oracle for the hook-source differential. Usage: shell-oracle.sh <corpus.hex> > shell.txt
# Output per line: <payload-hex> <awk-source-hex> <foreign 0|1>
# Each payload goes through P=$(...), as the wrapper's PAYLOAD=$(cat) does, so NUL bytes and
# trailing newlines are dropped exactly as the shell drops them.
set -u
while IFS= read -r hex; do
  PAYLOAD=$(printf '%s' "$hex" | xxd -r -p)
  SOURCE=$(printf '%s' "$PAYLOAD" | LC_ALL=C awk '
    BEGIN { RS = "\"" }
    seen == 2 { print; exit }
    seen == 1 && $0 ~ /^[[:space:]]*:[[:space:]]*$/ { seen = 2; next }
    seen == 1 { seen = 0 }
    $0 == "source" { seen = 1 }
  ')
  SRCHEX=$(printf '%s' "$SOURCE" | xxd -p | tr -d '\n')
  if printf '%s' "$PAYLOAD" | jq -e '
    type == "object" and has("cursor_version") and (.cursor_version | type) == "string"
  ' >/dev/null 2>&1; then FOREIGN=1; else FOREIGN=0; fi
  printf '%s %s %s\n' "$hex" "$SRCHEX" "$FOREIGN"
done < "$1"
```

### rust-harness.rs

```rust
// Temporary differential helper. Copy to crates/mcao-core/tests/hook_source_differential.rs,
// run with DPES951_CORPUS=<corpus.hex> DPES951_OUT=<rust.txt>, then delete the copy.
use mcao_core::hook_source::{extract_source_bytes, normalize_command_substitution, payload_is_foreign_host};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}
fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn differential_dump() {
    let Ok(corpus) = std::env::var("DPES951_CORPUS") else { return };
    let out_path = std::env::var("DPES951_OUT").unwrap();
    let text = std::fs::read_to_string(corpus).unwrap();
    let mut out = String::new();
    for line in text.lines() {
        let payload = normalize_command_substitution(&unhex(line));
        let src = extract_source_bytes(&payload);
        let foreign = u8::from(payload_is_foreign_host(&payload));
        out.push_str(&format!("{line} {} {foreign}\n", hex(&src)));
    }
    std::fs::write(out_path, out).unwrap();
}
```

### compare.py

```python
# Compare shell.txt against rust.txt line by line. Usage: compare.py shell.txt rust.txt
import sys
def rows(path):
    out = []
    for line in open(path):
        f = line.split()
        if len(f) == 1:          # empty payload and empty awk source
            f = ['', '', f[0]]
        elif len(f) == 2:        # empty awk source: hex column is empty
            f = [f[0], '', f[1]]
        out.append(tuple(f))
    return out
s, r = rows(sys.argv[1]), rows(sys.argv[2])
assert len(s) == len(r), (len(s), len(r))
awk_mis = [(a, b) for a, b in zip(s, r) if a[1] != b[1]]
pred_mis = [(a, b) for a, b in zip(s, r) if a[2] != b[2]]
dirs = {}
for a, b in pred_mis:
    key = ('shell-foreign/rust-not' if a[2] == '1' else 'shell-not/rust-foreign')
    dirs[key] = dirs.get(key, 0) + 1
print(f"cases={len(s)}")
print(f"awk_source_mismatches={len(awk_mis)}")
print(f"nonempty_awk_sources={sum(1 for a in s if a[1])}")
print(f"foreign_predicate_mismatches={len(pred_mis)}")
print(f"shell_foreign_true={sum(1 for a in s if a[2]=='1')}  rust_foreign_true={sum(1 for b in r if b[2]=='1')}")
print(f"predicate_mismatch_directions={dirs}")
for a, b in awk_mis[:5]: print('AWK-MISMATCH shell', a, 'rust', b)
for a, b in pred_mis[:3]: print('PRED-MISMATCH shell', a, 'rust', b)
```

### classify.py

```python
# Count predicate mismatches by the divergence class present in the payload.
import sys
classes = {
    'bom': b'\xef\xbb\xbf', 'NaN': b'NaN', 'Infinity': b'Infinity', 'leading-zero 01': b'01',
    'leading-plus +1': b'+1', 'leading-dot .5': b'.5', 'out-of-range 1e999': b'1e999',
    'lone-low-surrogate \\uDC00': b'\\uDC00',
    'out-of-range 1.8e308': b'1.8e308', 'nesting beyond 128 levels': b'[' * 129,
}
counts = {k: 0 for k in classes}
none = 0
rows = [l.split() for l in open(sys.argv[1])]
rust = [l.split() for l in open(sys.argv[2])]
for s, r in zip(rows, rust):
    sf = s[-1]; rf = r[-1]
    if sf == rf:
        continue
    payload = bytes.fromhex(s[0])
    hit = [k for k, tok in classes.items() if tok in payload]
    if not hit:
        none += 1
    for k in hit:
        counts[k] += 1
print('class-hits (a payload can hit several):', counts)
print('mismatches with no listed class token:', none)
```

## Results

The output below is step 4 from the run that produced this record.
The corpus has 2000 payloads, and `shell-stderr.txt` holds one NUL warning per payload that contains a NUL byte.

```text
cases=2000
awk_source_mismatches=0
nonempty_awk_sources=170
foreign_predicate_mismatches=40
shell_foreign_true=49  rust_foreign_true=9
predicate_mismatch_directions={'shell-foreign/rust-not': 40}
PRED-MISMATCH shell ('efbbbf7b22637572736f725f76657273696f6e223a2278227d', '', '1') rust ('efbbbf7b22637572736f725f76657273696f6e223a2278227d', '', '0')
PRED-MISMATCH shell ('7b22637572736f725f76657273696f6e223a2278222c226e223a4e614e7d', '', '1') rust ('7b22637572736f725f76657273696f6e223a2278222c226e223a4e614e7d', '', '0')
PRED-MISMATCH shell ('7b22637572736f725f76657273696f6e223a2278222c226e223a496e66696e6974797d', '', '1') rust ('7b22637572736f725f76657273696f6e223a2278222c226e223a496e66696e6974797d', '', '0')
class-hits (a payload can hit several): {'bom': 1, 'NaN': 8, 'Infinity': 1, 'leading-zero 01': 7, 'leading-plus +1': 1, 'leading-dot .5': 1, 'out-of-range 1e999': 10, 'lone-low-surrogate \\uDC00': 9, 'out-of-range 1.8e308': 1, 'nesting beyond 128 levels': 1}
mismatches with no listed class token: 0
```

## Limits

- The awk program is checked under the C locale only.
  The `en_US.UTF-8` U+00A0 difference is measured separately and recorded in [`hook-source.md`](../hook-source.md), and this corpus does not cover it.
- The corpus uses a single seed, 951.
  The directed rows cover each class, and the random rows add coverage only for the tokens in `gen-corpus.py`.
- The oracle is jq 1.8.1.
  Other jq releases may accept different edge cases.
- gawk and mawk are not tested, so the awk result for the Linux runner is unverified.
- The random generator produces shallow nesting, so deep nesting appears only in directed row 13.
