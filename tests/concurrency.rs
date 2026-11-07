//! Process-level stress tests: many real `tk` processes racing on one store, the way several
//! agents sharing a vault do. Each test gets an isolated HOME and vault, and stdin is never a
//! terminal, so every process is an untrusted (agent) caller.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const TK: &str = env!("CARGO_BIN_EXE_tk");

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("tkmd-it-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("vault")).unwrap();
        Self { root }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(TK);
        c.args(args)
            .current_dir(self.vault())
            .env("HOME", self.root.join("home"))
            .env("USER", "tester")
            .env("TASKMD_DIR", self.vault())
            .env("NO_COLOR", "1");
        for var in [
            "AGY_CONVERSATION_ID",
            "ANTIGRAVITY_CONVERSATION_ID",
            "CLAUDE_SESSION_ID",
            "CODEX_THREAD_ID",
            "CODEX_SESSION_ID",
            "HERMES_SESSION_ID",
            "OPENCODE_SESSION_ID",
            "PI_SESSION_ID",
            "GEMINI_SESSION_ID",
            "AGENT_SESSION_ID",
            "SESSION_ID",
        ] {
            c.env_remove(var);
        }
        c.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    fn tk(&self, args: &[&str]) -> Output {
        self.cmd(args).output().expect("failed to run tk")
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.tk(args);
        assert!(
            out.status.success(),
            "tk {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Run `n` copies of a command at once (all spawned before any is awaited).
    fn race(&self, n: usize, args_for: impl Fn(usize) -> Vec<String>) -> Vec<Output> {
        let children: Vec<_> = (0..n)
            .map(|i| {
                let args = args_for(i);
                let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                self.cmd(&refs).spawn().expect("spawn tk")
            })
            .collect();
        children
            .into_iter()
            .map(|c| c.wait_with_output().unwrap())
            .collect()
    }

    fn cards(&self) -> Vec<(PathBuf, String)> {
        let mut v: Vec<_> = std::fs::read_dir(self.vault())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension().is_some_and(|x| x == "md")
                    && p.file_name()
                        .and_then(|f| f.to_str())
                        .is_some_and(|f| f.starts_with("tk-"))
            })
            .map(|p| {
                let s = std::fs::read_to_string(&p).unwrap();
                (p, s)
            })
            .collect();
        v.sort();
        v
    }

    fn field(card: &str, key: &str) -> Option<String> {
        let fm = card.strip_prefix("---\n")?.split("\n---").next()?;
        fm.lines()
            .find_map(|l| l.strip_prefix(&format!("{}: ", key)))
            .map(|v| v.trim().trim_matches('\'').trim_matches('"').to_string())
    }

    fn card(&self, id: &str) -> String {
        self.cards()
            .into_iter()
            .find(|(_, s)| Self::field(s, "id").as_deref() == Some(id))
            .unwrap_or_else(|| panic!("card {} not found", id))
            .1
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The `blocked_by` list from a card's frontmatter.
fn blocked_by(card: &str) -> Vec<String> {
    let fm = card.split("\n---").next().unwrap_or("");
    let mut out = Vec::new();
    let mut inside = false;
    for line in fm.lines() {
        if line.starts_with("blocked_by:") {
            inside = true;
        } else if inside && line.starts_with("- ") {
            out.push(line[2..].trim().to_string());
        } else if inside {
            break;
        }
    }
    out
}

fn assert_valid_card(s: &str) {
    assert!(s.starts_with("---\n"), "card lost its frontmatter:\n{}", s);
    assert!(Sandbox::field(s, "id").is_some(), "card has no id:\n{}", s);
    assert!(
        Sandbox::field(s, "status").is_some(),
        "card has no status:\n{}",
        s
    );
}

/// RED (predicted): `next_id` scans and writes without a store lock, so racing adds reuse ids.
#[test]
fn concurrent_adds_get_unique_ids() {
    let sb = Sandbox::new("adds");
    let n = 16;
    let outs = sb.race(n, |i| vec!["add".into(), format!("p1 parallel task {}", i)]);
    for o in &outs {
        assert!(
            o.status.success(),
            "add failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
    let cards = sb.cards();
    assert_eq!(cards.len(), n, "expected {} card files", n);
    let ids: HashSet<String> = cards
        .iter()
        .map(|(_, s)| Sandbox::field(s, "id").unwrap())
        .collect();
    assert_eq!(
        ids.len(),
        n,
        "duplicate task ids: {:?}",
        cards.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
}

#[test]
fn concurrent_claims_exactly_one_agent_wins() {
    let sb = Sandbox::new("claims");
    sb.ok(&["add", "p1 contested task"]);
    let outs = sb.race(16, |i| {
        vec!["c".into(), "1".into(), "-o".into(), format!("agent{}", i)]
    });
    let winners: Vec<usize> = outs
        .iter()
        .enumerate()
        .filter(|(_, o)| o.status.success())
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        winners.len(),
        1,
        "expected exactly one successful claim, got {:?}",
        winners
    );
    let card = sb.card("tk-001");
    assert_valid_card(&card);
    assert_eq!(
        Sandbox::field(&card, "owner").as_deref(),
        Some(format!("agent{}", winners[0]).as_str())
    );
    assert_eq!(
        Sandbox::field(&card, "status").as_deref(),
        Some("in-progress")
    );
}

#[test]
fn concurrent_updates_to_different_fields_are_not_lost() {
    let sb = Sandbox::new("updates");
    sb.ok(&["add", "p1 shared card"]);
    let edits: Vec<Vec<&str>> = vec![
        vec!["set", "1", "-t", "renamed card"],
        vec!["set", "1", "-p", "p0"],
        vec!["set", "1", "--due", "2026-12-01"],
        vec!["set", "1", "--dod", "first,second"],
        vec!["set", "1", "--eval", "true"],
    ];
    let children: Vec<_> = edits.iter().map(|a| sb.cmd(a).spawn().unwrap()).collect();
    for c in children {
        let o = c.wait_with_output().unwrap();
        assert!(
            o.status.success(),
            "set failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
    let card = sb.card("tk-001");
    assert_valid_card(&card);
    assert_eq!(
        Sandbox::field(&card, "title").as_deref(),
        Some("renamed card")
    );
    assert_eq!(Sandbox::field(&card, "priority").as_deref(), Some("P0"));
    assert_eq!(Sandbox::field(&card, "due").as_deref(), Some("2026-12-01"));
    assert_eq!(Sandbox::field(&card, "eval").as_deref(), Some("true"));
    assert!(
        card.contains("- first") && card.contains("- second"),
        "dod lost:\n{}",
        card
    );
}

#[test]
fn concurrent_submits_leave_a_valid_card_and_event_log() {
    let sb = Sandbox::new("submits");
    sb.ok(&["add", "p1 reviewed card"]);
    let outs = sb.race(12, |i| {
        vec!["r".into(), "1".into(), "-a".into(), format!("agent{}", i)]
    });
    assert!(outs.iter().all(|o| o.status.success()));
    let card = sb.card("tk-001");
    assert_valid_card(&card);
    assert_eq!(
        Sandbox::field(&card, "status").as_deref(),
        Some("pending-close")
    );
    let events = std::fs::read_to_string(sb.vault().join(".events.jsonl")).unwrap();
    let mut n = 0;
    for line in events.lines() {
        serde_json::from_str::<serde_json::Value>(line)
            .unwrap_or_else(|e| panic!("corrupt event line {:?}: {}", line, e));
        n += 1;
    }
    assert!(
        n >= 13,
        "expected the add plus 12 submits, got {} events",
        n
    );
}

#[test]
fn agents_cannot_close_work_from_the_cli() {
    let sb = Sandbox::new("noclose");
    sb.ok(&["add", "p1 agent work"]);
    sb.ok(&["r", "1", "-a", "claude"]);
    for args in [
        vec!["d", "1"],
        vec!["d", "1", "-y"],
        vec!["d", "1", "-a", "tester"],
        vec!["d", "--pending"],
    ] {
        let o = sb.tk(&args);
        assert!(
            !o.status.success(),
            "tk {:?} should not close from a non-terminal",
            args
        );
    }
    assert_eq!(
        Sandbox::field(&sb.card("tk-001"), "status").as_deref(),
        Some("pending-close")
    );
}

#[test]
fn non_tty_yes_policy_allows_human_runners_and_blocks_agent_env() {
    let sb = Sandbox::new("nonttyyes");
    sb.ok(&["add", "p1 raycast task"]);
    sb.ok(&["r", "1", "-a", "claude"]);

    let cfg = sb.root.join("home/.config/tkmd/config.toml");
    let text = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("allow_non_tty_yes = false", "allow_non_tty_yes = true");
    std::fs::write(&cfg, text).unwrap();

    // Without `-y`, non-TTY still cannot close.
    assert!(!sb.tk(&["d", "1"]).status.success());

    // Even with `-y` and `allow_non_tty_yes = true`, an agent session env var blocks sign-off.
    let mut agent_cmd = sb.cmd(&["d", "1", "-y"]);
    agent_cmd.env("CLAUDE_SESSION_ID", "agent-session-123");
    let agent_out = agent_cmd.output().unwrap();
    assert!(
        !agent_out.status.success(),
        "agent env var must block -y in non-TTY"
    );
    assert_eq!(
        Sandbox::field(&sb.card("tk-001"), "status").as_deref(),
        Some("pending-close")
    );

    // Clean human runner (Raycast / Obsidian) with `-y` closes the task.
    sb.ok(&["d", "1", "-y"]);
    assert_eq!(
        Sandbox::field(&sb.card("tk-001"), "status").as_deref(),
        Some("done")
    );
}

/// RED (predicted): evals have no timeout, and now run under the card lock, so one hung eval
/// would block every other agent on that card forever.
#[test]
fn hung_eval_times_out_and_releases_the_lock() {
    let sb = Sandbox::new("hung");
    sb.ok(&["add", "p1 slow check"]);
    sb.ok(&["set", "1", "--eval", "sleep 30"]);
    // Configure a short timeout through the generated config.
    let cfg = sb.root.join("home/.config/tkmd/config.toml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    assert!(
        text.contains("eval_timeout_secs = 600"),
        "default timeout should be written:\n{}",
        text
    );
    let text = text.replace("eval_timeout_secs = 600", "eval_timeout_secs = 2");
    std::fs::write(&cfg, text).unwrap();

    let start = Instant::now();
    let slow = sb.cmd(&["eval", "1", "-a", "agentA"]).spawn().unwrap();
    thread::sleep(Duration::from_millis(300));
    let other = sb.tk(&["c", "1", "-o", "agentB"]);
    let slow_out = slow.wait_with_output().unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(10),
        "eval did not time out ({:?})",
        elapsed
    );
    assert!(!slow_out.status.success(), "timed-out eval must fail");
    assert!(
        String::from_utf8_lossy(&slow_out.stdout).contains("timed out")
            || String::from_utf8_lossy(&slow_out.stderr).contains("timed out"),
        "timeout should be reported"
    );
    assert!(
        other.status.success(),
        "lock was not released: {}",
        String::from_utf8_lossy(&other.stderr)
    );
}

/// Mixed workload: agents add, claim, update and submit at once. Every card must stay valid.
#[test]
fn mixed_agent_workload_keeps_every_card_valid() {
    let sb = Sandbox::new("mixed");
    for i in 0..8 {
        sb.ok(&["add", &format!("p1 seed {}", i)]);
    }
    let outs = sb.race(32, |i| match i % 4 {
        0 => vec!["add".into(), format!("p2 extra {}", i)],
        1 => vec![
            "c".into(),
            format!("{}", i % 8 + 1),
            "-o".into(),
            format!("agent{}", i),
        ],
        2 => vec![
            "set".into(),
            format!("{}", i % 8 + 1),
            "--due".into(),
            "2026-12-24".into(),
        ],
        _ => vec![
            "r".into(),
            format!("{}", i % 8 + 1),
            "-a".into(),
            format!("agent{}", i),
        ],
    });
    // Refused claims are fine; crashes and corrupt cards are not.
    for o in &outs {
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(!err.contains("panicked"), "tk panicked: {}", err);
        assert!(!err.contains("Invalid YAML"), "corrupt card: {}", err);
    }
    let cards = sb.cards();
    let ids: HashSet<String> = cards
        .iter()
        .map(|(_, s)| Sandbox::field(s, "id").unwrap())
        .collect();
    assert_eq!(ids.len(), cards.len(), "duplicate ids after mixed workload");
    for (_, s) in &cards {
        assert_valid_card(s);
    }
}

/// RED (predicted): an unparsable config is silently replaced by defaults, which would also
/// silently drop every `[policy]` guardrail.
#[test]
fn invalid_config_is_reported_not_ignored() {
    let sb = Sandbox::new("badcfg");
    sb.ok(&["add", "p1 first"]);
    let cfg = sb.root.join("home/.config/tkmd/config.toml");
    std::fs::write(&cfg, "mode = \"cli\"\n[policy\nrequire_eval = true\n").unwrap();
    let o = sb.tk(&["ls"]);
    assert!(
        !o.status.success(),
        "tk should refuse to run with a broken config"
    );
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("config"),
        "error should name the config file"
    );
}

#[test]
fn agents_cannot_turn_on_auto_accept() {
    let sb = Sandbox::new("noauto");
    sb.ok(&["add", "p1 checked work"]);
    sb.ok(&["set", "1", "--eval", "true"]);
    let o = sb.tk(&["auto", "1"]);
    assert!(
        !o.status.success(),
        "a non-terminal caller must not approve an eval"
    );
    assert!(Sandbox::field(&sb.card("tk-001"), "approved_eval").is_none());
    // Revoking is always allowed.
    sb.ok(&["auto", "1", "--off"]);
}

// ---- Agent ergonomics: what a non-terminal caller sees ----

#[test]
fn piped_output_has_no_color_codes() {
    let sb = Sandbox::new("nocolor");
    sb.ok(&["add", "p1 plain output"]);
    for args in [vec!["ls"], vec!["s", "1"], vec!["r"]] {
        let mut c = sb.cmd(&args);
        c.env_remove("NO_COLOR");
        let out = c.output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            !text.contains('\x1b'),
            "tk {:?} wrote ANSI codes to a pipe:\n{}",
            args,
            text
        );
    }
}

#[test]
fn unreadable_card_is_reported_not_hidden() {
    let sb = Sandbox::new("corrupt");
    sb.ok(&["add", "p1 good card"]);
    std::fs::write(
        sb.vault().join("tk-002-broken.md"),
        "---\nid: tk-002\nstatus: [broken\n---\nbody\n",
    )
    .unwrap();
    let o = sb.tk(&["ls"]);
    assert!(o.status.success(), "one bad card must not break listing");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("tk-002-broken.md"),
        "bad card should be named on stderr, got: {}",
        err
    );
}

#[test]
fn lock_files_are_hidden() {
    let sb = Sandbox::new("hiddenlock");
    sb.ok(&["add", "p1 locked"]);
    sb.ok(&["c", "1", "-o", "claude"]);
    let visible: Vec<_> = std::fs::read_dir(sb.vault())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".lock") && !n.starts_with('.'))
        .collect();
    assert!(visible.is_empty(), "visible lock files: {:?}", visible);
}

// ---- Findings from the independent Codex review ----

/// RED (Codex F4): a background child kept the eval's output open past the timeout, and the
/// eval then passed.
#[test]
fn background_eval_child_cannot_outlive_the_timeout() {
    let sb = Sandbox::new("bgtimeout");
    sb.ok(&["add", "p1 sneaky check"]);
    let cfg = sb.root.join("home/.config/tkmd/config.toml");
    let text = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("eval_timeout_secs = 600", "eval_timeout_secs = 1");
    std::fs::write(&cfg, text).unwrap();
    sb.ok(&["set", "1", "--eval", "sleep 6 &"]);
    let start = Instant::now();
    let o = sb.tk(&["eval", "1", "-a", "agent"]);
    let took = start.elapsed();
    assert!(
        took < Duration::from_secs(4),
        "eval outlived its 1s timeout: {:?}",
        took
    );
    assert!(!o.status.success(), "a timed-out eval must not pass");
}

#[cfg(unix)]
unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

/// RED (Codex F3): three `tk dep` commands validated against the same old graph and together
/// committed a cycle. Deterministic: hold every card lock, start all three, then release.
#[cfg(unix)]
#[test]
fn concurrent_dependency_edges_cannot_form_a_cycle() {
    use std::os::unix::io::AsRawFd;
    let sb = Sandbox::new("depcycle");
    for t in ["p1 a", "p1 b", "p1 c"] {
        sb.ok(&["add", t]);
    }
    let locks: Vec<std::fs::File> = sb
        .cards()
        .iter()
        .map(|(p, _)| {
            let stem = p.file_stem().unwrap().to_string_lossy().to_string();
            let f = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(p.with_file_name(format!(".{}.lock", stem)))
                .unwrap();
            assert_eq!(unsafe { flock(f.as_raw_fd(), 2) }, 0);
            f
        })
        .collect();
    let children: Vec<_> = [
        ["dep", "1", "--needs", "2"],
        ["dep", "2", "--needs", "3"],
        ["dep", "3", "--needs", "1"],
    ]
    .iter()
    .map(|a| sb.cmd(a).spawn().unwrap())
    .collect();
    thread::sleep(Duration::from_millis(300));
    drop(locks);
    let ok = children
        .into_iter()
        .filter(|_| true)
        .map(|c| c.wait_with_output().unwrap())
        .filter(|o| o.status.success())
        .count();
    assert!(
        ok <= 2,
        "all three edges committed: the graph now has a cycle"
    );
    let edges: usize = sb.cards().iter().map(|(_, s)| blocked_by(s).len()).sum();
    assert!(edges <= 2, "{} blocked_by edges written: a cycle", edges);
}

/// RED (Codex F6): `tk r` said "pending-close" when the task had auto-accepted into done.
#[test]
fn review_reports_auto_acceptance_truthfully() {
    let sb = Sandbox::new("automsg");
    sb.ok(&["add", "p1 routine"]);
    sb.ok(&["set", "1", "--eval", "true"]);
    let (path, card) = sb.cards().remove(0);
    std::fs::write(
        &path,
        card.replacen("eval: 'true'", "eval: 'true'\napproved_eval: 'true'", 1),
    )
    .unwrap();
    let out = sb.ok(&["r", "1", "-a", "agent"]);
    assert_eq!(
        Sandbox::field(&sb.card("tk-001"), "status").as_deref(),
        Some("done")
    );
    assert!(
        !out.contains("pending-close"),
        "review printed pending-close for a done task:\n{}",
        out
    );
    assert!(
        out.contains("Auto-accepted"),
        "review should say it auto-accepted:\n{}",
        out
    );
}

#[test]
fn task_note_cli_accepts_flags_after_the_text() {
    let sb = Sandbox::new("note-cli");
    sb.ok(&["add", "p1 card with a note"]);
    sb.ok(&[
        "note", "1", "built", "X", "instead", "of", "Y", "-a", "claude",
    ]);
    let (_, text) = sb.cards().into_iter().next().unwrap();
    assert!(
        text.contains("Note by `claude`: built X instead of Y"),
        "note not appended as expected:\n{}",
        text
    );
    assert!(!text.contains("-a claude"));
}
