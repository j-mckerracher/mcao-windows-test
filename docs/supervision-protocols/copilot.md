Mode: Copilot async-bash supervision.

When this session owns supervision and away mode is not active:

1. Drain first with `bin/fm-wake-drain.sh`.
   After handling all emitted wakes and reconciling open decisions and unread status lines, run the exact `--ack-through` command printed as `WAKE_ACK_REQUIRED`; until then the work remains durable for idempotent re-handling after interruption.
2. First cycle: arm with Copilot's bash tool in async mode, as its own call:

   `bash` with `mode="async"` and `shellId="fm-watch-arm"`, running:
   `exec __FM_COPILOT_ARM__`

3. Trust only the arm's one-line status, read with `read_bash` on `fm-watch-arm`.
4. `watcher: started ...` or `watcher: attached ...` means a live cycle exists.
5. `watcher: FAILED ...` means supervision is down; fix and re-arm.
6. After a successful start or attach status, end the turn.
   The async arm remains the live wait until it returns an actionable wake or failure.
7. Waiting is silent.
8. Never use shell `&` for firstmate supervision.
9. Never bundle the arm onto another command, and never arm with a synchronous bash call.

Copilot delivers a `<system_notification>` when the async `fm-watch-arm` shell completes.
When you see that notification for the arm:

1. Run `bin/fm-wake-drain.sh` first.
2. Optionally read the arm output with `read_bash` on `fm-watch-arm` for the reason line.
3. Handle `signal`, `stale`, `check`, or `heartbeat` using the harness-neutral contract in `AGENTS.md`.
4. Ordinary wake: re-arm exactly one cycle with the same async `fm-watch-arm` call if the home still needs supervision.
5. Do not invent a wake from an attach-status line alone.
   Drain the queue and act only on real wake records, the drain's `OPEN DECISIONS` and `UNREAD STATUS` entries, or a real watcher reason line.

No turn-end guard is verified for a Copilot primary, so no hook re-arms the watcher when a turn ends without a live cycle.
Whenever supervision is needed and no `watcher: started` or `watcher: attached` status is in view for the current cycle, re-arm with the call above.

Only the interactive Copilot TUI is verified as a primary surface.
