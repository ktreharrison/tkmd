---
name: tkmd
description: Manage Human + AI Agent tasks using the `tk` / `tkmd` CLI or `tkmd` MCP server (`tasks/*.md`, `bases/tasks.base`, and `tasks/.events.jsonl`). Use whenever listing, creating, claiming, attaching agent session IDs, evaluating, submitting for review (`pending-close`), or completing tasks.
---

# `tkmd` (`tk`) — Human + Agent Task Orchestrator

All active tasks live as atomic Markdown cards in `tasks/tk-XXX-*.md` (and render on the native Obsidian 1.14 Kanban board at `bases/tasks.base` when Obsidian mode is enabled).

## Mandatory Agent Protocol (Harness Engineering)

1. **Discover work**:
   ```bash
   tk ls
   tk ls --owner <agent-name> --status ready
   ```
2. **Create a task with Definition of Done (`--dod`), Automated Eval (`--eval`), and Session ID (`-s`)**:
   - **MANDATORY**: Where a DoD criteria produces or references a deliverable note, artifact, or document, agents **MUST** format it as an Obsidian clickable `[[wikilink]]` (e.g. `[[knowledge/auth-spec|Auth specification note]]`).
   ```bash
   tk a p1 Fix auth flow +agent-os @claude due:tomorrow \
     --dod "[[specs/auth-spec|Architecture specification approved]]" \
     --dod "Unit tests pass in CI" \
     --eval "cargo test" \
     -s "<your-session-id>"
   ```
3. **Claim a task & attach your Session ID (`status: in-progress`)**:
   Always pass your agent session ID (`-s <session_id>`, or full resume command like `-s "claude -r 8y9380573586"`) so the user can jump straight back into your conversation with `tk u <id>`:
   ```bash
   tk c 1 -o <agent-name> -s "<session-id>"
   ```
4. **Run automated verification (`eval`)**:
   ```bash
   tk eval 1 -a <agent-name>
   ```
5. **Submit for Sign-Off (`status: pending-close`)**:
   - **CRITICAL RULE**: AI agents **MUST NOT** mark tasks as `done` (`tk d`). Agents **MUST** submit finished work to `pending-close` using `tk r` (`tk review`) once `tk eval` passes.
   - `tk d` and MCP `task_complete` from an agent route the task to `pending-close`; on a task already in `pending-close` they fail. Passing `--actor <owner>` does not change this: sign-off needs a person at a terminal.
   - Do not claim a task another owner is working on. `tk c` refuses it; use `--force` only when the human asks you to take it over.
   - If your work differs from the spec, say so with a note instead of editing the card: `tk note <id> -a <agent-name> "built X instead of Y because ..."` (MCP `task_note`). Ken sees notes at sign-off.
   - Do not change a task's `eval` to make it pass. Changing `eval` invalidates the review, revokes any auto-accept, and the human sees the change.
   - For routine work with a good check, ask the human to run `tk auto <id>`: then your passing submit closes the task with no sign-off wait. You cannot turn auto-accept on yourself.
   - Only the task's owner can submit claimed work. Use ids (`tk r 7`), not name fragments: an ambiguous fragment is an error listing the matches.
   ```bash
   tk r 1 -a <agent-name> -s "<session-id>" -n "Summary of changes and [[artifact-wikilink]]"
   ```
6. **Resume an Agent Session (`tk u <id>`)**:
   ```bash
   tk u 1        # Executes `claude -r <sid>`, `codex resume <sid>`, `hermes -r <sid>`, etc.
   tk u 1 -p     # Prints the resume command string
   ```
7. **Manage Dependencies & Critical Path (`tk dep`)**:
   ```bash
   tk dep 3 --needs 1,2   # Blocks tk-003 until tk-001 and tk-002 are done
   tk dep --cpm           # Plain-text Critical Path table (Dur, ES, EF, LS, LF, Buffer, Condition)
   tk dep summary         # Plain-text STE-100 full technical schedule summary
   tk dep short           # Concise 1-paragraph summary (or tk dep summary --short)
   tk dep long            # Full multi-section breakdown (or tk dep --cpm --long)
   tk dep --cpm --json    # Colored JSON in terminal (raw for pipes), contains summary_short and summary_long
   tk dep tree            # Visualizes dependency DAG tree of active blocked tasks
   tk dep --dsm           # Renders Design Structure Matrix (N x N dependency check)
   tk dep --gantt         # Outputs Mermaid Gantt schedule chart
   tk dep --aon           # Outputs Activity-on-Node (AON) CPM network diagram
   tk dep --mermaid       # Outputs standard Mermaid flowchart
   ```
   - **MCP `task_list` / `task_get`**: list returns `{tasks, total, next_offset}` with compact summaries, 100 per page; pass `offset` for the next page, and call `task_get` for the one card you will work on instead of `detail: "full"`.
   - Never edit task cards or `.events.jsonl` directly: `tk doctor` flags state the event log does not explain.
   - **MCP Tool `task_cpm`**: AI agents can query project duration, critical path sequences, and task schedule buffers directly without diagrams or shell execution.
   - In `todo.txt` syntax, tasks can declare dependencies with `needs:1,2`, `after:1,2`, or `dep:1,2`.
   - Blocked tasks cannot be claimed (`claim` guardrail).
   - Completing a blocker (`tk d`) automatically triggers a cascade check, promoting newly unblocked tasks from `blocked` to `ready` and sending a notification.
8. **Archive and Prune (`tk archive` & `tk prune`)**:
   - Deliverable notes created as the Definition of Done (linked via `[[wikilink]]`) remain permanent in the user's workspace.
   - Task cards themselves can be archived or cleaned up once closed:
     ```bash
     tk archive 1        # Moves tk-001.md to tasks/archive/ and sets status: archived
     tk prune 1 -f       # Deletes tk-001.md from active disk
     ```
   - **Database Preservation**: Pruning `.md` cards does **not** erase project history. The append-only event log (`tasks/.events.jsonl`) is strictly preserved, retaining 100% of telemetry, audit history, and Little's Law / MTTC / TPM90 analytics (`tk st`).
9. **Edit Task Properties or Body (`tk set` / `tk edit` / Obsidian)**:
   - CLI property update:
     ```bash
     tk set 1 -t "New Title" -p p0 -o @claude --due 2026-10-25 --dod "[[new-dod]]"
     ```
   - Interactive editor:
     ```bash
     tk edit 1       # Opens markdown card in $EDITOR (hx, nvim, micro, code)
     ```
   - Obsidian visual UI:
     ```bash
     tk open 1       # Opens markdown card directly in Obsidian
     ```

## Task Scopes (Local Repository vs. Global Vault)

`tkmd` supports multi-scope backlogs:
- **Default (Auto)**: Inside a project repository with `./tasks`, `tk` scopes to the local repository. Outside, it defaults to the global vault (`~/Documents/tasks`).
- **`-G, --global`**: Explicitly target the central vault (`~/Documents/tasks`):
  ```bash
  tk -G ls            # List global tasks
  tk -G r             # View global review queue
  tk -G a p1 Title    # Add task to global vault
  ```
- **`-L, --local`**: Force operation strictly on the current repository's `./tasks`.
- **`-A, --all-scopes`**: Merge and view both local repository and global vault tasks:
  ```bash
  tk -A ls            # View merged task board
  tk -A r             # View merged review queue across all scopes
  ```
## Hooks (`tk hook`)


- **SessionStart / UserPromptSubmit Hook**:
  ```bash
  tk hook status
  ```
  Injects a compact list of active `ready`, `in-progress`, and `pending-close` tasks + resume commands.
- **Stop Hook**:
  ```bash
  tk hook stop --owner <agent-name>
  ```
  Warns the agent before exiting if it still has an `in-progress` task that hasn't been evaluated (`tk eval`) and submitted (`tk r`).
