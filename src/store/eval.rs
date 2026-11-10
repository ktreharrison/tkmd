use crate::model::TaskCard;
use crate::store::TaskStore;
use anyhow::{Context, Result};
use chrono::Local;
use std::path::Path;
use std::process::Command;

/// Most bytes kept from each of an eval's stdout and stderr; the rest is drained and dropped.
const EVAL_CAPTURE_LIMIT: usize = 1 << 20;
/// After the eval's shell exits, how long background children may keep its output open.
const EVAL_DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// A task has an eval only if its command is non-blank (`eval: ""` is no eval at all).
pub(crate) fn has_eval(card: &TaskCard) -> bool {
    card.meta
        .eval
        .as_deref()
        .is_some_and(|e| !e.trim().is_empty())
}

/// What to do with processes a command leaves running after its shell exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stragglers {
    /// Evals: the deadline covers the whole process group, and without a deadline the group is
    /// killed shortly after the shell exits, so a check cannot leave work running.
    Kill,
    /// Hooks: once the shell exits, stop waiting and leave its background work running
    /// (`curl ... &`). The timeout applies only while the shell itself runs.
    Leave,
}

/// How long to wait for output threads after the command is finished or killed. A process that
/// escaped the group (`setsid`) can hold a pipe open; we stop waiting instead of hanging.
const DRAIN_JOIN_LIMIT: std::time::Duration = std::time::Duration::from_millis(250);

/// Run a command, killing it and every process it started if it outlives `timeout`.
/// The deadline covers the whole process group and output collection, not just the shell:
/// a background child that keeps stdout open cannot stretch the eval past its timeout.
/// Returns `Ok(None)` on timeout. Output is drained on threads and capped per stream.
pub(crate) fn run_with_timeout(
    cmd: Command,
    timeout: Option<std::time::Duration>,
) -> std::io::Result<Option<std::process::Output>> {
    run_with_timeout_input(cmd, timeout, None, Stragglers::Kill)
}

/// [`run_with_timeout`], writing `input` to the command's stdin (stdin is empty when `None`),
/// with a choice of what happens to processes left behind by the shell.
/// The input is written on its own thread, so a command that never reads it cannot block us.
pub(crate) fn run_with_timeout_input(
    mut cmd: Command,
    timeout: Option<std::time::Duration>,
    input: Option<Vec<u8>>,
    stragglers: Stragglers,
) -> std::io::Result<Option<std::process::Output>> {
    use std::io::{Read, Write};
    use std::process::Stdio;
    use std::sync::{Arc, Mutex};
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
        // A broken pipe just means the command did not read its input.
        std::thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        });
    }
    type Sink = Arc<Mutex<Vec<u8>>>;
    // Output goes into shared buffers, so we can read what arrived without joining a thread
    // that may still be blocked on a pipe held open by an escaped process.
    fn drain(pipe: Option<Box<dyn Read + Send>>) -> (Sink, std::thread::JoinHandle<()>) {
        let sink: Sink = Arc::default();
        let kept = Arc::clone(&sink);
        let handle = std::thread::spawn(move || {
            if let Some(mut p) = pipe {
                let mut buf = [0u8; 8192];
                while let Ok(n) = p.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let mut kept = kept.lock().unwrap_or_else(|e| e.into_inner());
                    let room = EVAL_CAPTURE_LIMIT.saturating_sub(kept.len());
                    kept.extend_from_slice(&buf[..n.min(room)]);
                }
            }
        });
        (sink, handle)
    }
    let (out_sink, out_t) = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let (err_sink, err_t) = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );

    let start = std::time::Instant::now();
    let mut status = None;
    let mut exited_at: Option<std::time::Instant> = None;
    let timed_out = loop {
        if status.is_none()
            && let Some(st) = child.try_wait()?
        {
            status = Some(st);
            exited_at = Some(std::time::Instant::now());
        }
        if status.is_some() && out_t.is_finished() && err_t.is_finished() {
            break false;
        }
        let grace_over = exited_at.is_some_and(|t| t.elapsed() >= EVAL_DRAIN_GRACE);
        if stragglers == Stragglers::Leave && status.is_some() {
            // The shell is done; whatever it left in the background keeps running.
            if grace_over || timeout.is_some_and(|t| start.elapsed() >= t) {
                break false;
            }
        } else {
            if timeout.is_some_and(|t| start.elapsed() >= t) {
                break true;
            }
            // No timeout configured: still stop background children that outlive the shell.
            if timeout.is_none() && grace_over {
                kill_process_tree(&mut child);
                break false;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    if timed_out {
        kill_process_tree(&mut child);
        let _ = child.wait();
    }
    let joined_by = std::time::Instant::now() + DRAIN_JOIN_LIMIT;
    while !(out_t.is_finished() && err_t.is_finished()) && std::time::Instant::now() < joined_by {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let take = |s: &Sink| s.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let (stdout, stderr) = (take(&out_sink), take(&err_sink));
    if timed_out {
        return Ok(None);
    }
    Ok(status.map(|status| std::process::Output {
        status,
        stdout,
        stderr,
    }))
}

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

fn kill_process_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        const SIGKILL: i32 = 9;
        // The child leads its own process group, so -pid reaches everything it spawned.
        unsafe { kill(-(child.id() as i32), SIGKILL) };
    }
    #[cfg(windows)]
    {
        // `/T` ends the whole tree; `child.kill()` alone would leave grandchildren running.
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    let _ = child.kill();
}

/// The shell evals run under: `$SHELL` when it exists, else bash, zsh, then `sh`.
#[cfg(not(target_os = "windows"))]
pub(crate) fn eval_shell() -> String {
    if let Ok(s) = std::env::var("SHELL")
        && !s.is_empty()
        && Path::new(&s).exists()
    {
        s
    } else if Path::new("/bin/bash").exists() {
        "/bin/bash".to_string()
    } else if Path::new("/bin/zsh").exists() {
        "/bin/zsh".to_string()
    } else {
        "sh".to_string()
    }
}

/// Shell builtins and keywords a dry-run treats as always available.
const SHELL_BUILTINS: &[&str] = &[
    "!", ".", ":", "[", "[[", "alias", "cd", "command", "echo", "eval", "exec", "exit", "export",
    "false", "if", "printf", "pwd", "read", "set", "shift", "source", "test", "then", "time",
    "true", "type", "ulimit", "umask", "unset", "while", "for", "case",
];

/// Shells whose first argument is a script file the eval depends on.
const SCRIPT_RUNNERS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];

/// What `tk eval --dry-run` found about a task's check without running it.
#[derive(Debug)]
pub struct EvalDryRun {
    pub card: TaskCard,
    /// Defects that would make the check fail for reasons unrelated to the work.
    pub problems: Vec<String>,
    /// Facts worth knowing before claiming (pinned files, no eval, a changed check).
    pub notes: Vec<String>,
}

impl EvalDryRun {
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Split a command line into simple-command segments on `&&`, `||`, `;`, `|` and newlines.
/// Quote-aware enough for checks: separators inside single or double quotes are kept.
fn command_segments(cmd: &str) -> Vec<Vec<String>> {
    let mut segments = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = cmd.chars().peekable();
    let flush_word = |word: &mut String, words: &mut Vec<String>| {
        if !word.is_empty() {
            words.push(std::mem::take(word));
        }
    };
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                word.push(c);
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            ' ' | '\t' => flush_word(&mut word, &mut words),
            // Redirections such as `2>&1`, `>&2` and `&>file` stay one word.
            '&' if word.ends_with('>') || word.ends_with('<') || chars.peek() == Some(&'>') => {
                word.push(c)
            }
            '&' | '|' | ';' | '\n' => {
                if (c == '&' || c == '|') && chars.peek() == Some(&c) {
                    chars.next();
                }
                flush_word(&mut word, &mut words);
                if !words.is_empty() {
                    segments.push(std::mem::take(&mut words));
                }
            }
            _ => word.push(c),
        }
    }
    flush_word(&mut word, &mut words);
    if !words.is_empty() {
        segments.push(words);
    }
    segments
}

#[cfg(test)]
pub(crate) fn command_segments_for_test(cmd: &str) -> Vec<Vec<String>> {
    command_segments(cmd)
}

/// The program a segment runs, skipping leading `VAR=value` assignments and `!`.
fn segment_program(words: &[String]) -> Option<(usize, &str)> {
    words.iter().enumerate().find_map(|(i, w)| {
        let is_assignment = w.split_once('=').is_some_and(|(k, _)| {
            !k.is_empty() && k.chars().all(|c| c.is_alphanumeric() || c == '_')
        });
        (!is_assignment && w != "!").then_some((i, w.as_str()))
    })
}

/// True when `program` is a builtin, an existing path (relative to `root`), or on `PATH`.
fn program_exists(program: &str, root: &Path) -> bool {
    if SHELL_BUILTINS.contains(&program) || program.contains('$') {
        return true;
    }
    if program.contains('/') {
        let p = Path::new(program);
        return if p.is_absolute() {
            p.exists()
        } else {
            root.join(p).exists()
        };
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

/// Dry-run checks on shell text: absolute `cd` targets exist, programs exist, and scripts run by
/// `sh`/`bash`/... exist and parse. Scripts are checked one level deep with `in_script` set, which
/// also skips functions the script defines and reports problems against the script's name.
fn check_commands(text: &str, root: &Path, in_script: Option<&str>, problems: &mut Vec<String>) {
    let mut functions = std::collections::HashSet::new();
    let mut code = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        let without_comment = match line.find(" #") {
            Some(i) => &line[..i],
            None => line,
        };
        let decl = without_comment
            .trim()
            .trim_start_matches("function ")
            .trim();
        if let Some(name) = decl.split("()").next()
            && decl.contains("()")
            && !name.is_empty()
            && !name.contains(' ')
        {
            functions.insert(name.to_string());
        }
        code.push_str(without_comment);
        code.push('\n');
    }
    let at = |msg: String| match in_script {
        Some(script) => format!("in `{}`: {}", script, msg),
        None => msg,
    };
    for words in command_segments(&code) {
        let Some((idx, program)) = segment_program(&words) else {
            continue;
        };
        if program == "cd" {
            if let Some(dir) = words.get(idx + 1)
                && Path::new(dir).is_absolute()
                && !dir.contains('$')
                && !Path::new(dir).is_dir()
            {
                problems.push(at(format!("`cd {}`: directory does not exist", dir)));
            }
            continue;
        }
        if functions.contains(program)
            || program.ends_with("()")
            || program == "{"
            || program == "}"
        {
            continue;
        }
        if !program_exists(program, root) {
            problems.push(at(format!(
                "`{}` is not a builtin, a file, or on PATH",
                program
            )));
            continue;
        }
        let base = Path::new(program)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(program);
        if in_script.is_none()
            && SCRIPT_RUNNERS.contains(&base)
            && let Some(script) = words[idx + 1..].iter().find(|w| !w.starts_with('-'))
            && !script.contains('$')
        {
            let path = if Path::new(script).is_absolute() {
                Path::new(script).to_path_buf()
            } else {
                root.join(script)
            };
            if !path.is_file() {
                problems.push(format!("script `{}` does not exist", script));
                continue;
            }
            let mut check = Command::new(base);
            check.arg("-n").arg(&path).current_dir(root);
            if let Ok(Some(out)) = run_with_timeout(check, Some(std::time::Duration::from_secs(10)))
                && !out.status.success()
            {
                problems.push(format!(
                    "script `{}` does not parse: {}",
                    script,
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
                continue;
            }
            if let Ok(body) = std::fs::read_to_string(&path) {
                check_commands(&body, root, Some(script), problems);
            }
        }
    }
}

impl TaskStore {
    /// Check a task's eval without running it: shell syntax, that the programs and scripts it
    /// names exist, and that absolute `cd` targets exist. Read-only: no lock, write, pin,
    /// receipt or event. Meant for just before `tk c`, since the first claim pins the check.
    pub fn dry_run_eval(&self, query: &str) -> Result<EvalDryRun> {
        let card = self.find_task(query)?;
        let mut problems = Vec::new();
        let mut notes = Vec::new();
        let Some(cmd_str) = card.meta.eval.clone().filter(|e| !e.trim().is_empty()) else {
            if self.config.policy.require_eval {
                problems.push(
                    "no eval command, and `[policy] require_eval = true` blocks pending-close"
                        .to_string(),
                );
            } else {
                notes.push("no eval command: `tk eval` would auto-pass it as no-eval".to_string());
            }
            return Ok(EvalDryRun {
                card,
                problems,
                notes,
            });
        };
        let root = Path::new(&self.config.vault_path);

        #[cfg(not(target_os = "windows"))]
        {
            let shell = eval_shell();
            let mut syntax = Command::new(&shell);
            syntax.arg("-n").arg("-c").arg(&cmd_str).current_dir(root);
            match run_with_timeout(syntax, Some(std::time::Duration::from_secs(10))) {
                Ok(Some(out)) if !out.status.success() => problems.push(format!(
                    "eval does not parse under `{}`: {}",
                    shell,
                    String::from_utf8_lossy(&out.stderr).trim()
                )),
                Ok(None) => problems.push("syntax check timed out".to_string()),
                Err(e) => problems.push(format!(
                    "could not start `{}` for a syntax check: {}",
                    shell, e
                )),
                Ok(Some(_)) => {}
            }
        }

        check_commands(&cmd_str, root, None, &mut problems);

        let inputs = self.eval_inputs(Some(&cmd_str));
        if inputs.is_empty() {
            notes.push(
                "names no workspace files, so the first claim pins only the command text; \
                 name a script (`sh scripts/check.sh`) to pin its contents"
                    .to_string(),
            );
        } else {
            let files: Vec<&str> = inputs.keys().map(String::as_str).collect();
            notes.push(format!("the first claim pins: {}", files.join(", ")));
        }
        if let Some(change) = self.check_change(&card) {
            notes.push(format!("check changed since it was pinned: {}", change));
        }
        Ok(EvalDryRun {
            card,
            problems,
            notes,
        })
    }

    /// Runs the task's `eval` shell command (if configured) and records pass/fail on the card.
    pub fn run_eval(&self, query: &str, actor: &str) -> Result<(TaskCard, bool, String)> {
        let found = self.find_task(query)?;
        let _lock = Self::lock_card(Path::new(&found.path))?;
        let mut card = Self::read_card(Path::new(&found.path))?;
        card.scope = found.scope;
        let (passed, output) = self.eval_card(&mut card, actor)?;
        self.write_card(&card)?;
        let cur_status = card.meta.status.as_str().to_string();
        self.log_event(
            &card.meta.id,
            "eval",
            actor,
            card.meta.session_id.as_deref(),
            Some(&cur_status),
            &cur_status,
            Some(passed),
            &card.meta.title,
        )?;
        Ok((card, passed, output))
    }

    /// Evaluate a card the caller has already locked and re-read. Mutates the card; does not write it.
    pub(crate) fn eval_card(&self, card: &mut TaskCard, actor: &str) -> Result<(bool, String)> {
        let now_ts = Local::now().format("%Y-%m-%d %H:%M").to_string();

        let Some(cmd_str) = card.meta.eval.clone().filter(|e| !e.trim().is_empty()) else {
            card.meta.eval_status = Some("passed (no-eval)".to_string());
            return Ok((true, "No eval command configured (auto-pass)".to_string()));
        };

        #[cfg(target_os = "windows")]
        let mut cmd = {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(&cmd_str);
            c
        };
        #[cfg(not(target_os = "windows"))]
        let mut cmd = {
            let mut c = Command::new(eval_shell());
            c.arg("-c").arg(&cmd_str);
            c
        };

        cmd.current_dir(&self.config.vault_path);
        let timeout = match self.config.policy.eval_timeout_secs {
            0 => None,
            secs => Some(std::time::Duration::from_secs(secs)),
        };
        let output = run_with_timeout(cmd, timeout)
            .with_context(|| format!("Failed to execute eval command: {}", cmd_str))?;
        let Some(output) = output else {
            let secs = self.config.policy.eval_timeout_secs;
            card.meta.eval_status = Some("failed".to_string());
            card.body = format!(
                "{}\n- `{}`: Eval `{}` run by `{}` -> **TIMED OUT ⏱** (after {}s).",
                card.body.trim_end(),
                now_ts,
                cmd_str,
                actor,
                secs
            );
            return Ok((
                false,
                format!(
                    "eval timed out after {}s ([policy].eval_timeout_secs)",
                    secs
                ),
            ));
        };

        let passed = output.status.success();
        let stdout_s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr_s = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let combined = if !stdout_s.is_empty() && !stderr_s.is_empty() {
            format!("{} | {}", stdout_s, stderr_s)
        } else if !stdout_s.is_empty() {
            stdout_s
        } else if !stderr_s.is_empty() {
            stderr_s
        } else {
            format!("exit {}", output.status.code().unwrap_or(-1))
        };

        card.meta.eval_status = Some(if passed {
            "passed".to_string()
        } else {
            "failed".to_string()
        });
        card.meta.updated = Local::now().format("%Y-%m-%d").to_string();
        let badge = if passed { "PASS ✅" } else { "FAIL ❌" };
        card.body = format!(
            "{}\n- `{}`: Eval `{}` run by `{}` -> **{}** (`{}`).",
            card.body.trim_end(),
            now_ts,
            cmd_str,
            actor,
            badge,
            combined.lines().next().unwrap_or("")
        );
        Ok((passed, combined))
    }
}
