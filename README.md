# auto-software-factory (`asf`)

Turn an idea into issues, hand each issue to a Claude Code worker in its own git worktree, verify
the result independently and merge it. One binary, one SQLite file per repository, no server.

`asf` is a local software factory written in Rust. It keeps an issue store in your repository,
plans work with a pluggable methodology, dispatches Claude Code workers through
[Herdr](https://herdr.dev) (a terminal multiplexer for coding agents), checks their work with the
gates you configure and merges it through a local fast-forward queue or GitHub pull requests.
The language of the project you build does not matter: only the gates and the file scopes in the
configuration are project-specific.

## Choose your path

| What you need | Path | Outcome |
| --- | --- | --- |
| Turn a brainstorm into a backlog | [Plan from an idea](#2-plan-the-work) with `asf plan new` | An epic and small, scoped issues with acceptance criteria |
| Use a plan you already wrote | [Import a task list](#import-a-task-list) with `asf plan import` | The same issues, from a superpowers-style or spec-kit `tasks.md` |
| Let agents implement the backlog | [Run the factory](#4-start-the-factory) with `asf serve --dispatch` | Workers in worktrees, verified and merged without you typing `git` |
| Keep GitHub Issues and PRs in the loop | [GitHub integration](#github-integration) | Mirrored issues, pull requests merged after checks pass |

These are independent entry points. You can create issues by hand with `asf issue create` and
never run a planner, or plan with `asf` and merge by hand.

## Get started

You need:

- Rust 1.85 or newer (`cargo`), git 2.40 or newer.
- [Claude Code](https://docs.claude.com/en/docs/claude-code) on `PATH` (`claude --version`).
- [Herdr](https://herdr.dev) 0.9.5 (protocol 22) with its server running (`herdr status`). Workers
  are Herdr workspaces, so you can watch and talk to them.
- Optional: the [GitHub CLI](https://cli.github.com) (`gh`) when you merge through pull requests.

Install from a clone of this repository:

```bash
git clone https://github.com/numikel/auto-software-factory
cd auto-software-factory
cargo install --path crates/asf
asf --version
```

Then, inside the repository you want to build (not this one):

```bash
cd ~/code/my-project
asf repo init --prefix mp      # creates .factory/config.toml and .factory/factory.db
asf repo doctor                # git, config, database, herdr, claude, github
```

`repo init` also appends the runtime paths (`.factory/factory.db*`, `.factory/runs/`,
`.factory/plan/`, `.worktrees/`, `CLAUDE.local.md`, `.claude/settings.local.json`) to `.gitignore`
and `.git/info/exclude`; `repo doctor` warns when any of them is missing. `.factory/config.toml`
and `.factory/issues.jsonl` are meant to be committed.

## The loop, step by step

Every command accepts `--repo <path>` (default: the current directory, any path inside the
repository works) and `--json` for a machine-readable envelope
(`{"ok":true,"result":…}` / `{"ok":false,"error":{"code","message"}}`). Exit codes: 0 ok,
1 error, 2 usage, 3 conflict (for example "already claimed").

### 1. Configure the repository

Open `.factory/config.toml`. The three settings that matter most:

```toml
[scheduler]
max_parallel = 2                          # workers running at the same time
hotspots = ["Cargo.toml", "Cargo.lock"]   # files never edited by two workers at once

[worker]
model = "sonnet"
budget_usd = 5.0                          # per issue; the worker is stopped when exceeded
allowed_tools = ["Read", "Edit", "Write", "Glob", "Grep", "Bash(git *)", "Bash(asf *)", "Bash(cargo test *)"]

[[gate]]
name = "test"
cmd = ["cargo", "test", "--workspace"]
timeout_s = 900
```

Gates are the commands that must pass before anything is merged: formatter, linter, tests, a
build. Put the same commands your CI runs. The full annotated template is in
[Configuration](#configuration).

### 2. Plan the work

Write the idea down as Markdown, then let the planner turn it into a PRD and issues:

```bash
asf plan new --from docs/ideas/search.md --dry-run   # look at .factory/plan/<slug>/ first
asf plan new --from docs/ideas/search.md             # creates the epic and its issues
asf issue list
asf issue show mp-3
```

The planner runs `claude -p` twice (idea → PRD, PRD → issues) with JSON schemas, using the model
and budget from `[planner]` (default `claude-haiku-5-5`, 1 USD per step). Each issue comes with a
title, a body, acceptance criteria, a `file_scope` (the globs the worker may edit) and
dependencies. Review them: a tight `file_scope` is what lets two workers run in parallel without
stepping on each other, and the scheduler refuses to run an issue with an empty scope next to
anything else.

Fix what the planner got wrong:

```bash
asf issue edit mp-3 --scope "src/search/**" --scope "tests/search.rs"
asf issue dep add mp-4 mp-3
asf issue create --title "Add search index" --scope "src/index.rs" --accept "index builds in < 1 s" --dep mp-3
```

#### Import a task list

If you already have a plan, import it instead of generating one:

```bash
asf plan import docs/plans/2026-11-05-search.md --dry-run
asf plan import docs/plans/2026-11-05-search.md
asf plan import specs/001-search/tasks.md --format spec-kit
```

Two formats are understood. `md-tasks` is the shape produced by plan-writing skills such as
superpowers `writing-plans`: `### Task N: Title` headings, a `**Files:**` block, `- [ ]`
checkboxes as acceptance criteria, `[parallel]` to drop the implicit dependency on the previous
task and `**Depends on:** Task 1`. `spec-kit` reads `tasks.md` produced by GitHub spec-kit
(`- [ ] T001 [P] Description in path`). `--format auto` picks the right one.

### 3. Check what would run

```bash
asf issue ready        # backlog issues whose dependencies are all done become ready
asf run plan           # which ready issues start now, and why the rest wait
```

The allocation plan is deterministic: priority first, then the longest dependency chain, then id.
An issue waits when its dependencies are open (`deps_open`), when its scope overlaps a running
worker (`scope_conflict`), when it shares a hotspot, when all slots are taken (`slots_full`) or
when it has no scope at all (`no_scope`).

### 4. Start the factory

Open Herdr, then in any pane:

```bash
asf serve --dispatch --interval 30
```

Every tick the orchestrator:

1. watches active workers (posts a BLOCKER when one waits for input, verifies finished work),
2. processes the merge queue (rebase → gates → fast-forward or pull request),
3. promotes issues whose dependencies are now done,
4. closes epics whose children are all done or cancelled,
5. with `--dispatch`, starts a worker for every slot of the allocation plan.

Without `--dispatch` the loop only watches and merges, and you start workers by hand:

```bash
asf run start mp-3
asf run start mp-3 --approve <inputs_hash>   # refuses if the plan changed since `asf run plan`
```

`asf serve --once` runs a single tick, which is handy in scripts and tests. For an unattended
run use `asf serve --dispatch --until-done [--max-ticks N]`: the loop stops on its own and prints a
summary with the reason `all_done` (exit 0), `max_ticks` (exit 0) or `stuck` (exit 3: open issues
remain but no worker is active, nothing is queued and the plan has no slot, which includes workers
waiting for your answer).

### 5. Watch and answer

```bash
asf tui                 # board, agents, inbox, merge queue, event log (run it as a Herdr pane)
asf run list
asf run show mp-3       # run state, cost, tool calls, live agent state
asf issue inbox         # unread comments addressed to you (`human`) or to all
```

When a worker asks a question (AskUserQuestion in its pane) or hits a permission prompt, its run
becomes `blocked`, a BLOCKER comment with the last screen lines lands in your inbox, and every TUI
view shows a banner `asf-N is waiting for your answer`. A non-blocking question is a QUESTION
comment addressed to `human`; `asf issue finish` refuses until it has an ANSWER. Answer from the
TUI (`a` opens the question with its options: `1`–`4` choose, `t` type, `Esc` close) or from the
shell:

```bash
asf run reply mp-3 "Use the existing Tokenizer; do not add a dependency."
asf run keys mp-3 Enter                     # submit the highlighted option of a dialog
asf issue scope add mp-3 src/other.rs       # grant a SCOPE: request and answer it in one go
asf issue comment mp-3 --type ANSWER --to mp-3 --reply-to 12 --body "Keep the alias."
asf run nudge mp-3      # remind the worker about the finish ritual
asf run abort mp-3      # stop the worker, release the claim, keep the worktree
```

TUI keys: `1`–`7` views (Now, Board, Agents, Inbox, Queue, Log, Config), `Tab`/`Shift+Tab` cycle,
`j`/`k` move, `g`/`G` jump, `Enter` details, `a` answer, `s` start the selected ready issue,
`m` enqueue for merge (in the Inbox: mark one issue read, `M` all), `f` focus the worker's pane,
`v` re-verify a failed run, `x` abort, `t` tick, `?` every binding, `q` quit. The Now view is the
operator's desk: NEEDS YOU lists questions, blocked or stalled runs and failed merges, oldest
first, each with its age and the one key that acts on it; RUNNING shows workers and reviewers
with cost and elapsed time. Tick and worker start run in the background with a spinner. The
Config view lists the editable settings; `Enter` edits one in place, with validation, and writes
it to `.factory/config.toml`. A running `asf serve` reloads the configuration on every tick.

### 6. Verification and merge

A worker finishes by committing, writing `.factory/runs/<id>/report.md` and running
`asf issue finish <id> --report …`. The orchestrator never trusts that report alone. It checks
that the worktree is clean, that there are commits since the base, that every changed file is
inside `file_scope`, that the report has no "open questions" section (questions belong in comments,
see below), and it runs the gates whose `when` includes `verify` in the worktree. The report is
copied to `.factory/runs/<id>/report.md` in the repository root so it survives `lane clean`. Only
then is the run `succeeded`, the issue becomes `validating` and the branch `feat/<id>` is enqueued.
A failed verification sets the issue to `blocked` with a BLOCKER; after a fix in the worktree,
`asf run verify <id>` reruns it.

The merge queue processes items in order: rebase onto the default branch (a conflict is never
resolved automatically: the item and the issue go `blocked` with a BLOCKER naming the files),
scope re-check, gates again after the rebase, then `git merge --ff-only`. The issue becomes
`done`, the worker's Herdr workspace is closed and an FYI comment is posted. With
`[merge] verify_main_after = true` the gates run once more in the repository root after every
merge: a red default branch keeps the item `merged` (the commit is there), records the failing
gate on it, posts a BLOCKER and stops the queue for that tick so nothing else lands on a red branch.

With `[review] required = true` (or a label from `[review] on_labels`) the queue inserts a review
stage after the gates: a second worker with a clean context, running on `[review] model` (default
`opus`), gets a detached checkout of the branch in `.worktrees/<review-id>`, read-only tools and a
brief with the diff and the acceptance criteria. It never edits; it ends with one DECISION comment
on the issue. `approve` lets the merge continue. `changes:` with findings writes them to
`.factory/runs/<id>/review-<n>.md` in the worker's worktree, prompts the worker with one line and
reopens its run; the worker fixes, runs the gates and finishes again, and the same queue item is
re-queued without an operator command. Only when the worker cannot be reached do you get a BLOCKER
with the findings and `asf merge retry <id>`. The review issue and its run are closed as soon as
the verdict is read, so reviewers never hold worker slots; at most `[review] max_parallel`
reviewers run at once. An approved branch whose fast-forward fails only because `main` moved goes
back to `queued`; after the rebase and the gates the review is skipped when the patch fingerprint
(`git patch-id --verbatim`) is unchanged. After `[review] max_rounds` rounds of `changes` the queue
stops and asks you to decide.

```bash
asf merge list
asf merge retry mp-3     # after you fixed a conflict or a failing gate in the worktree
asf merge skip mp-3
asf run verify mp-3      # rerun the independent verification (for example after a flaky test)
asf merge enqueue mp-3 --force   # operator override: queue a branch without a verified run
```

### 7. Clean up

```bash
asf lane list                 # active claims and their worktrees
asf lane clean mp-3 --dry-run
asf lane clean mp-3           # removes .worktrees/mp-3 once merged; refuses dirty or unmerged lanes
```

## How a worker runs

For each issue the orchestrator:

1. claims the issue atomically (a second claim exits with code 3),
2. creates `.worktrees/<id>` on branch `feat/<id>` from the default branch,
3. writes `.factory/runs/<id>/brief.md` (issue, acceptance criteria, scope, hand-offs from
   dependencies, relevant comments, the protocol), a `CLAUDE.local.md` pointing at it, and
   `.factory/runs/<id>/settings.json` with the permission allowlist, deny rules and hooks,
4. opens a Herdr workspace with `ASF_ISSUE` and `ASF_RUN` in the environment and starts
   `claude --permission-mode auto --settings …` in it,
5. sends the kick-off prompt and polls the agent state.

Hooks keep the worker inside its lane: `PreToolUse` denies edits outside `file_scope` or the
worktree, `PostToolUse` counts tool calls, the status line feeds the cost estimate into the
budget check, and `Stop` blocks the session once until `asf issue finish` ran. Inside a worker
every orchestrator command (`run`, `merge`, `plan`, `gh`, `serve`, `export`, `import`,
`issue claim/release/status`) is refused, whatever the allowlist says. Workers never see
secrets: `.env`, `.env.*` and `**/secrets/**` are denied by default.

Workers talk through typed comments on issues:

```bash
asf issue comment mp-3 --type QUESTION --to human --body "Keep the old index format?"
asf issue comment mp-3 --type HANDOFF --to mp-4 --body "Index API: Index::open(path) -> Result<Index>"
asf issue comment mp-3 --type STATUS --body "Parser done, tests green."
```

Types: `QUESTION`, `ANSWER`, `DECISION`, `HANDOFF`, `BLOCKER`, `STATUS`, `FYI`. `--to` is an
issue id, `all` or `human`. Questions never get lost: a question that changes the deliverable is
asked with Claude Code's `AskUserQuestion` tool in the pane (the run shows up as `blocked` with the
question in your inbox; answer with `asf run keys <id> 1` or `asf run reply`), a question that does
not block is a QUESTION comment to `human`, and `asf issue finish` is refused while such a question
has no ANSWER. Decisions a worker takes on its own are posted as DECISION comments, not buried in
the report. `asf issue inbox` shows the unread ones for the current lane and
advances a read cursor (`--peek` does not). Hand-offs addressed to an issue are copied into the
brief of the worker that later picks it up.

## Configuration

`asf repo init` writes this template to `.factory/config.toml`. Unknown sections are ignored.
Change the common settings without opening the file:

```bash
asf config show                      # every editable key with its value and meaning
asf config set review.required true  # validated, comments in the file are kept
asf config set worker.model sonnet
asf config set planner.model opus
```

A sensible split: planning and review on a stronger model (`opus` or `sonnet`), implementation on
`sonnet` or `haiku`.

```toml
[repo]
prefix = "asf"            # issue id prefix: <prefix>-<n>; ^[a-z][a-z0-9]{0,15}$
default_branch = "main"
worktrees_dir = ".worktrees"

[scheduler]
max_parallel = 2          # workers running at the same time (reviewers and issues waiting in validating do not count)
hotspots = []             # globs that are never edited in parallel, e.g. ["Cargo.toml", "Cargo.lock"]

[store]
auto_export = true        # rewrite .factory/issues.jsonl after every mutating command
export_comments = true

[ui]
language = "en"           # en | pl

[methodology]
name = "asf-default"      # `asf method list`; repo-local ones live in .factory/methodologies/<name>/

[worker]
runtime = "herdr"         # herdr | fake
# model = "sonnet"
budget_usd = 5.0          # per issue; from statusLine cost estimate
stall_minutes = 15
max_wall_minutes = 90
max_tool_calls = 400
allowed_tools = ["Read", "Edit", "Glob", "Grep", "Bash(git *)", "Bash(asf *)"]
deny = ["Read(./.env)", "Read(./.env.*)", "Read(**/secrets/**)"]
permission_mode = "auto"  # auto | acceptEdits | dontAsk | bypassPermissions (deny rules always win)
auto_trust = true         # answer Claude's trust dialog for worktrees of this repo
pre_trust = false         # write hasTrustDialogAccepted into ~/.claude.json before launch

# Gates run in order; first failure stops. `when` picks the stages: verify (worker verification),
# merge (after the rebase in the queue), main (repo root after the merge). Default: all three.
# [[gate]]
# name = "fmt"
# cmd = ["cargo", "fmt", "--all", "--", "--check"]
# [[gate]]
# name = "test"
# cmd = ["cargo", "test", "--workspace"]
# timeout_s = 900
# [[gate]]
# name = "ci"
# cmd = ["bash", "scripts/ci-local.sh"]   # e.g. the same job your CI runs
# when = ["merge"]

[merge]
target = "local"          # local (rebase + fast-forward) | github (push + PR via gh)
require_clean_main = true
verify_main_after = false # run the gates in the repo root after each merge; a red main keeps the item `merged`, posts a BLOCKER and halts the tick
# [merge.github]           # used when target = "github"
# remote = "origin"
# base = "main"            # default: [repo] default_branch
# method = "merge"         # merge | rebase | squash (rebase/squash rewrite commits: `asf lane clean` then refuses)
# wait_checks = true       # merge only after every check passed
# delete_branch = false

[review]
required = false          # a read-only reviewer with a clean context before every merge
on_labels = []            # e.g. ["risky"]: labels that force a review worker before merge
model = "opus"            # model the reviewer runs on
max_rounds = 2            # `changes` verdicts allowed before the human decides
max_parallel = 2          # reviewers running at once; the next verified item waits without failing

[planner]
runner = "claude"         # claude (headless `claude -p`, read-only tools) | fake (tests)
model = "claude-haiku-5-5"
budget_usd = 1.0          # per planning step
max_turns = 15
```

Gates on Windows: the gate runner retries a gate whose output contains `LNK1104` (an antivirus
holding a freshly linked test binary) up to five times and moves the locked file aside, so a
scanner does not fail your merges.

## Methodologies

A methodology is a manifest (`manifest.toml`) with stages: `prompt` stages run headless Claude
with a prompt and a JSON schema, `external` stages point at a tool you run yourself and name
the file `asf` imports.

```bash
asf method list                                   # shipped and repo-local; `*` marks the active one
asf method validate .factory/methodologies/mine   # checks stages and referenced files
```

Shipped: `asf-default` (idea → PRD → issues, two headless prompts), `spec-kit` (import
`specs/*/tasks.md` written with GitHub spec-kit), `superpowers` (import a plan written with the
superpowers `writing-plans` skill). A repo-local methodology in
`.factory/methodologies/<name>/` shadows a shipped one with the same name; set it active with
`[methodology] name`. Prompts may use the `{{language}}` placeholder, filled from the
manifest's `language`.

## GitHub integration

Set `[merge] target = "github"` to merge through pull requests. The queue then syncs the
default branch from the remote before rebasing, pushes `feat/<id>` with `--force-with-lease`,
opens a PR, polls checks and mergeability, merges with the configured method (`merge` by
default, so worker commits stay intact and worktrees stay cleanable), and pulls the result.
`asf repo doctor` checks `gh auth status` and the remote URL when this target is set.

```bash
asf gh sync --dry-run
asf gh sync       # one GitHub issue per local issue; closes the ones done/cancelled here
```

The sync is one-way. Each mirrored GitHub issue carries a marker `<!-- asf:<id> uid:<uid> -->`
in its body and the local issue gets the label `gh:#<n>`. Only issues authored by the
authenticated account are trusted as mirrors; others are reported as `foreign`, duplicates are
reported and never fatal.

## Command reference

| Command | Purpose |
| --- | --- |
| `asf repo init [--prefix p]` | Create `.factory/` (config + database); idempotent, adopts a committed config |
| `asf repo status` / `asf repo doctor` | Counts per status and claims / environment and store health (git, config, database, herdr, claude, github, gitignore) |
| `asf config show` / `asf config set <key> <value>` | Read and change the editable settings in `.factory/config.toml` (validated, comments kept) |
| `asf issue list [--status s] [--type t] [--label l] [--ready]` | List issues |
| `asf issue show <id> [--dossier]` | One issue with dependencies; `--dossier` adds comments and hand-offs |
| `asf issue create --title t [--body b] [--scope g]… [--accept a]… [--dep id]… [--parent id] [--priority n] [--type task\|review\|epic] [--ready]` | Create an issue |
| `asf issue edit <id> …` / `asf issue status <id> <status>` | Patch fields / set the status directly |
| `asf issue ready` | Promote backlog issues whose dependencies are all done |
| `asf issue dep add\|rm <id> <on>` | Dependency edges |
| `asf issue claim\|release <id>` | Atomic claim (exit 3 if taken) / release |
| `asf issue finish <id> --report <file>` | Worker ritual: `in-review` plus a STATUS comment |
| `asf issue comment <id> --type T [--to lane] [--reply-to n] --body b` / `asf issue handoff <id> --to lane …` | Typed comments; an ANSWER with `--reply-to` resolves a QUESTION |
| `asf issue scope add\|rm <id> <glob>…` | Grow or shrink the file scope; `add` answers an open `SCOPE:` question |
| `asf issue inbox [--peek] [--all]` | Unread comments for the current lane |
| `asf plan new --from <idea.md> [--slug s] [--dry-run] [--no-epic]` | Idea → PRD → issues with the active methodology |
| `asf plan import <file> [--format auto\|md-tasks\|spec-kit] [--dry-run] [--no-epic]` | Import a task list |
| `asf run plan` | Allocation plan: slots and deferred issues with reasons |
| `asf run start <id> [--approve hash]` | Claim, create the worktree, launch the worker |
| `asf run list` / `asf run show <id>` | Runs with state, cost and tool calls |
| `asf run reply <id> <text>` / `asf run keys <id> <keys>…` / `asf run nudge <id>` | Talk to a worker |
| `asf run abort <id>` | Stop the worker, release the claim, keep the worktree |
| `asf run watch` | One watch pass: BLOCKERs for waiting agents, verification, enqueue |
| `asf run verify <id>` | Rerun the independent verification of the latest run |
| `asf merge enqueue <id> [--force]` / `list` / `tick` / `retry <id>` / `skip <id>` | The merge queue |
| `asf lane list` / `check` / `clean <id> [--dry-run]` | Claims and worktrees |
| `asf method list` / `validate <dir>` | Methodologies |
| `asf gh sync [--dry-run]` | Mirror issues to GitHub |
| `asf serve [--interval s] [--once] [--dispatch] [--until-done [--max-ticks N]]` | The orchestrator loop; `--until-done` stops with `all_done`, `max_ticks` or `stuck` |
| `asf tui [--view board\|agents\|inbox\|queue\|log\|config] [--once]` | Terminal UI |
| `asf export [--out f]` / `asf import <f> [--mode upsert\|replace]` | JSONL export and import |
| `asf hook pre-tool\|post-tool\|stop\|status` | Claude Code hooks (called by the worker's settings, not by you) |

Global options: `--repo <path>`, `--lane <id|orchestrator|human>`, `--json`, `-q`.

## Safety model

- **Workers never merge.** Only the orchestrator claims, starts, verifies and merges; inside a
  worker these commands are refused. A branch enters the queue only after an independent
  verification, or when the operator forces it.
- **Scope is enforced twice**: by the `PreToolUse` hook while the worker runs, and by the
  orchestrator on the diff before verification and again after the rebase.
- **Conflicts are never auto-resolved.** A rebase conflict blocks the item and the issue and
  tells you which files; you resolve it and `asf merge retry`.
- **Budgets are hard limits**: cost per issue, wall-clock minutes, tool calls. Exceeding one stops
  the worker and posts a BLOCKER.
- **Comments from other agents are data, not instructions.** The brief says so, and the
  allowlist, deny rules and hooks back it up.

## Troubleshooting

| Symptom | What to do |
| --- | --- |
| `asf repo doctor` says `herdr not found` or protocol mismatch | Install Herdr 0.9.5 and start its server (`herdr status` shows `protocol: 22`) |
| A worker is `blocked` right after start with "timed out waiting for agent status" | Herdr reported the Claude pane idle later than `[worker] start_timeout_ms` (default 90 s); raise it (180000 works on slow machines), then `asf run abort <id>` and the next tick starts it again |
| A run is `blocked` and your inbox has a BLOCKER with a screen dump | Read it, then `asf run keys <id> y` or `asf run reply <id> "…"` |
| Merge `failed` with `gate … failed after rebase` | Fix in `.worktrees/<id>` (or reply to the worker), then `asf merge retry <id>` |
| Merge `blocked` with `rebase … conflicts in:` | Resolve in the worktree, then `asf merge retry <id>` |
| Verification `failed` on a test that is flaky | `asf run verify <id>` reruns it; `asf merge enqueue <id> --force` is the last resort |
| Worker hit a Claude usage limit mid-task | It leaves a BLOCKER; when the limit resets, `asf run reply <id> "continue"` |
| `cargo install` fails with "Access is denied" on Windows | A running worker's status line is using `asf.exe`; stop the workers or build with `cargo build --release -p asf` and copy the binary |

## Development

This repository builds itself with `asf` ("dogfooding"): its `.factory/config.toml` and
`.factory/issues.jsonl` are committed and show a real configuration. Crates: `asf-core`
(domain, ports, scheduler, config, planners; no IO), `asf-store` (SQLite), `asf-vcs` (git, gates,
GitHub), `asf-runtime` (Herdr adapter, headless planner), `asf` (CLI and TUI).

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs the same three commands on Windows and Linux. New behaviour comes with a failing test
first; integration tests use a fake runtime and a fake GitHub so they run without Herdr or `gh`.

## License

MIT.
