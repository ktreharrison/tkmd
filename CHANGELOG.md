# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **`tk eval --dry-run <ids>`** checks an eval without running it: shell syntax of the command and of any script it runs (`sh check.sh`), that programs, scripts and absolute `cd` targets exist (inside the named script too), and which files the first claim will pin. It writes no card, receipt or event and exits `1` when it finds a problem. The first claim pins the check, so this is the step before `tk c`.
- **`tk set <id> -d "<text>"`** replaces a card's details (the text between `# Title` and the first `## ` section), matching `tk add -d`. `-d ""` clears it. Like a DoD change, it sends a `pending-close` task back to `in-progress`.
- **`tk note <id> <text>` and MCP `task_note`** append one line to a card's execution log and log a `note` event. They never touch the spec, DoD, eval, owner or status, refuse done and archived cards, and cap notes at 2,000 characters. `tk d` lists notes added since the claim, so an agent can explain why its work differs from the spec without editing the spec. `tk h` shows `✎ note` events.
- **`[policy] assign_on_claim`** (default `false`): new tasks without `@owner` stay unassigned until the first `tk c` assigns them, and `tk ls` shows `—` for them. With the default, new tasks still belong to the configured owner.

### Changed
- `TaskStore::update_task` takes a final `new_details: Option<String>` argument.

## [0.2.3] - 2026-11-01

### Added
- `tk archive`, `tk prune`, `tk eval`, `tk c` and `tk r` accept several IDs (`tk archive 2 3`, or `tk archive 2,3`), matching `tk d`. Before this, `tk archive 2 3` failed with `unexpected argument '3'`.
- `TaskStore::archive_many`, `prune_many` and `sweep_orphan_locks`. `archive_tasks` and `prune_tasks` stay as single-ID wrappers.

### Fixed
- **`tk prune` reaches `tasks/archive/`**: `prune_many` now inspects `tasks/archive/` as well as active cards, so archived cards can be deleted from disk without touching `.events.jsonl`.
- **No more leftover `.tk-*.lock` files**: `CardLock` deletes its lock file on drop while it still holds `LOCK_EX`. Before, every card that was ever locked left an empty `.lock` file in `tasks/`.
- `acquire_exclusive` checks that the locked file is still the one at the lock path (device and inode). If another process deleted it in between, it retries, so two processes never hold locks on different files for the same card.
- `tk archive` and `tk prune` delete the card's lock file. They also sweep orphan `.tk-*.lock` files in both `tasks/` and `tasks/archive/` whose card is gone, which cleans up stores used by older versions.

### Changed
- `tk eval` with several IDs runs every eval and exits `1` if any of them fail.

## [0.2.2] - 2026-10-31

### Added
- `-y` / `--yes` on `tk d` and `tk auto` skips the `[y/N]` confirmation prompt at a terminal.
- `[policy].allow_non_tty_yes` (default `false`): lets non-terminal human runners (Raycast, Obsidian shell plugins) close or approve tasks with `-y` when no agent session environment variable is set.
- `Dockerfile` and `glama.json` for Glama MCP (#5, #6): runs `tkmd mcp --local` with `HOME=/tmp` so cards stay in `/workspace/tasks`.

### Fixed
- **Eval pins work without `git` (#4)**: `eval_inputs` hashes files in-process instead of calling `git hash-object`.
- **Eval pins cover sourced scripts (#3)**: `eval_inputs` scans pinned scripts for sourced or referenced workspace files (up to 32 files) and pins them too.

### Changed
- Split `src/store.rs` into `src/store/{mod,lock,config,eval,receipt,doctor,tests}.rs`.

## [0.2.1] - 2026-10-21

### Security
- **The check is pinned at claim** (suggested by prpatel05 on dev.to). The first claim records the eval command and a git blob hash of each repo file it names; re-claiming keeps the pin. If an agent changes the eval or its script before submitting, the receipt records what changed and `tk d` refuses until the human passes `--accept-changed-eval`. Previously this only warned.
- `tk auto` pins the eval's files too: a script changed after approval no longer auto-closes; the task waits for manual sign-off.
- A file the eval names that changes after submit now blocks approval even outside git, where there is no worktree snapshot.
- The pin includes each file's executable bit: making `./check.sh` executable after claim is a change (the blob hash alone covers contents only).
- Closing your own task directly after accepting a changed check records `interactive (accepted changed check)`, and the sign-off summary shows the change, as it already did for reviewed tasks.

### Changed
- `tk d` gains `--accept-changed-eval`; the sign-off summary shows exactly what changed (`check.sh changed`, ``eval `a` -> `b` ``).

## [0.2.0] - 2026-10-21

### Security
- **Card locks now hold**: `CardLock` kept only the raw fd and dropped the `File`, which closed the descriptor and released the `flock` immediately. The lock now lives until dropped. `eval`, submit, done, the unblock cascade and `tk dep` hold the card lock for the whole read, eval and write.
- **Sign-off no longer trusts the `--actor` name**: `tk done` defaulted `--actor` to `ken` and the engine compared that string with `default_owner`, so any caller could close a task by naming the owner. Approval now depends on how `tk done` runs: MCP and non-interactive callers can only submit for review, and interactive `tk done` shows the receipt and asks for confirmation.
- **Claim guard**: `tk c` refuses `pending-close`, `done` and `archived` tasks and tasks another owner has in progress, unless `--force` (logged as `claimed-forced`).

### Added
- **Review receipt**: `pending-close` records a git tree snapshot of the work (task cards excluded), the eval command and result, submitter and time. Approval fails if the output or eval changed since.
- **`[policy]` config**: `require_eval`.
- **Bulk sign-off**: `tk done 1 2 3` or `tk done --pending` shows one summary and asks once. Tasks that fail their checks are skipped and reported; the rest close.
- **Auto-accept (`tk auto <ids>`)**: approve a task's eval in advance; when the owner submits and that exact eval passes, the task closes as `approval: auto:eval` and unblocks dependents. Changing the eval revokes it, `tk auto <ids> --off` turns it off, and agents cannot grant it. You approve the check once instead of every outcome.
- **Eval timeout**: `[policy].eval_timeout_secs` (default 600, `0` = no limit). A timed-out eval fails, and every process it started is killed, so it cannot hold the card lock.
- **`tk doctor`**: checks every card against the event log and flags cards hand-edited to `done` or given an approval `tk auto` never granted (exit 1 on issues).
- **Trust model** in `docs/design.md`: what tkmd prevents, what it only detects, and how to isolate the store from untrusted agents.
- **MCP `task_get`**: fetch one full card (body, log, sessions, receipt).
- **Test suites**: `tests/concurrency.rs` races real `tk` processes (adds, claims, updates, submits, mixed workloads); `tests/scale.rs` (opt-in) times commands on 3,000 cards.
- Changing `eval` on a `pending-close` task moves it back to `in-progress`; the card records the eval at claim time so the sign-off prompt can show a changed check.

### Fixed
- From the Sourcery review of PR #1 (each covered by a test that failed first):
  - Archiving a done blocker made its dependents blocked again (a regression from the missing-blocker fix): blocker lookups now include the archive.
  - `tk archive 1` and `tk prune 1` matched nothing: the id filter now accepts `1`, `tk-1` and `tk-001`.
  - `tk d` approved whatever receipt the card held when it wrote, even one an agent re-submitted while the prompt was open. Approval is now bound to the receipt shown, and every decision is made on the card read under its lock.
  - A blocker added to a `pending-close` task did not stop its approval.
  - Files changed by the eval itself, or while it ran, were receipted: submit now snapshots before and after the eval.
  - A Definition of Done change after submit left the review valid; `eval: ""` passed `require_eval`.
  - `claim` could pass the blocker check while `tk dep` added a blocker: claims now take the graph lock.
  - A failing daily-note write returned before the unblock cascade, and cascade write errors were ignored. Both are now warnings that do not skip later work.
  - A global task closed through the local store did not unblock its global dependents: transitions now run in the store that owns the card.
  - Duplicate ids in `tk d 1 tk-001`, MCP `limit: 0` paging forever, and an unrecorded forced take-back from review.
  - MCP requests that are not objects, or have no `method`, get `-32600`.
  - When local and global stores configure different owners, only the first counted as human.
  - `tk doctor` also checks archived cards and flags `blocked` tasks whose blockers are all closed.
- From an independent Codex review (each reproduced first, then covered by a failing test):
  - Concurrent `tk dep` commands each validated against the same old graph and together committed a cycle; dependency edits now run under a store-wide lock.
  - A background child (`sleep 4 &`) kept the eval's output open past `eval_timeout_secs` and the eval then passed; the deadline now covers the whole process group and output collection, and captured output is capped at 1 MiB per stream.
  - A blocker whose card was deleted counted as done, so `tk c` failed open; missing blockers now block, with a message saying how to repair.
  - `tk r` printed "pending-close" when the task had auto-accepted into `done`.
  - A receipt made without git looked like any other; it now records why there is no snapshot, and warns when git is unavailable.
  - MCP ignored malformed JSON and answered unknown methods with an empty success; they now get `-32700` and `-32601` errors.
  - The CPM summary said "Ken Sign-Off"; it now says "Human sign-off".
- Single-task commands parsed every card in the store (about 0.9 s each at 20,000 cards, 2.9 s for `tk dep`). Id lookups now use the `tk-NNN-slug.md` file name and parse one card; `next_id`, blocker checks and the cycle check follow. Measured at 20,000 cards, release build, median of 5: `show` 876 to 15 ms, `claim` 930 to 14 ms, `dep` 2,880 to 25 ms. `ls` and `doctor` still read every card (about 0.9 s).
- Deleting or archiving the newest card let `tk add` reuse its id, so old references pointed at a new task; ids now also count archived cards and the event log.
- Racing `tk add` processes reused task ids; id allocation now runs under a store lock.
- An unparsable config was silently replaced by defaults (dropping `[policy]`); it is now an error naming the file.
- Task lookup matched parent directory names, took the first of several matches, and resolved `100` to `tk-1000`. Ids now match exactly; a name fragment must match one card, otherwise the error lists the candidates.
- Dependency cycles were accepted; `tk dep` now rejects them and shows the cycle.
- Blocked tasks could be submitted for review, and any agent could submit another agent's claimed work.
- The unblock cascade re-read every card for each dependent; it now takes one status snapshot.
- Unreadable cards vanished silently from listings; they are now named on stderr.

### Changed
- MCP `task_list` returns `{tasks, total, next_offset}`: compact summaries, 100 per page by default (82% smaller per card on 50 realistic cards: 8.4 KB vs 45.7 KB). Pass `detail: "full"` for complete cards, and `limit`/`offset` to page.
- Colour is off when stdout is not a terminal (agents get plain text); `NO_COLOR` still wins.
- Card lock files are hidden (`.tk-001-x.lock`). Do not mix this version with older `tk` binaries on one store: they lock different files.
- `tk r` records you as the submitter when run at a terminal, `agent` otherwise.
- `tk c`, `tk eval` and `tk d` default their owner/actor to `default_owner` instead of the literal `ken`; `$HOME`/`$USER` fallbacks no longer assume `/Users/ken`.
- The configured `default_owner` is treated as human everywhere: no invented `<owner> -c` resume command, human colour in listings.
- `tk r` reports "no eval ran" instead of "eval passed" for tasks without an eval; `tk add` no longer pads the owner name.
- An agent can no longer close a task as the human's proxy.
- Tasks in `pending-close` from before this release have no receipt: re-submit them with `tk r <id>` before approving.

## [0.1.2] - 2026-10-20

### Added
- **ANSI Colorized Help Output**: Vibrant CLI styling for `-h` and `--help` with bold green headers, bold cyan subcommands/flags, and cyan placeholders. Respects `NO_COLOR` spec.

### Fixed
- **Accurate Review Diversion Message**: `tk done` reports "Submitted for review" instead of "Completed" when an AI agent is intercepted by review guardrails to `pending-close`.

## [0.1.1] - 2026-10-19

### Fixed
- Updated visible `taskmd` strings to `TKMD` across telemetry outputs and Gantt chart headers.

## [0.1.0] - 2026-10-18

### Added
- **Markdown-Native Card Store**: Atomic task cards (`tasks/tk-XXX-*.md`) with YAML frontmatter and Markdown body.
- **Obsidian 1.14 Kanban Base**: Auto-generated `bases/tasks.base` with 5 views (Kanban, Pending Close, Personal Queue, Agent Queue, Completed Tasks) and safe `.obsidian/types.json` merging.
- **Append-Only Event Stream**: `tasks/.events.jsonl` logging every state transition, eval check, and agent session handoff.
- **Flow Telemetry (`tk st`)**:
  - Little's Law engine ($L = \lambda W$) calculating active WIP, cycle times, and implied system throughput.
  - Mean Time to Complete (MTTC / Lead Time) with Mean, P50, P90, and TPM90 (Trimmed Percentile Mean 90).
  - Queue wait time, execution duration, and sign-off wait duration.
  - Review yield and agent execution splits.
- **Dependency Management (`tk dep`)**:
  - Declaring dependencies via `tk dep <id> --needs <ids>` or inline `needs:`, `after:`, `dep:`.
  - Claim guardrail preventing work on blocked tasks until prerequisites finish.
  - Cascade unblocking automatically promoting downstream tasks to `ready` with macOS notifications.
  - ASCII DAG dependency tree rendering (`tk dep tree`).
- **Lifecycle Cleanup (`tk archive` & `tk prune`)**:
  - `tk archive` moves closed task cards to `tasks/archive/`.
  - `tk prune [-f]` purges finished cards from disk while preserving 100% of telemetry in `.events.jsonl`.
- **Property Editing (`tk set`)**:
  - CLI command `tk set` (alias `tk update`) to update title, priority, owner, due date, DoD, and eval shell command without opening an editor.
- **Automated Verification Gates (`tk eval`)**:
  - Configurable shell commands executed in the project workspace, logging exit status and output.
- **Agent Governance & Guardrails**:
  - AI agents calling `tk done` or MCP `complete_task` are automatically routed to `pending-close` for human sign-off.
- **Multi-Agent Session Resumption (`tk u`)**:
  - Session tracking across `agy`, `claude`, `codex`, `hermes`, `opencode`, `pi`, and `gemini`.
  - Auto-generated `## Agent Sessions` markdown table with copy-pasteable resume and continue commands.
- **Dual-Framing stdio MCP Server (`tk mcp`)**:
  - Auto-detects newline-delimited JSON and `Content-Length` header framing for universal AI assistant compatibility.
- **Cross-Platform Releases & Zero-Rust Installer**:
  - Automated GitHub Actions build matrix for macOS (arm64, x86_64), Linux (x86_64), and Windows (x86_64).
  - Single-command shell installer: `curl -fsSL .../install.sh | sh`.
