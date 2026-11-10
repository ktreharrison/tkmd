use crate::model::{ReviewReceipt, TaskCard, TaskStatus};
use crate::store::TaskStore;
use crate::store::config::Approval;
use crate::store::eval::has_eval;
use anyhow::{Context, Result, bail};
use chrono::Local;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Most files an eval command may pin (anything beyond is ignored).
const MAX_EVAL_INPUTS: usize = 32;

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    false
}

/// How the check moved from `(old_eval, old_inputs)` to `(new_eval, new_inputs)`, or `None`.
pub(crate) fn describe_check_change(
    old_eval: Option<&str>,
    old_inputs: &std::collections::BTreeMap<String, String>,
    new_eval: Option<&str>,
    new_inputs: &std::collections::BTreeMap<String, String>,
) -> Option<String> {
    let mut parts = Vec::new();
    if old_eval.map(str::trim) != new_eval.map(str::trim) {
        parts.push(format!(
            "eval `{}` -> `{}`",
            old_eval.unwrap_or("none"),
            new_eval.unwrap_or("none")
        ));
    }
    for (path, hash) in old_inputs {
        match new_inputs.get(path) {
            Some(h) if h == hash => {}
            Some(_) => parts.push(format!("{} changed", path)),
            None => parts.push(format!("{} removed", path)),
        }
    }
    for path in new_inputs.keys() {
        if !old_inputs.contains_key(path) {
            parts.push(format!("{} added", path));
        }
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// Run `git -C <dir> <args>`, optionally against a throwaway index. `None` on any failure.
pub(crate) fn git_output(dir: &Path, args: &[&str], index: Option<&Path>) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    if let Some(i) = index {
        cmd.env("GIT_INDEX_FILE", i);
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Deterministic in-process content fingerprint + executable bit (works without `git` on PATH).
fn hash_file_pin(path: &Path) -> Option<(String, Vec<u8>)> {
    let bytes = fs::read(path).ok()?;
    let mut h1: u64 = 0xcbf2_9ce4_8422_2325;
    let mut h2: u64 = 0x8422_2325_cbf2_9ce4;
    for (i, &b) in bytes.iter().enumerate() {
        h1 ^= b as u64;
        h1 = h1.wrapping_mul(0x0100_0000_01b3);
        h2 ^= (b as u64).wrapping_add(i as u64);
        h2 = h2.wrapping_mul(0x0100_0000_01b3).rotate_left(5);
    }
    let mode_suffix = if is_executable(path) { ":x" } else { "" };
    Some((
        format!("{:016x}{:016x}:{}{}", h1, h2, bytes.len(), mode_suffix),
        bytes,
    ))
}

/// Extract candidate file tokens referenced by a shell command or script body (`source <file>`,
/// `. <file>`, or relative path tokens inside the workspace).
fn extract_script_references(text: &str, script_dir: Option<&Path>, root: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        for token in line
            .split(|c: char| c.is_whitespace() || ";&|()<>'\"`=".contains(c))
            .filter(|t| !t.is_empty() && !t.starts_with('-'))
        {
            if let Some(dir) = script_dir {
                for prefix in ["$0", "${0}", "$BASH_SOURCE", "${BASH_SOURCE[0]}"] {
                    if let Some(rest) = token.find(prefix).map(|idx| &token[idx + prefix.len()..]) {
                        let cleaned = rest.trim_start_matches(['}', ')', '/']);
                        if !cleaned.is_empty() {
                            candidates.push(dir.join(cleaned));
                        }
                    }
                }
                candidates.push(dir.join(token));
            }
            candidates.push(root.join(token));
        }
    }
    candidates
}

impl TaskStore {
    /// Content hashes and executable bits of the workspace files an eval command names
    /// (`sh scripts/check.sh` pins `scripts/check.sh`), plus any workspace files those scripts
    /// transitively source or reference. Computed in-process so it never depends on `git` on PATH.
    pub fn eval_inputs(&self, eval: Option<&str>) -> std::collections::BTreeMap<String, String> {
        let mut out = std::collections::BTreeMap::new();
        let Some(eval) = eval.filter(|e| !e.trim().is_empty()) else {
            return out;
        };
        let root = Path::new(&self.config.vault_path);
        let Ok(root_canon) = root.canonicalize() else {
            return out;
        };
        let tasks_canon = self.tasks_path.canonicalize().ok();
        let mut queue: std::collections::VecDeque<PathBuf> =
            extract_script_references(eval, None, root)
                .into_iter()
                .collect();
        let mut visited = std::collections::HashSet::new();

        while let Some(candidate) = queue.pop_front() {
            if out.len() >= MAX_EVAL_INPUTS {
                break;
            }
            let Ok(canon) = candidate.canonicalize() else {
                continue;
            };
            if !canon.is_file()
                || !canon.starts_with(&root_canon)
                || tasks_canon.as_ref().is_some_and(|tp| canon.starts_with(tp))
                || !visited.insert(canon.clone())
            {
                continue;
            }
            let rel = canon
                .strip_prefix(&root_canon)
                .map(|r| r.to_string_lossy().to_string())
                .unwrap_or_else(|_| canon.to_string_lossy().to_string());
            let Some((pin, bytes)) = hash_file_pin(&canon) else {
                continue;
            };
            out.insert(rel, pin);
            // If this pinned file is a text script (<256 KB), scan it for sourced/referenced files.
            if bytes.len() <= 256 * 1024
                && let Ok(script_text) = std::str::from_utf8(&bytes)
            {
                let script_dir = canon.parent();
                for next in extract_script_references(script_text, script_dir, &root_canon) {
                    queue.push_back(next);
                }
            }
        }
        out
    }

    /// How the task's check changed since it was pinned at claim (or `tk auto`), if it did.
    pub fn check_change(&self, card: &TaskCard) -> Option<String> {
        card.meta.check_pinned_at.as_ref()?;
        describe_check_change(
            card.meta.claimed_eval.as_deref(),
            &card.meta.claimed_eval_inputs,
            card.meta.eval.as_deref(),
            &self.eval_inputs(card.meta.eval.as_deref()),
        )
    }

    /// Git tree hash of the working tree that evals run in, excluding the task cards themselves
    /// (they change on every transition). Built with a throwaway index, so the user's index and
    /// refs are untouched. `None` outside a git repo, or when the cards are the repo root.
    pub fn worktree_snapshot(&self) -> Option<String> {
        self.snapshot_with_note().0
    }

    /// The snapshot, or the reason there is none (stored on the receipt so it never looks
    /// stronger than its evidence).
    pub fn snapshot_with_note(&self) -> (Option<String>, Option<String>) {
        let dir = Path::new(&self.config.vault_path);
        if Command::new("git").arg("--version").output().is_err() {
            return (None, Some("git unavailable".to_string()));
        }
        let Some(top) = git_output(dir, &["rev-parse", "--show-toplevel"], None).map(PathBuf::from)
        else {
            return (None, Some("not a git repository".to_string()));
        };
        let Ok(top_canon) = top.canonicalize() else {
            return (None, Some("repository path unreadable".to_string()));
        };
        let mut pathspecs = vec![".".to_string()];
        if let Ok(tasks_canon) = self.tasks_path.canonicalize()
            && let Ok(rel) = tasks_canon.strip_prefix(&top_canon)
        {
            if rel.as_os_str().is_empty() {
                return (None, Some("task cards are the repository root".to_string()));
            }
            pathspecs.push(format!(":(exclude){}", rel.display()));
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let index =
            std::env::temp_dir().join(format!("tkmd-index-{}-{}", std::process::id(), nanos));
        let mut add_args = vec!["add", "-A", "--"];
        add_args.extend(pathspecs.iter().map(String::as_str));
        let tree = git_output(&top, &add_args, Some(index.as_path()))
            .and_then(|_| git_output(&top, &["write-tree"], Some(index.as_path())));
        let _ = fs::remove_file(&index);
        match tree.filter(|t| !t.is_empty()) {
            Some(t) => (Some(t), None),
            None => (None, Some("git snapshot failed".to_string())),
        }
    }

    /// Submits a task to `pending-close`. Runs `eval` under the card lock; a failing eval blocks
    /// the transition. Records a `ReviewReceipt` binding the pass to the exact output snapshot.
    pub fn submit_for_review(
        &self,
        query: &str,
        actor: &str,
        note: Option<&str>,
        session_id: Option<String>,
    ) -> Result<TaskCard> {
        let found = self.find_task(query)?;
        if let Some(peer) = self.owning_peer(&found) {
            return peer.submit_for_review(&found.meta.id, actor, note, session_id);
        }
        let _lock = Self::lock_card(Path::new(&found.path))?;
        let mut card = Self::read_card(Path::new(&found.path))?;
        card.scope = found.scope;

        if matches!(card.meta.status, TaskStatus::Done | TaskStatus::Archived) {
            bail!(
                "Cannot submit {}: it is already `{}`",
                card.meta.id,
                card.meta.status.as_str()
            );
        }
        let open = self.open_blockers(&card)?;
        if !open.is_empty() {
            bail!(
                "Cannot submit {}: still blocked by open task(s): {}",
                card.meta.id,
                open.join(", ")
            );
        }
        let submitter = actor.trim().trim_start_matches('@').to_lowercase();
        if card.meta.status == TaskStatus::InProgress
            && !card.meta.owner.eq_ignore_ascii_case(&submitter)
        {
            bail!(
                "Cannot submit {}: `{}` claimed it. Only the owner submits; take it over with `tk c {} --force`",
                card.meta.id,
                card.meta.owner,
                card.meta.id
            );
        }
        if self.config.policy.require_eval && !has_eval(&card) {
            bail!(
                "Cannot submit {}: [policy].require_eval is on and the task has no eval command. Add one with `tk set {} --eval '<cmd>'`",
                card.meta.id,
                card.meta.id
            );
        }

        let prev = card.meta.status.as_str().to_string();
        // Snapshot before and after the eval: the receipt must describe what the eval tested.
        let (before, _) = self.snapshot_with_note();
        let (passed, eval_out) = self.eval_card(&mut card, actor)?;
        if !passed {
            self.write_card(&card)?;
            self.log_event(
                &card.meta.id,
                "eval",
                actor,
                card.meta.session_id.as_deref(),
                Some(&prev),
                &prev,
                Some(false),
                &card.meta.title,
            )?;
            bail!(
                "Eval failed for {}! Fix before moving to pending-close. Output: {}",
                card.meta.id,
                eval_out
            );
        }

        let now_ts = Local::now().format("%Y-%m-%d %H:%M").to_string();
        let resolved_sid = session_id
            .filter(|s| !s.trim().is_empty())
            .or_else(Self::detect_env_session_id);
        Self::record_session_on_card(&mut card, actor, resolved_sid.as_deref());
        let (snapshot, snapshot_note) = self.snapshot_with_note();
        if before.is_some() && before != snapshot {
            self.write_card(&card)?;
            bail!(
                "Cannot submit {}: the output changed while the eval ran (the eval or another process edited files). Re-run `tk r {}` once nothing is changing",
                card.meta.id,
                card.meta.id
            );
        }
        if let Some(note) = snapshot_note.as_deref()
            && note != "not a git repository"
        {
            eprintln!(
                "warning: {}: review receipt has no output snapshot ({})",
                card.meta.id, note
            );
        }
        let eval_inputs = self.eval_inputs(card.meta.eval.as_deref());
        let check_changed = card.meta.check_pinned_at.as_ref().and_then(|_| {
            describe_check_change(
                card.meta.claimed_eval.as_deref(),
                &card.meta.claimed_eval_inputs,
                card.meta.eval.as_deref(),
                &eval_inputs,
            )
        });
        let approved_inputs_changed =
            card.meta.approved_eval.is_some() && card.meta.approved_eval_inputs != eval_inputs;
        card.meta.receipt = Some(ReviewReceipt {
            snapshot,
            snapshot_note,
            eval: card.meta.eval.clone(),
            eval_inputs,
            check_changed,
            eval_result: if has_eval(&card) { "passed" } else { "no-eval" }.to_string(),
            submitted_by: actor.to_string(),
            submitted_at: Local::now().to_rfc3339(),
        });
        let approved_matches =
            card.meta.approved_eval.is_some() && card.meta.approved_eval == card.meta.eval;
        let auto_accept = has_eval(&card) && approved_matches && !approved_inputs_changed;
        if approved_matches && approved_inputs_changed {
            card.body = format!(
                "{}\n- `{}`: Auto-accept skipped: a file the approved eval runs changed since `tk auto`. Waiting for manual sign-off.",
                card.body.trim_end(),
                Local::now().format("%Y-%m-%d %H:%M")
            );
        }
        card.meta.status = TaskStatus::PendingClose;
        card.meta.updated = Local::now().format("%Y-%m-%d").to_string();
        let suffix = match note {
            Some(n) if !n.trim().is_empty() => format!(" — {}", n.trim()),
            _ => String::new(),
        };
        let session_note = card
            .meta
            .resume_command()
            .map(|cmd| format!(" (resume: `{}`)", cmd))
            .unwrap_or_default();
        card.body = format!(
            "{}\n- `{}`: Submitted for `pending-close` review by `{}` 🔍{}{}.",
            card.body.trim_end(),
            now_ts,
            actor,
            session_note,
            suffix
        );

        Self::sync_card_sessions(&mut card);

        if auto_accept {
            // The human approved this exact check in advance; its pass is the sign-off.
            let today = Local::now().format("%Y-%m-%d").to_string();
            card.meta.status = TaskStatus::Done;
            card.meta.approval = Some("auto:eval".to_string());
            card.body = card.body.replace("- [ ] ", "- [x] ");
            card.body = format!(
                "{}\n- `{}`: Auto-accepted (`done`): eval `{}` approved by `{}` passed ✅.",
                card.body.trim_end(),
                now_ts,
                card.meta.eval.as_deref().unwrap_or(""),
                self.config.default_owner
            );
            self.write_card(&card)?;
            // The card is done from here on: run its follow-ups even if a later step fails.
            let logged = self.log_event(
                &card.meta.id,
                "auto-accepted",
                actor,
                card.meta.session_id.as_deref(),
                Some(&prev),
                "done",
                Some(true),
                &card.meta.title,
            );
            drop(_lock);
            let unblocked = self.cascade_unblock(&card.meta.id, &today, &now_ts);
            self.note_completion(&card, note);
            logged?;
            unblocked?;
            return Ok(card);
        }

        self.write_card(&card)?;
        self.log_event(
            &card.meta.id,
            "pending-close",
            actor,
            card.meta.session_id.as_deref(),
            Some(&prev),
            "pending-close",
            Some(true),
            &card.meta.title,
        )?;
        Ok(card)
    }

    /// Approve a task's current eval for auto-acceptance: when the owner submits and this exact
    /// check passes, the task closes without a manual sign-off. Human-only (`Interactive`).
    pub fn approve_eval(&self, query: &str, approval: Approval) -> Result<TaskCard> {
        if approval == Approval::Untrusted {
            bail!(
                "Only a person at an interactive terminal can approve an eval for auto-accept (`tk auto`)"
            );
        }
        let found = self.find_task(query)?;
        let _lock = Self::lock_card(Path::new(&found.path))?;
        let mut card = Self::read_card(Path::new(&found.path))?;
        card.scope = found.scope;
        if matches!(card.meta.status, TaskStatus::Done | TaskStatus::Archived) {
            bail!(
                "{} is already `{}`",
                card.meta.id,
                card.meta.status.as_str()
            );
        }
        let Some(eval) = card.meta.eval.clone().filter(|e| !e.trim().is_empty()) else {
            bail!(
                "{} has no eval to approve. Add one first: `tk set {} --eval '<cmd>'`",
                card.meta.id,
                card.meta.id
            );
        };
        card.meta.approved_eval = Some(eval.clone());
        // Approving the check is also the human blessing it as the task's contract.
        let inputs = self.eval_inputs(Some(&eval));
        card.meta.approved_eval_inputs = inputs.clone();
        card.meta.claimed_eval = Some(eval.clone());
        card.meta.claimed_eval_inputs = inputs;
        card.meta.check_pinned_at = Some(Local::now().to_rfc3339());
        card.meta.updated = Local::now().format("%Y-%m-%d").to_string();
        card.body = format!(
            "{}\n- `{}`: Auto-accept on: closes when eval `{}` passes (approved by `{}`).",
            card.body.trim_end(),
            Local::now().format("%Y-%m-%d %H:%M"),
            eval,
            self.config.default_owner
        );
        self.write_card(&card)?;
        let st = card.meta.status.as_str().to_string();
        self.log_event(
            &card.meta.id,
            "eval-approved",
            &self.config.default_owner,
            None,
            Some(&st),
            &st,
            None,
            &card.meta.title,
        )?;
        Ok(card)
    }

    /// Turn auto-accept off: the task goes back to manual sign-off. Anyone may revoke.
    pub fn revoke_eval_approval(&self, query: &str) -> Result<TaskCard> {
        let found = self.find_task(query)?;
        let _lock = Self::lock_card(Path::new(&found.path))?;
        let mut card = Self::read_card(Path::new(&found.path))?;
        card.scope = found.scope;
        if card.meta.approved_eval.take().is_some() {
            card.body = format!(
                "{}\n- `{}`: Auto-accept off: manual sign-off required.",
                card.body.trim_end(),
                Local::now().format("%Y-%m-%d %H:%M")
            );
            self.write_card(&card)?;
            let st = card.meta.status.as_str().to_string();
            self.log_event(
                &card.meta.id,
                "eval-approval-revoked",
                "system",
                None,
                Some(&st),
                &st,
                None,
                &card.meta.title,
            )?;
        }
        Ok(card)
    }

    /// Completes a task as an untrusted caller (agents, MCP, scripts).
    /// Built-in guardrail: this can only ever submit for review, never mark `done`.
    pub fn complete_task(&self, query: &str, actor: &str, note: Option<&str>) -> Result<TaskCard> {
        self.complete_task_with(query, actor, note, Approval::Untrusted)
    }

    /// Completes a task (`status: done`) when `approval` is a human sign-off.
    /// `Untrusted` callers are redirected to `pending-close`, and refused if the task is already there.
    /// Approving `pending-close` work requires the review receipt to still match the output.
    pub fn complete_task_with(
        &self,
        query: &str,
        actor: &str,
        note: Option<&str>,
        approval: Approval,
    ) -> Result<TaskCard> {
        self.complete_task_confirmed(query, actor, note, approval, None, false)
    }

    /// Like `complete_task_with`, but refuses if the card's review receipt is no longer the one
    /// the human was shown (`expected`), e.g. because an agent re-submitted during the prompt.
    /// Every decision is made on the card as read under its lock.
    pub fn complete_task_confirmed(
        &self,
        query: &str,
        actor: &str,
        note: Option<&str>,
        approval: Approval,
        expected: Option<&ReviewReceipt>,
        accept_changed_eval: bool,
    ) -> Result<TaskCard> {
        let found = self.find_task(query)?;
        if let Some(peer) = self.owning_peer(&found) {
            return peer.complete_task_confirmed(
                &found.meta.id,
                actor,
                note,
                approval,
                expected,
                accept_changed_eval,
            );
        }
        let owner = self.config.default_owner.clone();

        let lock = Self::lock_card(Path::new(&found.path))?;
        let mut card = Self::read_card(Path::new(&found.path))?;
        card.scope = found.scope;
        let prev = card.meta.status.as_str().to_string();

        if approval == Approval::Untrusted {
            drop(lock);
            if card.meta.status == TaskStatus::PendingClose {
                bail!(
                    "{} is awaiting human sign-off. Only `tk done {}` run by a person at an interactive terminal can close it",
                    card.meta.id,
                    card.meta.id
                );
            }
            return self.submit_for_review(query, actor, note, None);
        }

        // Agent-owned work goes through review first, even when a human closes it.
        if !card.meta.owner.eq_ignore_ascii_case(&owner)
            && card.meta.status != TaskStatus::PendingClose
            && !matches!(card.meta.status, TaskStatus::Done | TaskStatus::Archived)
        {
            drop(lock);
            return self.submit_for_review(query, &card.meta.owner, note, None);
        }

        let open = self.open_blockers(&card)?;
        if !open.is_empty() {
            bail!(
                "Cannot close {}: still blocked by open task(s): {}",
                card.meta.id,
                open.join(", ")
            );
        }

        match card.meta.status {
            TaskStatus::Done | TaskStatus::Archived => {
                bail!("{} is already `{}`", card.meta.id, prev);
            }
            TaskStatus::PendingClose => {
                let receipt = card.meta.receipt.clone().with_context(|| {
                    format!(
                        "{} has no review receipt (submitted before receipts existed). Re-submit with `tk review {}`",
                        card.meta.id, card.meta.id
                    )
                })?;
                if let Some(shown) = expected
                    && *shown != receipt
                {
                    bail!(
                        "Cannot approve {}: it changed since you reviewed it (re-submitted at {}). Run `tk d {}` again to see the new receipt",
                        card.meta.id,
                        receipt.submitted_at,
                        card.meta.id
                    );
                }
                if receipt.eval != card.meta.eval {
                    bail!(
                        "Cannot approve {}: the eval command changed after it was submitted. Re-submit with `tk review {}`",
                        card.meta.id,
                        card.meta.id
                    );
                }
                let now_inputs = self.eval_inputs(card.meta.eval.as_deref());
                if let Some(change) = describe_check_change(
                    receipt.eval.as_deref(),
                    &receipt.eval_inputs,
                    card.meta.eval.as_deref(),
                    &now_inputs,
                ) {
                    bail!(
                        "Cannot approve {}: the check changed after it was submitted ({}). Re-submit with `tk review {}`",
                        card.meta.id,
                        change,
                        card.meta.id
                    );
                }
                if let Some(change) = &receipt.check_changed
                    && !accept_changed_eval
                {
                    bail!(
                        "Cannot approve {}: its check changed since it was claimed ({}). If you accept the new check, run `tk d {} --accept-changed-eval`",
                        card.meta.id,
                        change,
                        card.meta.id
                    );
                }
                if receipt.snapshot.is_some() && self.worktree_snapshot() != receipt.snapshot {
                    bail!(
                        "Cannot approve {}: the output changed since the eval passed (receipt {}). Re-submit with `tk review {}`",
                        card.meta.id,
                        receipt.snapshot.as_deref().unwrap_or("none"),
                        card.meta.id
                    );
                }
            }
            _ => {
                if self.config.policy.require_eval && !has_eval(&card) {
                    bail!(
                        "Cannot close {}: [policy].require_eval is on and the task has no eval command",
                        card.meta.id
                    );
                }
                if card.meta.check_pinned_at.is_some()
                    && !accept_changed_eval
                    && let Some(change) = describe_check_change(
                        card.meta.claimed_eval.as_deref(),
                        &card.meta.claimed_eval_inputs,
                        card.meta.eval.as_deref(),
                        &self.eval_inputs(card.meta.eval.as_deref()),
                    )
                {
                    bail!(
                        "Cannot close {}: its check changed since it was claimed ({}). If you accept the new check, run `tk d {} --accept-changed-eval`",
                        card.meta.id,
                        change,
                        card.meta.id
                    );
                }
            }
        }

        // Captured before the eval runs: a direct close has no receipt to carry it.
        let changed_since_claim =
            card.meta.status != TaskStatus::PendingClose && self.check_change(&card).is_some();
        let (passed, eval_out) = self.eval_card(&mut card, &owner)?;
        if !passed {
            self.write_card(&card)?;
            bail!(
                "Cannot mark {} as done — Automated Eval FAILED: {}",
                card.meta.id,
                eval_out
            );
        }

        let today = Local::now().format("%Y-%m-%d").to_string();
        let now_ts = Local::now().format("%Y-%m-%d %H:%M").to_string();
        let check_was_changed = card
            .meta
            .receipt
            .as_ref()
            .is_some_and(|r| r.check_changed.is_some())
            || changed_since_claim;
        let approval_label = if accept_changed_eval && check_was_changed {
            "interactive (accepted changed check)".to_string()
        } else {
            "interactive".to_string()
        };

        card.meta.status = TaskStatus::Done;
        card.meta.updated = today.clone();
        card.meta.approval = Some(approval_label.clone());
        // Check off DoD items in body when completed
        card.body = card.body.replace("- [ ] ", "- [x] ");
        let summary_suffix = match note {
            Some(n) if !n.trim().is_empty() => format!(" — {}", n.trim()),
            _ => String::new(),
        };
        let via = if actor.eq_ignore_ascii_case(&owner) {
            String::new()
        } else {
            format!(" (requested as `{}`)", actor)
        };
        card.body = format!(
            "{}\n- `{}`: Completed (`done`) by `{}`, approval `{}`{} ✅{}.",
            card.body.trim_end(),
            now_ts,
            owner,
            approval_label,
            via,
            summary_suffix
        );

        self.write_card(&card)?;
        // The card is done from here on: run its follow-ups even if a later step fails.
        let logged = self.log_event(
            &card.meta.id,
            "completed",
            &owner,
            card.meta.session_id.as_deref(),
            Some(&prev),
            "done",
            Some(true),
            &card.meta.title,
        );
        drop(lock);

        let unblocked = self.cascade_unblock(&card.meta.id, &today, &now_ts);
        self.note_completion(&card, note);
        logged?;
        unblocked?;
        Ok(card)
    }
}
