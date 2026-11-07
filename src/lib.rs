#![allow(clippy::too_many_arguments, clippy::manual_clamp)]

mod analytics;
mod ansi;
mod mcp;
mod model;
mod store;
mod todotxt;

use ansi::Ansi;
use anyhow::Result;
use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::{ColorChoice, CommandFactory, Parser, Subcommand};
use model::{CardScope, TaskCard, TaskStatus};
use std::process::Command;
use store::{Approval, ScopeSelection, TaskStore};

fn get_color_choice() -> ColorChoice {
    if std::env::var("NO_COLOR").is_ok() {
        ColorChoice::Never
    } else {
        ColorChoice::Auto
    }
}

fn cli_styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::BrightGreen.on_default().effects(Effects::BOLD))
        .usage(AnsiColor::BrightGreen.on_default().effects(Effects::BOLD))
        .literal(AnsiColor::BrightCyan.on_default().effects(Effects::BOLD))
        .placeholder(AnsiColor::Cyan.on_default())
        .valid(AnsiColor::BrightGreen.on_default())
        .invalid(AnsiColor::Yellow.on_default())
        .error(AnsiColor::Red.on_default().effects(Effects::BOLD))
}

// ANSI colors tuned to Ken's Starship palette (`ken-custom`)
const RESET: Ansi = Ansi("\x1b[0m");
const BOLD: Ansi = Ansi("\x1b[1m");
const BOLD_WHITE: Ansi = Ansi("\x1b[1;97m");
const DIM: Ansi = Ansi("\x1b[2m");
const RED: Ansi = Ansi("\x1b[38;2;224;108;117m"); // #e06c75
const ORANGE: Ansi = Ansi("\x1b[38;2;254;128;25m"); // #fe8019
const AQUA: Ansi = Ansi("\x1b[38;2;86;182;194m"); // #56b6c2
const BLUE: Ansi = Ansi("\x1b[38;2;74;142;194m"); // #4a8ec2
const GREEN: Ansi = Ansi("\x1b[38;2;152;195;121m"); // #98c379
const PURPLE: Ansi = Ansi("\x1b[38;2;198;120;221m"); // #c678dd

#[derive(Parser)]
#[command(
    name = "tkmd",
    version,
    about = "Deterministic Program Management Engine & Multi-Agent MCP Server",
    styles = cli_styles(),
    color = get_color_choice()
)]
struct Cli {
    /// Target repository-local project tasks only (./tasks)
    #[arg(short = 'L', long, global = true)]
    local: bool,

    /// Target central global workspace/vault tasks only
    #[arg(short = 'G', long, global = true)]
    global: bool,

    /// Merge and display tasks across all scopes (both local repository and global vault)
    #[arg(short = 'A', long = "all-scopes", global = true)]
    all_scopes: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

impl Cli {
    pub fn scope(&self) -> ScopeSelection {
        ScopeSelection::from_flags(self.local, self.global, self.all_scopes)
    }
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Add a task (alias: `a`), e.g.: tk a p1 Review EMBA notes +emba due:tomorrow
    #[command(visible_alias = "a")]
    Add {
        /// Task words (no quotes needed! Use p0/p1/p2, +project, @owner, due:tomorrow, sid:8y9380573586)
        shorthand: Vec<String>,
        /// Optional markdown details / specification
        #[arg(short, long)]
        details: Option<String>,
        /// Definition of Done checklist item (--dod "Item 1" --dod "Item 2")
        #[arg(long)]
        dod: Vec<String>,
        /// Shell command to verify task completion (must exit 0)
        #[arg(short, long)]
        eval: Option<String>,
        /// Agent session ID or full resume command (e.g. 8y9380573586 or "claude -r 8y9380573586")
        #[arg(short, long)]
        session: Option<String>,
    },
    /// List tasks (alias: `l`)
    #[command(visible_alias = "l")]
    Ls {
        /// Filter by owner (e.g. ken, hermes, pi, codex, claude)
        #[arg(short, long)]
        owner: Option<String>,
        /// Filter by project (e.g. -p agent-os or --project quantic-emba)
        #[arg(short, long)]
        project: Option<String>,
        /// Filter by status (inbox, ready, in-progress, pending-close, blocked, done)
        #[arg(short, long)]
        status: Option<String>,
        /// Include completed/archived tasks
        #[arg(short, long)]
        all: bool,
        /// Output as JSON (for jq / duckdb)
        #[arg(long)]
        json: bool,
    },
    /// Show full details, DoD checklist, session resume command, and execution log (alias: `s`)
    #[command(visible_alias = "s")]
    Show {
        /// Task ID (e.g. 1 or tk-001)
        id: String,
    },
    /// Claim / start task(s) -> in-progress (alias: `c`)
    #[command(visible_alias = "c")]
    Claim {
        /// Task ID(s) (e.g. 1 or tk-001; several: `tk c 1 2 3`)
        #[arg(required = true, value_delimiter = ',')]
        ids: Vec<String>,
        /// Who is claiming the task (e.g. claude, codex, hermes, pi). Defaults to the configured owner.
        #[arg(short, long)]
        owner: Option<String>,
        /// Agent session ID or resume command (e.g. 8y9380573586 or "claude -r 8y9380573586")
        #[arg(short, long)]
        session: Option<String>,
        /// Take over a task another owner is working on, or pull it back from review (logged)
        #[arg(long)]
        force: bool,
    },
    /// Resume or continue an AI agent session attached to a task (e.g. `agy -c`, `agy --conversation <id>`, `claude -r <id>`) (alias: `u`)
    #[command(visible_alias = "u")]
    Resume {
        /// Task ID (e.g. 1 or tk-001)
        id: String,
        /// Use the agent's `--continue` / `-c` flag (most recent session in workspace, e.g. `agy -c`, `claude -c`, `codex resume --last`, `hermes -c`)
        #[arg(short = 'c', long = "continue")]
        continue_latest: bool,
        /// Target a specific agent from the task's multi-agent session history (e.g. agy, claude, codex, hermes, opencode, gemini, pi)
        #[arg(short, long)]
        agent: Option<String>,
        /// Print the resume/continue command(s) without executing
        #[arg(short, long)]
        print: bool,
        /// Attach or update a session ID on this task (syncs YAML `sessions` + Markdown `## Agent Sessions`)
        #[arg(short, long)]
        set: Option<String>,
    },
    /// Run the automated eval command on one or more tasks
    Eval {
        /// Task ID(s) (e.g. 1 or tk-001; several: `tk eval 1 2 3`)
        #[arg(required = true, value_delimiter = ',')]
        ids: Vec<String>,
        /// Who is running the eval. Defaults to the configured owner.
        #[arg(short, long)]
        actor: Option<String>,
        /// Check the eval without running it: shell syntax, named programs and scripts, absolute
        /// `cd` targets, and which files the first claim will pin. Writes nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Submit task(s) to pending-close (tk r 1 2) or view pending sign-off review queue (tk r / tk review)
    #[command(visible_alias = "r", alias = "rev")]
    Review {
        /// Task ID(s) (e.g. 1 or tk-001; several: `tk r 1 2`). If omitted, displays review queue of tasks awaiting sign-off.
        #[arg(value_delimiter = ',')]
        ids: Vec<String>,
        /// Who is submitting the task for review (default: you at a terminal, `agent` otherwise)
        #[arg(short, long)]
        actor: Option<String>,
        /// Summary note or [[wikilink]] to artifact
        #[arg(short, long)]
        note: Option<String>,
        /// Agent session ID or resume command (e.g. 8y9380573586 or "claude -r 8y9380573586")
        #[arg(short, long)]
        session: Option<String>,
    },
    /// Complete a task -> done + Daily Note log (alias: `d`)
    #[command(visible_alias = "d")]
    Done {
        /// Task ID(s) (e.g. 1 or tk-001; several: `tk d 1 2 3`)
        #[arg(required_unless_present = "pending")]
        ids: Vec<String>,
        /// Close every task awaiting sign-off (`pending-close`) after one confirmation
        #[arg(long)]
        pending: bool,
        /// Skip the interactive `[y/N]` confirmation prompt (requires a terminal, or `[policy] allow_non_tty_yes = true`)
        #[arg(short = 'y', long = "yes")]
        yes: bool,
        /// Accept a check (eval command or its script) that changed since the task was claimed
        #[arg(long)]
        accept_changed_eval: bool,
        /// Who is asking to close the task (log context only). Approval comes from a person at an
        /// interactive terminal.
        #[arg(short, long)]
        actor: Option<String>,
        /// Optional completion note or [[wikilink]]
        #[arg(short, long)]
        note: Option<String>,
    },
    /// Edit a task card directly in Helix (`hx`) or $EDITOR (alias: `e`)
    #[command(visible_alias = "e")]
    Edit {
        /// Task ID (e.g. 1 or tk-001)
        id: String,
    },
    /// Update task title, priority, owner, due date, DoD, eval, or details directly (alias: `set`)
    #[command(visible_alias = "set")]
    Update {
        /// Task ID (e.g. 1 or tk-001)
        id: String,
        /// Update task title
        #[arg(short, long)]
        title: Option<String>,
        /// Update task priority (p0, p1, p2)
        #[arg(short, long)]
        priority: Option<String>,
        /// Update task owner handle (e.g. ken, claude, codex)
        #[arg(short, long)]
        owner: Option<String>,
        /// Update due date (YYYY-MM-DD or tomorrow/today)
        #[arg(long)]
        due: Option<String>,
        /// Update Definition of Done acceptance criteria
        #[arg(long, value_delimiter = ',')]
        dod: Vec<String>,
        /// Update automated eval shell command
        #[arg(long)]
        eval: Option<String>,
        /// Replace the details / specification text (as `tk add -d`); pass "" to clear it
        #[arg(short, long)]
        details: Option<String>,
    },
    /// Open a task card or Kanban board in Obsidian (or $EDITOR in CLI mode) (alias: `o`)
    #[command(visible_alias = "o")]
    Open {
        /// Optional Task ID (if omitted, opens `bases/tasks.base` in Obsidian or `tasks/` in $EDITOR)
        id: Option<String>,
    },
    /// Initialize tkmd locally with Obsidian 1.14 (recommended) or standalone CLI mode
    Init {
        /// Set up with Obsidian 1.14 Kanban Base + Daily Notes + CLI + MCP (Recommended)
        #[arg(long, conflicts_with = "cli")]
        obsidian: bool,
        /// Set up as standalone CLI-only plain Markdown directory (no Obsidian required)
        #[arg(long, conflicts_with = "obsidian")]
        cli: bool,
        /// Initialize as project-local store (.tkmd.toml + ./tasks) in the current directory
        #[arg(short = 'L', long)]
        local: bool,
        /// Target vault path (Obsidian mode) or directory path (CLI mode)
        #[arg(short, long)]
        path: Option<String>,
        /// Default human owner handle (defaults to $USER)
        #[arg(short, long)]
        owner: Option<String>,
        /// Preferred editor for `tk e` and CLI-mode `tk o` (e.g. hx, nvim, zed, code, vim)
        #[arg(short, long)]
        editor: Option<String>,
        /// Relative path to Obsidian 1.14 .base file (default: bases/tasks.base)
        #[arg(long, default_value = "bases/tasks.base")]
        base: String,
        /// Relative path to Daily Notes folder in Obsidian mode (default: journals/daily)
        #[arg(long, default_value = "journals/daily")]
        daily: String,
    },
    /// Live watch dashboard — auto-reflows when you resize the terminal pane or when tasks change (alias: `w`)
    #[command(visible_alias = "w")]
    Watch,
    /// Audit event history & elapsed step deltas from `.events.jsonl` (alias: `h`)
    #[command(visible_alias = "h")]
    History {
        /// Optional Task ID to filter history (e.g. 1 or tk-001)
        id: Option<String>,
        /// Maximum number of recent events to display
        #[arg(short, long, default_value_t = 30)]
        limit: usize,
    },
    /// Flow telemetry: duration, wait time, category/agent split, eval yield, insights & opportunities (alias: `st`)
    #[command(visible_alias = "st")]
    Stats,
    /// Fuzzy search tasks (interactive `fzf` + `bat` preview when no query or `-i` passed) (alias: `f`, `search`)
    #[command(visible_alias = "f", alias = "search")]
    Find {
        /// Optional search words (if omitted, launches interactive `fzf` picker with live card preview)
        query: Vec<String>,
        /// Force interactive `fzf` picker even when an initial query is provided
        #[arg(short, long)]
        interactive: bool,
        /// Open selected task directly in $EDITOR (`hx`)
        #[arg(short, long)]
        edit: bool,
        /// Open selected task directly in Obsidian
        #[arg(short, long)]
        open: bool,
    },
    /// Render dependency tree DAG for active tasks (alias: `t`, also `tk dep tree`)
    #[command(visible_alias = "t")]
    Tree {
        /// Optional task ID to inspect (e.g. 2 or tk-002)
        id: Option<String>,
        /// Filter by project
        #[arg(short, long)]
        project: Option<String>,
    },
    /// Manage task dependencies (alias: `deps`), e.g. `tk dep 2 --needs 1` or `tk dep tree`
    #[command(visible_alias = "deps")]
    Dep {
        /// Dependent task ID (e.g. 2 or tk-002)
        id: Option<String>,
        /// Blocker task IDs that must complete first (--needs 1,2)
        #[arg(short, long, value_delimiter = ',')]
        needs: Vec<String>,
        /// Render dependency tree DAG for active tasks
        #[arg(short, long)]
        tree: bool,
        /// Output dependency graph as a Markdown Mermaid flowchart (rendered by Obsidian/Pele or GitHub)
        #[arg(short, long)]
        mermaid: bool,
        /// Output Design Structure Matrix (DSM) N x N dependency table
        #[arg(long)]
        dsm: bool,
        /// Render native ANSI terminal timeline / Gantt bar chart
        #[arg(long, visible_alias = "chart")]
        timeline: bool,
        /// Output Mermaid Gantt execution schedule
        #[arg(long)]
        gantt: bool,
        /// Render native ANSI terminal Activity-on-Node (AON) execution network
        #[arg(long, visible_alias = "network")]
        network: bool,
        /// Output Activity-on-Node (AON) CPM network diagram (Mermaid markdown)
        #[arg(long)]
        aon: bool,
        /// Filter by project (e.g. -p agent-os or --project quantic-emba)
        #[arg(short, long)]
        project: Option<String>,
        /// Output Critical Path Method (CPM) calculation schedule as a plain text table
        #[arg(long)]
        cpm: bool,
        /// Output data as structured JSON (for AI agents and automation scripts)
        #[arg(long)]
        json: bool,
        /// Output plain-text narrative schedule summary (default: full technical summary)
        #[arg(long)]
        summary: bool,
        /// Output short 1-paragraph summary
        #[arg(long)]
        short: bool,
        /// Output full multi-section technical summary
        #[arg(long)]
        long: bool,
        /// Force ANSI colorization of JSON output (even when piped)
        #[arg(long)]
        color: bool,
    },
    /// Check every card against the event log; exit 1 if a card's state was not recorded
    /// (for example a card hand-edited to `done`)
    Doctor,
    /// Approve a task's eval so the task closes itself when that check passes on submit.
    /// Human-only: run it at a terminal. `--off` returns the task(s) to manual sign-off.
    Auto {
        /// Task ID(s) (e.g. 1 or tk-001; several: `tk auto 1 2 3`)
        #[arg(required = true)]
        ids: Vec<String>,
        /// Turn auto-accept off (manual sign-off again)
        #[arg(long)]
        off: bool,
        /// Skip the interactive `[y/N]` confirmation prompt (requires a terminal, or `[policy] allow_non_tty_yes = true`)
        #[arg(short = 'y', long = "yes")]
        yes: bool,
    },
    /// Archive completed tasks: moves done task .md cards into tasks/archive/ (preserves .events.jsonl DB)
    Archive {
        /// Optional specific Task ID(s) to archive (e.g. `tk archive 1 2 3`, or omit to archive all done tasks)
        #[arg(value_delimiter = ',')]
        ids: Vec<String>,
    },
    /// Prune completed/archived tasks: removes task .md cards from active disk (preserves .events.jsonl DB for telemetry)
    Prune {
        /// Optional specific Task ID(s) to prune (e.g. `tk prune 1 2 3 -f`, or omit to prune all done/archived tasks)
        #[arg(value_delimiter = ',')]
        ids: Vec<String>,
        /// Confirm pruning without interactive prompt
        #[arg(short, long)]
        force: bool,
    },
    /// Agent lifecycle hook (`tk hook status` for SessionStart, `tk hook stop` for Stop/PreToolUse)
    Hook {
        /// Hook event: `status` (injects active tasks) or `stop` (verifies in-progress tasks are reviewed)
        #[arg(default_value = "status")]
        event: String,
        /// Optional agent owner name (e.g. claude, codex, hermes, pi)
        #[arg(short, long)]
        owner: Option<String>,
    },
    /// Export tasks in standard todo.txt format
    Export,
    /// Run the stdio Model Context Protocol (MCP) server for AI agents
    Mcp,
    /// Display brand banner and program metadata
    Banner,
    /// Generate shell auto-completion script for zsh, bash, fish, powershell, or elvish (alias: `completion`)
    #[command(visible_alias = "completion")]
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
        /// Target binary name (defaults to "tkmd", also supports "tk")
        #[arg(short, long, default_value = "tkmd")]
        bin: String,
    },
}

#[cfg(not(target_os = "windows"))]
#[repr(C)]
struct WinSize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

#[cfg(not(target_os = "windows"))]
unsafe extern "C" {
    fn ioctl(fd: i32, request: u64, ...) -> i32;
}

/// Detect live terminal column width via `$COLUMNS` or POSIX `ioctl(TIOCGWINSZ)`.
fn terminal_width() -> usize {
    if let Ok(cols) = std::env::var("COLUMNS")
        && let Ok(n) = cols.trim().parse::<usize>()
        && n >= 30
    {
        return n;
    }

    #[cfg(target_os = "windows")]
    return 100;

    #[cfg(not(target_os = "windows"))]
    {
        #[cfg(target_os = "macos")]
        const TIOCGWINSZ: u64 = 0x4008_7468;
        #[cfg(not(target_os = "macos"))]
        const TIOCGWINSZ: u64 = 0x5413;

        for fd in [1, 0, 2] {
            let mut ws = WinSize {
                ws_row: 0,
                ws_col: 0,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            let rc = unsafe { ioctl(fd, TIOCGWINSZ, &mut ws) };
            if rc == 0 && ws.ws_col >= 30 {
                return ws.ws_col as usize;
            }
        }
        100
    }
}

/// Truncate a string to `max_chars` visible characters, adding `…` if truncated.
fn truncate_str(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else if max_chars == 1 {
        "…".to_string()
    } else {
        let mut out: String = s.chars().take(max_chars - 1).collect();
        out.push('…');
        out
    }
}

fn color_priority(pri: &str) -> String {
    match pri {
        "P0" => format!(" {BOLD}{RED}P0{RESET} "),
        "P1" => format!(" {BOLD}{ORANGE}P1{RESET} "),
        "P2" => format!(" {BOLD}{BLUE}P2{RESET} "),
        _ => format!(" {DIM}{pri}{RESET} "),
    }
}

fn color_priority_compact(pri: &str) -> String {
    match pri {
        "P0" => format!("{BOLD}{RED}P0{RESET}"),
        "P1" => format!("{BOLD}{ORANGE}P1{RESET}"),
        "P2" => format!("{BOLD}{BLUE}P2{RESET}"),
        _ => format!("{DIM}{:<2}{RESET}", truncate_str(pri, 2)),
    }
}

fn color_status(st: &TaskStatus) -> String {
    match st {
        TaskStatus::Inbox => format!("{DIM}◌ inbox{RESET}        "),
        TaskStatus::Ready => format!("{AQUA}○ ready{RESET}        "),
        TaskStatus::InProgress => format!("{BOLD}{ORANGE}● in-progress{RESET}  "),
        TaskStatus::PendingClose => format!("{BOLD}{PURPLE}◐ pending-close{RESET}"),
        TaskStatus::Blocked => format!("{BOLD}{RED}✖ blocked{RESET}      "),
        TaskStatus::Done => format!("{GREEN}✓ done{RESET}         "),
        TaskStatus::Archived => format!("{DIM}▫ archived{RESET}     "),
    }
}

/// 5-character compact status badge for split-screen / medium terminals
fn color_status_compact(st: &TaskStatus) -> String {
    match st {
        TaskStatus::Inbox => format!("{DIM}◌ inb{RESET}"),
        TaskStatus::Ready => format!("{AQUA}○ rdy{RESET}"),
        TaskStatus::InProgress => format!("{BOLD}{ORANGE}● wip{RESET}"),
        TaskStatus::PendingClose => format!("{BOLD}{PURPLE}◐ rev{RESET}"),
        TaskStatus::Blocked => format!("{BOLD}{RED}✖ blk{RESET}"),
        TaskStatus::Done => format!("{GREEN}✓ don{RESET}"),
        TaskStatus::Archived => format!("{DIM}▫ arc{RESET}"),
    }
}

fn color_owner(owner: &str, width: usize) -> String {
    if owner.is_empty() {
        return format!("{DIM}{:<width$}{RESET}", "—", width = width);
    }
    let truncated = truncate_str(owner, width.saturating_sub(1));
    let padded = format!("@{:<w$}", truncated, w = width.saturating_sub(1));
    if model::is_human(owner) {
        format!("{BOLD}{GREEN}{}{RESET}", padded)
    } else {
        format!("{PURPLE}{}{RESET}", padded)
    }
}

fn format_title_cell(title: &str, proj: Option<&str>, col_width: usize) -> String {
    let proj_raw = proj.unwrap_or("");
    let proj_len = if proj_raw.is_empty() {
        0
    } else {
        proj_raw.chars().count() + 1
    };

    // Show right-aligned [[project]] badge when at least 22 chars remain for the title itself
    if !proj_raw.is_empty() && col_width >= proj_len + 22 {
        let max_title = col_width - proj_len;
        let t = truncate_str(title, max_title);
        let vis = t.chars().count() + proj_len;
        let pad = " ".repeat(col_width.saturating_sub(vis));
        format!("{t}{pad} {AQUA}{proj_raw}{RESET}")
    } else {
        let t = truncate_str(title, col_width);
        let vis = t.chars().count();
        let pad = " ".repeat(col_width.saturating_sub(vis));
        format!("{t}{pad}")
    }
}

fn print_dashboard_table(store: &TaskStore, cards: &[TaskCard]) {
    let scope_badge = match store.active_scope {
        ScopeSelection::Local => format!("{DIM}[Scope: {AQUA}Local{RESET}{DIM} (./tasks)]{RESET}"),
        ScopeSelection::Global => {
            format!("{DIM}[Scope: {PURPLE}Global{RESET}{DIM} (~/Documents/tasks)]{RESET}")
        }
        ScopeSelection::All => {
            format!("{DIM}[Scope: {BOLD_WHITE}Merged{RESET}{DIM} (Local + Global)]{RESET}")
        }
        ScopeSelection::Auto => {
            if store.scope == CardScope::Local {
                format!(
                    "{DIM}[Scope: {AQUA}Local{RESET}{DIM} (./tasks) · use -A for all, -G for global]{RESET}"
                )
            } else if store.uninitialized_git_repo.is_some() {
                format!(
                    "{DIM}[Scope: {PURPLE}Global{RESET}{DIM} (~/Documents/tasks) · Git repo without ./tasks · run {AQUA}tk init -L{RESET}{DIM} to init]{RESET}"
                )
            } else {
                format!(
                    "{DIM}[Scope: {PURPLE}Global{RESET}{DIM} (~/Documents/tasks) · use -L for local]{RESET}"
                )
            }
        }
    };

    if store.is_uninitialized_local {
        println!(
            "{ORANGE}  ⚠ No local task tracking in this project (no ./tasks directory).{RESET}  {scope_badge}"
        );
        println!(
            "  {DIM}↳ Initialize with:{RESET} {AQUA}tk init -L{RESET}  {DIM}(or run tk -G for global tasks){RESET}"
        );
        if let Some(ref peer) = store.peer
            && let Ok(peer_cards) = peer.list_store_cards()
        {
            let peer_active: Vec<_> = peer_cards
                .iter()
                .filter(|c| {
                    c.meta.status != TaskStatus::Done && c.meta.status != TaskStatus::Archived
                })
                .collect();
            let peer_pending: Vec<_> = peer_active
                .iter()
                .filter(|c| c.meta.status == TaskStatus::PendingClose)
                .collect();
            if !peer_active.is_empty() {
                if !peer_pending.is_empty() {
                    println!(
                        "  {DIM}↳ Your global vault has {BOLD}{}{RESET}{DIM} active tasks ({BOLD}{}{RESET}{DIM} awaiting review · run {AQUA}tk -G{RESET}{DIM} or {AQUA}tk -G r{RESET}{DIM}){RESET}",
                        peer_active.len(),
                        peer_pending.len()
                    );
                } else {
                    println!(
                        "  {DIM}↳ Your global vault has {BOLD}{}{RESET}{DIM} active tasks (run {AQUA}tk -G{RESET}{DIM} to view){RESET}",
                        peer_active.len()
                    );
                }
            }
        }
        println!();
        return;
    }

    if cards.is_empty() {
        if store.scope == CardScope::Local
            && store.active_scope != ScopeSelection::All
            && let Some(ref peer) = store.peer
            && let Ok(peer_cards) = peer.list_store_cards()
        {
            let peer_active: Vec<_> = peer_cards
                .iter()
                .filter(|c| {
                    c.meta.status != TaskStatus::Done && c.meta.status != TaskStatus::Archived
                })
                .collect();
            let peer_pending: Vec<_> = peer_active
                .iter()
                .filter(|c| c.meta.status == TaskStatus::PendingClose)
                .collect();
            if !peer_pending.is_empty() {
                println!("{DIM}  ✨ Local queue is clear.{RESET}  {scope_badge}");
                println!(
                    "  {BOLD}{PURPLE}↳ Notice:{RESET} {BOLD}{}{RESET} tasks awaiting review in global vault (run {AQUA}tk -G r{RESET} or {AQUA}tk -A r{RESET})",
                    peer_pending.len()
                );
                return;
            } else if !peer_active.is_empty() {
                println!("{DIM}  ✨ Local queue is clear.{RESET}  {scope_badge}");
                println!(
                    "  {BOLD}{PURPLE}↳ Notice:{RESET} {BOLD}{}{RESET} active tasks in global vault (run {AQUA}tk -G{RESET} or {AQUA}tk -A{RESET})",
                    peer_active.len()
                );
                return;
            }
        }
        println!(
            "{DIM}  ✨ Queue is clear. Add a task with: tk a p1 Task title +project{RESET}  {scope_badge}"
        );
        return;
    }

    let term_w = terminal_width();
    let is_all = store.active_scope == ScopeSelection::All;

    let pending_review: Vec<_> = cards
        .iter()
        .filter(|c| c.meta.status == TaskStatus::PendingClose)
        .collect();
    if pending_review.is_empty()
        && store.scope == CardScope::Local
        && store.active_scope != ScopeSelection::All
        && let Some(ref peer) = store.peer
        && let Ok(peer_cards) = peer.list_store_cards()
    {
        let peer_pending: Vec<_> = peer_cards
            .iter()
            .filter(|c| c.meta.status == TaskStatus::PendingClose)
            .collect();
        if !peer_pending.is_empty() {
            println!(
                "\n{DIM}  ↳ Notice: {BOLD}{}{RESET}{DIM} tasks awaiting sign-off in global vault (run {AQUA}tk -G r{RESET}{DIM} or {AQUA}tk -A r{RESET}{DIM}){RESET}",
                peer_pending.len()
            );
        }
    }
    if !pending_review.is_empty() {
        if term_w >= 105 {
            println!(
                "\n{BOLD}{PURPLE}🔍 AWAITING YOUR SIGN-OFF (pending-close: {}){RESET}  {scope_badge}",
                pending_review.len()
            );
        } else {
            let compact_scope = if is_all {
                "[All Scopes]"
            } else {
                match store.scope {
                    CardScope::Local => "[Local]",
                    CardScope::Global => "[Global]",
                }
            };
            println!(
                "\n{BOLD}{PURPLE}🔍 AWAITING YOUR SIGN-OFF (pending-close: {}){RESET}  {DIM}{compact_scope}{RESET}",
                pending_review.len()
            );
        }

        for c in &pending_review {
            let short_id = c
                .meta
                .id
                .trim_start_matches("tk-0")
                .trim_start_matches("tk-");
            let ev = c.meta.eval_status.as_deref().unwrap_or("no-eval");
            let scope_tag = if is_all {
                match c.scope {
                    CardScope::Local => format!("{AQUA}[loc]{RESET} "),
                    CardScope::Global => format!("{PURPLE}[glo]{RESET} "),
                }
            } else {
                String::new()
            };
            let scope_tag_len = if is_all { 6 } else { 0 };

            if term_w >= 115 {
                let ev_label = match ev {
                    "passed (no-eval)" => "passed (auto)",
                    other => other,
                };
                let resume_hint = c
                    .meta
                    .resume_command()
                    .map(|cmd| format!(" · {AQUA}`{cmd}`{RESET}"))
                    .unwrap_or_default();
                let resume_vis_len = c
                    .meta
                    .resume_command()
                    .map(|cmd| cmd.chars().count() + 5)
                    .unwrap_or(0);
                let overhead = 51
                    + resume_vis_len
                    + scope_tag_len
                    + ev_label.chars().count().saturating_sub(6);
                let max_t = term_w.saturating_sub(overhead).max(10);
                let t = truncate_str(&c.meta.title, max_t);
                println!(
                    "  {BOLD}{:<6}{RESET} {} {} {DIM}[eval: {}]{RESET} {}{}{} {DIM}-> tk d {}{RESET}",
                    c.meta.id,
                    color_priority(&c.meta.priority),
                    color_owner(&c.meta.owner, 9),
                    ev_label,
                    scope_tag,
                    t,
                    resume_hint,
                    short_id
                );
            } else {
                // Compact responsive layout for split-screen / non-fullscreen panes (< 115 cols)
                let ev_badge = match ev {
                    "passed" => format!("{GREEN}pass{RESET}"),
                    "passed (no-eval)" => format!("{DIM}pass{RESET}"),
                    "failed" => format!("{RED}fail{RESET}"),
                    _ => format!("{DIM}none{RESET}"),
                };
                let overhead = 32 + scope_tag_len;
                let max_t = term_w.saturating_sub(overhead).max(12);
                let t = truncate_str(&c.meta.title, max_t);
                println!(
                    "  {BOLD}{:<6}{RESET} {} {DIM}[{}]{RESET} {}{} {DIM}-> tk d {}{RESET}",
                    c.meta.id,
                    color_priority_compact(&c.meta.priority),
                    ev_badge,
                    scope_tag,
                    t,
                    short_id
                );
                if let Some(cmd) = c.meta.resume_command() {
                    let max_cmd_w = term_w.saturating_sub(18).max(15);
                    let truncated_cmd = truncate_str(&cmd, max_cmd_w);
                    println!("         {DIM}↳ session:{RESET} {AQUA}{truncated_cmd}{RESET}");
                }
            }
        }
    } else {
        println!("\n  {scope_badge}");
    }

    if term_w >= 115 {
        // TIER 1: Full 6-column table — stretches edge-to-edge across the terminal pane!
        // Fixed overhead = 63 chars
        let title_col_width = term_w.saturating_sub(63).max(16);
        let title_bar = "─".repeat(title_col_width + 2);
        let title_hdr_pad = " ".repeat(title_col_width.saturating_sub(5));

        println!(
            "\n{DIM}╭────────┬──────┬─────────────────┬───────────┬────────────┬{}╮{RESET}",
            title_bar
        );
        println!(
            "{DIM}│{RESET} {BOLD_WHITE}ID{RESET}     {DIM}│{RESET} {BOLD_WHITE}PRI{RESET}  {DIM}│{RESET} {BOLD_WHITE}STATUS{RESET}          {DIM}│{RESET} {BOLD_WHITE}OWNER{RESET}     {DIM}│{RESET} {BOLD_WHITE}DUE{RESET}        {DIM}│{RESET} {BOLD_WHITE}TITLE{RESET}{} {DIM}│{RESET}",
            title_hdr_pad
        );
        println!(
            "{DIM}├────────┼──────┼─────────────────┼───────────┼────────────┼{}┤{RESET}",
            title_bar
        );

        for c in cards {
            let due = c.meta.due.as_deref().unwrap_or("-");
            let cell = format_title_cell(&c.meta.title, c.meta.project.as_deref(), title_col_width);
            let id_scope_suffix = if is_all {
                match c.scope {
                    CardScope::Local => format!("{AQUA}L{RESET}"),
                    CardScope::Global => format!("{PURPLE}G{RESET}"),
                }
            } else {
                " ".to_string()
            };
            println!(
                "{DIM}│{RESET} {BOLD}{:<6}{RESET}{id_scope_suffix}{DIM}│{RESET} {} {DIM}│{RESET} {} {DIM}│{RESET} {} {DIM}│{RESET} {:<10} {DIM}│{RESET} {} {DIM}│{RESET}",
                c.meta.id,
                color_priority(&c.meta.priority),
                color_status(&c.meta.status),
                color_owner(&c.meta.owner, 9),
                due,
                cell
            );
        }
        println!(
            "{DIM}╰────────┴──────┴─────────────────┴───────────┴────────────┴{}╯{RESET}",
            title_bar
        );
    } else if term_w >= 56 {
        // TIER 2: Split-Screen / Medium 5-column table — stretches edge-to-edge across the pane!
        // Fixed overhead = 36 chars
        let title_col_width = term_w.saturating_sub(36).max(12);
        let title_bar = "─".repeat(title_col_width + 2);
        let title_hdr_pad = " ".repeat(title_col_width.saturating_sub(5));

        println!(
            "\n{DIM}╭────────┬────┬───────┬─────────┬{}╮{RESET}",
            title_bar
        );
        println!(
            "{DIM}│{RESET} {BOLD_WHITE}ID{RESET}     {DIM}│{RESET} {BOLD_WHITE}PR{RESET} {DIM}│{RESET} {BOLD_WHITE}STAT{RESET}  {DIM}│{RESET} {BOLD_WHITE}OWNER{RESET}   {DIM}│{RESET} {BOLD_WHITE}TITLE{RESET}{} {DIM}│{RESET}",
            title_hdr_pad
        );
        println!(
            "{DIM}├────────┼────┼───────┼─────────┼{}┤{RESET}",
            title_bar
        );

        for c in cards {
            let cell = format_title_cell(&c.meta.title, c.meta.project.as_deref(), title_col_width);
            let id_scope_suffix = if is_all {
                match c.scope {
                    CardScope::Local => format!("{AQUA}L{RESET}"),
                    CardScope::Global => format!("{PURPLE}G{RESET}"),
                }
            } else {
                " ".to_string()
            };
            println!(
                "{DIM}│{RESET} {BOLD}{:<6}{RESET}{id_scope_suffix}{DIM}│{RESET} {} {DIM}│{RESET} {} {DIM}│{RESET} {} {DIM}│{RESET} {} {DIM}│{RESET}",
                c.meta.id,
                color_priority_compact(&c.meta.priority),
                color_status_compact(&c.meta.status),
                color_owner(&c.meta.owner, 7),
                cell
            );
        }
        println!(
            "{DIM}╰────────┴────┴───────┴─────────┴{}╯{RESET}",
            title_bar
        );
    } else {
        // TIER 3: Ultra-narrow pane (< 56 cols, fixed overhead = 20 chars)
        let title_col_width = term_w.saturating_sub(20).max(8);
        let title_bar = "─".repeat(title_col_width + 2);
        let title_hdr_pad = " ".repeat(title_col_width.saturating_sub(5));

        println!("\n{DIM}╭────────┬───────┬{}╮{RESET}", title_bar);
        println!(
            "{DIM}│{RESET} {BOLD_WHITE}ID{RESET}     {DIM}│{RESET} {BOLD_WHITE}STAT{RESET}  {DIM}│{RESET} {BOLD_WHITE}TITLE{RESET}{} {DIM}│{RESET}",
            title_hdr_pad
        );
        println!("{DIM}├────────┼───────┼{}┤{RESET}", title_bar);
        for c in cards {
            let cell = format_title_cell(&c.meta.title, c.meta.project.as_deref(), title_col_width);
            let id_scope_suffix = if is_all {
                match c.scope {
                    CardScope::Local => format!("{AQUA}L{RESET}"),
                    CardScope::Global => format!("{PURPLE}G{RESET}"),
                }
            } else {
                " ".to_string()
            };
            println!(
                "{DIM}│{RESET} {BOLD}{:<6}{RESET}{id_scope_suffix}{DIM}│{RESET} {} {DIM}│{RESET} {} {DIM}│{RESET}",
                c.meta.id,
                color_status_compact(&c.meta.status),
                cell
            );
        }
        println!("{DIM}╰────────┴───────┴{}╯{RESET}", title_bar);
    }

    if store.uninitialized_git_repo.is_some() {
        println!(
            "{DIM}  ↳ Tip: Git repository without ./tasks. Run {AQUA}tk init -L{RESET}{DIM} to initialize a local project backlog.{RESET}\n"
        );
    }
}

fn print_review_dashboard(store: &TaskStore) -> Result<()> {
    let cards = store.list_all()?;
    let pending: Vec<_> = cards
        .iter()
        .filter(|c| c.meta.status == TaskStatus::PendingClose)
        .collect();

    let scope_badge = match store.active_scope {
        ScopeSelection::Local => format!("{DIM}[Scope: {AQUA}Local{RESET}{DIM} (./tasks)]{RESET}"),
        ScopeSelection::Global => {
            format!("{DIM}[Scope: {PURPLE}Global{RESET}{DIM} (~/Documents/tasks)]{RESET}")
        }
        ScopeSelection::All => {
            format!("{DIM}[Scope: {BOLD_WHITE}Merged{RESET}{DIM} (Local + Global)]{RESET}")
        }
        ScopeSelection::Auto => {
            if store.scope == CardScope::Local {
                format!(
                    "{DIM}[Scope: {AQUA}Local{RESET}{DIM} (./tasks) · Use -A for all, -G for global]{RESET}"
                )
            } else if store.uninitialized_git_repo.is_some() {
                format!(
                    "{DIM}[Scope: {PURPLE}Global{RESET}{DIM} (~/Documents/tasks) · Git repo without ./tasks · run {AQUA}tk init -L{RESET}{DIM} to init]{RESET}"
                )
            } else {
                format!(
                    "{DIM}[Scope: {PURPLE}Global{RESET}{DIM} (~/Documents/tasks) · Use -L for local]{RESET}"
                )
            }
        }
    };

    println!(
        "\n{BOLD_WHITE}══════════════════════════════════════════════════════════════════════════════{RESET}"
    );
    println!("{BOLD}{PURPLE}📋 TASK SIGN-OFF & REVIEW QUEUE{RESET}  {scope_badge}");
    println!(
        "{BOLD_WHITE}══════════════════════════════════════════════════════════════════════════════{RESET}"
    );

    if store.is_uninitialized_local {
        println!(
            "{ORANGE}  ⚠ No local task tracking in this project (no ./tasks directory).{RESET}"
        );
        println!(
            "  {DIM}↳ Initialize with:{RESET} {AQUA}tk init -L{RESET}  {DIM}(or run tk -G r for global review tasks){RESET}"
        );
        if let Some(ref peer) = store.peer
            && let Ok(peer_cards) = peer.list_store_cards()
        {
            let peer_pending: Vec<_> = peer_cards
                .iter()
                .filter(|c| c.meta.status == TaskStatus::PendingClose)
                .collect();
            if !peer_pending.is_empty() {
                println!(
                    "  {DIM}↳ Your global vault has {BOLD}{}{RESET}{DIM} tasks awaiting sign-off (run {AQUA}tk -G r{RESET}{DIM}){RESET}",
                    peer_pending.len()
                );
            }
        }
        println!();
        return Ok(());
    }

    if pending.is_empty() {
        if store.scope == CardScope::Local
            && store.active_scope != ScopeSelection::All
            && let Some(ref peer) = store.peer
            && let Ok(peer_cards) = peer.list_store_cards()
        {
            let peer_pending: Vec<_> = peer_cards
                .iter()
                .filter(|c| c.meta.status == TaskStatus::PendingClose)
                .collect();
            if !peer_pending.is_empty() {
                println!("{GREEN}  ✨ No local tasks awaiting sign-off!{RESET}");
                println!(
                    "  {BOLD}{PURPLE}↳ Notice:{RESET} {BOLD}{}{RESET} tasks awaiting review in global vault (run {AQUA}tk -G r{RESET} or {AQUA}tk -A r{RESET})\n",
                    peer_pending.len()
                );
                return Ok(());
            }
        }
        println!("{GREEN}  ✨ No tasks awaiting sign-off! All tasks are clear or done.{RESET}");
        println!("  {DIM}Submit a task for review with: tk r <id> -n \"Summary\"{RESET}");
        if store.uninitialized_git_repo.is_some() {
            println!(
                "{DIM}  ↳ Tip: Git repository without ./tasks. Run {AQUA}tk init -L{RESET}{DIM} to initialize a local project backlog.{RESET}"
            );
        }
        println!();
        return Ok(());
    }

    let local_pending: Vec<_> = pending
        .iter()
        .filter(|c| c.scope == CardScope::Local)
        .collect();
    let global_pending: Vec<_> = pending
        .iter()
        .filter(|c| c.scope == CardScope::Global)
        .collect();

    if !local_pending.is_empty() {
        println!(
            "\n{BOLD}{AQUA}📦 Local Project Reviews ({}){RESET}",
            local_pending.len()
        );
        for c in &local_pending {
            print_review_card_summary(c);
        }
    }

    if !global_pending.is_empty() {
        println!(
            "\n{BOLD}{PURPLE}🌐 Global Workspace Reviews ({}){RESET}",
            global_pending.len()
        );
        for c in &global_pending {
            print_review_card_summary(c);
        }
    }

    println!(
        "\n{DIM}Sign-off and complete with:{RESET}  {AQUA}tk d <id>{RESET}  {DIM}(or `tk d <id> -n \"LGTM\"`){RESET}"
    );
    if store.uninitialized_git_repo.is_some() {
        println!(
            "{DIM}  ↳ Tip: Git repository without ./tasks. Run {AQUA}tk init -L{RESET}{DIM} to initialize a local project backlog.{RESET}"
        );
    }
    println!();
    Ok(())
}

fn print_review_card_summary(c: &TaskCard) {
    let short_id = c
        .meta
        .id
        .trim_start_matches("tk-0")
        .trim_start_matches("tk-");
    let ev = c.meta.eval_status.as_deref().unwrap_or("no-eval");
    let ev_colored = match ev {
        "passed" => format!("{GREEN}eval: passed{RESET}"),
        "failed" => format!("{RED}eval: failed{RESET}"),
        _ => format!("{DIM}no-eval{RESET}"),
    };

    println!(
        "  {BOLD}{:<6}{RESET} {} {} [{}] {} {DIM}-> tk d {}{RESET}",
        c.meta.id,
        color_priority(&c.meta.priority),
        color_owner(&c.meta.owner, 9),
        ev_colored,
        c.meta.title,
        short_id
    );
    if let Some(cmd) = c.meta.resume_command() {
        println!(
            "         {DIM}↳ resume:{RESET} {AQUA}{cmd}{RESET} {DIM}(or `tk u {short_id}`){RESET}"
        );
    }
    if !c.meta.dod.is_empty() {
        let done_dod = c
            .meta
            .dod
            .iter()
            .filter(|d| d.starts_with("[x]") || d.starts_with("✓"))
            .count();
        println!(
            "         {DIM}↳ DoD criteria:{RESET} {}/{} checked",
            done_dod,
            c.meta.dod.len()
        );
    }
}

pub fn run() -> Result<()> {
    ansi::init_for_stdout();
    let cli = Cli::parse();

    if let Some(Commands::Completions { shell, ref bin }) = cli.command {
        let mut cmd = Cli::command();
        cmd.set_bin_name(bin);
        let mut buf = Vec::new();
        clap_complete::generate(shell, &mut cmd, bin, &mut buf);
        let s = String::from_utf8_lossy(&buf);
        if bin == "tkmd" && shell == clap_complete::Shell::Zsh {
            let mut customized = s.replacen("#compdef tkmd", "#compdef tkmd tk", 1);
            customized = customized.replace(
                "if [ \"$funcstack[1]\" = \"_tkmd\" ]; then",
                "_tk() {\n    _tkmd \"$@\"\n}\n\nif [ \"$funcstack[1]\" = \"_tkmd\" ] || [ \"$funcstack[1]\" = \"_tk\" ]; then",
            );
            print!("{}", customized);
            println!("\ncompdef _tkmd tk 2>/dev/null || true");
        } else {
            print!("{}", s);
        }
        return Ok(());
    }

    let scope_sel = cli.scope();
    let store = TaskStore::load_with_scope(scope_sel)?;

    match cli.command.unwrap_or(Commands::Ls {
        owner: None,
        project: None,
        status: None,
        all: false,
        json: false,
    }) {
        Commands::Add {
            shorthand,
            details,
            dod,
            eval,
            session,
        } => {
            let input = shorthand.join(" ");
            let card = store.add_task(&input, details.as_deref(), dod, eval, session)?;
            println!(
                "{GREEN}✓ Created{RESET} {BOLD}{}{RESET} [{}] ({}) -> {BOLD}{}{RESET}",
                card.meta.id,
                color_priority(&card.meta.priority).trim(),
                color_owner(&card.meta.owner, card.meta.owner.chars().count() + 1),
                card.meta.title
            );
        }
        Commands::Ls {
            owner,
            project,
            status,
            all,
            json,
        } => {
            let cards = store.list_all()?;
            let filtered: Vec<_> = cards
                .into_iter()
                .filter(|c| {
                    if !all
                        && matches!(c.meta.status, TaskStatus::Done | TaskStatus::Archived)
                        && status.is_none()
                    {
                        return false;
                    }
                    let owner_ok = owner
                        .as_deref()
                        .is_none_or(|o| c.meta.owner.eq_ignore_ascii_case(o));
                    let status_ok = status
                        .as_deref()
                        .is_none_or(|s| c.meta.status.as_str().eq_ignore_ascii_case(s));
                    let project_ok = project.as_deref().is_none_or(|p| {
                        let p_lower = p.to_lowercase();
                        if p_lower == "global" || p_lower == "none" {
                            c.meta.project.is_none()
                        } else {
                            c.meta
                                .project
                                .as_deref()
                                .unwrap_or("")
                                .to_lowercase()
                                .contains(&p_lower)
                        }
                    });
                    owner_ok && status_ok && project_ok
                })
                .collect();

            if json {
                crate::analytics::print_json_value(&filtered, false)?;
                return Ok(());
            }

            print_dashboard_table(&store, &filtered);
        }
        Commands::Show { id } => {
            let mut card = store.find_task(&id)?;
            TaskStore::sync_card_sessions(&mut card);
            let scope_badge = match card.scope {
                CardScope::Local => format!("{AQUA}[local]{RESET}"),
                CardScope::Global => format!("{PURPLE}[global]{RESET}"),
            };
            println!(
                "\n{BOLD}{}{RESET} · {} · {} · {} · {}",
                card.meta.id,
                color_priority(&card.meta.priority).trim(),
                color_status(&card.meta.status).trim(),
                color_owner(&card.meta.owner, 9).trim(),
                scope_badge
            );
            if let Some(cmd) = card.meta.resume_command() {
                let cont = card
                    .meta
                    .continue_command()
                    .filter(|c| c != &cmd)
                    .map(|c| {
                        format!(
                            "  {DIM}Continue:{RESET} {AQUA}{c}{RESET} {DIM}(`tk u {id} -c`){RESET}"
                        )
                    })
                    .unwrap_or_default();
                println!(
                    "{DIM}Resume:{RESET}   {AQUA}{cmd}{RESET} {DIM}(`tk u {id}`){RESET}{cont}"
                );
            }
            if !card.meta.blocked_by.is_empty() {
                println!(
                    "{DIM}Blocked by:{RESET} {RED}{}{RESET}",
                    card.meta.blocked_by.join(", ")
                );
            }
            if !card.meta.blocks.is_empty() {
                println!(
                    "{DIM}Blocks:{RESET}     {AQUA}{}{RESET}",
                    card.meta.blocks.join(", ")
                );
            }
            if let Some(a) = &card.meta.approved_eval {
                println!(
                    "{DIM}Auto-accept:{RESET} {GREEN}closes when `{}` passes{RESET}",
                    a
                );
            }
            println!("{DIM}File:       {}{RESET}\n", card.path);
            println!("{}", card.body);
        }
        Commands::Claim {
            ids,
            owner,
            session,
            force,
        } => {
            let owner = owner.unwrap_or_else(|| store.config.default_owner.clone());
            for id in &ids {
                let card = store.claim_task(id, &owner, session.clone(), force)?;
                let sess_info = card
                    .meta
                    .resume_command()
                    .map(|cmd| format!(" [session: {AQUA}{cmd}{RESET}]"))
                    .unwrap_or_default();
                println!(
                    "{ORANGE}▶ Claimed{RESET} {BOLD}{}{RESET} for {} (`in-progress`){}: {}",
                    card.meta.id,
                    color_owner(&card.meta.owner, 9).trim(),
                    sess_info,
                    card.meta.title
                );
            }
        }
        Commands::Resume {
            id,
            continue_latest,
            agent,
            print,
            set,
        } => {
            let mut card = store.find_task(&id)?;
            if let Some(ref new_sid) = set {
                let target_agent = agent.as_deref().unwrap_or(&card.meta.owner).to_string();
                TaskStore::record_session_on_card(&mut card, &target_agent, Some(new_sid));
                TaskStore::sync_card_sessions(&mut card);
                store.write_card(&card)?;
                println!(
                    "{GREEN}✓ Recorded session on {BOLD}{}{RESET}: `@{}` -> `{}` (synced to YAML + `## Agent Sessions` in Markdown)",
                    card.meta.id, target_agent, new_sid
                );
                if !print && !continue_latest {
                    return Ok(());
                }
            }

            let cmd_opt = if let Some(ref ag) = agent {
                if continue_latest {
                    model::agent_continue_cmd(ag)
                } else {
                    // Find matching agent in `card.meta.sessions` (most recent first)
                    let found_sid = card.meta.sessions.iter().rev().find_map(|entry| {
                        let (a, s) = entry.split_once(':')?;
                        if a.trim().eq_ignore_ascii_case(ag) {
                            Some(s.trim().to_string())
                        } else {
                            None
                        }
                    });
                    model::agent_resume_cmd(
                        ag,
                        found_sid.as_deref().or(card.meta.session_id.as_deref()),
                    )
                }
            } else if continue_latest {
                card.meta.continue_command()
            } else {
                card.meta.resume_command()
            };

            let Some(cmd_str) = cmd_opt else {
                anyhow::bail!(
                    "No agent session recorded on {}. Attach one with: `tk u {} -a agy -s <session_id>` or `tk c {} -o agy`",
                    card.meta.id,
                    card.meta.id,
                    card.meta.id
                );
            };
            if print {
                println!("{}", cmd_str);
            } else {
                println!(
                    "{AQUA}↻ Launching agent session for {BOLD}{}{RESET}{AQUA} -> `{}`{RESET}",
                    card.meta.id, cmd_str
                );
                let parts: Vec<&str> = cmd_str.split_whitespace().collect();
                if let Some((bin, args)) = parts.split_first() {
                    Command::new(bin).args(args).status()?;
                }
            }
        }
        Commands::Eval {
            ids, dry_run: true, ..
        } => {
            let mut any_problem = false;
            for id in &ids {
                let report = store.dry_run_eval(id)?;
                if report.ok() {
                    println!(
                        "{GREEN}✅ Dry-run OK{RESET} for {BOLD}{}{RESET} (nothing was run)",
                        report.card.meta.id
                    );
                } else {
                    any_problem = true;
                    println!(
                        "{RED}❌ Dry-run found {} problem(s){RESET} for {BOLD}{}{RESET} (nothing was run)",
                        report.problems.len(),
                        report.card.meta.id
                    );
                }
                for p in &report.problems {
                    println!("   {RED}✖{RESET} {}", p);
                }
                for n in &report.notes {
                    println!("   {DIM}•{RESET} {}", n);
                }
            }
            if any_problem {
                std::process::exit(1);
            }
        }
        Commands::Eval { ids, actor, .. } => {
            let actor = actor.unwrap_or_else(|| store.config.default_owner.clone());
            let mut any_failed = false;
            for id in &ids {
                let (card, passed, output) = store.run_eval(id, &actor)?;
                if passed {
                    println!(
                        "{GREEN}✅ Eval PASSED{RESET} for {BOLD}{}{RESET}: {}",
                        card.meta.id, output
                    );
                } else {
                    println!(
                        "{RED}❌ Eval FAILED{RESET} for {BOLD}{}{RESET}: {}",
                        card.meta.id, output
                    );
                    any_failed = true;
                }
            }
            if any_failed {
                std::process::exit(1);
            }
        }
        Commands::Review {
            ids,
            actor,
            note,
            session,
        } => {
            if !ids.is_empty() {
                let actor = actor.unwrap_or_else(|| {
                    if is_interactive() {
                        store.config.default_owner.clone()
                    } else {
                        "agent".to_string()
                    }
                });
                for task_id in &ids {
                    let card = store.submit_for_review(
                        task_id,
                        &actor,
                        note.as_deref(),
                        session.clone(),
                    )?;
                    let sess_info = card
                        .meta
                        .resume_command()
                        .map(|cmd| format!(" [resume: `{cmd}`]"))
                        .unwrap_or_default();
                    let eval_note = match card.meta.receipt.as_ref().map(|r| r.eval_result.as_str())
                    {
                        Some("no-eval") => format!("{ORANGE}no eval ran{RESET}"),
                        _ => "eval passed".to_string(),
                    };
                    if card.meta.status == TaskStatus::Done {
                        println!(
                            "{GREEN}⚡ Auto-accepted{RESET} {BOLD}{}{RESET} (`done`): its approved eval passed: {}",
                            card.meta.id, card.meta.title
                        );
                    } else {
                        println!(
                            "{PURPLE}🔍 Submitted{RESET} {BOLD}{}{RESET} to `pending-close` ({}){}: {}",
                            card.meta.id, eval_note, sess_info, card.meta.title
                        );
                    }
                }
            } else {
                print_review_dashboard(&store)?;
            }
        }
        Commands::Done {
            ids,
            pending,
            yes,
            accept_changed_eval,
            actor,
            note,
        } => {
            let actor = actor.unwrap_or_else(|| store.config.default_owner.clone());
            let cards = done_targets(&store, &ids, pending)?;
            if cards.is_empty() {
                println!("{GREEN}  ✨ No tasks awaiting sign-off.{RESET}");
                return Ok(());
            }
            let approval = cli_approval(&store, &cards, yes)?;
            let mut failed = 0;
            for card in &cards {
                // Bind each approval to the receipt shown in the summary.
                let shown = (card.meta.status == TaskStatus::PendingClose)
                    .then_some(card.meta.receipt.as_ref())
                    .flatten();
                match store.complete_task_confirmed(
                    &card.meta.id,
                    &actor,
                    note.as_deref(),
                    approval,
                    shown,
                    accept_changed_eval,
                ) {
                    Ok(c) => println!(
                        "{}",
                        done_message(&c.meta.status, &c.meta.id, &c.meta.title)
                    ),
                    Err(e) if cards.len() > 1 => {
                        failed += 1;
                        eprintln!("{RED}✖ Skipped{RESET} {BOLD}{}{RESET}: {}", card.meta.id, e);
                    }
                    Err(e) => return Err(e),
                }
            }
            if failed > 0 {
                anyhow::bail!("{} of {} task(s) not closed", failed, cards.len());
            }
        }
        Commands::Edit { id } => {
            let card = store.find_task(&id)?;
            store.open_in_editor(std::path::Path::new(&card.path))?;
        }
        Commands::Update {
            id,
            title,
            priority,
            owner,
            due,
            dod,
            eval,
            details,
        } => {
            let card = store.update_task(&id, title, priority, owner, due, dod, eval, details)?;
            println!(
                "{GREEN}✏️  Updated{RESET} {BOLD}{}{RESET} ({})",
                card.meta.id, card.meta.title
            );
            println!("   Path: {}", card.path);
        }
        Commands::Open { id } => {
            if store.config.mode == "cli" {
                if let Some(q) = id {
                    let card = store.find_task(&q)?;
                    store.open_in_editor(std::path::Path::new(&card.path))?;
                } else {
                    store.open_in_editor(&store.tasks_path)?;
                }
            } else {
                let rel_path = if let Some(q) = id {
                    let card = store.find_task(&q)?;
                    let fname = std::path::Path::new(&card.path)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("");
                    format!("{}/{}", store.config.tasks_dir, fname)
                } else if !store.config.base_file.trim().is_empty() {
                    store.config.base_file.clone()
                } else {
                    "bases/tasks.base".to_string()
                };
                let vault_name = store
                    .config
                    .obsidian_vault_name
                    .clone()
                    .or_else(|| {
                        std::path::Path::new(&store.config.vault_path)
                            .file_name()
                            .and_then(|s| s.to_str())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_else(|| "Documents".to_string());
                let uri = format!(
                    "obsidian://open?vault={}&file={}",
                    vault_name.replace(' ', "%20"),
                    rel_path.replace(' ', "%20")
                );
                Command::new("open").arg(uri).status()?;
                println!(
                    "{AQUA}↗ Opened {} in Obsidian ({}){RESET}",
                    rel_path, vault_name
                );
            }
        }
        Commands::Init {
            obsidian,
            cli,
            local,
            path,
            owner,
            editor,
            base,
            daily,
        } => {
            use std::io::{self, Write};
            let home = std::env::var("HOME").unwrap_or_else(|_| "/Users/ken".to_string());
            let default_user = std::env::var("USER").unwrap_or_else(|_| "ken".to_string());
            let detected_vault = store::detect_obsidian_vault();
            let installed_editors = store::detect_installed_editors();
            let default_editor = editor.clone().unwrap_or_else(store::detect_editor);

            let chosen_mode = if local {
                if obsidian { "obsidian" } else { "cli" }
            } else if obsidian {
                "obsidian"
            } else if cli {
                "cli"
            } else if let Some(ref p) = path {
                // If user passed --path without --obsidian/--cli, inspect `.obsidian` inside that path!
                let expanded = if let Some(stripped) = p.strip_prefix("~/") {
                    format!("{}/{}", home, stripped)
                } else {
                    p.clone()
                };
                if std::path::Path::new(&expanded).join(".obsidian").is_dir() {
                    "obsidian"
                } else {
                    "cli"
                }
            } else {
                let auto_mode = if detected_vault.is_some() { "1" } else { "2" };
                println!("\n{BOLD}⚡ tkmd (`tk`) Local Setup Wizard{RESET}");
                if let Some(ref v) = detected_vault {
                    println!(
                        "  {DIM}Auto-detected `.obsidian` vault:{RESET} {GREEN}{}{RESET}",
                        v.display()
                    );
                } else {
                    println!(
                        "  {DIM}No `.obsidian` directory found in current path or ~/Documents — defaulting to CLI mode.{RESET}"
                    );
                }
                println!(
                    "  {DIM}Auto-detected editors:{RESET}          {AQUA}{}{RESET} (default: {BOLD}{}{RESET})",
                    if installed_editors.is_empty() {
                        "none".to_string()
                    } else {
                        installed_editors.join(", ")
                    },
                    default_editor
                );
                println!(
                    "  {GREEN}[1]{RESET} {BOLD}Obsidian 1.14 + CLI (Recommended){RESET} — Kanban `.base` board + Daily Notes + CLI + MCP"
                );
                println!(
                    "  {AQUA}[2]{RESET} {BOLD}Standalone CLI Only{RESET}               — Plain Markdown cards + Editor ({}) + MCP",
                    default_editor
                );
                print!("\nChoose mode [1/2, default: {}]: ", auto_mode);
                io::stdout().flush()?;
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                let trimmed = input.trim();
                if trimmed.is_empty() {
                    if auto_mode == "1" { "obsidian" } else { "cli" }
                } else if trimmed == "2" || trimmed.eq_ignore_ascii_case("cli") {
                    "cli"
                } else {
                    "obsidian"
                }
            };

            let default_path = if local {
                ".".to_string()
            } else if chosen_mode == "obsidian" {
                detected_vault
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| format!("{}/Documents", home))
            } else {
                format!("{}/.local/share/tkmd", home)
            };

            let chosen_path = if let Some(p) = path {
                p
            } else if local || obsidian || cli {
                default_path
            } else {
                print!("Target path [default: {}]: ", default_path);
                io::stdout().flush()?;
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                let trimmed = input.trim();
                if trimmed.is_empty() {
                    default_path
                } else {
                    trimmed.to_string()
                }
            };

            let chosen_editor = if let Some(ed) = editor {
                Some(ed)
            } else if local || obsidian || cli {
                Some(default_editor)
            } else {
                print!(
                    "Preferred editor ({}) [default: {}]: ",
                    installed_editors.join("/"),
                    default_editor
                );
                io::stdout().flush()?;
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                let trimmed = input.trim();
                if trimmed.is_empty() {
                    Some(default_editor)
                } else {
                    Some(trimmed.to_string())
                }
            };

            let chosen_owner = owner.unwrap_or(default_user);
            let (cfg, cfg_path) = TaskStore::init_workspace(
                chosen_mode,
                &chosen_path,
                &chosen_owner,
                &base,
                &daily,
                chosen_editor,
                local,
            )?;

            println!(
                "\n{GREEN}✅ Initialized tkmd in {BOLD}{}{RESET}{GREEN} mode!{RESET}",
                cfg.mode
            );
            println!("  {DIM}• Config:{RESET}      {}", cfg_path.display());
            println!(
                "  {DIM}• Tasks dir:{RESET}   {}/{}",
                cfg.vault_path, cfg.tasks_dir
            );
            println!(
                "  {DIM}• Editor:{RESET}      {}  {DIM}(used by `tk e <id>`{})",
                cfg.editor.as_deref().unwrap_or("hx"),
                if cfg.mode == "cli" { " and `tk o`" } else { "" }
            );
            if cfg.mode == "obsidian" {
                println!(
                    "  {DIM}• Kanban Base:{RESET} {}/{}  {DIM}(open with: tk o){RESET}",
                    cfg.vault_path, cfg.base_file
                );
                println!(
                    "  {DIM}• Daily dir:{RESET}   {}/{}",
                    cfg.vault_path, cfg.daily_dir
                );
            }
            println!(
                "\n{BOLD}🤖 AI Agent MCP Registration (Claude / Codex / Gemini / OpenCode):{RESET}"
            );
            println!(
                r#"  "mcpServers": {{
    "tkmd": {{
      "command": "tkmd",
      "args": ["mcp"]
    }}
  }}"#
            );
            println!("\n{BOLD}⚡ Quickstart Cheatsheet (1-Letter Daily Shortcuts):{RESET}");
            println!(
                "  {AQUA}tk{RESET}                            Render live task dashboard & review banner"
            );
            println!(
                "  {AQUA}tk a p1 Title +proj due:today{RESET}  Create task (supports p0/p1/p2, @owner, +project, sid:<id>)"
            );
            println!("  {AQUA}tk c 1 -o agy{RESET}                  Claim task -> in-progress");
            println!(
                "  {AQUA}tk eval 1{RESET}                      Run automated bash verification command"
            );
            println!(
                "  {AQUA}tk r 1 -n \"Ready\"{RESET}              Submit to pending-close (auto-evals + triggers macOS notification)"
            );
            println!(
                "  {AQUA}tk d 1{RESET}                          Sign off and complete task (appends to Daily Note in Obsidian mode)"
            );
            println!(
                "  {AQUA}tk u 1{RESET}                          Resume AI agent session (or `tk u 1 -c` to continue latest in workspace)"
            );
            println!(
                "  {AQUA}tk f [query]{RESET}                    Fuzzy search cards (interactive fzf picker with live bat preview)"
            );
            println!(
                "  {AQUA}tk h [id]{RESET}                       View audit event history & elapsed transition deltas"
            );
            println!(
                "  {AQUA}tk st{RESET}                           Flow telemetry, agent/human breakdown & bottleneck insights"
            );
            println!(
                "  {AQUA}tk e 1{RESET}                          Edit task card markdown directly in {}",
                cfg.editor.as_deref().unwrap_or("hx")
            );
            if cfg.mode == "obsidian" {
                println!(
                    "  {AQUA}tk o [id]{RESET}                      Open Kanban board (or card) in Obsidian"
                );
            } else {
                println!(
                    "  {AQUA}tk o [id]{RESET}                      Open tasks directory in {}",
                    cfg.editor.as_deref().unwrap_or("hx")
                );
            }
            println!(
                "  {AQUA}tk w{RESET}                            Live dashboard auto-refresh (watches pane resize & task changes)"
            );
        }
        Commands::Watch => {
            use std::io::{self, Write};
            use std::time::Duration;
            let mut last_w = 0usize;
            let mut last_fingerprint = String::new();

            loop {
                let cur_w = terminal_width();
                let cards: Vec<_> = store
                    .list_all()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| !matches!(c.meta.status, TaskStatus::Done | TaskStatus::Archived))
                    .collect();
                let fp = cards
                    .iter()
                    .map(|c| {
                        format!(
                            "{}:{}:{}:{}:{:?}",
                            c.meta.id,
                            c.meta.status.as_str(),
                            c.meta.owner,
                            c.meta.updated,
                            c.meta.session_id
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("|");

                if cur_w != last_w || fp != last_fingerprint {
                    print!("\x1b[2J\x1b[H");
                    println!(
                        "{DIM}⚡ tk watch (live auto-resize · {} cols · Ctrl-C to exit){RESET}",
                        cur_w
                    );
                    print_dashboard_table(&store, &cards);
                    let _ = io::stdout().flush();
                    last_w = cur_w;
                    last_fingerprint = fp;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        Commands::History { id, limit } => {
            analytics::print_history(&store, id.as_deref(), limit, terminal_width())?;
        }
        Commands::Stats => {
            analytics::print_stats(&store, terminal_width())?;
        }
        Commands::Find {
            query,
            interactive,
            edit,
            open,
        } => {
            analytics::run_fuzzy_finder(
                &store,
                &query,
                interactive,
                edit,
                open,
                print_dashboard_table,
            )?;
        }
        Commands::Tree { id, project } => {
            render_dependency_tree(&store, id.as_deref(), project.as_deref())?;
        }
        Commands::Dep {
            id,
            needs,
            tree,
            mermaid,
            dsm,
            timeline,
            gantt,
            network,
            aon,
            project,
            cpm,
            json,
            summary,
            short,
            long,
            color,
        } => {
            let is_short = short || id.as_deref() == Some("short");
            let is_long = long || id.as_deref() == Some("long");
            let is_summary = summary || id.as_deref() == Some("summary") || is_short || is_long;
            let is_cpm = cpm || id.as_deref() == Some("cpm") || id.as_deref() == Some("table");
            let is_json = json;
            let is_dsm = dsm || id.as_deref() == Some("dsm");
            let is_timeline = timeline
                || id.as_deref() == Some("timeline")
                || id.as_deref() == Some("chart")
                || id.as_deref() == Some("bars")
                || id.as_deref() == Some("gantt");
            let is_network = network
                || id.as_deref() == Some("network")
                || id.as_deref() == Some("aon")
                || id.as_deref() == Some("dag");
            let is_gantt = gantt;
            let is_aon = aon;
            let is_mermaid = mermaid || id.as_deref() == Some("mermaid");
            let is_tree = tree
                || id.as_deref() == Some("tree")
                || (id.is_none()
                    && needs.is_empty()
                    && !is_mermaid
                    && !is_dsm
                    && !is_gantt
                    && !is_aon
                    && !is_cpm
                    && !is_summary
                    && !is_json
                    && !is_timeline
                    && !is_network);

            if is_summary && !is_cpm && !is_json {
                let all_cards = store.list_all()?;
                let analysis = crate::analytics::compute_cpm(&all_cards);
                if is_short && !is_long {
                    println!("\n{BOLD}📋 SCHEDULE SUMMARY (SHORT){RESET}");
                    println!("{}\n", analysis.summary_short);
                } else {
                    println!("\n{}", analysis.summary_long);
                }
            } else if is_cpm || (is_json && !is_dsm && !is_gantt && !is_aon && !is_mermaid && !tree)
            {
                let all_cards = store.list_all()?;
                let analysis = crate::analytics::compute_cpm(&all_cards);

                if is_json {
                    crate::analytics::print_json_value(&analysis, color)?;
                } else {
                    let term_w = terminal_width();
                    println!("\n{BOLD}🎯 CRITICAL PATH & SCHEDULE ANALYSIS (CPM){RESET}");
                    println!(
                        "{DIM}Mathematical schedule calculated from dependencies and durations.{RESET}\n"
                    );

                    if analysis.schedule.is_empty() {
                        println!("  {DIM}No tasks recorded.{RESET}\n");
                    } else if term_w >= 105 {
                        // TIER 1: Full 11-column table for wide displays (>= 105 cols)
                        let title_w = term_w.saturating_sub(84).max(16);
                        let table_w = 82 + title_w;
                        let div_line = "─".repeat(table_w);

                        println!(
                            "  {BOLD_WHITE}{:<8} {:>5}  {:>5}  {:>5}  {:>5}  {:>5}  {:>6}  {:<21} {:<10} {:<10} Title{RESET}",
                            "ID",
                            "Dur",
                            "ES",
                            "EF",
                            "LS",
                            "LF",
                            "Buffer",
                            "Condition",
                            "Owner",
                            "Status"
                        );
                        println!("  {div_line}");

                        for t in &analysis.schedule {
                            let cond_text = if t.is_critical {
                                "CRITICAL (0s buffer)"
                            } else {
                                &format!("Float ({} buf)", t.buffer_display)
                            };

                            let cond_colored = if t.is_critical {
                                format!("{BOLD}{RED}{:<21}{RESET}", cond_text)
                            } else {
                                format!("{DIM}{:<21}{RESET}", cond_text)
                            };

                            let owner_fmt = format!("@{}", t.owner);
                            let title_fmt = truncate_str(&t.title, title_w);
                            let es_str = crate::analytics::format_duration_concise(t.early_start);
                            let ef_str = crate::analytics::format_duration_concise(t.early_finish);
                            let ls_str = crate::analytics::format_duration_concise(t.late_start);
                            let lf_str = crate::analytics::format_duration_concise(t.late_finish);

                            println!(
                                "  {:<8} {:>5}  {:>5}  {:>5}  {:>5}  {:>5}  {:>6}  {} {:<10} {:<10} {}",
                                t.id,
                                t.duration_display,
                                es_str,
                                ef_str,
                                ls_str,
                                lf_str,
                                t.buffer_display,
                                cond_colored,
                                owner_fmt,
                                t.status,
                                title_fmt
                            );
                        }
                        println!("  {div_line}");
                        println!(
                            "\n  {BOLD}Project Duration:{RESET} {}",
                            analysis.project_duration_display
                        );
                        if !analysis.critical_path.is_empty() {
                            println!(
                                "  {BOLD}Critical Path:{RESET}    {BOLD}{RED}{}{RESET}",
                                analysis.critical_path.join(" ➔ ")
                            );
                        }
                        let crit_count = analysis.schedule.iter().filter(|t| t.is_critical).count();
                        println!(
                            "  {BOLD}Summary:{RESET}          {} of {} tasks on critical path (zero buffer).\n",
                            crit_count,
                            analysis.schedule.len()
                        );
                    } else if term_w >= 70 {
                        // TIER 2: Split-screen / Medium table (70..104 cols, ideal for 80-col panes)
                        let title_w = term_w.saturating_sub(48).max(12);
                        let table_w = 46 + title_w;
                        let div_line = "─".repeat(table_w);

                        println!(
                            "  {BOLD_WHITE}{:<7} {:<5} {:<5} {:<5} {:<6} {:<10} {:<8} Title{RESET}",
                            "ID", "Dur", "ES", "EF", "Buffer", "Condition", "Owner"
                        );
                        println!("  {div_line}");

                        for t in &analysis.schedule {
                            let cond_text = if t.is_critical {
                                "CRITICAL"
                            } else {
                                &format!("Float {}", t.buffer_display)
                            };

                            let cond_colored = if t.is_critical {
                                format!("{BOLD}{RED}{:<10}{RESET}", cond_text)
                            } else {
                                format!("{DIM}{:<10}{RESET}", cond_text)
                            };

                            let owner_fmt = format!("@{}", truncate_str(&t.owner, 7));
                            let title_fmt = truncate_str(&t.title, title_w);
                            let es_str = crate::analytics::format_duration_concise(t.early_start);
                            let ef_str = crate::analytics::format_duration_concise(t.early_finish);

                            println!(
                                "  {:<7} {:<5} {:<5} {:<5} {:<6} {} {:<8} {}",
                                t.id,
                                t.duration_display,
                                es_str,
                                ef_str,
                                t.buffer_display,
                                cond_colored,
                                owner_fmt,
                                title_fmt
                            );
                        }
                        println!("  {div_line}");
                        println!(
                            "\n  {BOLD}Project Duration:{RESET} {}",
                            analysis.project_duration_display
                        );
                        if !analysis.critical_path.is_empty() {
                            println!(
                                "  {BOLD}Critical Path:{RESET}    {BOLD}{RED}{}{RESET}",
                                analysis.critical_path.join(" ➔ ")
                            );
                        }
                        let crit_count = analysis.schedule.iter().filter(|t| t.is_critical).count();
                        println!(
                            "  {BOLD}Summary:{RESET}          {} of {} tasks on critical path (zero buffer).\n",
                            crit_count,
                            analysis.schedule.len()
                        );
                    } else {
                        // TIER 3: Ultra-narrow table (< 70 cols)
                        let title_w = term_w.saturating_sub(26).max(8);
                        let table_w = 24 + title_w;
                        let div_line = "─".repeat(table_w);

                        println!(
                            "  {BOLD_WHITE}{:<7} {:<4} {:<4} {:<4} {:<5} Title{RESET}",
                            "ID", "ES", "EF", "Buf", "Cond"
                        );
                        println!("  {div_line}");

                        for t in &analysis.schedule {
                            let cond_colored = if t.is_critical {
                                format!("{BOLD}{RED}{:<5}{RESET}", "CRIT")
                            } else {
                                format!("{DIM}+{:<4}{RESET}", t.buffer_display)
                            };
                            let title_fmt = truncate_str(&t.title, title_w);
                            let es_str = crate::analytics::format_duration_concise(t.early_start);
                            let ef_str = crate::analytics::format_duration_concise(t.early_finish);

                            println!(
                                "  {:<7} {:<4} {:<4} {:<4} {} {}",
                                t.id, es_str, ef_str, t.buffer_display, cond_colored, title_fmt
                            );
                        }
                        println!("  {div_line}");
                        println!(
                            "\n  {BOLD}Duration:{RESET} {}",
                            analysis.project_duration_display
                        );
                        if !analysis.critical_path.is_empty() {
                            println!(
                                "  {BOLD}Path:{RESET}     {BOLD}{RED}{}{RESET}",
                                analysis.critical_path.join(" ➔ ")
                            );
                        }
                        let crit_count = analysis.schedule.iter().filter(|t| t.is_critical).count();
                        println!(
                            "  {BOLD}Summary:{RESET}  {}/{} critical\n",
                            crit_count,
                            analysis.schedule.len()
                        );
                    }

                    if is_short {
                        println!("  {}\n", analysis.summary_short);
                    } else if is_long {
                        println!("{}", analysis.summary_long);
                    }
                }
            } else if is_dsm {
                let mut all_cards = store.list_all()?;
                all_cards.sort_by(|a, b| a.meta.id.cmp(&b.meta.id));

                println!("\n{BOLD}📐 DEPENDENCY STRUCTURE MATRIX (DSM){RESET}");
                println!(
                    "{DIM}Read each ROW: \"This task needs Column [#] to complete first\"{RESET}\n"
                );

                let n = all_cards.len();
                if n == 0 {
                    println!("  {DIM}No tasks recorded.{RESET}");
                } else {
                    let term_w = terminal_width();
                    let dsm_prefix_w = 12 + n * 3;
                    let dsm_status_w = 16;
                    let dsm_title_w = term_w
                        .saturating_sub(dsm_prefix_w + dsm_status_w + 4)
                        .max(10);
                    let dsm_bar_w =
                        (dsm_prefix_w + dsm_title_w + dsm_status_w).min(term_w.saturating_sub(4));
                    let dsm_div = "─".repeat(dsm_bar_w);

                    print!("  {BOLD_WHITE}#   ID     {RESET}");
                    for j in 0..n {
                        print!(" {:>2}", j + 1);
                    }
                    println!("  {BOLD_WHITE}Task Title (Status){RESET}");
                    println!("  {dsm_div}");

                    let mut circular_deadlocks = Vec::new();

                    for (i, row_card) in all_cards.iter().enumerate() {
                        print!("  {:>2}  {:<6} ", i + 1, row_card.meta.id);
                        for (j, col_card) in all_cards.iter().enumerate() {
                            if i == j {
                                print!("  {DIM}·{RESET}");
                            } else if row_card
                                .meta
                                .blocked_by
                                .iter()
                                .any(|b| b.eq_ignore_ascii_case(&col_card.meta.id))
                            {
                                if j > i {
                                    circular_deadlocks.push((
                                        row_card.meta.id.clone(),
                                        col_card.meta.id.clone(),
                                        i + 1,
                                        j + 1,
                                    ));
                                    print!("  {BOLD}{RED}X{RESET}");
                                } else {
                                    print!("  {BOLD}{AQUA}X{RESET}");
                                }
                            } else {
                                print!("   ");
                            }
                        }
                        let truncated_title = truncate_str(&row_card.meta.title, dsm_title_w);
                        println!("  {} ({})", truncated_title, row_card.meta.status.as_str());
                    }
                    println!("  {dsm_div}");

                    println!(
                        "  {DIM}Legend: {BOLD}{AQUA}X{RESET} = Normal prerequisite (row waits on earlier column) | {DIM}·{RESET} = Self{RESET}"
                    );

                    if circular_deadlocks.is_empty() {
                        println!(
                            "  {GREEN}✓ Clean Execution Order:{RESET} No circular dependencies or deadlocks detected."
                        );
                    } else {
                        println!(
                            "  {RED}⚠️  Circular Deadlocks Detected (Task waits on a later task):{RESET}"
                        );
                        for (r_id, c_id, r_num, c_num) in circular_deadlocks {
                            println!(
                                "    • Row {} ({}) waits on future Column {} ({})",
                                r_num, r_id, c_num, c_id
                            );
                        }
                    }
                }
                println!();
            } else if is_timeline {
                let all_cards = store.list_all()?;
                let analysis = crate::analytics::compute_cpm(&all_cards);
                let term_w = terminal_width();
                crate::analytics::render_terminal_gantt(&analysis, term_w);
            } else if is_gantt {
                let all_cards = store.list_all()?;
                let today = chrono::Local::now().format("%Y-%m-%d").to_string();
                let analysis = crate::analytics::compute_cpm(&all_cards);
                let crit_set: std::collections::HashSet<_> =
                    analysis.critical_path.into_iter().collect();

                println!("```mermaid");
                println!("gantt");
                println!("    dateFormat YYYY-MM-DD");
                println!("    title TKMD Execution Schedule");
                println!("    axisFormat %m-%d\n");

                for status in &[
                    TaskStatus::PendingClose,
                    TaskStatus::InProgress,
                    TaskStatus::Blocked,
                    TaskStatus::Ready,
                    TaskStatus::Inbox,
                    TaskStatus::Done,
                ] {
                    let matching: Vec<&TaskCard> = all_cards
                        .iter()
                        .filter(|c| &c.meta.status == status)
                        .collect();
                    if !matching.is_empty() {
                        println!("    section {}", status.as_str());
                        for c in matching {
                            let is_crit = crit_set.contains(&c.meta.id);
                            let safe_id = c.meta.id.replace('-', "_");
                            let safe_title = c.meta.title.replace(':', " -").replace('"', "'");
                            let start_date = if c.meta.created.is_empty() {
                                &today
                            } else {
                                &c.meta.created
                            };
                            let end_date = c.meta.due.as_deref().unwrap_or(&today);

                            match c.meta.status {
                                TaskStatus::Done => {
                                    let tag = if is_crit { ":crit, done" } else { ":done" };
                                    println!(
                                        "    {} {}, {}, {}, {}",
                                        safe_title, tag, safe_id, start_date, end_date
                                    );
                                }
                                TaskStatus::InProgress | TaskStatus::PendingClose => {
                                    let tag = if is_crit { ":crit, active" } else { ":active" };
                                    println!(
                                        "    {} {}, {}, {}, {}",
                                        safe_title, tag, safe_id, start_date, end_date
                                    );
                                }
                                TaskStatus::Blocked => {
                                    if !c.meta.blocked_by.is_empty() {
                                        let blockers = c
                                            .meta
                                            .blocked_by
                                            .iter()
                                            .map(|b| b.replace('-', "_"))
                                            .collect::<Vec<_>>()
                                            .join(" ");
                                        println!(
                                            "    {} :crit, {}, after {}, 1d",
                                            safe_title, safe_id, blockers
                                        );
                                    } else {
                                        println!(
                                            "    {} :crit, {}, {}, 1d",
                                            safe_title, safe_id, start_date
                                        );
                                    }
                                }
                                _ => {
                                    let tag = if is_crit { ":crit" } else { ":active" };
                                    println!(
                                        "    {} {}, {}, {}, 1d",
                                        safe_title, tag, safe_id, start_date
                                    );
                                }
                            }
                        }
                    }
                }
                println!("```");
            } else if is_network {
                let all_cards = store.list_all()?;
                let analysis = crate::analytics::compute_cpm(&all_cards);
                let term_w = terminal_width();
                crate::analytics::render_terminal_aon(&analysis, term_w, project.as_deref());
            } else if is_aon {
                let all_cards = store.list_all()?;
                let analysis = crate::analytics::compute_cpm(&all_cards);

                println!("```mermaid");
                println!("graph LR");
                println!(
                    "    classDef critical fill:#fee2e2,stroke:#ef4444,stroke-width:2.5px,color:#991b1b;"
                );
                println!(
                    "    classDef criticalReview fill:#fae8ff,stroke:#c026d3,stroke-width:2.5px,color:#701a75;"
                );
                println!(
                    "    classDef blocked fill:#fef2f2,stroke:#dc2626,stroke-width:2px,stroke-dasharray: 4 4,color:#991b1b;"
                );
                println!(
                    "    classDef done fill:#dcfce7,stroke:#16a34a,stroke-width:1.5px,color:#14532d;"
                );
                println!(
                    "    classDef inprogress fill:#fef3c7,stroke:#d97706,stroke-width:2px,color:#78350f;"
                );
                println!(
                    "    classDef noncritical fill:#f8fafc,stroke:#94a3b8,stroke-width:1.5px,color:#1e293b;\n"
                );

                for t in &analysis.schedule {
                    let safe_id = t.id.replace('-', "_");
                    let class_tag = if t.status == "done" || t.status == "archived" {
                        ":::done"
                    } else if t.is_critical {
                        if t.status == "pending-close" {
                            ":::criticalReview"
                        } else {
                            ":::critical"
                        }
                    } else if t.status == "blocked" {
                        ":::blocked"
                    } else if t.status == "in-progress" {
                        ":::inprogress"
                    } else {
                        ":::noncritical"
                    };

                    let crit_badge = if t.is_critical { " [CRITICAL]" } else { "" };
                    let safe_title = t.title.replace('"', "'");
                    let es_str = crate::analytics::format_duration_concise(t.early_start);
                    let ef_str = crate::analytics::format_duration_concise(t.early_finish);
                    println!(
                        "    {}[\"<b>{}</b>{}<br>{}<br><i>d: {} | ES: {} · EF: {} | Buffer: {}</i><br><small>@{} · {}</small>\"]{}",
                        safe_id,
                        t.id,
                        crit_badge,
                        safe_title,
                        t.duration_display,
                        es_str,
                        ef_str,
                        t.buffer_display,
                        t.owner,
                        t.status,
                        class_tag
                    );
                }

                println!();
                let sched_map: std::collections::HashMap<_, _> =
                    analysis.schedule.iter().map(|s| (&s.id, s)).collect();
                for t in &analysis.schedule {
                    let target_id = t.id.replace('-', "_");
                    for bid in &t.blocked_by {
                        let source_id = bid.replace('-', "_");
                        let is_crit_edge = if let Some(pred) = sched_map.get(bid) {
                            pred.is_critical && t.is_critical && pred.early_finish == t.early_start
                        } else {
                            false
                        };

                        if is_crit_edge {
                            println!("    {} ==> {}", source_id, target_id);
                        } else {
                            println!("    {} --> {}", source_id, target_id);
                        }
                    }
                }
                println!("```");

                if !analysis.critical_path.is_empty() {
                    println!(
                        "\n{BOLD}🎯 Critical Path ({} project duration, 0s buffer):{RESET}",
                        analysis.project_duration_display
                    );
                    println!(
                        "  {BOLD}{RED}{}{RESET}\n",
                        analysis.critical_path.join(" ➔ ")
                    );
                }
            } else if is_mermaid {
                let all_cards = store.list_all()?;
                let mut node_ids = std::collections::BTreeSet::new();
                for c in &all_cards {
                    if !c.meta.blocked_by.is_empty() || !c.meta.blocks.is_empty() {
                        node_ids.insert(c.meta.id.clone());
                        for bid in &c.meta.blocked_by {
                            node_ids.insert(bid.clone());
                        }
                    }
                }

                println!("```mermaid");
                println!("graph TD");
                println!(
                    "    classDef ready fill:#e0f2fe,stroke:#0284c7,stroke-width:2px,color:#0369a1;"
                );
                println!(
                    "    classDef inprogress fill:#fef3c7,stroke:#d97706,stroke-width:2px,color:#b45309;"
                );
                println!(
                    "    classDef pendingclose fill:#f3e8ff,stroke:#9333ea,stroke-width:2px,color:#7e22ce;"
                );
                println!(
                    "    classDef blocked fill:#fee2e2,stroke:#ef4444,stroke-width:2px,color:#b91c1c;"
                );
                println!(
                    "    classDef done fill:#dcfce7,stroke:#16a34a,stroke-width:2px,color:#15803d;"
                );
                println!(
                    "    classDef inbox fill:#f1f5f9,stroke:#64748b,stroke-width:2px,color:#475569;"
                );
                println!(
                    "    classDef archived fill:#f8fafc,stroke:#94a3b8,stroke-width:1.5px,color:#64748b;\n"
                );

                for nid in &node_ids {
                    let safe_id = nid.replace('-', "_");
                    if let Ok(c) = store.find_task(nid) {
                        let status_str = c.meta.status.as_str();
                        let safe_title = c.meta.title.replace('"', "'");
                        let class_tag = match c.meta.status {
                            TaskStatus::Ready => ":::ready",
                            TaskStatus::InProgress => ":::inprogress",
                            TaskStatus::PendingClose => ":::pendingclose",
                            TaskStatus::Blocked => ":::blocked",
                            TaskStatus::Done => ":::done",
                            TaskStatus::Inbox => ":::inbox",
                            TaskStatus::Archived => ":::archived",
                        };
                        println!(
                            "    {}[\"{} [{}]: {}\"]{}",
                            safe_id, c.meta.id, status_str, safe_title, class_tag
                        );
                    } else {
                        println!("    {}[\"{}\"]", safe_id, nid);
                    }
                }
                for c in &all_cards {
                    let target_id = c.meta.id.replace('-', "_");
                    for bid in &c.meta.blocked_by {
                        let source_id = bid.replace('-', "_");
                        println!("    {} --> {}", source_id, target_id);
                    }
                }
                println!("```");
            } else if is_tree {
                render_dependency_tree(&store, id.as_deref(), project.as_deref())?;
            } else if let Some(dep_id) = id {
                if needs.is_empty() {
                    let card = store.find_task(&dep_id)?;
                    println!("\n{BOLD}Dependencies for {}:{RESET}", card.meta.id);
                    if card.meta.blocked_by.is_empty() {
                        println!("  {DIM}None (unblocked){RESET}");
                    } else {
                        println!(
                            "  {RED}Blocked by:{RESET} {}",
                            card.meta.blocked_by.join(", ")
                        );
                    }
                    if !card.meta.blocks.is_empty() {
                        println!(
                            "  {AQUA}Blocks downstream:{RESET} {}",
                            card.meta.blocks.join(", ")
                        );
                    }
                } else {
                    let updated = store.add_dependency(&dep_id, &needs)?;
                    println!(
                        "{GREEN}✓ Updated dependencies:{RESET} {BOLD}{}{RESET} is now blocked by: {RED}{}{RESET} (status: {})",
                        updated.meta.id,
                        updated.meta.blocked_by.join(", "),
                        color_status(&updated.meta.status).trim()
                    );
                }
            }
        }
        Commands::Doctor => {
            let issues = store.integrity_report()?;
            if issues.is_empty() {
                println!("{GREEN}✓ Every card matches the event log{RESET}");
            } else {
                for i in &issues {
                    println!("{RED}✖{RESET} {}", i);
                }
                anyhow::bail!("{} integrity issue(s)", issues.len());
            }
        }
        Commands::Auto { ids, off, yes } => {
            let cards = ids
                .iter()
                .map(|id| store.find_task(id))
                .collect::<Result<Vec<_>>>()?;
            if off {
                for c in &cards {
                    store.revoke_eval_approval(&c.meta.id)?;
                    println!(
                        "{ORANGE}⏸ Auto-accept off{RESET} {BOLD}{}{RESET}: manual sign-off",
                        c.meta.id
                    );
                }
                return Ok(());
            }
            let approval = if is_interactive() || non_tty_yes_allowed(&store, yes) {
                let mut err = std::io::stderr();
                use std::io::Write;
                writeln!(
                    err,
                    "{BOLD}Auto-accept ({} task(s)):{RESET} each closes itself when this check passes",
                    cards.len()
                )?;
                for c in &cards {
                    writeln!(
                        err,
                        "  {BOLD}{}{RESET} {}: `{}`",
                        c.meta.id,
                        c.meta.title,
                        c.meta
                            .eval
                            .as_deref()
                            .unwrap_or("(no eval: cannot auto-accept)")
                    )?;
                }
                if !yes && !confirm("Approve these checks?")? {
                    anyhow::bail!("Not approved: nothing changed");
                }
                Approval::Interactive
            } else {
                Approval::Untrusted
            };
            for c in &cards {
                let card = store.approve_eval(&c.meta.id, approval)?;
                println!(
                    "{GREEN}⚡ Auto-accept on{RESET} {BOLD}{}{RESET}: closes when `{}` passes",
                    card.meta.id,
                    card.meta.approved_eval.as_deref().unwrap_or("")
                );
            }
        }
        Commands::Archive { ids } => {
            let archived = store.archive_many(&ids)?;
            if archived.is_empty() {
                println!("{DIM}No completed tasks found to archive.{RESET}");
            } else {
                println!(
                    "{GREEN}📦 Archived {} completed task(s) into tasks/archive/:{RESET}",
                    archived.len()
                );
                for tid in &archived {
                    println!("  • {BOLD}{}{RESET}", tid);
                }
                println!(
                    "  {DIM}(Full lifecycle metrics preserved in .events.jsonl for `tk st` & DuckDB){RESET}"
                );
            }
        }
        Commands::Prune { ids, force } => {
            if !force {
                use std::io::{self, Write};
                print!(
                    "Are you sure you want to prune completed task notes from active disk? [y/N]: "
                );
                io::stdout().flush()?;
                let mut resp = String::new();
                io::stdin().read_line(&mut resp)?;
                if !resp.trim().eq_ignore_ascii_case("y") {
                    println!("{DIM}Pruning cancelled.{RESET}");
                    return Ok(());
                }
            }
            let pruned = store.prune_many(&ids)?;
            if pruned.is_empty() {
                println!("{DIM}No completed or archived tasks found to prune.{RESET}");
            } else {
                println!(
                    "{ORANGE}🗑️ Pruned {} task markdown card(s) from disk:{RESET}",
                    pruned.len()
                );
                for tid in &pruned {
                    println!("  • {DIM}{}{RESET}", tid);
                }
                println!(
                    "  {GREEN}✅ Database .events.jsonl is 100% intact — all historical telemetry remains accessible!{RESET}"
                );
            }
        }
        Commands::Hook { event, owner } => {
            let cards = store.list_all()?;
            match event.to_lowercase().as_str() {
                "stop" => {
                    let in_prog: Vec<_> = cards
                        .iter()
                        .filter(|c| {
                            c.meta.status == TaskStatus::InProgress
                                && owner
                                    .as_deref()
                                    .is_none_or(|o| c.meta.owner.eq_ignore_ascii_case(o))
                        })
                        .collect();
                    if !in_prog.is_empty() {
                        println!("⚠️ [tkmd hook] The following task(s) are still `in-progress`:");
                        for c in in_prog {
                            println!(
                                "  - {} (@{}): {} -> Run `tk eval {}` and `tk r {} -n \"summary\"` when ready for sign-off.",
                                c.meta.id, c.meta.owner, c.meta.title, c.meta.id, c.meta.id
                            );
                        }
                    }
                }
                _ => {
                    let active: Vec<_> = cards
                        .iter()
                        .filter(|c| {
                            !matches!(c.meta.status, TaskStatus::Done | TaskStatus::Archived)
                                && owner
                                    .as_deref()
                                    .is_none_or(|o| c.meta.owner.eq_ignore_ascii_case(o))
                        })
                        .collect();
                    if !active.is_empty() {
                        println!("[tkmd active queue ({})]:", active.len());
                        for c in active {
                            let sess = c
                                .meta
                                .resume_command()
                                .map(|s| format!(" [{}]", s))
                                .unwrap_or_default();
                            println!(
                                "  • {} [{}] ({} / @{}){}: {}",
                                c.meta.id,
                                c.meta.priority,
                                c.meta.status.as_str(),
                                c.meta.owner,
                                sess,
                                c.meta.title
                            );
                        }
                    }
                }
            }
        }
        Commands::Export => {
            for c in store.list_all()? {
                let done_prefix = if c.meta.status == TaskStatus::Done {
                    format!("x {} ", c.meta.updated)
                } else {
                    String::new()
                };
                let pri = match c.meta.priority.as_str() {
                    "P0" => "(A) ",
                    "P1" => "(B) ",
                    "P2" => "(C) ",
                    _ => "(D) ",
                };
                let proj = c
                    .meta
                    .project
                    .as_deref()
                    .map(|p| format!(" +{}", p.trim_matches(['[', ']'])))
                    .unwrap_or_default();
                let due = c
                    .meta
                    .due
                    .as_deref()
                    .map(|d| format!(" due:{}", d))
                    .unwrap_or_default();
                println!(
                    "{}{}{} {} @{}{}{}",
                    done_prefix, pri, c.meta.created, c.meta.title, c.meta.owner, proj, due
                );
            }
        }
        Commands::Mcp => {
            mcp::run_mcp_server(&store)?;
        }
        Commands::Banner => {
            let cyan = "\x1b[38;2;56;189;248m";
            let coral = "\x1b[38;2;244;63;94m";
            println!(
                "\n{}{BOLD}  ████████╗██╗  ██╗{}{BOLD}███╗   ███╗██████╗ {RESET}",
                cyan, coral
            );
            println!(
                "{}{BOLD}  ╚══██╔══╝██║ ██╔╝{}{BOLD}████╗ ████║██╔══██╗{RESET}",
                cyan, coral
            );
            println!(
                "{}{BOLD}     ██║   █████═╝ {}{BOLD}██╔████╔██║██║  ██║{RESET}",
                cyan, coral
            );
            println!(
                "{}{BOLD}     ██║   ██╔═██╗ {}{BOLD}██║╚██╔╝██║██║  ██║{RESET}",
                cyan, coral
            );
            println!(
                "{}{BOLD}     ██║   ██║ ╚██╗{}{BOLD}██║ ╚═╝ ██║██████╔╝{RESET}",
                cyan, coral
            );
            println!(
                "{}{BOLD}     ╚═╝   ╚═╝  ╚═╝{}{BOLD}╚═╝     ╚═╝╚═════╝ {RESET}\n",
                cyan, coral
            );
            println!(
                "  {BOLD}tkmd{RESET} {DIM}v{}{RESET} • {BOLD}{BLUE}Deterministic Program Management Engine & Multi-Agent MCP Server{RESET}",
                env!("CARGO_PKG_VERSION")
            );
            println!(
                "  {DIM}Zero daemons • 100% local • Mathematical CPM & DSM • Universal Stdio MCP{RESET}"
            );
            println!(
                "  {DIM}Dual Licensed:{RESET} {GREEN}MIT OR Apache-2.0{RESET} • Copyright (c) 2026 Ken Harrison\n"
            );
        }
        Commands::Completions { .. } => unreachable!(),
    }

    Ok(())
}

fn render_dependency_tree(
    store: &TaskStore,
    target_id: Option<&str>,
    project: Option<&str>,
) -> anyhow::Result<()> {
    let all_cards = store.list_all()?;
    println!("\n{BOLD}🌲 TASK DEPENDENCY DAG{RESET}");
    println!("{DIM}────────────────────────────────────────────{RESET}");
    let mut has_deps = false;
    for c in &all_cards {
        if let Some(p) = project
            && c.meta.project.as_deref() != Some(p)
        {
            continue;
        }
        if let Some(tid) = target_id {
            let clean_tid = tid.trim_start_matches("tk-").trim_start_matches('0');
            let clean_cid = c.meta.id.trim_start_matches("tk-").trim_start_matches('0');
            if clean_tid != clean_cid && tid != c.meta.id {
                continue;
            }
        }
        if !c.meta.blocked_by.is_empty() {
            has_deps = true;
            let status_badge = color_status(&c.meta.status);
            println!(
                "  {BOLD}{}{RESET} ({}) -> blocked by: {RED}{}{RESET}",
                c.meta.id,
                status_badge.trim(),
                c.meta.blocked_by.join(", ")
            );
            for bid in &c.meta.blocked_by {
                if let Ok(b_card) = store.find_task(bid) {
                    println!(
                        "    └── {DIM}{}{RESET} [{}] ({})",
                        b_card.meta.id,
                        b_card.meta.title,
                        color_status(&b_card.meta.status).trim()
                    );
                }
            }
        }
    }
    if !has_deps {
        println!("  {DIM}No dependency constraints recorded yet.{RESET}");
        println!("  {DIM}Add one with: tk dep <id> --needs <blocker_id>{RESET}");
    }
    println!("{DIM}────────────────────────────────────────────{RESET}\n");
    Ok(())
}

/// Resolve `tk done` targets: the given ids, or every `pending-close` task with `--pending`.
/// With `--pending`, a global card whose id also exists locally is skipped, because `find_task`
/// would resolve that id to the local card.
fn done_targets(store: &TaskStore, ids: &[String], pending: bool) -> Result<Vec<TaskCard>> {
    let mut cards: Vec<TaskCard> = Vec::new();
    for id in ids {
        let card = store.find_task(id)?;
        // `1`, `tk-1` and `tk-001` are the same task: process it once.
        if !cards.iter().any(|c| c.path == card.path) {
            cards.push(card);
        }
    }
    if pending {
        let local = store.list_store_cards()?;
        let local_ids: Vec<String> = local.iter().map(|c| c.meta.id.to_lowercase()).collect();
        let mut candidates = local;
        if matches!(
            store.active_scope,
            ScopeSelection::Auto | ScopeSelection::All
        ) && let Some(peer) = store.peer.as_ref()
        {
            for c in peer.list_store_cards()? {
                if !local_ids.contains(&c.meta.id.to_lowercase()) {
                    candidates.push(c);
                } else if c.meta.status == TaskStatus::PendingClose {
                    eprintln!(
                        "warning: skipped global {} ({}): a local task has the same id. Close it with `tk d -G {}`",
                        c.meta.id, c.meta.title, c.meta.id
                    );
                }
            }
        }
        for c in candidates {
            if c.meta.status == TaskStatus::PendingClose && !cards.iter().any(|x| x.path == c.path)
            {
                cards.push(c);
            }
        }
    }
    Ok(cards)
}

/// Decide how `tk done` may approve. A person must be at an interactive terminal (stdin and
/// stderr are TTYs), or pass `-y` / `--yes` when `[policy] allow_non_tty_yes = true` is enabled
/// and no AI agent session environment variables are set; agents, pipes and scripts otherwise get
/// `Untrusted`, which can only submit for review.
/// Several tasks are summarised together and confirmed with one answer (or skipped with `-y`).
/// Limitation: an agent that allocates its own pseudo-terminal passes the TTY check. This is a
/// workflow harness, not authentication.
fn cli_approval(store: &TaskStore, cards: &[TaskCard], yes: bool) -> Result<Approval> {
    use std::io::Write;
    if !is_interactive() && !non_tty_yes_allowed(store, yes) {
        return Ok(Approval::Untrusted);
    }
    // Only cards that would actually close need sign-off; agent-owned work is routed to review.
    let closing: Vec<&TaskCard> = cards
        .iter()
        .filter(|c| {
            c.meta.status == TaskStatus::PendingClose
                || c.meta
                    .owner
                    .eq_ignore_ascii_case(&store.config.default_owner)
        })
        .collect();
    if closing.is_empty() {
        return Ok(Approval::Interactive);
    }

    let mut err = std::io::stderr();
    writeln!(err, "{BOLD}Sign-off ({} task(s)):{RESET}", closing.len())?;
    for card in &closing {
        writeln!(
            err,
            "  {BOLD}{}{RESET} {} (@{}, {})",
            card.meta.id,
            card.meta.title,
            card.meta.owner,
            card.meta.status.as_str()
        )?;
        if let Some(r) = &card.meta.receipt {
            writeln!(
                err,
                "      submitted by {} at {}; eval: {}; snapshot: {}",
                r.submitted_by,
                r.submitted_at,
                r.eval.as_deref().unwrap_or("none"),
                r.snapshot
                    .as_deref()
                    .map(|s| &s[..s.len().min(12)])
                    .unwrap_or("not checked (not a git repo)")
            )?;
            if r.eval_result == "no-eval" {
                writeln!(err, "      {ORANGE}warning:{RESET} no automated check ran")?;
            }
        }
        let change = card
            .meta
            .receipt
            .as_ref()
            .and_then(|r| r.check_changed.clone())
            .or_else(|| {
                (card.meta.status != TaskStatus::PendingClose)
                    .then(|| store.check_change(card))
                    .flatten()
            });
        if let Some(change) = change {
            writeln!(
                err,
                "      {RED}check changed since claim:{RESET} {} (approve only with --accept-changed-eval)",
                change
            )?;
        }
    }
    if !yes {
        let target = if closing.len() == 1 {
            closing[0].meta.id.clone()
        } else {
            format!("all {}", closing.len())
        };
        if !confirm(&format!("Approve and close {}?", target))? {
            anyhow::bail!("Not approved: nothing closed");
        }
    }
    Ok(Approval::Interactive)
}

/// True when the user explicitly passed `-y` / `--yes`, `[policy] allow_non_tty_yes = true` is
/// configured (locally or globally), and no AI agent session environment variable is present.
fn non_tty_yes_allowed(store: &TaskStore, yes: bool) -> bool {
    if !yes || TaskStore::detect_env_session_id().is_some() {
        return false;
    }
    store.config.policy.allow_non_tty_yes
        || store
            .peer
            .as_ref()
            .is_some_and(|p| p.config.policy.allow_non_tty_yes)
}

/// A person is at the keyboard: stdin and stderr are both terminals.
fn is_interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Ask a yes/no question on stderr; only `y`/`yes` counts as yes.
fn confirm(question: &str) -> Result<bool> {
    use std::io::{BufRead, Write};
    let mut err = std::io::stderr();
    write!(err, "{} [y/N] ", question)?;
    err.flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// Message for `tk done`. Says `Completed` only when the card is actually done.
/// Agent completions stop at `pending-close` and wait for human sign-off.
fn done_message(status: &TaskStatus, id: &str, title: &str) -> String {
    if *status == TaskStatus::Done {
        format!(
            "{GREEN}✅ Completed{RESET} {BOLD}{}{RESET} ({}) and logged to today's Daily Note!",
            id, title
        )
    } else {
        format!(
            "{PURPLE}🔍 Submitted{RESET} {BOLD}{}{RESET} to `pending-close`: {}. Human sign-off is next.",
            id, title
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_done_message_pending_close() {
        let msg = done_message(&TaskStatus::PendingClose, "tk-003", "Fix bug");
        assert!(msg.contains("Submitted"));
        assert!(msg.contains("pending-close"));
        assert!(msg.contains("Human sign-off"));
        assert!(!msg.contains("Completed"));
    }

    #[test]
    fn test_done_message_done() {
        let msg = done_message(&TaskStatus::Done, "tk-003", "Fix bug");
        assert!(msg.contains("Completed"));
        assert!(!msg.contains("Submitted"));
    }

    #[test]
    fn test_cli_parse_scope_flags() {
        let cli_local = Cli::try_parse_from(["tk", "-L"]).unwrap();
        assert_eq!(cli_local.scope(), ScopeSelection::Local);

        let cli_global = Cli::try_parse_from(["tk", "-G"]).unwrap();
        assert_eq!(cli_global.scope(), ScopeSelection::Global);

        let cli_all = Cli::try_parse_from(["tk", "-A"]).unwrap();
        assert_eq!(cli_all.scope(), ScopeSelection::All);

        let cli_auto = Cli::try_parse_from(["tk"]).unwrap();
        assert_eq!(cli_auto.scope(), ScopeSelection::Auto);
    }

    #[test]
    fn test_cli_parse_review_command() {
        // `tk r` (dashboard view, no id)
        let cli_dash = Cli::try_parse_from(["tk", "r"]).unwrap();
        match cli_dash.command {
            Some(Commands::Review { ids, .. }) => assert!(ids.is_empty()),
            other => panic!("Expected Review, got {:?}", other),
        }

        // `tk r 1 -n "Ready for review"`
        let cli_sub = Cli::try_parse_from(["tk", "r", "1", "-n", "Ready for review"]).unwrap();
        match cli_sub.command {
            Some(Commands::Review { ids, note, .. }) => {
                assert_eq!(ids, vec!["1"]);
                assert_eq!(note.as_deref(), Some("Ready for review"));
            }
            other => panic!("Expected Review with id, got {:?}", other),
        }
    }

    #[test]
    fn test_cli_parse_tree_and_dep_commands() {
        // `tk tree`
        let cli_tree = Cli::try_parse_from(["tk", "tree"]).unwrap();
        match cli_tree.command {
            Some(Commands::Tree { id, project }) => {
                assert!(id.is_none());
                assert!(project.is_none());
            }
            other => panic!("Expected Tree, got {:?}", other),
        }

        // `tk dep 2 --needs 1`
        let cli_dep = Cli::try_parse_from(["tk", "dep", "2", "--needs", "1"]).unwrap();
        match cli_dep.command {
            Some(Commands::Dep { id, needs, .. }) => {
                assert_eq!(id.as_deref(), Some("2"));
                assert_eq!(needs, vec!["1"]);
            }
            other => panic!("Expected Dep, got {:?}", other),
        }
    }

    #[test]
    fn test_cli_parse_claim_and_resume() {
        let cli_claim =
            Cli::try_parse_from(["tk", "c", "1", "--owner", "claude", "-s", "claude -r 999"])
                .unwrap();
        match cli_claim.command {
            Some(Commands::Claim {
                ids,
                owner,
                session,
                force,
            }) => {
                assert_eq!(ids, vec!["1"]);
                assert_eq!(owner.as_deref(), Some("claude"));
                assert_eq!(session.as_deref(), Some("claude -r 999"));
                assert!(!force);
            }
            other => panic!("Expected Claim, got {:?}", other),
        }

        let cli_res = Cli::try_parse_from(["tk", "u", "1"]).unwrap();
        match cli_res.command {
            Some(Commands::Resume { id, .. }) => assert_eq!(id, "1"),
            other => panic!("Expected Resume, got {:?}", other),
        }
    }

    /// RED (Sourcery #22): the same task given twice (or by two aliases) was processed twice.
    #[test]
    fn test_done_targets_dedupes_ids() {
        let tmp = std::env::temp_dir().join(format!("tkmd-lib-dedupe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("tasks")).unwrap();
        let store = TaskStore {
            tasks_path: tmp.join("tasks"),
            daily_path: tmp.clone(),
            config: store::AppConfig {
                mode: "cli".into(),
                vault_path: tmp.to_string_lossy().to_string(),
                tasks_dir: "tasks".into(),
                base_file: String::new(),
                daily_dir: String::new(),
                default_owner: "ken".into(),
                obsidian_vault_name: None,
                editor: None,
                policy: store::Policy::default(),
            },
            scope: CardScope::Local,
            peer: None,
            active_scope: ScopeSelection::Local,
            uninitialized_git_repo: None,
            is_uninitialized_local: false,
        };
        store.add_task("p1 once", None, vec![], None, None).unwrap();
        let ids: Vec<String> = ["1", "tk-001", "tk-1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(done_targets(&store, &ids, false).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_cli_parse_accept_changed_eval() {
        match Cli::try_parse_from(["tk", "d", "1", "--accept-changed-eval"])
            .unwrap()
            .command
        {
            Some(Commands::Done {
                accept_changed_eval,
                ..
            }) => assert!(accept_changed_eval),
            other => panic!("Expected Done, got {:?}", other),
        }
    }

    #[test]
    fn test_cli_parse_auto() {
        match Cli::try_parse_from(["tk", "auto", "1", "2"])
            .unwrap()
            .command
        {
            Some(Commands::Auto { ids, off, yes }) => {
                assert_eq!(ids, vec!["1", "2"]);
                assert!(!off);
                assert!(!yes);
            }
            other => panic!("Expected Auto, got {:?}", other),
        }
        assert!(Cli::try_parse_from(["tk", "auto"]).is_err());
    }

    #[test]
    fn test_cli_parse_done_and_auto_yes_flag() {
        match Cli::try_parse_from(["tk", "d", "1", "-y"]).unwrap().command {
            Some(Commands::Done { ids, yes, .. }) => {
                assert_eq!(ids, vec!["1"]);
                assert!(yes);
            }
            other => panic!("Expected Done, got {:?}", other),
        }
        match Cli::try_parse_from(["tk", "auto", "1", "-y"])
            .unwrap()
            .command
        {
            Some(Commands::Auto { ids, yes, .. }) => {
                assert_eq!(ids, vec!["1"]);
                assert!(yes);
            }
            other => panic!("Expected Auto, got {:?}", other),
        }
    }

    #[test]
    fn test_cli_parse_bulk_done() {
        match Cli::try_parse_from(["tk", "d", "1", "2", "tk-003"])
            .unwrap()
            .command
        {
            Some(Commands::Done { ids, pending, .. }) => {
                assert_eq!(ids, vec!["1", "2", "tk-003"]);
                assert!(!pending);
            }
            other => panic!("Expected Done, got {:?}", other),
        }
        match Cli::try_parse_from(["tk", "d", "--pending"])
            .unwrap()
            .command
        {
            Some(Commands::Done { ids, pending, .. }) => {
                assert!(ids.is_empty());
                assert!(pending);
            }
            other => panic!("Expected Done, got {:?}", other),
        }
        assert!(Cli::try_parse_from(["tk", "d"]).is_err());
    }

    #[test]
    fn test_format_title_cell() {
        let cell = format_title_cell("Fix login screen", Some("[[auth]]"), 40);
        assert!(cell.contains("Fix login screen"));
        assert!(cell.contains("[[auth]]"));
    }

    #[test]
    fn test_cli_parse_completions_command() {
        let cli_comp = Cli::try_parse_from(["tkmd", "completions", "zsh"]).unwrap();
        match cli_comp.command {
            Some(Commands::Completions { shell, bin }) => {
                assert_eq!(shell, clap_complete::Shell::Zsh);
                assert_eq!(bin, "tkmd");
            }
            other => panic!("Expected Completions, got {:?}", other),
        }
    }
}
