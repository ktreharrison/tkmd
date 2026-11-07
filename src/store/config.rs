use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

fn default_mode() -> String {
    "obsidian".to_string()
}

fn default_base_file() -> String {
    "bases/tasks.base".to_string()
}

/// Auto-detect an Obsidian vault by checking for a `.obsidian` directory in:
/// 1. Current working directory (or any parent directory)
/// 2. `~/Documents`
/// 3. `~/Obsidian` or `~/Notes`
pub fn detect_obsidian_vault() -> Option<PathBuf> {
    if let Ok(mut cwd) = std::env::current_dir() {
        loop {
            if cwd.join(".obsidian").is_dir() {
                return Some(cwd);
            }
            if !cwd.pop() {
                break;
            }
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let home_path = PathBuf::from(home);
        for candidate in ["Documents", "Obsidian", "Notes", "vault"] {
            let p = home_path.join(candidate);
            if p.join(".obsidian").is_dir() {
                return Some(p);
            }
        }
    }
    None
}

/// Auto-detect a local project store by checking for `.tkmd.toml`, `tkmd.toml`, `.tkmd/config.toml`,
/// or legacy `.taskmd.toml`, `taskmd.toml`, `.taskmd/config.toml`,
/// or a `.git` root containing a `tasks/` directory.
/// Skips if the detected directory is the global Obsidian vault path.
pub fn detect_local_store(cwd: &Path, global_vault_path: &Path) -> Option<PathBuf> {
    let mut cur = cwd.to_path_buf();
    let global_canon = global_vault_path.canonicalize().ok();

    loop {
        if let Some(ref g) = global_canon {
            if let Ok(c) = cur.canonicalize()
                && c == *g
            {
                return None;
            }
        } else if cur == global_vault_path {
            return None;
        }

        if cur.join(".tkmd.toml").is_file()
            || cur.join("tkmd.toml").is_file()
            || cur.join(".tkmd/config.toml").is_file()
            || cur.join(".taskmd.toml").is_file()
            || cur.join("taskmd.toml").is_file()
            || cur.join(".taskmd/config.toml").is_file()
        {
            return Some(cur);
        }

        if cur.join(".git").exists() && cur.join("tasks").is_dir() {
            return Some(cur);
        }

        if !cur.pop() {
            break;
        }
    }
    None
}

/// Auto-detect the root directory of a git repository by checking for `.git` upwards.
pub fn detect_git_root(cwd: &Path) -> Option<PathBuf> {
    let mut cur = cwd.to_path_buf();
    loop {
        if cur.join(".git").exists() {
            return Some(cur);
        }
        if !cur.pop() {
            break;
        }
    }
    None
}

/// Scope selection for command execution:
/// - `Auto`: default behavior (detects local project store if in a repo, otherwise global vault)
/// - `Local` (`-L`, `--local`): forces local repository project store
/// - `Global` (`-G`, `--global`): forces central global vault store
/// - `All` (`-A`, `--all-scopes`): aggregates cards across both local and global stores
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeSelection {
    Auto,
    Local,
    Global,
    All,
}

impl ScopeSelection {
    pub fn from_flags(local: bool, global: bool, all: bool) -> Self {
        if all {
            Self::All
        } else if local {
            Self::Local
        } else if global {
            Self::Global
        } else {
            Self::Auto
        }
    }
}

/// Check whether a CLI binary exists on `$PATH`.
fn command_exists(bin: &str) -> bool {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if dir.join(bin).is_file() {
                return true;
            }
        }
    }
    false
}

/// Auto-detect all installed editors on `$PATH` (Helix, Neovim, Zed, VS Code, Cursor, Vim, Micro, Nano).
pub fn detect_installed_editors() -> Vec<String> {
    let candidates = [
        "hx", "nvim", "zed", "code", "cursor", "vim", "micro", "nano",
    ];
    candidates
        .iter()
        .filter(|b| command_exists(b))
        .map(|s| s.to_string())
        .collect()
}

/// Auto-detect the user's preferred editor from `$VISUAL`, `$EDITOR`, or installed binaries.
pub fn detect_editor() -> String {
    for env_key in ["VISUAL", "EDITOR"] {
        if let Ok(val) = std::env::var(env_key) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    detect_installed_editors()
        .into_iter()
        .next()
        .unwrap_or_else(|| "vim".to_string())
}

/// Configuration loaded from `~/.config/tkmd/config.toml` (or legacy `~/.config/taskmd/config.toml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// "obsidian" (Recommended: Kanban Base + Daily Note + CLI + MCP) or "cli" (Standalone CLI + MCP)
    #[serde(default = "default_mode")]
    pub mode: String,
    pub vault_path: String,
    pub tasks_dir: String,
    #[serde(default = "default_base_file")]
    pub base_file: String,
    #[serde(default)]
    pub daily_dir: String,
    pub default_owner: String,
    #[serde(default)]
    pub obsidian_vault_name: Option<String>,
    /// Preferred editor for `tk e` and CLI-mode `tk o` (e.g., "hx", "nvim", "zed", "code", "vim")
    #[serde(default)]
    pub editor: Option<String>,
    /// Lifecycle guardrails (`[policy]` table). Must stay the last field: TOML tables follow values.
    #[serde(default)]
    pub policy: Policy,
}

/// `[policy]` guardrails for the submit -> review -> accept lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    /// Refuse `pending-close` for tasks with no `eval` command (instead of auto-passing as `no-eval`).
    #[serde(default)]
    pub require_eval: bool,
    /// Kill an eval (and everything it started) after this many seconds; `0` means no limit.
    /// Evals run under the card lock, so a hung eval would otherwise block that card forever.
    #[serde(default = "default_eval_timeout")]
    pub eval_timeout_secs: u64,
    /// Allow `tk d -y` and `tk auto -y` from non-TTY human runners (Raycast, Obsidian shell plugins)
    /// when no AI agent session environment variables are present.
    #[serde(default)]
    pub allow_non_tty_yes: bool,
    /// Leave new tasks unassigned unless `@owner` is given; the first `tk c` assigns them.
    /// Off by default: new tasks belong to the configured owner.
    #[serde(default)]
    pub assign_on_claim: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            require_eval: false,
            eval_timeout_secs: default_eval_timeout(),
            allow_non_tty_yes: false,
            assign_on_claim: false,
        }
    }
}

fn default_eval_timeout() -> u64 {
    600
}

/// How a caller asks to close a task. Only the CLI, after checking for a human at a
/// terminal (or `-y` under `allow_non_tty_yes`), constructs `Interactive`; MCP and scripts
/// always pass `Untrusted`.
/// This is a workflow harness, not authentication: an agent that fakes a terminal passes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    /// Agents, MCP and any non-interactive caller: can submit for review, can never mark done.
    Untrusted,
    /// A human confirmed at an interactive terminal (or via explicit `-y` opt-in).
    Interactive,
}

impl Default for AppConfig {
    fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let user = std::env::var("USER").unwrap_or_else(|_| "owner".to_string());
        let editor = Some(detect_editor());

        if let Some(vault) = detect_obsidian_vault() {
            let vault_name = vault
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string());
            let base_file =
                if vault.join("bases").is_dir() || vault.join("bases/tasks.base").exists() {
                    "bases/tasks.base".to_string()
                } else {
                    "tasks.base".to_string()
                };
            Self {
                mode: "obsidian".to_string(),
                vault_path: vault.to_string_lossy().to_string(),
                tasks_dir: "tasks".to_string(),
                base_file,
                daily_dir: "journals/daily".to_string(),
                default_owner: user,
                obsidian_vault_name: vault_name,
                editor,
                policy: Policy::default(),
            }
        } else {
            Self {
                mode: "cli".to_string(),
                vault_path: format!("{}/.local/share/tkmd", home),
                tasks_dir: "tasks".to_string(),
                base_file: String::new(),
                daily_dir: String::new(),
                default_owner: user,
                obsidian_vault_name: None,
                editor,
                policy: Policy::default(),
            }
        }
    }
}
