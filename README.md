# Motif HVE Multi-Agent Orchestrator (`mcao`)

> Clone it, run `mcao setup`, and hand your work to a supervised crew of coding agents that ships verified pull requests.

`mcao` is a ready-to-use multi-agent engineering setup for Mayo Clinic teams.
You talk to one agent.
It runs several coding agents in parallel, each in its own isolated git worktree, and every change they produce passes through an AI review, test, and lint pipeline before it reaches a pull request.
The pull request carries the proof.

It combines two open-source projects and keeps both of their defaults on:

- [firstmate](https://github.com/kunchenguid/firstmate) supplies the orchestration: one supervisor agent, a visible crew, isolated worktrees, explicit merge authority, and the [no-mistakes](https://github.com/kunchenguid/no-mistakes) verification pipeline that stands between a worker's confidence and your main branch.
- [pstack](https://github.com/cursor/plugins/tree/main/pstack) supplies the engineering discipline: every worker starts in a workflow router that picks a rigorous playbook for the task and applies pstack's principles, such as the smallest change that solves the problem and verification before any claim of done.
  `mcao` installs it as the [`hve` plugin](https://github.com/mc-innersource/mayo-skills/tree/main/skills/engineering/hve), Mayo's adaptation of pstack in the Mayo skills marketplace.

`mcao` exists for developers who want this way of working without building and maintaining their own agent setup.
It supports Mayo Clinic's Hypervelocity Engineering (HVE) practice, MOTIF.

## Status

| | |
|---|---|
| **Maturity** | Experimental |
| **Owned by** | `@mc-innersource/mc-innersource-admin` |
| **Contributions** | Open, see [Contributing](#contributing) |

## Why speed and safety go together here

Hypervelocity comes from parallelism: several agents working at once on independent tasks while you make only the decisions that need a human.
That is only safe when verification is the default path rather than something a busy developer remembers to run.
`mcao` makes it the default at three layers:

1. **Inside the worker.**
   Every worker launches as `hve:workflow-agent`, the workflow agent from the `hve` plugin.
   It reads the task, picks the playbook that fits (feature, bug fix, refactor, investigation, review), and follows that playbook's own verification steps.
   A worker does not get to improvise its process.
2. **Before the pull request.**
   The default delivery mode runs the finished branch through no-mistakes: an AI review, fixes for its findings, the test suite, lint, and documentation checks, all in an isolated worktree.
   Only a passing branch is pushed, and the pull request it opens carries a signed attestation of what ran.
   This repository's own CI refuses a pull request without that attestation, so the proof is checked, not assumed.
3. **Around the whole crew.**
   The supervising agent never edits a project itself, never merges without your explicit word, and never discards work that has not landed.
   Workers report to it, not to you, so you see outcomes and decisions instead of a wall of agent chatter.

The result is that going fast does not mean skipping review.
The review runs every time, on every branch, by construction.

## What you get

- **One point of contact.**
  You chat with the orchestrator; it dispatches, supervises, escalates only real decisions, and reports plain outcomes with pull request links.
- **Parallel workers in isolation.**
  Each task runs in its own disposable git worktree, so several agents can work on one repository without colliding.
- **A visible crew.**
  Every worker runs in a tmux window or Herdr tab you can watch or type into; the orchestrator reconciles whatever you do there.
- **Two task shapes.**
  A *ship* task delivers a code change through the project's delivery mode.
  A *scout* task delivers a written investigation, diagnosis, or plan and never opens a pull request.
- **Explicit delivery rigor per project.**
  `no-mistakes` is the default; `direct-PR` and `local-only` exist for work where you have a stated reason to lower rigor, and merge autonomy is a separate opt-in.
- **Evidence-backed pull requests.**
  Review findings, test runs, and lint results are published with the pull request, not described from memory.
- **Restart-proof.**
  Work in flight, open decisions, and preferences live on disk; closing a session loses nothing.
- **Room to grow.**
  Optional persistent second mates run additional crews from their own isolated homes, locally or on another host, and an away mode keeps supervision running while you step out.

Full detail on every feature lives in [docs/architecture.md](docs/architecture.md).

## Getting started

### Prerequisites

- macOS or Linux.
- Git and the GitHub CLI, already signed in.
- An orchestrator harness: [Claude Code](https://docs.anthropic.com/en/docs/claude-code), [Codex CLI](https://github.com/openai/codex), or Oh My Pi (`omp`).
- A worker harness: Claude Code, Codex CLI, Pi, or GitHub Copilot CLI.
  The orchestrator and worker harnesses may be the same tool.
- `tmux`, the default session backend, or [Herdr](docs/herdr-backend.md).
- `jq`, used by `mcao setup`.
- Node.js and [no-mistakes](https://github.com/kunchenguid/no-mistakes).
  The orchestrator detects missing tools at session start and offers to install them after you approve.
- Read access to [`mc-innersource/mayo-skills`](https://github.com/mc-innersource/mayo-skills), the private repository that hosts the `hve` plugin.

### Install

```bash
git clone https://github.com/mc-innersource/motif-multi-agent-orchestrator.git
cd motif-multi-agent-orchestrator
./install.sh   # links mcao into ~/.local/bin; add that directory to your PATH if the installer says it is missing
mcao setup
```

Using Oh My Pi (`omp`)?
Add the `hve` plugin from the Mayo skills marketplace:

```bash
omp plugin marketplace add mc-innersource/mayo-skills
omp plugin install hve@mayo-skills
```

`mcao setup` asks the first-run questions in your terminal and writes each answer as a plain file under `config/`.
It asks for your name and branch prefix, your orchestrator and worker harnesses, the worker model and effort, the session backend, the Claude permission mode for workers, and optional per-task-type dispatch rules.
Press Enter to keep a shown default.
After the questions, setup installs the `hve` plugin into every installed Claude Code, Copilot CLI, and Codex, then reports whether each one is ready.
Re-run it any time, re-ask one part with `mcao setup <section>`, or reinstall only the plugin with `mcao setup plugins`.
Pi and omp cannot install the plugin, so setup copies the agent's prompt and the plugin's skills from the Claude Code or Codex install into a `mcao-workflow-agent` folder under their own config directory.
Install the plugin for Claude Code or Codex first, because that install is what setup copies from.

| Setting | Default |
|---|---|
| Worker harness | `claude` |
| Worker model and effort | `claude-haiku-5-5`, `medium` |
| Session backend | `tmux` when installed, otherwise `herdr` |
| Worker permission mode (Claude) | `bypass`, inside the worker's own worktree |
| Branch prefix | `mcao/` |

Every setting, including the files `mcao setup` writes, is documented in [docs/configuration.md](docs/configuration.md).

### Launch

Start your orchestrator harness from the repository root.
The tracked `AGENTS.md` takes over and the agent becomes your orchestrator.

```bash
claude      # or: codex, omp
```

### Talk to it

```text
> look at the rls-orders-cnsmr-api project, fix the flaky order-status test, and add request logging

# The orchestrator clones the project under projects/, files two tasks, and starts two
# isolated workers. Each runs the pstack playbook for its task and then the no-mistakes
# pipeline. Minutes later:

  PR ready for review, captain: https://github.com/<org>/rls-orders-cnsmr-api/pull/42
  (fix flaky order-status test - risk: low - review, tests, and lint passed)

> merge it
```

You approve merges.
Nothing lands on `main` on the orchestrator's own initiative unless you have granted a project standing merge autonomy, and even then only green, in-scope work merges.

## How a task flows

```text
            you
             │  requests, decisions, "merge it"
             ▼
 ┌───────────────────────────────────────┐
 │ orchestrator (this repo, AGENTS.md)   │  reads projects/, writes task records
 └──┬───────────────┬──────────────────┬─┘
    ▼               ▼                  ▼
 worker 1        worker 2    ...    worker N      one tmux window or Herdr tab each
 workflow-agent  workflow-agent     workflow-agent
 own worktree    own worktree       own worktree
    │               │                  │
    ├─ ship:  pstack playbook ► no-mistakes review/test/lint ► PR with attestation ► your merge call
    │
    └─ scout: pstack playbook ► written report ► findings relayed to you
```

1. **Intake.**
   The orchestrator resolves which project the request is about, decides whether it is a ship or a scout, and fixes the delivery mode and merge posture before any worker starts.
2. **Dispatch.**
   A worker launches as `hve:workflow-agent` in a fresh worktree with a written brief.
   Dispatch refuses to start a worker whose tool does not have the agent installed, and names `mcao setup plugins` as the fix.
3. **Work.**
   The worker follows the playbook the router chose and reports only events that need a supervisor: a question, a blocker, or completion.
4. **Verification.**
   A ship task pushes through no-mistakes, which reviews, fixes, tests, lints, and documents in isolation, then opens the pull request with its attestation.
5. **Decision.**
   You get the pull request link and a one-line risk summary.
   You say merge, or you don't.
6. **Cleanup.**
   Landed work is torn down; unlanded work is never discarded.

### Delivery modes

| Mode | What happens | Use it when |
|---|---|---|
| `no-mistakes` (default) | Full review, test, lint, and documentation pipeline, then a pull request with attestation. | Anything product-facing, mixed, or uncertain. |
| `direct-PR` | The worker pushes and opens a pull request without the pipeline. | Internal tooling or process changes where you can state why the pipeline is unnecessary. |
| `local-only` | The worker stops on a clean, ready branch; nothing is pushed until you approve a local merge. | Work that must not leave the machine. |

Merge autonomy (`yolo`) is orthogonal to delivery mode and off by default.
With it on for a project, the orchestrator merges green, in-scope pull requests itself and tells you the result.
Red pull requests, destructive changes, and security-sensitive merges still come to you.

## Everyday commands

These run inside the orchestrator session.
Claude Code uses the slash form shown; Codex uses the same names with `$`.
Oh My Pi has no separate verified command form, so ask for the command by name in plain language.

| Command | What it does |
|---|---|
| `/bearings` | A short digest of everything in flight: tasks, open decisions, pull requests awaiting you. |
| `/ahoy` | Recap what happened since your last message and walk through any unanswered decisions. |
| `/afk` | Hand supervision to the away mode while you step out; only real decisions wait for your return. |
| `/quiet` | Keep routine notifications off your screen while you stay and chat. |
| `/setup` | Show the current `mcao setup` answers. |
| `/stow` | Capture what the session learned into durable records before a reset. |
| `/updatefirstmate` | Pull the latest orchestrator instructions and tooling from `main`, guardedly. |

## Hypervelocity Engineering at Mayo

Microsoft's [HVE Core](https://github.com/microsoft/hve-core) describes an agentic software development lifecycle: agents for research, planning, implementation, and review, instructions that apply standards automatically, and skills that package reusable workflows.
Its [HVE Guide](https://microsoft.github.io/hve-core/docs/hve-guide/) maps those tools onto each stage of a project.

`mcao` is the execution layer for that way of working.
It does not replace lifecycle guidance; it is the place where the implementation, review, and delivery stages actually run, with parallel agents, isolation, and verification built in.
Microsoft's framing of the approach is in [Hypervelocity Engineering: Accelerating Enterprise AI with Azure AI Landing Zones](https://techcommunity.microsoft.com/blog/azurearchitectureblog/hypervelocity-engineering-accelerating-enterprise-ai-with-azure-ai-landing-zones/4536192), and the Visual Studio Code extension is [HVE Core Essentials](https://marketplace.visualstudio.com/items?itemName=ise-hve-essentials.hve-core-all).

## Configuration and operations

- [docs/configuration.md](docs/configuration.md) owns every setting: `mcao setup`, environment variables, `FM_HOME`, harness and backend selection, dispatch profiles, and delivery-mode registration.
- Session backends: [tmux](docs/tmux-backend.md) (default), [Herdr](docs/herdr-backend.md).
- [docs/wedge-alarm.md](docs/wedge-alarm.md) configures the alert for an away-mode escalation that gets stuck.
- [docs/architecture.md](docs/architecture.md) is the maintainer view of supervision, worktrees, project modes, and self-update.
- [docs/RELEASE.md](docs/RELEASE.md) is the maintainer release procedure: the `UPSTREAM` record, semver tags, and the ai-local-setup notice.
- [docs/scripts.md](docs/scripts.md) is the `bin/` reference.
- [docs/documentation-audiences.md](docs/documentation-audiences.md) lists which document is written for whom.
- [docs/FIRSTMATE_README.md](docs/FIRSTMATE_README.md) is the upstream firstmate README, kept for the full feature catalogue and harness notes.
- [VISION.md](VISION.md) states the design principles the orchestrator is held to.

## Project structure

```text
.
├── AGENTS.md            # the orchestrator's operating contract; CLAUDE.md points at it
├── bin/                 # mcao, mcao-setup.sh, and the fm-* toolbelt (plain bash)
├── .agents/skills/      # skills the orchestrator loads at defined trigger points
├── skills/              # standalone public skills with no orchestrator dependency
├── docs/                # operator and maintainer documentation
├── tests/               # behavior tests for bin/ (plain bash)
├── .github/workflows/   # lint, policy, CI, and the "Require no-mistakes" check
└── config/ data/ state/ projects/ .env .no-mistakes/   # yours, gitignored, never committed
```

Only the shared material is tracked.
Everything personal to your crew, including project clones, task records, and setup answers, stays in the gitignored directories.

## Development

```bash
bin/fm-lint.sh                      # shellcheck + workflow lint, pinned versions
bin/fm-test-run.sh --changed        # behavior tests affected by your change
bin/fm-test-run.sh --all            # full regression suite
bin/fm-doc-audience-check.sh        # documentation placement and link check
```

Changes to this repository ship the same way project changes do.
Human-authored pull requests to `main` must be raised through no-mistakes (`git push no-mistakes`), and the `Require no-mistakes` check enforces the attestation.
[CONTRIBUTING.md](CONTRIBUTING.md) has the full workflow, repository conventions, and required-check maintenance.

Pull requests and pushes to `main` also run the organization's language-aware lint workflow and central policy controls from `.github/workflows/`.
The default-branch policy run maintains one **Repository policy findings** issue; pull request runs use annotations only.

## Contributing

This is an innersource project.
Contributions from anywhere in the organization are welcome, and you do not need to be on the owning team.

1. Open an issue first for anything non-trivial so the approach can be agreed.
2. Branch from `main`.
3. Keep changes focused and update tests and documentation alongside them.
4. Raise the pull request through no-mistakes and describe *why*, not just *what*.
   CI must pass, and [CODEOWNER](.github/CODEOWNERS) review is required before merge.

Good first contributions are labelled `good first issue`.

## Support and ownership

| | |
|---|---|
| **Maintainers** | `@mc-innersource/mc-innersource-admin` |
| **Questions and bugs** | [Open an issue](https://github.com/mc-innersource/motif-multi-agent-orchestrator/issues) |
| **Chat** | none, open an issue |
| **Response expectations** | best effort within two business days |

## Security

Do not report security issues in public issues or pull requests.
Contact the maintainers listed above directly.

## Credits and license

Built on [firstmate](https://github.com/kunchenguid/firstmate) and [no-mistakes](https://github.com/kunchenguid/no-mistakes) by kunchenguid, and on [pstack](https://github.com/cursor/plugins/tree/main/pstack) by Lauren Tan (poteto), packaged for Mayo as the [`hve` plugin](https://github.com/mc-innersource/mayo-skills/tree/main/skills/engineering/hve).
MIT, see [LICENSE](LICENSE).

---

*Generated from [`mc-innersource/mayo-innersource-template`](https://github.com/mc-innersource/mayo-innersource-template).*
*`.copier-answers.yml` records the template version; run `copier update` to pull in later template improvements.*
*Do not delete that file.*
