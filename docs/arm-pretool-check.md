# Watcher arm PreToolUse seatbelt

This document is the authoritative human-readable contract for the watcher arm PreToolUse seatbelt.
`bin/fm-arm-command-policy.mjs` is the single semantic owner.
`bin/fm-arm-pretool-check.sh` is only the stable harness transport and output renderer.
The tracked harness adapters forward command text without classifying it.
`bin/fm-arm-command-policy.mjs` is also the sole owner of firstmate's shell classification: it exports the tokenizer and command-position analysis, which the sibling cd-guard seatbelt (`bin/fm-cd-pretool-check.sh`, `docs/cd-guard.md`) reuses instead of duplicating shell lexing.

## Advisory status

This seatbelt is an advisory, non-sandbox check for agent mistakes.
It is not a sandbox, an operating-system isolation layer, or a security boundary.
The name refers to this check, not to macOS Seatbelt sandbox profiles.
It classifies the command text a harness hands it before execution, and it does not constrain what an allowed command does at run time.
Any use that needs a security boundary is outside this guard, stays blocked pending a boundary redesign, and is not covered by any acceptance in this document.
The DPES-1001 accepted residual and its open items are listed in [DPES-1001 advisory acceptance and open items](#dpes-1001-advisory-acceptance-and-open-items).

## Purpose and boundary

A firstmate primary must arm `bin/fm-watch-arm.sh` or run `bin/fm-watch-checkpoint.sh` through an observable harness call.
A shell background operator, pipeline, redirection, wrapper, or unrelated command list can hide failure or let the watcher child die with the tool call.
The seatbelt rejects those command shapes before execution.

This policy is not a post-arm liveness guarantee.
`bin/fm-guard.sh` and `bin/fm-turnend-guard.sh` apply their respective post-arm supervision predicates to the watcher lock and beacon after an allowed call.

The classifier never executes, sources, evaluates, or expands any part of the submitted command.
It tokenizes the bytes and classifies lexical execution positions only.

## Transport and fail-open behavior

`bin/fm-arm-pretool-check.sh` supports these entry forms:

- Stdin JSON at `.tool_input.command` for Claude and Codex.
- Stdin JSON at `.toolInput.command` for Grok.
- The command is handed to the classifier on stdin (`--command-stdin`), so its size never meets the argument-length limit (DPES-966 AC5). The OpenCode, Pi, and omp adapters use the same stdin form.
- `--command-stdin` for OpenCode, Pi, and omp (raw command text on stdin). `--command <exact string>` remains for direct callers and is subject to the argument-length limit.
- `--background` as a compatibility-only field that never changes the decision.
- `--claude` to preserve Claude's stderr-only deny requirement.

The wrapper discovers the code root from its own location.
The active firstmate home is `${FM_HOME:-<code-root>}`.
It passes both roots and the exact command string to the Node policy owner.

The wrapper fast-allows a command without invoking the Node policy owner only when the command cannot assemble or name a watcher even after the classifier's decoders run.
The fast path may allow only when all of these hold:

1. The stripped text lacks the `watch` text in any case, after mirroring the classifier's cheapest byte normalizations - dropping line-continuation and escape backslashes, quotes, and newlines.
2. The raw command has no parameter expansion, command substitution, backtick, glob (`*`, `?`, `[`), or brace metacharacter, because any of these can assemble a protected name from pieces with no watch bytes (for example `fm-$X`, `fm-wat{ch,}-arm.sh`, or `bin/*-arm.sh`).
3. The raw command carries no quoting-decoder marker: a `$` immediately followed by a single quote (ANSI-C `$'...'`) or a double quote (bash locale `$"..."`).

Any watch match, any metacharacter in rule 2, or any quoting-decoder marker delegates to the classifier.
The cost is one Node start for every command that contains `$`, a glob, a brace, or a backtick, including ordinary non-watcher commands such as `echo $HOME`.
Normalizing first keeps this a strict superset: a protected watcher path obfuscated as `fm-watc\<newline>h-arm.sh` or `fm-"watch"-arm.sh` still delegates, and stripping only those non-alphanumeric bytes can never destroy an existing `fm-watch` run.
The quoting-decoder marker closes the case the byte strip cannot: `bin/fm-$'\x77'atch-arm.sh` and `bin/fm-$"watch"-arm.sh` both resolve to `bin/fm-watch-arm.sh` only after the classifier decodes the encoded character, so a cheap byte strip would otherwise lose the `fm-watch` bytes and fast-allow them.
This marker set is coupled to the classifier's decoder set in `bin/fm-arm-command-policy.mjs`: adding any new quote or expansion form the classifier decodes requires extending this marker set in the same change, or the prefilter stops being a strict superset.
The prefilter owns no semantic exception: it can only ever fast-allow a command that is definitely not a watcher command, so it never flips a classification and the classifier remains the single owner of every decision.

The seatbelt's threat model is agent mistakes: no one accidentally writes an ANSI-C- or locale-obfuscated watcher path, and deliberate obfuscation is outside this seatbelt's coverage (see [Advisory status](#advisory-status)).
The marker guard closes the static gap anyway because it is cheap and provable per encoding class.
Tripwire: if a third strict-superset gap is ever found after this marker generalization, that falsifies the "provable per encoding class" claim and the decision flips to Option B - drop the prefilter and always invoke the classifier.
Deeper decode-required obfuscation beyond the coupled marker set is outside this seatbelt's coverage; the post-arm liveness predicates check only the watcher lock and beacon, not the command text.

Malformed or empty stdin, invalid JSON, missing `jq` for stdin transport, missing Node, a missing classifier, or an invalid classifier response fail open with exit 0 and no output.
This transport behavior prevents a broken hook from denying every shell tool call.
Malformed or unsupported shell syntax that contains a protected command is a semantic classification result and fails closed.

## Command-position classification

The tokenizer recognizes cooked words with quote provenance, comments, heredoc bodies, shell list operators, pipelines, redirections, command and process substitutions, parenthesized subshells, brace groups, and literal nested execution payloads.
Quoted text, comments, heredoc bodies, and later argument words are data positions unless a recognized execution sink recursively executes them.

A command word in executed position is a protected execution when its normalized path suffix matches one of the protected watcher scripts:

```text
bin/fm-watch-arm.sh          (arm; blessed entry point)
bin/fm-watch-checkpoint.sh   (checkpoint; blessed entry point)
bin/fm-watch.sh              (watch; protected but never blessed)
```

The relative form, the `<code-root>`-anchored absolute form, and any word ending in `/bin/<script>` all resolve to that identity.
A bare script name with no directory part, such as `fm-watch-arm.sh`, also resolves to that identity because PATH resolution reaches the watcher by that name.
The bare name `fm-watch` is protected by identity alone, although no file of that name is installed: a bare `fm-watch` denies `watcher-direct`, and `fm-watch > file` denies `watcher-redirection`.
Identity is compared case-folded, so `FM-WATCH-ARM.SH` and `Fm-Watch-Arm.sh` resolve to the same identity because the default macOS volume is case-insensitive.
Suffix matching recognizes an expanded-path prefix statically, so `$FM_HOME/bin/fm-watch-arm.sh`, `$HOME/firstmate/bin/fm-watch-arm.sh`, and `~/firstmate/bin/fm-watch-arm.sh` are the arm identity.
The classifier never expands the variable or tilde; it matches the literal bytes only.
Static quote forms are cooked before the suffix match, so a command word split by ordinary quotes (`fm-"watch"-arm.sh`), ANSI-C quoting (`fm-$'\x77'atch-arm.sh`), or a bash locale string (`fm-$"watch"-arm.sh`) all resolve to the same identity; this reads the fixed literal bytes as the shell would cook them and never runs an expansion or a command.
This covers statically-visible literal words in command position; opaque dynamic dataflow such as `bash -lc "$WHOLE_COMMAND"` remains out of scope.

A command-position word built with a parameter expansion, command substitution, glob, or brace group that could yield a watcher name is refused as `unclassifiable-protected-command`, for example `${X:-fm-watch-arm.sh}`, `"${X:-fm-watch-arm.sh}"`, `${X:-fm}-watch-arm.sh`, `$X-watch-arm.sh`, `bin/*-arm.sh`, `bin/{fm-watch-arm.sh,x}`, and `bin/$(echo fm-watch-arm.sh)`.
Variables assigned earlier in the same command line resolve to their literal values, so `X=watch-arm.sh; fm-$X | cat` is classified by the resolved name.
A variable assigned from a command substitution is opaque, so a command-position use of it fails closed as `unclassifiable-protected-command`.
The classifier never reads the environment, so a name assembled only from inherited variables with no literal `watch` text remains undetected.

`bin/fm-watch.sh` is protected but is not a blessed entry point.
A direct `bin/fm-watch.sh` execution - relative, `<code-root>`-anchored, `$VAR`-prefixed, or `~`-prefixed - always denies with `watcher-direct`, whose reason points the caller at `bin/fm-watch-arm.sh` and `bin/fm-watch-checkpoint.sh`.

The same bytes in an argument, comment, assertion, documentation query, or `printf` payload are data and do not make the outer command relevant.
A protected name passed as code to an inline interpreter (`python3 -c`, `R -e`, and similar) or sent through `tmux send-keys` is watcher-nested and denies.

Literal `sh`, `bash`, `zsh`, `dash`, `ksh`, `ksh93`, `mksh`, `oksh`, `pdksh`, `ash`, `yash`, `posh`, `fish`, `csh`, or `tcsh` `-c` payloads and literal `eval` payloads are recursively classified.
A `$'...'` inside a double-quoted payload is decoded by the inner shell, so `sh -c "$'bin/fm-watch-arm.sh'"` is classified as the protected command.
A command word that is an expansion, such as `$SHELL -c '...'`, names its program only at run time, so a node that also names a watcher denies as `watcher-nested`.
`source <(...)` and `. <(...)` deny `unclassifiable-protected-command`, the same as `sh <(...)`, because the sourced text comes from a substitution that cannot be read statically.
A literal nested payload that only runs a data-bearing command is allowed.
A literal nested payload that executes a protected command is denied as `watcher-nested`, even when that inner protected call would be allowed at top level.
A heredoc or literal here-string fed to a shell that reads its program from stdin is classified the same way.
With `-s`, later operands set positional parameters rather than naming a script, so `bash -s sentinel <<< 'bin/fm-watch.sh'` is denied.
An operand after `-s --` remains positional, while a protected watcher path in the first operand position is still denied.

Dynamic payloads such as `bash -lc "$WATCHER_COMMAND"` cannot be proven statically and are outside this seatbelt's coverage.
If the submitted command first constructs a protected literal assignment and then feeds a dynamic value to a recognized shell or `eval` sink, the classifier denies conservatively as `watcher-nested`.

Comments and heredoc bodies are ignored as execution syntax.
An actual protected command with a heredoc still has a redirection and is denied.

## Blessed syntax tree

An allowed watcher program is one linear outer command list with zero or more approved setup nodes followed by exactly one direct protected node.
`bin/fm-watch-arm.sh` and `bin/fm-watch-checkpoint.sh` are the only blessed final nodes, including their expanded-path forms; a `bin/fm-watch.sh` final node is never blessed and denies with `watcher-direct`.

Approved setup nodes are:

- `cd <one path word>`.
  When the final protected path is relative, such as `bin/fm-watch-arm.sh`, the cd target must be the code root itself; any other target denies `watcher-bundled`, because a relative path after a changed directory names a different file.
- `export NAME=<one shell word>` with no command substitution, process substitution, or redirection, except a `PATH`-class, loader, or startup variable.
  Refused names are `PATH`, `CDPATH`, `BASH_ENV`, `ENV`, `IFS`, `SHELLOPTS`, `BASHOPTS`, `PS4`, `PROMPT_COMMAND`, `HOME`, `ZDOTDIR`, `LD_*`, `DYLD_*`, `PERL*`, `RUBY*`, `NODE_*`, `PYTHON*`, and `GIT_*`, because each changes which executable, module, or startup code a protected call loads, or which file a `~` path names.

Approved nodes may be separated by `;`, a real newline, or `&&`.
`&&` is accepted after setup so a failed `cd`, `export`, or source prevents the protected call from running under the wrong setup.

The final protected node may have one immediate `exec` wrapper.
Its arguments are ordinary shell words and may contain quoted semicolons or watcher names.
No other wrapper is approved.
`!` negation, `nice`, `stdbuf`, `caffeinate`, and `builtin` are not approved wrappers, so a protected name behind them is not blessed.

Inline environment assignments, `env`, `sudo`, `nohup`, nested shells, `eval`, subshell groups, substitutions, redirections, pipelines, asynchronous lists, `disown`, unrelated list nodes, and unsupported compound syntax are not blessed.

## Broad watcher kills

An actually executed `pkill` command is denied when its parsed pattern arguments target `fm-watch`.
Matching is case-insensitive, so `pkill -if FM-WATCH` and `pkill -fi FM-WATCH` are denied, because macOS `pkill -i` folds case in the process table.
A pattern that is a regular expression matching any protected basename, such as `pkill -f 'fm-w.*tch'`, is also denied.
Path-qualified `pkill`, `command pkill`, and `sudo pkill` are recognized.
`killall` takes the same name rule, so `killall fm-watch.sh` is denied.
`fuser` is a kill command under the same rule, so `fuser -k bin/fm-watch.sh` is denied.
A pattern is read under two interpretations: the JavaScript regex, and the ERE that `pkill` uses, where a backslash before X is the literal X and POSIX bracket classes are valid.
Either reading that matches a protected basename denies, and a pattern that cannot be read under either reading fails closed.
Line-local variables resolve before the match, so `P=fm-wat; Q=ch-arm.sh; pkill -f "$P$Q"` is denied.
A name set by a non-prefix construct, or by a command substitution, matches any text.
A pipeline that selects the watcher with `grep`, `awk`, `pgrep`, or similar and feeds `kill`, `killall`, or `xargs kill` is denied, as is a kill whose command substitution names `fm-watch`, such as `kill -9 $(ps -ef | grep fm-watch | awk ...)`.
A command substitution in a kill-family pattern denies, because its output cannot be known.

`kill "$(pgrep -f '/bin/fm-watch.sh')"` is also denied because the executed `kill` consumes an executed watcher-wide `pgrep` substitution.
A standalone read-only `pgrep`, including `pgrep -if FM-WATCH`, is allowed; `pgrep -if FM-WATCH | xargs kill` is denied as a watcher kill.
Quoted text such as `echo 'pkill -f fm-watch'` is data and is allowed.

Unsupported compound grammar - a loop, `case`, `if`, or other construct the classifier does not model - is failed closed for broad kills the same way it is for protected executions.
When the command carries such grammar and its raw bytes reference both a `fm-watch` target and a `pkill` or `kill` verb, the classifier cannot prove which command position the kill occupies, so it denies with `broad-watcher-kill` rather than allowing.
This backstop mirrors the protected-execution fail-closed rule and covers forms like `while true; do pkill -f fm-watch; done`, `for x in 1; do pkill -f fm-watch; done`, `case x in x) pkill -f fm-watch ;; esac`, and `until false; do kill $(pgrep -f fm-watch); done`.
It is gated on the grammar being unsupported: in grammar the classifier does model, command-position analysis is authoritative, so data mentions such as `echo 'pkill -f fm-watch'` and a loop that only names the watcher without a kill verb such as `for f in 1; do echo fm-watch; done` remain allowed.

## Stable reason codes

Every semantic deny includes one stable code in square brackets before its prose reason.

| Code | Meaning |
| --- | --- |
| `watcher-background` | A protected execution is in an asynchronous list or uses `nohup` or `disown`. |
| `watcher-pipeline` | A protected execution participates in any pipeline. |
| `watcher-redirection` | A protected execution uses shell redirection. |
| `watcher-bundled` | The outer command list is not the blessed setup-plus-final tree. |
| `watcher-nested` | A wrapper, group, substitution, nested shell, `eval`, or constructed dynamic payload executes the protected command. |
| `broad-watcher-kill` | An actual broad process kill targets the watcher. |
| `unclassifiable-protected-command` | Malformed or unsupported syntax contains a protected command and cannot be safely classified. |
| `watcher-direct` | A direct `bin/fm-watch.sh` execution; the watcher must be reached through `bin/fm-watch-arm.sh` or `bin/fm-watch-checkpoint.sh`. |

Reason codes are the stable contract for tests and adapters.
Prose may improve without changing adapter behavior.

## Output contract

- Allow returns exit 0 with both streams empty.
- Deny returns exit 2 and writes `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny"},"systemMessage":"[code] reason"}` to stderr.
- Default deny mode also writes `{"decision":"deny","reason":"[code] reason"}` to stdout for Grok.
- `--claude` suppresses stdout completely because Claude ignores a PreToolUse deny when stdout is nonempty.
- Codex blocks on exit 2 and displays stderr.
- OpenCode throws only when the checker exits 2.
- Pi, pi-signed, and omp return `{block: true}` only when the checker exits 2.

## Harness wiring

| Harness | Exact command field | Adapter behavior on checker exit 2 |
| --- | --- | --- |
| Codex | `.tool_input.command` | The `.codex/hooks.json` command forwards the complete stdin payload and Codex blocks on exit 2. |
| Claude | `.tool_input.command` | `.claude/settings.json` forwards stdin with `--claude`, leaving stdout empty and returning the stderr deny object. |
| Grok | `.toolInput.command` | `.grok/hooks/fm-primary-pretool-check.json` forwards stdin and Grok consumes the stdout `decision=deny` object. |
| OpenCode | `output.args.command` | `.opencode/plugins/fm-primary-pretool-check.js` sends the command on stdin (`--command-stdin`) and throws only for exit 2. |
| Pi / pi-signed | `event.input.command` | `.pi/extensions/fm-primary-turnend-guard.ts` sends the command on stdin (`--command-stdin`) and returns `{block: true}` only for exit 2. |
| omp | `event.input.command` | `.omp/extensions/fm-primary-turnend-guard.ts` sends the command on stdin (`--command-stdin`) and returns `{block: true, reason}` only for exit 2; omp surfaces the reason verbatim to the model (verified 18.1.2). |
| Cursor | `.tool_input.command` | `.cursor/hooks.json` matches `tool_name` `Shell` and forwards stdin with `--cursor`. Cursor reads the RETURNED object rather than the exit status, so `--cursor` prints `{"permission":"deny","user_message":"[code] reason"}` on stdout and exits 0; only that rendering is verified to block the command and surface the reason. |

Cursor also loads `<project>/.claude/settings.json`, so the tracked Claude entry receives the same event. Without `--cursor` a Cursor-delivered payload is that duplicate and allows without re-classifying, decided from the payload's own `cursor_version` by `bin/fm-hook-host-lib.sh`; [`turnend-guard.md`](turnend-guard.md#harness-integrations) owns why that predicate reads the payload rather than the environment.

Grok project hooks require folder trust.
Cursor project hooks require the workspace to be launched with `--trust`.
Every shell variable reference in a Grok hook command must carry an inline default such as `${GROK_WORKSPACE_ROOT:-}` because Grok expands the raw hook command before `bash -lc` runs it.
The tracked Grok adapter therefore references `${GROK_WORKSPACE_ROOT:-}` directly instead of assigning and later reading a shell-local `$root` variable.

## Live validation record, 2026-07-09

Validation ran in a git-initialized scratch firstmate-shaped project under this task worktree.
The scratch project contained copies of the modified checker and policy, unchanged tracked adapters, a dummy checkpoint, a dummy arm script, a harmless `tmux` argument-capture fixture, and a private sentinel path.
No modified file was installed into the primary checkout or a live harness configuration.
No live watcher, fleet state, or herdr lifecycle command was used.
The OpenCode interactive check used the dedicated tmux socket `fm-pretool-smoke`.

Harness versions were:

```text
Claude Code 2.1.206
codex-cli 0.144.0
grok 0.2.93 (f00f96316d4b)
OpenCode 1.17.15
Pi 0.80.5
```

Every harness was instructed to issue these exact shell command strings as separate tool calls:

```sh
printf 'UNRELATED_EXECUTED\n'
pgrep -fl '/bin/fm-watch.sh' || true
bin/fm-watch-checkpoint.sh --seconds 180
tmux send-keys -t isolated-pi-lab "printf '%s\n' 'bin/fm-watch-arm.sh &'"; tmux send-keys -t isolated-pi-lab Enter
bin/fm-watch-arm.sh &
```

The real harness launch commands were:

```sh
claude -p "$PROMPT" --dangerously-skip-permissions --output-format text
codex exec --dangerously-bypass-hook-trust --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check "$PROMPT"
GROK_HOME="$SCRATCH_GROK_HOME" RUST_LOG=xai_grok_hooks=debug GROK_LOG_FILE="$SCRATCH_LOG" grok --trust -p "$PROMPT" --permission-mode bypassPermissions --output-format plain
OPENCODE_CONFIG_CONTENT='{"permission":{"*":"allow"}}' opencode run --print-logs --log-level INFO "$PROMPT"
pi -p -e .pi/extensions/fm-primary-turnend-guard.ts --no-context-files --no-session "$PROMPT"
```

Observed output for the four allowed calls was `UNRELATED_EXECUTED`, a successful read-only `pgrep`, `CHECKPOINT_EXECUTED`, and two `TMUX_ARGS:` lines that preserved the watcher text as data.
Each harness blocked the final command with exit 2 mapped through its native adapter behavior.
The stable reason was `[watcher-background] a protected watcher command cannot run in an asynchronous shell list or through nohup/disown`.
The dummy arm body would have created `<harness>.sentinel` if the denied command executed.
All five sentinel files remained absent.

The Codex transcript showed `PreToolUse Completed` for all three originally reported false-positive shapes and `PreToolUse Blocked` only for the backgrounded arm.
The Grok debug transcript showed four exit-0 results from `project/fm-primary-pretool-check`, then exit 2 with 145 stdout bytes, 214 stderr bytes, and `hook denied` for the backgrounded arm.
OpenCode displayed the four allowed command outputs and then `bin/fm-watch-arm.sh & failed` with the stderr deny object.
Claude and Pi both reported that calls one through four ran and the final call was blocked.

Native supervision paths were also validated in the same scratch project:

- Claude ran `bin/fm-watch-arm.sh --restart` with its native tracked background option and produced `watcher: started pid=<scratch> (scratch)`.
- Grok ran the same exact command with `background: true`, its hook returned exit 0, and the dummy arm produced the same started line.
- Codex ran the foreground checkpoint above and produced `CHECKPOINT_EXECUTED`.
- OpenCode ran in an interactive TUI on `tmux -L fm-pretool-smoke`, reached `session.idle`, and its unchanged watch-arm plugin created the scratch automatic-arm marker.
- Pi loaded both primary extensions, called `fm_watch_arm_pi`, and created the scratch automatic-arm marker.

Every native-path automatic marker was present and every deny sentinel remained absent.

## Compatibility notes

These are the known differences between the DPES-893 and DPES-954 acceptance text and the behavior the classifier produces.
DPES-893 AC8 is met for the bare name: `fm-watch > file` denies `watcher-redirection`, and a bare `fm-watch` is protected by identity.
`fm-watch.sh > file` and `bin/fm-watch.sh > file` deny as `watcher-direct`, not `watcher-redirection`, because the direct-watcher check runs before the redirection check.
`xargs` launching a protected name, as in `echo x | xargs fm-watch-arm.sh`, denies `watcher-pipeline`.
`xargs` launching a shell, a pipe into a shell, a shell fed by a command or process substitution, and a shell reading a file or unknown stdin deny `unclassifiable-protected-command`.
`xargs -I{} {}` (a replacement used as the command) denies `unclassifiable-protected-command` when its input is not visible: a file redirect (`< names.txt`), or an upstream command other than a literal `echo` or `printf` (`cat names.txt | xargs -I{} {}`). `printf x | xargs -I{} {}` stays allowed, because its input is a literal that names no watcher.
A heredoc or herestring payload is classified as literal text.
A command substitution that yields a protected name, as in `bin/$(echo fm-watch-arm.sh) | cat`, denies `unclassifiable-protected-command`.

Still allowed by design (residuals):

- A name assembled only from inherited environment variables with no literal text, such as `$X | cat` or `"$EDITOR" | cat` when `X` or `EDITOR` inherits `fm-watch-arm.sh`, and an inherited variable inside a kill pattern, such as `pkill -f "fm-w${X}tch"`. The classifier never reads the environment.
  A name set by any construct in the same command line is not inherited, and is denied; see the over-blocks below.
- An approved `export FM_HOME=...` changes the home the policy and watcher read, and a final `$FM_HOME/bin/fm-watch-arm.sh` path expands from that value, which the classifier does not check.
- Dynamic payloads with no literal protected text, such as `eval "$X" | cat`. These are outside this seatbelt's coverage.
- `eval "$(printf ...)"` with a command substitution that yields a watcher name is allowed. Tracked in DPES-963.
- Aliases. `alias a=...; a` is allowed, because non-interactive shells do not expand aliases. This is DPES-1001 finding F; DPES-1001 does not accept it, and no owner disposition is recorded yet.
- Copying, linking, or writing the protected script to a new file and then running that file is allowed. This is the file-execution residual (DPES-1001 finding B). Owner decision 11 accepts it for this advisory seatbelt only; see [DPES-1001 advisory acceptance and open items](#dpes-1001-advisory-acceptance-and-open-items).
- Globs and variables that cannot match any protected basename, such as `bin/*.md | cat`.
- A GNU `xargs -I{} bin/{}` launch names its program only after its input arrives, so `echo fm-watch-arm.sh | xargs -I{} bin/{}` is allowed. On macOS that form does not launch (`bin/{}` is missing); GNU xargs behavior is not verified.
- The contents of a file sourced with `.` or `source`, and of a file a shell runs by name, are not classified.

Over-blocks, refused although they name no watcher:

- An arithmetic expansion containing a command substitution followed by an operator and operand, such as `echo $(( $(date +%s) - 5 ))`, denies `watcher-nested`. `echo $(( 5 - $(date +%s) ))` and `echo $(( 1 + $(date +%s) ))` are allowed. Lexer arithmetic handling is not changed.
- A command-position use of a variable assigned from a command substitution, such as `X=$(date); $X | cat`, denies `unclassifiable-protected-command` because its value cannot be known.
- A command-position use of a name that a non-prefix construct sets anywhere in the command line, such as `read X <<< fm-watch.sh; bin/$X &`, `printf -v`, `declare`, `getopts`, `mapfile`, `read`, a `for` or `select` variable, `${X:=...}`, or a name assigned inside `eval`, a brace group, or a nested shell, denies `unclassifiable-protected-command`, because the same line sets its value.
- A relative or non-root cd target before a relative protected path, such as `cd ../firstmate; bin/fm-watch-checkpoint.sh`, or `cd "$ROOT"` expanded at run time, denies `watcher-bundled`, because the classifier cannot verify that the target is the code root.
  Run the watcher with an absolute path or from a bare name instead.

- A parameter expansion other than a plain `${NAME}` in command position, such as `${X/a/b}`, `${!X}`, `${A[0]}`, or `${X:-cmd}`, denies `unclassifiable-protected-command`, because the classifier cannot resolve its value.
- A `!` negation, or `nice`, `stdbuf`, `caffeinate`, or `builtin`, before an arm denies `watcher-nested`, and before a `bin/fm-watch.sh` denies `watcher-direct`.
  Unlisted options of those wrappers fail closed when the same line names a protected watcher.

PATH trade-off:

- A `PATH` export is refused as setup. `export PATH=/tmp/x:$PATH; bin/fm-watch-arm.sh` denies `watcher-bundled`, because an approved `PATH` export let a bare `fm-watch-arm.sh` resolve to a substituted executable (DPES-954 AC1 wording).
  The cost is that a legitimate PATH override before arming is refused; set PATH outside the tool call instead.
  A bare `fm-watch-arm.sh` still resolves through the inherited PATH unchanged.
- Setup exports of loader, module, and home variables, such as `PERL5OPT`, `NODE_OPTIONS`, `GIT_EXEC_PATH`, `PYTHONSTARTUP`, and `HOME`, are refused for the same reason: each changes which code a protected call loads.
  Set them outside the tool call.
- Setup exports of other variables, such as `FM_HOME`, remain approved.

Platforms: verified on macOS with APFS, whose default volume is case-insensitive. Windows and inherited-PATH behavior are not verified.

## DPES-966 wrapper classification

The policy reads command position through `Lexer`, `splitProgram`, and `commandPosition`, and `armCommandPosition` skips the arm-specific wrappers before that. The following rules close the wrapper bypasses reproduced on PR 333 attempt 1 (review report `data/review-333-a1/report.md`):

- Unlisted executors. A node whose command word is in `UNMODELED_EXECUTORS` (`ssh`, `su`, `script`, `strace`, `docker`, `nsenter`, `runuser`, `busybox`, `bunx`, `fd`, `fdfind`, `npx`, `parallel`, `pnpx`, `expect`, `screen`, `gdb`, `make`, `flock`, `setsid`, `ionice`, `chrt`, `taskset`, `systemd-run`, `unshare`, `numactl`, `prlimit`, `cgexec`, `firejail`, `valgrind`, `perf`, `chronic`, `launchctl`, and similar), a `find` with `-exec`, `-execdir`, `-ok`, or `-okdir`, a `git` with `-c`, a `git rebase` with `--exec`/`-x`, a `git submodule foreach`, a `git bisect run`, an `npm exec`, a `sed` script with an `e` command or s///e flag (text between slashes is ignored), or a `tmux` with an executing subcommand (`new-window`, `split-window`, `respawn-pane`, `run-shell`, `if-shell`, `pipe-pane`) denies `watcher-nested` when any text of the node names a watcher. Read-only data arguments are not on the list and stay allowed. `xargs` whose launched word is its replacement string (`xargs -I{} {}`) denies when a watcher name appears anywhere in the command line.
- Shell option words. `-o`, `+o`, `-O`, and `+O` take the next word as their argument unless the rest of the cluster is attached, and `--rcfile` and `--init-file` take one. `-c` is then read from the remaining words, so `bash -o pipefail -c 'bin/fm-watch-arm.sh &'` denies.
- Inline interpreters and package runners. `awk`, `perl`, `python`, `ruby`, `node`, `tclsh`, `lua`, `php`, `deno`, and `bun` run inline code (`-c`, `-e`, an awk program, or stdin) that this parser does not read. Versioned `lua`, `luajit`, `perl`, `php`, `ruby`, and `node` names (for example `lua5.4`, `luajit2.1`, `perl5.36`, `php8.2`, `ruby3.2`, and `node20`) are included. `pnpm` and `yarn` with `exec` or `dlx`, and `pnpx` and `bunx`, run an operand as a command. A node that names one of these and also names a protected watcher fails closed as `watcher-nested`, so a benign mention inside `python3 -c` also denies.
- Editors and fzf. The vi family (`vi`, `view`, `ex`, `gvim`, `gview`, `vimdiff`, `vim`, `nvim`, and aliases such as `vim.basic`), `ed`, `red`, and `emacs` are unmodeled executors, so a protected watcher name in their arguments fails closed. An editor fed commands on a pipe is checked too: a pipeline into an editor denies when any part of the program names a watcher. For fzf, every action argument is classified as shell text whatever the action name is, because the executing actions change across fzf releases. An action name is a word after `:` or `+`. Its argument is delimited by `(...)`, `[...]`, `<...>`, `{...}`, or any other punctuation (`~...~`, `!...!`, `@...@`, `#...#`, `$...$`, `%...%`, `^...^`, `&...&`, `*...*`, `;...;`, `/.../`, `|...|`), and `:` is the open-ended form that runs to the end of the binding. The fzf(1) pages for 0.24.3, 0.35.0, 0.44.1, and v0.74.5 were checked for these forms. A protected command denies while a data-only mention and a benign payload such as `execute[echo ok]` do not. A protected name in a non-executing action argument also denies, which is conservative.
- `tmux send-keys` and `tmux send` type their text into a pane, which runs it as a shell command line, so they are in `TMUX_EXECUTING_SUBCOMMANDS`.
- `sed` `e` (GNU sed only; BSD sed has no `e` command). Each command of the script is read in turn: addresses and the bodies of `s` and `y` are skipped, and an `e` command or an `s///e` flag denies. A `-f` script file fails closed.
- `git` accepts an unambiguous prefix of a long option, so `git rebase --exe` is `--exec` and denies.
- Brace words. A `{` opens a brace group only before whitespace, as bash does. `{}` is an ordinary word, so `xargs -I {} bin/fm-watch-arm.sh` reaches `xargs` option parsing and denies.
- `env`. `-P` takes an argument (macOS `env -P utilpath`); GNU rejects `-P`, so both platforms are covered. A long option (`--unset`, `--chdir`, and others) makes the operand platform-dependent, so a node with a watcher name fails closed. A dash word after an assignment is the utility on GNU but may be read as an option on BSD, so it is unresolved and fails closed when a watcher name is present. A bare `-` before any assignment is GNU's `-i` synonym.
- `watch` runs its operand words as one shell command line, so they are analyzed as a payload. `watch -n 2 'pgrep -fl fm-watch'` stays allowed; `watch -n 2 bin/fm-watch-arm.sh` denies.
- Kill selectors. `lsof`, `fuser`, and `pidof` feeding `kill`, `killall`, or `xargs kill` in one pipeline select the watcher and deny as a broad kill.
- `xargs` options are consumed before the launched command, as `commandPosition` models them. An unknown option stays unresolved.

DPES-1001 attempt 1 (PR 369, findings 1 to 9). Each rule below has a matrix row in `tests/fm-arm-pretool-check.test.sh` (Z01 to Z167; the DPES-1005 rows are Z72 to Z167, the review-1 rows are Z108 to Z131, the review-2 rows are Z132 to Z143, and the review-3 rows are Z144 to Z167):

- `sh -c` keeps reading options after `-c` (`-e`, `-s`, `-l`, `-o NAME`, and `--`). The first non-option word is the command string. The words after it (`$0`, `$@`, `$N`) are operands the string can run, so a watcher name among them denies `watcher-nested`.
  Positional parameters are tracked through the line: `set -- ...` replaces them and `shift` drops them. Clustered options that contain `o`, such as `set -eo pipefail` and `set -oe pipefail`, consume the option-name operand and do not replace the positional list. A known literal `$N` is resolved; `${1:-x}`, `${1-x}`, `${1:0}`, `${@:1}`, and other positional operators are treated as unknown. Unknown or watcher-derived positional values deny when used in command position, as an `eval` argument, or as a sourced script (`sh -c 'set -- bin/fm-watch.sh; eval "$1"'`). Assignments from positionals retain that uncertainty, including `$@` and `$*`; known benign `$1` assignments remain resolvable. An untracked positional used at one of these execution sinks fails closed.
  A positional or assigned value containing an unquoted glob or brace metacharacter (`*`, `?`, `[`, `{`) is unknown where it runs, because the shell expands it to paths the literal text does not name, as in `set -- bin/fm-w*.sh; eval "$1"` and `set -- bin/fm-watch.{sh,}; eval "$1"`. Brace expansion is shell-specific: bash and `/bin/sh` on the verifying host expand it, while dash does not. The rule denies in every shell.
  Indirection, `${!1}` and `${!NAME}`, expands to the variable a value names. The classifier cannot resolve that variable, so the expansion is unknown and fails closed at an execution sink. A benign use such as `echo "${!1}"` allows.
- `trap` runs its action as shell text when the signal fires. A `trap` node that names a watcher denies `watcher-nested`.
- Inside double quotes a backslash escapes only `$`, backtick, quote, and backslash. Before any other character it stays literal, so `"$'\x62in/fm-watch.sh'"` keeps the escape for the inner shell to decode.
- Runners and interpreters added to the fail-closed lists: `uv`, `poetry`, `bundle`, `mise`, `asdf`, `corepack`, `nix`, `direnv`, `devbox`, `pypy`, `ipython`, `luajit`, `Rscript`, `julia`, `groovy`, `guile`, `ts-node`, `tsx`, `coffee`, `erl`, `elixir`, `scala`. Shells and launchers added to the same lists: `elvish`, `nu`, `xonsh`, `osh`, `ion`, `pwsh`, `xterm`, `gnome-terminal`, `watchexec`, `entr`, `setpriv`.
- `git filter-branch`, `git filter-repo`, and `git difftool -x` (or `--extcmd`) run an operand as a command.
- `xargs -a` and `--arg-file` read their arguments from a file this policy cannot see, so a replacement command (`xargs -I{} {}`) fails closed. GNU long options may be abbreviated as `getopt_long` allows (`--arg-f`, `--arg-f=names.txt`, `--ar=`); an ambiguous or unknown prefix is unresolved and fails closed.
- GNU `sed`: an `e` command after an address range (`/a/,/b/e`) or inside a `{` block is read. With `-e`, the operands are also read as script text, because a static reader cannot pair each word with its option.
- Residual: a runner, interpreter, or shell that is not on these lists, with a protected name in its arguments, is still allowed. The lists are closed by name, so a new runner needs its own entry.

Known gaps (tracked, not closed by DPES-966):

- pi-signed was not separately checked.
- Prefix-assignment expansion order (`W=bin/fm-watch-arm.sh nice -n 5 "$W"`): bash expands `"$W"` before the prefix assignment takes effect, so this allows. It is DPES-991's residual and is unchanged here.
- Command substitution inside `eval` remains a DPES-963 residual: `eval "$(echo bin/fm-watch-arm.sh)"` can execute a protected command, but this analyzer does not resolve the substitution output. This gap is documented, not fixed here.
- `Z103` (`eval "$(printf '%s' 'echo hi')"` allow) pins the allow direction of the DPES-963 residual; it must flip if DPES-963 is ever fixed.
- Named-variable parameter operators such as `${NAME%.sh}` and `${NAME:0}` can carry a protected value past variable tracking. This is pre-existing (the base commit allows them), it is outside DPES-1005's positional scope, and it is reported as a separate outstanding issue, not fixed here.
- A redirected file fed to an editor (`ex -s < cmds.txt`, `ed -s < cmds.txt`) fails closed in the classifier, but the hook's fast path allows a command that has no watcher bytes and no expansion metacharacters without calling the classifier, so a plain `< file` editor command is allowed at the hook. File contents are unknowable there. This is a residual, not closed, and no transport row pins it.
- Classifier error paths (missing Node, unusable output, policy exception) still fail open. DPES-966 changes only the transport for the stdin adapters, and does not broaden deny-on-error.
- `env` platform behavior is verified on macOS only. GNU env was not available in this environment, so the GNU stop-at-assignment reading is taken from the PR 333 review and not re-run here.
- The Rust port (`crates/mcao-core/src/shell_command_pos.rs`) mirrors `commandPosition` only and is not wired to any gate. The Node analyzer rules above (executors, shell option words, watch payloads, kill selectors) have no Rust mirror yet; the port must not gate until both are reviewed.

## Rust verdict port

`crates/mcao-core/src/arm_verdict.rs` mirrors the policy verdict over node-level inputs, including the bare `fm-watch` identity and its own `watcher-direct` reason (`DenyCode::WatcherDirectBare`).
Its unsafe setup-variable list matches the Node list above.
`NodeInfo.cd_target_is_root` is supplied by the caller, because the verdict has no code root of its own; a relative final protected path after a `cd` is blessed only when that flag is set.
It is not wired into the live seatbelt; the Node policy remains the only live owner, and `crates/mcao-core/src/lib.rs` only declares the module.
The public enums (`ProtectedKind`, `ArmDecision`) and public-field structs (`NodeInfo`, `Analysis`) are `#[non_exhaustive]`, so a variant or field added with a later policy change is not a breaking change for a caller.
External callers must match enums with a wildcard arm and must not build these structs with literals; they read fields.
This is the DPES-963 AC8 decision, taken before any external caller exists; revisit it if a caller arrives first.

## DPES-1001 advisory acceptance and open items

Owner decision 11, recorded on DPES-1001 when it was reopened on 2026-11-01, accepts the documented copy, link, and write-then-execute residual for this seatbelt only, as an advisory, non-sandbox check.
DPES-1001 accepts nothing else, and no unexplored bypass is accepted.

Each probe below is a classifier input.
Run it through the shell transport with `bin/fm-arm-pretool-check.sh --claude --command '<probe>'; echo $?`.
Exit 0 means allow and exit 2 means deny.
The transport never executes the probe text, and no probe may be run as a live command.

### Accepted residual: copy, link, or write-then-execute

The policy models executors by name, in `UNMODELED_EXECUTORS` and the shell names in `SHELL_NAMES`, both in `bin/fm-arm-command-policy.mjs`.
`cp`, `ln`, and `cat` are not modeled, and a file that a shell runs by name is not classified.
So a copy, link, or regenerated file is a new path that no rule names, and the protected script appears only as a data argument.
The nearest pinned row is `E18 allow 'cat fm-watch-arm.sh'` in `tests/fm-arm-pretool-check.test.sh`.
No test row pins copy-then-run, so the probes below are the evidence for this residual.

| Probe | Classifier input | Result on this branch |
| --- | --- | --- |
| copy | `cp bin/fm-watch-arm.sh /tmp/fm-copy.sh && bash /tmp/fm-copy.sh` | allow, exit 0 |
| link | `ln -s bin/fm-watch-arm.sh /tmp/fm-link.sh && /tmp/fm-link.sh` | allow, exit 0 |
| write | `cat bin/fm-watch-arm.sh > /tmp/fm-write.sh && sh /tmp/fm-write.sh` | allow, exit 0 |
| control, blessed form | `bin/fm-watch-arm.sh --restart` | allow, exit 0 |
| control, background form | `bin/fm-watch-arm.sh &` | deny, exit 2, `watcher-background` |

The acceptance covers only this residual.
It does not extend to any later command that reads the copy's contents or name.

### Open, not accepted: actionable bypasses under DPES-1005 and DPES-1017

These bypasses are actionable and are not accepted.
They stay open under DPES-1005, the item that recorded PR 369's third-review bypasses, and DPES-1017, the follow-up that holds the remaining PR 399 review findings.

- Formatted text piped to an editor (DPES-1005 finding A1; DPES-1017 A1). Probe `printf 'bin/fm-%s-arm.sh\n' watch | vim -es` is allowed, exit 0. Rows Z165 and Z166 deny only the literal form, `printf '!bin/fm-watch.sh\n' | ex -s` and `| ed -s`.
- Editor commands reading a redirected file pass the hook fast path (DPES-1005 A2; DPES-1017 A2). Probe `ex -s < /tmp/fm-cmds.txt` is allowed, exit 0, because it has no watcher bytes and the prefilter in `bin/fm-arm-pretool-check.sh` allows it without calling the classifier.
- Named-variable parameter operators carry a protected value into `eval` (DPES-1005 A3; DPES-1017 A3). Probe `X=fm-watch-arm.sh; eval "${X%.sh}.sh"` is allowed, exit 0.
- An executing fzf binding supplied through `FZF_DEFAULT_OPTS` (DPES-1005 F19; DPES-1017 F19). Probe `FZF_DEFAULT_OPTS='--bind enter:execute(bin/fm-watch-arm.sh)' fzf </dev/null` is allowed, exit 0. DPES-1005 records that the binding needs an interactive TTY to run.

### Open, not accepted: bypasses under DPES-991, DPES-1001, and DPES-966

These bypasses are actionable and are not accepted.
Each probe below was run through the classifier on this head with `bin/fm-arm-pretool-check.sh --claude --command`; no probe was executed.

- Prefix-assignment expansion order (DPES-991 R1). Probe `X=fm-watch-arm.sh; X=x bin/$X` is allowed, exit 0, and the quoted form `X=fm-watch-arm.sh; X=x "bin/$X"` is allowed, exit 0. The shell expands `$X` before the later assignment takes effect.
- Append assignment (DPES-991 R2). Probe `X=watch-arm; X+=.sh; bin/fm-$X` is allowed, exit 0.
- Keyless fzf execute binding (PR 369 finding 5, recorded under DPES-1001). Probe `fzf --bind 'execute(bin/fm-watch-arm.sh)'` is allowed, exit 0. The keyed form `fzf --bind 'ctrl-x:execute(bin/fm-watch-arm.sh)'` is denied, exit 2; row Z99 pins the keyed deny for `bin/fm-watch.sh`, not for `fm-watch-arm.sh`. Whether fzf accepts a key-less `execute(...)` binding is unverified; fzf was not run.
- Rust xargs long-option parity (PR 369 finding 7, recorded under DPES-1001). The classifier denies `xargs --arg-f names.txt -I{} {}` and `xargs --arg-file=names.txt -I{} {}`, each exit 2 with `unclassifiable-protected-command`. The Rust resolver `resolve_xargs_long_option` in `crates/mcao-core/src/shell_command_pos.rs` resolves exact names and unambiguous prefixes, and `crates/mcao-core/tests/arm_wrapper_differential.rs` carries xargs long-option rows. No Rust run was made for this change, so Rust parity is not verified, and the port is not wired to any gate.
- Wrapper-option table coverage (DPES-966 AC4). Every wrapper-option table entry is not yet exercised in committed tests with an independently runnable reference differential; the PR 333 record counts 76 of 96 mutation cases as untested. This change does not close it.

### Open, owner disposition or another owner required

- Aliases (DPES-1001 finding F). Probe `alias a=bin/fm-watch-arm.sh; a` is allowed, exit 0. No owner disposition is recorded, so DPES-1001 cannot close while this stays open.
- `eval` command substitution (DPES-1001 finding C; DPES-963). Probe `eval "$(printf %s bin/fm-watch-arm.sh)"` is allowed, exit 0. No committed row pins this protected form. Row Z103, `eval "$(printf '%s' 'echo hi')"`, is a benign control that pins allow for a non-watcher payload only. The nearest protected command-substitution row, H33, pins deny for `sh -c "$(printf bin/fm-watch.sh)"`, not for `eval`. A DPES-963 change to this form would not be caught until a row is added.
- Classifier error paths fail open (DPES-966 AC5). The 2026-11-01 owner decision keeps this behavior under DPES-994, and DPES-1001 does not accept it.

### Verification not yet established

- The complete unsharded `tests/fm-arm-pretool-check.test.sh` run is a DPES-1001 acceptance requirement. Partitioned runs are not an equivalent pass, as DPES-1005 and DPES-1017 record. The result for each head is recorded in the PR body.
- `tests/fm-arm-wrapper-differential.test.sh`, `cargo test -p mcao-core --locked shell_command_pos`, and `cargo test -p mcao-core --locked --test arm_wrapper_differential` are DPES-1001 acceptance commands. This change does not run them.
- `tests/fm-watch-arm.test.sh`, the watcher lifecycle suite, has not been run for DPES-1001 (finding E).
- The Rust port does not mirror the Node analyzer's executor, shell-option, watch-payload, and kill-selector rules; see [Rust verdict port](#rust-verdict-port).

## Automated validation

`tests/fm-arm-pretool-check.test.sh` owns the adversarial acceptance matrix.
The DPES-963 rows F01-F12, G01-G06, B01-B04, S01-S05, P01, K01-K05, W01-W02, and H01-H50 cover the variable-split, glob, xargs, substitution, PATH-export, broad-kill, bare-name, builtin-set-name, loader-export, setup-cd, `${...}` expansion, shell-launch, wrapper, negation, and `fuser` shapes.
`test_dynamic_command_position_names_through_path_stub` proves each denied shape against a sandbox stub, with unguarded controls.
`test_review_bypasses_through_path_stub` proves the review's bypass shapes against the same kind of stub; kill-family shapes are denied without ever being run.
Every row runs through Codex-shaped stdin, Claude-shaped stdin, Grok-shaped stdin, OpenCode-shaped CLI, and Pi-shaped CLI entry forms.
The suite also verifies real newline bytes, direct classifier reason codes, comments, heredoc data, malformed and unsupported protected syntax, constructed dynamic payloads, malformed transport fail-open behavior, missing runtime fail-open behavior, output shapes, and exact adapter field forwarding plus exit-2 mapping.

Run:

```sh
bash -n bin/fm-arm-pretool-check.sh
shellcheck bin/fm-arm-pretool-check.sh tests/fm-arm-pretool-check.test.sh
node --check bin/fm-arm-command-policy.mjs
tests/fm-arm-pretool-check.test.sh
bin/fm-test-run.sh --all
```
