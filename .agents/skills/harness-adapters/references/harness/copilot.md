# GitHub Copilot CLI

Copilot is verified for crewmate, scout, and primary work, never a secondmate.
`../../../bin/fm-spawn.sh` refuses `--secondmate` on it.
Its primary supervision protocol is [`copilot.md`](../../../../../docs/supervision-protocols/copilot.md), which arms the watcher through async bash; no turn-end guard is wired for a Copilot primary.

## Launch

`launch_template()` in `../../../bin/fm-spawn.sh` owns the exact command: `copilot --no-auto-update --yolo --agent hve:workflow-agent`, the model and effort flags, and the launch brief as `-i`'s interactive prompt.
The executable is resolved from `PATH` and a missing install refuses the spawn.
`--yolo` is Copilot's allow-all posture for a worker.

## Workflow-agent

The agent comes from the `hve@mayo-skills` plugin installed under `${COPILOT_HOME:-~/.copilot}`; Copilot resolves only the namespaced `hve:workflow-agent`, never the bare name.
A missing definition only prints an error in the pane, so `../../../bin/fm-spawn.sh` refuses the spawn first; `references/common/dispatch.md` owns the contract.
`COPILOT_HOME` is forwarded onto the launch so a long-lived multiplexer daemon reads the same store the check did.

## Identity

Copilot sets `COPILOT_CLI=1` in its process environment, which `bin/fm-harness.sh` reads as a marker ordered before the inherited `CLAUDECODE` line, so a copilot launched from a Claude pane is not renamed.
Copilot 1.0.98 sets it itself: its shell tool's environment shows `COPILOT_CLI=1` while the launching shell has none.
`ps -E` is not evidence either way, because it did not show even the launching shell's own `CLAUDECODE`.
The marker is evidence only: a nearer copilot process in the ancestry still decides the verdict.
Its live process name is exactly `copilot`, and `bin/fm-session-lock-lib.sh` and `bin/fm-harness.sh` match that exact name rather than a substring, so `copilot-language-server` and its node children are not mistaken for the harness.

## Busy state and control

No busy-state source is wired for Copilot, so its busy verdict is unknown rather than idle.
Interrupt is Escape and exit is `/exit`, as `../../../bin/fm-control-lib.sh` records.
On 1.0.98, `--yolo` does not skip the folder-trust gate: a folder Copilot has not trusted opens `Confirm folder trust` with `1. Yes` preselected, observed on 2026-10-18 in a scratch folder under `/private/tmp`.
`../../../bin/fm-spawn.sh` does not pre-register that folder, so a spawned worker can park on the dialog.
Answering it writes Copilot's trust store, which is the operator's decision, so firstmate reports a parked dialog rather than sending keys to it.
Confirm the worker is processing its brief after spawn before relying on it.
