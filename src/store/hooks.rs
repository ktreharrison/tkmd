//! `[hooks]`: user-configured commands tkmd runs after lifecycle events.
//!
//! Hooks come only from the user config (`~/.config/tkmd/config.toml`). A board's own
//! `.tkmd.toml` is shared and cloned with the board, so tkmd never runs a hook it sets.

use super::eval::{Stragglers, run_with_timeout_input};
use super::*;
use crate::model::TaskCard;
use serde::Serialize;

/// What happened when tkmd tried to run a hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    /// No hook is configured in the user config.
    NotConfigured,
    /// The hook exited `0`.
    Ran,
    /// The hook could not start or exited non-zero; the text says why.
    Failed(String),
    /// The hook outlived `hook_timeout_secs` and was killed.
    TimedOut(u64),
}

/// JSON written to an `on_done` hook's stdin.
#[derive(Debug, Serialize)]
struct DoneHookPayload<'a> {
    event: &'static str,
    timestamp: String,
    id: &'a str,
    title: &'a str,
    owner: &'a str,
    status: &'static str,
    approval: Option<&'a str>,
    session_id: Option<&'a str>,
    card: &'a str,
    board: &'a str,
}

/// Lines of hook stderr shown when a hook fails.
const HOOK_STDERR_LINES: usize = 5;

impl TaskStore {
    /// The hooks from the user's own config: this store's when it is the global store, else
    /// its global peer's. A local board's `[hooks]` table is never used.
    pub(crate) fn user_hooks(&self) -> Option<&Hooks> {
        match self.scope {
            CardScope::Global => Some(&self.config.hooks),
            CardScope::Local => self.peer.as_deref().map(|p| &p.config.hooks),
        }
    }

    /// The root of the board a card lives on (the global peer for a global card seen from a
    /// local store), as an absolute path with `~/` expanded.
    fn board_root_for(&self, card: &TaskCard) -> PathBuf {
        let root = match (&self.peer, card.scope, self.scope) {
            (Some(peer), CardScope::Global, CardScope::Local) => &peer.config.vault_path,
            _ => &self.config.vault_path,
        };
        absolute(root)
    }

    /// Run the user's `on_done` hook for a card that just became `done`.
    pub(crate) fn run_on_done_hook(&self, card: &TaskCard) -> HookOutcome {
        let Some(hooks) = self.user_hooks() else {
            return HookOutcome::NotConfigured;
        };
        let Some(cmd_str) = hooks
            .on_done
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
        else {
            return HookOutcome::NotConfigured;
        };

        let board_path = self.board_root_for(card);
        let board = board_path.to_string_lossy().into_owned();
        let card_path = absolute(&card.path).to_string_lossy().into_owned();
        let board = board.as_str();
        let card_path = card_path.as_str();
        let payload = DoneHookPayload {
            event: "done",
            timestamp: Local::now().to_rfc3339(),
            id: &card.meta.id,
            title: &card.meta.title,
            owner: &card.meta.owner,
            status: "done",
            approval: card.meta.approval.as_deref(),
            session_id: card.meta.session_id.as_deref(),
            card: card_path,
            board,
        };
        let input = match serde_json::to_vec(&payload) {
            Ok(mut v) => {
                v.push(b'\n');
                v
            }
            Err(e) => return HookOutcome::Failed(format!("could not encode the event: {e}")),
        };

        #[cfg(target_os = "windows")]
        let mut cmd = {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(cmd_str);
            c
        };
        #[cfg(not(target_os = "windows"))]
        let mut cmd = {
            let mut c = Command::new(super::eval::eval_shell());
            c.arg("-c").arg(cmd_str);
            c
        };
        if board_path.is_dir() {
            cmd.current_dir(&board_path);
        }
        cmd.env("TK_EVENT", "done")
            .env("TK_ID", &card.meta.id)
            .env("TK_TITLE", &card.meta.title)
            .env("TK_OWNER", &card.meta.owner)
            .env("TK_STATUS", "done")
            .env("TK_APPROVAL", card.meta.approval.as_deref().unwrap_or(""))
            .env("TK_CARD", card_path)
            .env("TK_BOARD", board);

        let timeout = match hooks.hook_timeout_secs {
            0 => None,
            secs => Some(std::time::Duration::from_secs(secs)),
        };
        match run_with_timeout_input(cmd, timeout, Some(input), Stragglers::Leave) {
            Err(e) => HookOutcome::Failed(format!("could not start: {e}")),
            Ok(None) => HookOutcome::TimedOut(hooks.hook_timeout_secs),
            Ok(Some(out)) if out.status.success() => HookOutcome::Ran,
            Ok(Some(out)) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let lines: Vec<&str> = stderr.lines().collect();
                let tail = lines[lines.len().saturating_sub(HOOK_STDERR_LINES)..].join("\n");
                let code = out
                    .status
                    .code()
                    .map_or_else(|| "a signal".to_string(), |c| format!("exit {c}"));
                if tail.trim().is_empty() {
                    HookOutcome::Failed(code)
                } else {
                    HookOutcome::Failed(format!("{code}: {}", tail.trim()))
                }
            }
        }
    }
}

/// `path` with a leading `~/` expanded and made absolute against the current directory, so a
/// hook that runs in the board's root still gets paths that point at the right files.
pub(crate) fn absolute(path: &str) -> PathBuf {
    let expanded = match (path.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    };
    let joined = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()
            .map(|d| d.join(&expanded))
            .unwrap_or(expanded)
    };
    joined.canonicalize().unwrap_or(joined)
}
