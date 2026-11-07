use super::*;

#[test]
fn test_update_task_properties() {
    let tmp = std::env::temp_dir().join(format!("tkmd-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let cfg = AppConfig {
        mode: "cli".to_string(),
        vault_path: tmp.to_string_lossy().to_string(),
        tasks_dir: "tasks".to_string(),
        base_file: String::new(),
        daily_dir: String::new(),
        default_owner: "ken".to_string(),
        obsidian_vault_name: None,
        editor: None,
        policy: Policy::default(),
    };

    let store = TaskStore {
        tasks_path: tmp.join("tasks"),
        daily_path: tmp.clone(),
        config: cfg,
        scope: CardScope::Global,
        peer: None,
        active_scope: ScopeSelection::Global,
        uninitialized_git_repo: None,
        is_uninitialized_local: false,
    };
    fs::create_dir_all(&store.tasks_path).unwrap();

    // 1. Add task
    let card = store
        .add_task("p1 Initial task +test @ken", None, vec![], None, None)
        .unwrap();
    assert_eq!(card.meta.title, "Initial task");
    assert_eq!(card.meta.priority, "P1");

    // 2. Update task with update_task
    let updated = store
        .update_task(
            &card.meta.id,
            Some("Refactored Task".to_string()),
            Some("p0".to_string()),
            Some("claude".to_string()),
            Some("2026-10-25".to_string()),
            vec!["DoD 1".to_string(), "DoD 2".to_string()],
            Some("cargo check".to_string()),
            None,
        )
        .unwrap();

    assert_eq!(updated.meta.title, "Refactored Task");
    assert_eq!(updated.meta.priority, "P0");
    assert_eq!(updated.meta.owner, "claude");
    assert_eq!(updated.meta.due.as_deref(), Some("2026-10-25"));
    assert_eq!(updated.meta.dod.len(), 2);
    assert_eq!(updated.meta.eval.as_deref(), Some("cargo check"));
    assert!(updated.body.contains("# Refactored Task"));
    assert!(updated.body.contains("## Definition of Done (DoD)"));
    assert!(updated.body.contains("- [ ] DoD 1"));

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_concurrent_atomic_writes() {
    let tmp = std::env::temp_dir().join(format!("tkmd-concurrent-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let cfg = AppConfig {
        mode: "cli".to_string(),
        vault_path: tmp.to_string_lossy().to_string(),
        tasks_dir: "tasks".to_string(),
        base_file: String::new(),
        daily_dir: String::new(),
        default_owner: "ken".to_string(),
        obsidian_vault_name: None,
        editor: None,
        policy: Policy::default(),
    };

    let store = std::sync::Arc::new(TaskStore {
        tasks_path: tmp.join("tasks"),
        daily_path: tmp.clone(),
        config: cfg,
        scope: CardScope::Global,
        peer: None,
        active_scope: ScopeSelection::Global,
        uninitialized_git_repo: None,
        is_uninitialized_local: false,
    });
    fs::create_dir_all(&store.tasks_path).unwrap();

    let card = store
        .add_task("p1 Shared agent task @ken", None, vec![], None, None)
        .unwrap();

    let mut handles = Vec::new();
    for i in 0..8 {
        let s = store.clone();
        let card_id = card.meta.id.clone();
        let handle = std::thread::spawn(move || {
            let owner = format!("agent_{}", i);
            let _ = s.update_task(&card_id, None, None, Some(owner), None, vec![], None, None);
        });
        handles.push(handle);
    }

    for h in handles {
        h.join().unwrap();
    }

    // Re-read card to verify file is valid, non-corrupted Markdown/YAML
    let final_card = store.find_task(&card.meta.id).unwrap();
    assert!(final_card.meta.owner.starts_with("agent_"));
    assert_eq!(final_card.meta.title, "Shared agent task");

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_local_vs_global_aggregation() {
    let tmp_global = std::env::temp_dir().join(format!("tkmd-global-{}", std::process::id()));
    let tmp_local = std::env::temp_dir().join(format!("tkmd-local-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp_global);
    let _ = fs::remove_dir_all(&tmp_local);
    fs::create_dir_all(tmp_global.join("tasks")).unwrap();
    fs::create_dir_all(tmp_local.join("tasks")).unwrap();

    let cfg_global = AppConfig {
        mode: "cli".to_string(),
        vault_path: tmp_global.to_string_lossy().to_string(),
        tasks_dir: "tasks".to_string(),
        base_file: String::new(),
        daily_dir: String::new(),
        default_owner: "ken".to_string(),
        obsidian_vault_name: None,
        editor: None,
        policy: Policy::default(),
    };
    let cfg_local = AppConfig {
        mode: "cli".to_string(),
        vault_path: tmp_local.to_string_lossy().to_string(),
        tasks_dir: "tasks".to_string(),
        base_file: String::new(),
        daily_dir: String::new(),
        default_owner: "ken".to_string(),
        obsidian_vault_name: None,
        editor: None,
        policy: Policy::default(),
    };

    let global_store = TaskStore {
        tasks_path: tmp_global.join("tasks"),
        daily_path: tmp_global.clone(),
        config: cfg_global,
        scope: CardScope::Global,
        peer: None,
        active_scope: ScopeSelection::Global,
        uninitialized_git_repo: None,
        is_uninitialized_local: false,
    };
    let mut local_store = TaskStore {
        tasks_path: tmp_local.join("tasks"),
        daily_path: tmp_local.clone(),
        config: cfg_local,
        scope: CardScope::Local,
        peer: Some(Box::new(global_store)),
        active_scope: ScopeSelection::All,
        uninitialized_git_repo: None,
        is_uninitialized_local: false,
    };

    // Add task to local store
    let c_loc = local_store
        .add_task("p1 Local repo task", None, vec![], None, None)
        .unwrap();
    assert_eq!(c_loc.scope, CardScope::Local);

    // Add task to peer global store
    let c_glob = local_store
        .peer
        .as_ref()
        .unwrap()
        .add_task("p0 Global vault task", None, vec![], None, None)
        .unwrap();
    assert_eq!(c_glob.scope, CardScope::Global);

    // When listing with ScopeSelection::All, should return both!
    let all = local_store.list_all().unwrap();
    assert_eq!(all.len(), 2);
    assert!(
        all.iter()
            .any(|c| c.scope == CardScope::Local && c.meta.title == "Local repo task")
    );
    assert!(
        all.iter()
            .any(|c| c.scope == CardScope::Global && c.meta.title == "Global vault task")
    );

    // When active_scope is Local, returns only local
    local_store.active_scope = ScopeSelection::Local;
    let loc_only = local_store.list_all().unwrap();
    assert_eq!(loc_only.len(), 1);
    assert_eq!(loc_only[0].scope, CardScope::Local);

    let _ = fs::remove_dir_all(&tmp_global);
    let _ = fs::remove_dir_all(&tmp_local);
}

#[test]
fn test_eval_gate_blocks_failing_command() {
    let tmp = std::env::temp_dir().join(format!("tkmd-test-eval-fail-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("tasks")).unwrap();

    let cfg = AppConfig {
        mode: "cli".to_string(),
        vault_path: tmp.to_string_lossy().to_string(),
        tasks_dir: "tasks".to_string(),
        base_file: String::new(),
        daily_dir: String::new(),
        default_owner: "ken".to_string(),
        obsidian_vault_name: None,
        editor: None,
        policy: Policy::default(),
    };

    let store = TaskStore {
        tasks_path: tmp.join("tasks"),
        daily_path: tmp.clone(),
        config: cfg,
        scope: CardScope::Local,
        peer: None,
        active_scope: ScopeSelection::Local,
        uninitialized_git_repo: None,
        is_uninitialized_local: false,
    };

    // Add task with failing eval command
    let card = store
        .add_task(
            "p0 Failing task",
            None,
            vec!["dod item".to_string()],
            Some("exit 1".to_string()),
            None,
        )
        .unwrap();
    assert_eq!(card.meta.status, TaskStatus::Ready);

    // Submitting for review must fail!
    let res = store.submit_for_review("1", "agent", Some("Finished work"), None);
    assert!(res.is_err());
    let err_msg = res.unwrap_err().to_string();
    assert!(err_msg.contains("Eval failed for tk-001"));

    // Card status must still be ready
    let reloaded = store.find_task("1").unwrap();
    assert_eq!(reloaded.meta.status, TaskStatus::Ready);

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_eval_gate_passes_valid_command() {
    let tmp = std::env::temp_dir().join(format!("tkmd-test-eval-pass-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("tasks")).unwrap();

    let cfg = AppConfig {
        mode: "cli".to_string(),
        vault_path: tmp.to_string_lossy().to_string(),
        tasks_dir: "tasks".to_string(),
        base_file: String::new(),
        daily_dir: String::new(),
        default_owner: "ken".to_string(),
        obsidian_vault_name: None,
        editor: None,
        policy: Policy::default(),
    };

    let store = TaskStore {
        tasks_path: tmp.join("tasks"),
        daily_path: tmp.clone(),
        config: cfg,
        scope: CardScope::Local,
        peer: None,
        active_scope: ScopeSelection::Local,
        uninitialized_git_repo: None,
        is_uninitialized_local: false,
    };

    // Add task with passing eval command
    store
        .add_task(
            "p0 Passing task",
            None,
            vec![],
            Some("echo 'all tests green' && exit 0".to_string()),
            None,
        )
        .unwrap();

    // Submitting for review should succeed and transition to pending-close
    let reviewed = store
        .submit_for_review(
            "1",
            "claude",
            Some("Ready for Ken"),
            Some("claude -r 123".to_string()),
        )
        .unwrap();
    assert_eq!(reviewed.meta.status, TaskStatus::PendingClose);
    assert_eq!(reviewed.meta.eval_status.as_deref(), Some("passed"));
    assert!(reviewed.body.contains("PASS ✅"));

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_agent_guardrail_cannot_mark_done() {
    let tmp = std::env::temp_dir().join(format!("tkmd-test-guardrail-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("tasks")).unwrap();

    let cfg = AppConfig {
        mode: "cli".to_string(),
        vault_path: tmp.to_string_lossy().to_string(),
        tasks_dir: "tasks".to_string(),
        base_file: String::new(),
        daily_dir: String::new(),
        default_owner: "ken".to_string(),
        obsidian_vault_name: None,
        editor: None,
        policy: Policy::default(),
    };

    let store = TaskStore {
        tasks_path: tmp.join("tasks"),
        daily_path: tmp.clone(),
        config: cfg,
        scope: CardScope::Local,
        peer: None,
        active_scope: ScopeSelection::Local,
        uninitialized_git_repo: None,
        is_uninitialized_local: false,
    };

    store
        .add_task("p1 Agent work", None, vec![], None, None)
        .unwrap();

    // An AI agent attempts to directly complete the task
    let res = store
        .complete_task("1", "claude", Some("I am done!"))
        .unwrap();

    // Guardrail MUST intercept and force status to pending-close (not done!)
    assert_eq!(res.meta.status, TaskStatus::PendingClose);
    assert_ne!(res.meta.status, TaskStatus::Done);

    // Even if an agent task is called with default_owner ("ken") while in-progress,
    // it must still divert to pending-close first because it has not yet been reviewed!
    let card2 = store
        .add_task("p1 Another agent task @claude", None, vec![], None, None)
        .unwrap();
    let res2 = store
        .complete_task(&card2.meta.id, "ken", Some("Agent tries to skip review"))
        .unwrap();
    assert_eq!(res2.meta.status, TaskStatus::PendingClose);
    assert_ne!(res2.meta.status, TaskStatus::Done);

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_human_sign_off_marks_done_and_checks_dod() {
    let tmp = std::env::temp_dir().join(format!("tkmd-test-signoff-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("tasks")).unwrap();

    let cfg = AppConfig {
        mode: "cli".to_string(),
        vault_path: tmp.to_string_lossy().to_string(),
        tasks_dir: "tasks".to_string(),
        base_file: String::new(),
        daily_dir: String::new(),
        default_owner: "ken".to_string(),
        obsidian_vault_name: None,
        editor: None,
        policy: Policy::default(),
    };

    let store = TaskStore {
        tasks_path: tmp.join("tasks"),
        daily_path: tmp.clone(),
        config: cfg,
        scope: CardScope::Local,
        peer: None,
        active_scope: ScopeSelection::Local,
        uninitialized_git_repo: None,
        is_uninitialized_local: false,
    };

    // Add task with DoD checklist
    let card = store
        .add_task(
            "p0 Deliverable",
            None,
            vec!["Unit tests pass".to_string(), "Docs written".to_string()],
            Some("exit 0".to_string()),
            None,
        )
        .unwrap();
    assert!(card.body.contains("- [ ] Unit tests pass"));

    // Agent submits for review
    store
        .submit_for_review("1", "agent", Some("All ready"), None)
        .unwrap();

    // Human completes task at an interactive terminal
    let done_card = store
        .complete_task_with("1", "ken", Some("LGTM approved"), Approval::Interactive)
        .unwrap();
    assert_eq!(done_card.meta.status, TaskStatus::Done);
    assert_eq!(done_card.meta.approval.as_deref(), Some("interactive"));
    // DoD items must now be checked off (- [x])
    assert!(done_card.body.contains("- [x] Unit tests pass"));
    assert!(done_card.body.contains("- [x] Docs written"));

    // Verify lifecycle event was written to .events.jsonl
    let events = store.read_events().unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.action == "completed" && e.to_status == "done" && e.actor == "ken")
    );

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_uninitialized_local_scope_does_not_create_directory() {
    let tmp = std::env::temp_dir().join(format!("tkmd-test-uninit-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    // Simulate loading an uninitialized local store
    let local_store = TaskStore::load_local_store(&tmp).unwrap();
    assert!(!local_store.is_uninitialized_local);
    // The tasks directory MUST NOT have been created by load_local_store!
    assert!(!local_store.tasks_path.exists());

    // Listing cards on non-existent directory must return empty Vec cleanly without error
    let cards = local_store.list_store_cards().unwrap();
    assert!(cards.is_empty());
    assert!(!local_store.tasks_path.exists());

    let _ = fs::remove_dir_all(&tmp);
}

fn hardening_store(name: &str) -> (TaskStore, PathBuf) {
    let tmp = std::env::temp_dir().join(format!("tkmd-harden-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("tasks")).unwrap();
    let cfg = AppConfig {
        mode: "cli".to_string(),
        vault_path: tmp.to_string_lossy().to_string(),
        tasks_dir: "tasks".to_string(),
        base_file: String::new(),
        daily_dir: String::new(),
        default_owner: "ken".to_string(),
        obsidian_vault_name: None,
        editor: None,
        policy: Policy::default(),
    };
    let store = TaskStore {
        tasks_path: tmp.join("tasks"),
        daily_path: tmp.clone(),
        config: cfg,
        scope: CardScope::Local,
        peer: None,
        active_scope: ScopeSelection::Local,
        uninitialized_git_repo: None,
        is_uninitialized_local: false,
    };
    (store, tmp)
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {:?} failed", args);
}

#[cfg(unix)]
#[test]
fn test_card_lock_is_held_until_dropped() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let (store, tmp) = hardening_store("lock");
    let card = store
        .add_task("p1 Locked card", None, vec![], None, None)
        .unwrap();
    let path = PathBuf::from(&card.path);

    let held = TaskStore::lock_card(&path).unwrap();
    let acquired = Arc::new(AtomicBool::new(false));
    let flag = acquired.clone();
    let p2 = path.clone();
    let handle = std::thread::spawn(move || {
        let _l = TaskStore::lock_card(&p2).unwrap();
        flag.store(true, Ordering::SeqCst);
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        !acquired.load(Ordering::SeqCst),
        "second lock acquired while first was held"
    );
    drop(held);
    handle.join().unwrap();
    assert!(acquired.load(Ordering::SeqCst));
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_untrusted_caller_cannot_close_pending_review() {
    let (store, tmp) = hardening_store("untrusted");
    store
        .add_task(
            "p1 Agent work @claude",
            None,
            vec![],
            Some("exit 0".to_string()),
            None,
        )
        .unwrap();
    store.submit_for_review("1", "claude", None, None).unwrap();

    // Claiming to be the owner by name is not enough.
    let res = store.complete_task("1", "ken", Some("self-approve"));
    assert!(res.is_err());
    assert!(
        res.unwrap_err()
            .to_string()
            .contains("awaiting human sign-off")
    );
    assert_eq!(
        store.find_task("1").unwrap().meta.status,
        TaskStatus::PendingClose
    );
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_claim_guards_owner_and_state() {
    let (store, tmp) = hardening_store("claim");
    store
        .add_task("p1 Shared work", None, vec![], None, None)
        .unwrap();
    store.claim_task("1", "claude", None, false).unwrap();

    // Another agent cannot take it silently.
    let steal = store.claim_task("1", "codex", None, false);
    assert!(
        steal
            .unwrap_err()
            .to_string()
            .contains("`claude` is working on it")
    );
    // Re-claiming your own task is fine.
    store.claim_task("1", "claude", None, false).unwrap();
    // Forced takeover is allowed and recorded.
    let taken = store.claim_task("1", "codex", None, true).unwrap();
    assert_eq!(taken.meta.owner, "codex");
    assert!(taken.body.contains("forced takeover from `claude`"));

    // Work under review cannot be reclaimed without --force.
    store.submit_for_review("1", "codex", None, None).unwrap();
    assert!(store.claim_task("1", "claude", None, false).is_err());

    // Closed work can never be reclaimed.
    store.add_task("p1 Mine", None, vec![], None, None).unwrap();
    store
        .complete_task_with("2", "ken", None, Approval::Interactive)
        .unwrap();
    assert!(store.claim_task("2", "claude", None, true).is_err());
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_require_eval_policy_blocks_no_eval_submit() {
    let (mut store, tmp) = hardening_store("require-eval");
    store.config.policy.require_eval = true;
    store
        .add_task("p1 Unchecked work", None, vec![], None, None)
        .unwrap();
    let res = store.submit_for_review("1", "claude", None, None);
    assert!(res.unwrap_err().to_string().contains("require_eval"));
    assert_eq!(store.find_task("1").unwrap().meta.status, TaskStatus::Ready);
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_no_eval_receipt_is_explicit_and_claim_records_eval() {
    let (store, tmp) = hardening_store("no-eval");
    store
        .add_task("p1 Light task", None, vec![], None, None)
        .unwrap();
    store.claim_task("1", "claude", None, false).unwrap();
    let reviewed = store.submit_for_review("1", "claude", None, None).unwrap();
    let receipt = reviewed.meta.receipt.unwrap();
    assert_eq!(receipt.eval_result, "no-eval");
    assert_eq!(receipt.submitted_by, "claude");
    assert_eq!(reviewed.meta.claimed_eval, None);
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_eval_change_invalidates_review() {
    let (store, tmp) = hardening_store("eval-change");
    store
        .add_task(
            "p1 Checked work",
            None,
            vec![],
            Some("exit 0".to_string()),
            None,
        )
        .unwrap();
    store.submit_for_review("1", "claude", None, None).unwrap();
    let updated = store
        .update_task(
            "1",
            None,
            None,
            None,
            None,
            vec![],
            Some("true".to_string()),
            None,
        )
        .unwrap();
    assert_eq!(updated.meta.status, TaskStatus::InProgress);
    assert!(updated.meta.receipt.is_none());
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_receipt_goes_stale_when_output_changes() {
    if !git_available() {
        eprintln!("skipping: git not installed");
        return;
    }
    let (store, tmp) = hardening_store("stale");
    git(&tmp, &["init", "-q"]);
    fs::write(tmp.join("work.txt"), "v1").unwrap();
    store
        .add_task(
            "p1 Edit work file",
            None,
            vec![],
            Some("exit 0".to_string()),
            None,
        )
        .unwrap();

    let reviewed = store.submit_for_review("1", "claude", None, None).unwrap();
    let snap = reviewed.meta.receipt.as_ref().unwrap().snapshot.clone();
    assert!(snap.is_some(), "expected a git snapshot in a git repo");

    // Card writes must not change the snapshot (cards are excluded).
    assert_eq!(store.worktree_snapshot(), snap);

    // Output changes after the eval passed: approval is refused.
    fs::write(tmp.join("work.txt"), "v2").unwrap();
    let res = store.complete_task_with("1", "ken", None, Approval::Interactive);
    assert!(res.unwrap_err().to_string().contains("output changed"));

    // Re-submitting binds the new output; approval then succeeds.
    store.submit_for_review("1", "claude", None, None).unwrap();
    let done = store
        .complete_task_with("1", "ken", None, Approval::Interactive)
        .unwrap();
    assert_eq!(done.meta.status, TaskStatus::Done);
    let _ = fs::remove_dir_all(&tmp);
}

// ---- Edge cases (TDD: RED marks behaviour the pre-change code got wrong) ----

/// RED: `find_task` matched the query anywhere in the full path, so a word from a parent
/// directory selected the first card in the store.
#[test]
fn test_find_task_ignores_directory_names() {
    let (store, tmp) = hardening_store("findpath-unique");
    store
        .add_task("p1 alpha", None, vec![], None, None)
        .unwrap();
    assert!(
        store.find_task("findpath").is_err(),
        "directory name matched a card"
    );
    assert!(store.find_task("alpha").is_ok());
    let _ = fs::remove_dir_all(&tmp);
}

/// RED: an ambiguous slug query silently picked the first match (dangerous for `tk d`).
#[test]
fn test_find_task_ambiguous_query_is_an_error() {
    let (store, tmp) = hardening_store("ambiguous");
    store
        .add_task("p1 smoke one", None, vec![], None, None)
        .unwrap();
    store
        .add_task("p1 smoke two", None, vec![], None, None)
        .unwrap();
    let err = store.find_task("smoke").unwrap_err().to_string();
    assert!(
        err.contains("tk-001") && err.contains("tk-002"),
        "should list candidates: {}",
        err
    );
    assert_eq!(store.find_task("smoke-two").unwrap().meta.id, "tk-002");
    let _ = fs::remove_dir_all(&tmp);
}

/// RED: numeric queries fell through to substring path matching, so `100` hit `tk-1000`.
#[test]
fn test_numeric_query_never_matches_a_longer_id() {
    let (store, tmp) = hardening_store("ids");
    let first = store.add_task("p1 seed", None, vec![], None, None).unwrap();
    let raw = fs::read_to_string(&first.path)
        .unwrap()
        .replace("id: tk-001", "id: tk-999");
    fs::remove_file(&first.path).unwrap();
    fs::write(store.tasks_path.join("tk-999-seed.md"), raw).unwrap();
    let big = store
        .add_task("p1 thousandth", None, vec![], None, None)
        .unwrap();
    assert_eq!(big.meta.id, "tk-1000");
    assert_eq!(store.find_task("1000").unwrap().meta.id, "tk-1000");
    assert_eq!(store.find_task("999").unwrap().meta.id, "tk-999");
    assert!(
        store.find_task("100").is_err(),
        "`100` must not resolve to tk-1000"
    );
    let _ = fs::remove_dir_all(&tmp);
}

/// RED: `add_dependency` accepted cycles, which deadlock the schedule.
#[test]
fn test_dependency_cycle_is_rejected() {
    let (store, tmp) = hardening_store("cycle");
    for t in ["p1 a", "p1 b", "p1 c"] {
        store.add_task(t, None, vec![], None, None).unwrap();
    }
    store.add_dependency("2", &["1".to_string()]).unwrap();
    store.add_dependency("3", &["2".to_string()]).unwrap();
    let err = store
        .add_dependency("1", &["3".to_string()])
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("cycle"),
        "expected a cycle error, got: {}",
        err
    );
    assert!(
        store.find_task("1").unwrap().meta.blocked_by.is_empty(),
        "cycle edge was written"
    );
    let _ = fs::remove_dir_all(&tmp);
}

/// RED: a blocked task (open dependencies) could be submitted for review.
#[test]
fn test_blocked_task_cannot_be_submitted() {
    let (store, tmp) = hardening_store("blocked-submit");
    store.add_task("p1 base", None, vec![], None, None).unwrap();
    store
        .add_task("p1 top needs:1", None, vec![], None, None)
        .unwrap();
    assert_eq!(
        store.find_task("2").unwrap().meta.status,
        TaskStatus::Blocked
    );
    let err = store
        .submit_for_review("2", "claude", None, None)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("tk-001"),
        "should name the open blocker: {}",
        err
    );
    let _ = fs::remove_dir_all(&tmp);
}

/// RED: any agent could submit another agent's claimed work.
#[test]
fn test_only_the_owner_submits_claimed_work() {
    let (store, tmp) = hardening_store("owner-submit");
    store
        .add_task("p1 owned", None, vec![], None, None)
        .unwrap();
    store.claim_task("1", "claude", None, false).unwrap();
    let err = store
        .submit_for_review("1", "codex", None, None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("claude"), "should name the owner: {}", err);
    store.submit_for_review("1", "claude", None, None).unwrap();
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_yaml_in_a_title_is_inert() {
    let (store, tmp) = hardening_store("yaml-title");
    let card = store
        .add_task(
            "p1 innocent\nstatus: done\nowner: mallory",
            None,
            vec![],
            None,
            None,
        )
        .unwrap();
    let back = TaskStore::read_card(Path::new(&card.path)).unwrap();
    assert_ne!(back.meta.status, TaskStatus::Done);
    assert_ne!(back.meta.owner, "mallory");
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_receipt_survives_unrelated_card_changes() {
    if !git_available() {
        return;
    }
    let (store, tmp) = hardening_store("unrelated");
    git(&tmp, &["init", "-q"]);
    fs::write(tmp.join("work.txt"), "v1").unwrap();
    store
        .add_task(
            "p1 real work",
            None,
            vec![],
            Some("exit 0".to_string()),
            None,
        )
        .unwrap();
    store.submit_for_review("1", "claude", None, None).unwrap();
    // Other agents keep creating and editing cards while the review waits.
    store
        .add_task("p1 other work", None, vec![], None, None)
        .unwrap();
    store.claim_task("2", "codex", None, false).unwrap();
    let done = store
        .complete_task_with("1", "ken", None, Approval::Interactive)
        .unwrap();
    assert_eq!(done.meta.status, TaskStatus::Done);
    let _ = fs::remove_dir_all(&tmp);
}

/// Scale: one blocker with many dependents. RED (predicted): the cascade re-read every card
/// for every dependent (quadratic). Run with `cargo test --release -- --ignored`.
#[test]
#[ignore]
fn scale_cascade_unblocks_many_dependents_quickly() {
    let (store, tmp) = hardening_store("scale-cascade");
    let n = 1000;
    store
        .add_task("p1 blocker", None, vec![], None, None)
        .unwrap();
    let base = fs::read_to_string(store.find_task("1").unwrap().path).unwrap();
    for i in 2..=n + 1 {
        let id = format!("tk-{:03}", i);
        let raw = base
            .replace("id: tk-001", &format!("id: {}", id))
            .replace("status: ready", "status: blocked")
            .replacen("---\n", "---\nblocked_by:\n- tk-001\n", 1);
        fs::write(store.tasks_path.join(format!("{}-dep.md", id)), raw).unwrap();
    }
    let start = std::time::Instant::now();
    store
        .complete_task_with("1", "ken", None, Approval::Interactive)
        .unwrap();
    let took = start.elapsed();
    eprintln!("cascade over {} dependents: {:?}", n, took);
    let ready = store
        .list_store_cards()
        .unwrap()
        .into_iter()
        .filter(|c| c.meta.status == TaskStatus::Ready)
        .count();
    assert_eq!(ready, n);
    assert!(
        took < std::time::Duration::from_secs(5),
        "cascade too slow: {:?}",
        took
    );
    let _ = fs::remove_dir_all(&tmp);
}

// ---- Contract approval: the human approves the check, the agent delivers against it ----

#[test]
fn test_auto_accepted_task_closes_when_its_approved_eval_passes() {
    let (store, tmp) = hardening_store("auto-close");
    store
        .add_task("p1 base", None, vec![], Some("exit 0".to_string()), None)
        .unwrap();
    store
        .add_task("p1 next needs:1", None, vec![], None, None)
        .unwrap();
    let approved = store.approve_eval("1", Approval::Interactive).unwrap();
    assert_eq!(approved.meta.approved_eval.as_deref(), Some("exit 0"));
    store.claim_task("1", "claude", None, false).unwrap();
    let done = store
        .submit_for_review("1", "claude", Some("finished"), None)
        .unwrap();
    assert_eq!(done.meta.status, TaskStatus::Done);
    assert_eq!(done.meta.approval.as_deref(), Some("auto:eval"));
    // Auto-closing unblocks dependents like a manual sign-off.
    assert_eq!(store.find_task("2").unwrap().meta.status, TaskStatus::Ready);
    let events = store.read_events().unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.action == "auto-accepted" && e.to_status == "done")
    );
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_agents_cannot_approve_an_eval() {
    let (store, tmp) = hardening_store("auto-untrusted");
    store
        .add_task("p1 base", None, vec![], Some("exit 0".to_string()), None)
        .unwrap();
    assert!(store.approve_eval("1", Approval::Untrusted).is_err());
    assert!(store.find_task("1").unwrap().meta.approved_eval.is_none());
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_changing_the_eval_revokes_auto_accept() {
    let (store, tmp) = hardening_store("auto-revoke");
    store
        .add_task("p1 base", None, vec![], Some("exit 0".to_string()), None)
        .unwrap();
    store.approve_eval("1", Approval::Interactive).unwrap();
    let changed = store
        .update_task(
            "1",
            None,
            None,
            None,
            None,
            vec![],
            Some("true".to_string()),
            None,
        )
        .unwrap();
    assert!(
        changed.meta.approved_eval.is_none(),
        "eval change must revoke the approval"
    );
    let reviewed = store.submit_for_review("1", "claude", None, None).unwrap();
    assert_eq!(reviewed.meta.status, TaskStatus::PendingClose);
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_a_failing_approved_eval_does_not_close() {
    let (store, tmp) = hardening_store("auto-fail");
    store
        .add_task("p1 base", None, vec![], Some("exit 1".to_string()), None)
        .unwrap();
    store.approve_eval("1", Approval::Interactive).unwrap();
    assert!(store.submit_for_review("1", "claude", None, None).is_err());
    assert_eq!(store.find_task("1").unwrap().meta.status, TaskStatus::Ready);
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_tasks_without_an_eval_cannot_auto_accept() {
    let (store, tmp) = hardening_store("auto-noeval");
    store.add_task("p1 base", None, vec![], None, None).unwrap();
    let err = store
        .approve_eval("1", Approval::Interactive)
        .unwrap_err()
        .to_string();
    assert!(err.contains("eval"), "{}", err);
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_revoking_auto_accept_returns_to_manual_review() {
    let (store, tmp) = hardening_store("auto-off");
    store
        .add_task("p1 base", None, vec![], Some("exit 0".to_string()), None)
        .unwrap();
    store.approve_eval("1", Approval::Interactive).unwrap();
    store.revoke_eval_approval("1").unwrap();
    let reviewed = store.submit_for_review("1", "claude", None, None).unwrap();
    assert_eq!(reviewed.meta.status, TaskStatus::PendingClose);
    let _ = fs::remove_dir_all(&tmp);
}

// ---- Findings from the independent Codex review (and one found while reproducing it) ----

/// RED (Codex F5): a blocker whose card was deleted counted as closed, so claim failed open.
#[test]
fn test_missing_blocker_keeps_the_task_blocked() {
    let (store, tmp) = hardening_store("missing-blocker");
    store.add_task("p1 top", None, vec![], None, None).unwrap();
    store.add_task("p1 base", None, vec![], None, None).unwrap();
    store.add_dependency("1", &["2".to_string()]).unwrap();
    fs::remove_file(store.find_task("2").unwrap().path).unwrap();
    let err = store
        .claim_task("1", "claude", None, false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("tk-002") && err.contains("missing"), "{}", err);
    assert!(store.submit_for_review("1", "claude", None, None).is_err());
    let _ = fs::remove_dir_all(&tmp);
}

/// RED (found reproducing F5): deleting or archiving the newest card let `add` reuse its id,
/// so old references and history silently pointed at a different task.
#[test]
fn test_ids_are_never_reused_after_delete_or_archive() {
    let (store, tmp) = hardening_store("id-reuse");
    for t in ["p1 a", "p1 b", "p1 c"] {
        store.add_task(t, None, vec![], None, None).unwrap();
    }
    fs::remove_file(store.find_task("3").unwrap().path).unwrap();
    assert_eq!(
        store
            .add_task("p1 d", None, vec![], None, None)
            .unwrap()
            .meta
            .id,
        "tk-004"
    );
    store
        .complete_task_with("4", "ken", None, Approval::Interactive)
        .unwrap();
    assert_eq!(
        store.archive_tasks(Some("4")).unwrap().len(),
        1,
        "`tk archive 4` must archive tk-004"
    );
    assert_eq!(
        store
            .add_task("p1 e", None, vec![], None, None)
            .unwrap()
            .meta
            .id,
        "tk-005"
    );
    let _ = fs::remove_dir_all(&tmp);
}

/// RED (Codex F7): without git the receipt silently had no snapshot.
#[test]
fn test_receipt_says_why_there_is_no_snapshot() {
    let (store, tmp) = hardening_store("no-snapshot");
    store
        .add_task("p1 work", None, vec![], Some("exit 0".to_string()), None)
        .unwrap();
    let r = store
        .submit_for_review("1", "claude", None, None)
        .unwrap()
        .meta
        .receipt
        .unwrap();
    assert!(r.snapshot.is_none());
    assert_eq!(r.snapshot_note.as_deref(), Some("not a git repository"));
    let _ = fs::remove_dir_all(&tmp);
}

/// Codex F1: hand-edited cards could claim `done` or an approval with no matching event.
/// tkmd cannot stop an agent that writes the store, but `tk doctor` must notice.
#[test]
fn test_doctor_flags_cards_the_event_log_does_not_explain() {
    let (store, tmp) = hardening_store("doctor");
    store
        .add_task("p1 honest", None, vec![], None, None)
        .unwrap();
    store
        .complete_task_with("1", "ken", None, Approval::Interactive)
        .unwrap();
    store
        .add_task("p1 forged done", None, vec![], None, None)
        .unwrap();
    store
        .add_task(
            "p1 forged approval",
            None,
            vec![],
            Some("true".to_string()),
            None,
        )
        .unwrap();
    assert!(
        store.integrity_report().unwrap().is_empty(),
        "clean store must pass"
    );

    let p2 = store.find_task("2").unwrap().path;
    fs::write(
        &p2,
        fs::read_to_string(&p2)
            .unwrap()
            .replace("status: ready", "status: done"),
    )
    .unwrap();
    let p3 = store.find_task("3").unwrap().path;
    fs::write(
        &p3,
        fs::read_to_string(&p3).unwrap().replacen(
            "eval: 'true'",
            "eval: 'true'\napproved_eval: 'true'",
            1,
        ),
    )
    .unwrap();

    let report = store.integrity_report().unwrap().join("\n");
    assert!(
        report.contains("tk-002") && report.contains("done"),
        "{}",
        report
    );
    assert!(
        report.contains("tk-003") && report.contains("approv"),
        "{}",
        report
    );
    assert!(
        !report.contains("tk-001"),
        "honest card flagged: {}",
        report
    );
    let _ = fs::remove_dir_all(&tmp);
}

// ---- Sourcery review findings (PR #1) ----

/// RED (Sourcery #30, a regression from the missing-blocker fix): an archived, done
/// blocker was treated as missing, so its dependents stayed blocked.
#[test]
fn test_archived_done_blocker_does_not_block() {
    let (store, tmp) = hardening_store("archived-blocker");
    store.add_task("p1 base", None, vec![], None, None).unwrap();
    store
        .add_task("p1 top needs:1", None, vec![], None, None)
        .unwrap();
    store
        .complete_task_with("1", "ken", None, Approval::Interactive)
        .unwrap();
    assert_eq!(
        store.archive_tasks(Some("1")).unwrap().len(),
        1,
        "`tk archive 1` must archive tk-001"
    );
    store.claim_task("2", "claude", None, false).unwrap();
    let _ = fs::remove_dir_all(&tmp);
}

/// RED (Sourcery #3, #28): approval must cover the receipt the human was shown, not a
/// newer one an agent wrote while the prompt was open.
#[test]
fn test_approval_refuses_a_receipt_changed_after_display() {
    let (store, tmp) = hardening_store("changed-receipt");
    store
        .add_task("p1 work", None, vec![], Some("exit 0".to_string()), None)
        .unwrap();
    let shown = store
        .submit_for_review("1", "claude", None, None)
        .unwrap()
        .meta
        .receipt
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    store
        .submit_for_review("1", "claude", Some("again"), None)
        .unwrap();
    let err = store
        .complete_task_confirmed("1", "ken", None, Approval::Interactive, Some(&shown), false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("changed"), "{}", err);
    assert_eq!(
        store.find_task("1").unwrap().meta.status,
        TaskStatus::PendingClose
    );
    let _ = fs::remove_dir_all(&tmp);
}

/// RED (Sourcery #10): a blocker added after submit did not stop approval.
#[test]
fn test_pending_task_with_a_new_blocker_cannot_be_approved() {
    let (store, tmp) = hardening_store("pending-blocked");
    store
        .add_task("p1 work", None, vec![], Some("exit 0".to_string()), None)
        .unwrap();
    store.submit_for_review("1", "claude", None, None).unwrap();
    store
        .add_task("p1 prerequisite", None, vec![], None, None)
        .unwrap();
    store.add_dependency("1", &["2".to_string()]).unwrap();
    let err = store
        .complete_task_with("1", "ken", None, Approval::Interactive)
        .unwrap_err()
        .to_string();
    assert!(err.contains("tk-002"), "{}", err);
    let _ = fs::remove_dir_all(&tmp);
}

/// RED (Sourcery #5): output changed by the eval itself (or while it ran) was receipted.
#[test]
fn test_output_changing_during_eval_is_refused() {
    if !git_available() {
        return;
    }
    let (store, tmp) = hardening_store("during-eval");
    git(&tmp, &["init", "-q"]);
    fs::write(tmp.join("work.txt"), "v1").unwrap();
    store
        .add_task(
            "p1 work",
            None,
            vec![],
            Some("echo more >> work.txt".to_string()),
            None,
        )
        .unwrap();
    let err = store
        .submit_for_review("1", "claude", None, None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("changed while the eval ran"), "{}", err);
    let _ = fs::remove_dir_all(&tmp);
}

/// RED (Sourcery #8): changing the Definition of Done after submit left the review valid.
#[test]
fn test_dod_change_invalidates_review() {
    let (store, tmp) = hardening_store("dod-change");
    store
        .add_task(
            "p1 work",
            None,
            vec!["original".into()],
            Some("exit 0".into()),
            None,
        )
        .unwrap();
    store.submit_for_review("1", "claude", None, None).unwrap();
    let c = store
        .update_task(
            "1",
            None,
            None,
            None,
            None,
            vec!["weaker".into()],
            None,
            None,
        )
        .unwrap();
    assert_eq!(c.meta.status, TaskStatus::InProgress);
    assert!(c.meta.receipt.is_none());
    let _ = fs::remove_dir_all(&tmp);
}

/// RED (Sourcery #9): a blank eval passed `require_eval` and then ran as an empty command.
#[test]
fn test_blank_eval_counts_as_no_eval() {
    let (mut store, tmp) = hardening_store("blank-eval");
    store.config.policy.require_eval = true;
    store
        .add_task("p1 work", None, vec![], Some("   ".into()), None)
        .unwrap();
    let err = store
        .submit_for_review("1", "claude", None, None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("require_eval"), "{}", err);
    let _ = fs::remove_dir_all(&tmp);
}

/// RED (Sourcery #16): a failing daily-note write returned before the unblock cascade, so the
/// task was done but its dependents stayed blocked.
#[test]
fn test_daily_note_failure_does_not_strand_dependents() {
    let (mut store, tmp) = hardening_store("note-fail");
    fs::write(tmp.join("not-a-dir"), "x").unwrap();
    store.config.mode = "obsidian".to_string();
    store.config.daily_dir = "not-a-dir/daily".to_string();
    store.daily_path = tmp.join("not-a-dir/daily");
    store.add_task("p1 base", None, vec![], None, None).unwrap();
    store
        .add_task("p1 top needs:1", None, vec![], None, None)
        .unwrap();
    let done = store
        .complete_task_with("1", "ken", None, Approval::Interactive)
        .unwrap();
    assert_eq!(done.meta.status, TaskStatus::Done);
    assert_eq!(store.find_task("2").unwrap().meta.status, TaskStatus::Ready);
    let _ = fs::remove_dir_all(&tmp);
}

/// RED (Sourcery #26): the owner forcing their own task back from review was not recorded.
#[test]
fn test_forced_take_back_from_review_is_recorded() {
    let (store, tmp) = hardening_store("take-back");
    store.add_task("p1 work", None, vec![], None, None).unwrap();
    store.claim_task("1", "claude", None, false).unwrap();
    store.submit_for_review("1", "claude", None, None).unwrap();
    let c = store.claim_task("1", "claude", None, true).unwrap();
    assert!(
        c.body.contains("forced take-back from review"),
        "{}",
        c.body
    );
    assert!(
        store
            .read_events()
            .unwrap()
            .iter()
            .any(|e| e.action == "claimed-forced")
    );
    let _ = fs::remove_dir_all(&tmp);
}

/// RED (Sourcery #15): a global task closed through the local store did not unblock its
/// global dependents.
#[test]
fn test_closing_a_peer_task_cascades_in_the_peer_store() {
    let (global, gtmp) = hardening_store("peer-global");
    global
        .add_task("p1 global base", None, vec![], None, None)
        .unwrap();
    global
        .add_task("p1 global top needs:1", None, vec![], None, None)
        .unwrap();
    let (mut local, ltmp) = hardening_store("peer-local");
    local.scope = CardScope::Local;
    let mut global = global;
    global.scope = CardScope::Global;
    local.peer = Some(Box::new(global));
    local.active_scope = ScopeSelection::Auto;
    local
        .complete_task_with("1", "ken", None, Approval::Interactive)
        .unwrap();
    let top = local.peer.as_ref().unwrap().find_task("2").unwrap();
    assert_eq!(top.meta.status, TaskStatus::Ready);
    let _ = fs::remove_dir_all(&gtmp);
    let _ = fs::remove_dir_all(&ltmp);
}

/// RED (Sourcery #27): doctor ignored archived cards and stuck dependents.
#[test]
fn test_doctor_checks_archive_and_stuck_dependents() {
    let (store, tmp) = hardening_store("doctor-archive");
    store.add_task("p1 base", None, vec![], None, None).unwrap();
    store
        .add_task("p1 top needs:1", None, vec![], None, None)
        .unwrap();
    store
        .complete_task_with("1", "ken", None, Approval::Interactive)
        .unwrap();
    let p2 = store.find_task("2").unwrap().path;
    fs::write(
        &p2,
        fs::read_to_string(&p2)
            .unwrap()
            .replace("status: ready", "status: blocked"),
    )
    .unwrap();
    let arch = store.tasks_path.join("archive");
    fs::create_dir_all(&arch).unwrap();
    let forged = fs::read_to_string(store.find_task("1").unwrap().path)
        .unwrap()
        .replace("id: tk-001", "id: tk-050");
    fs::write(arch.join("tk-050-forged.md"), forged).unwrap();
    let report = store.integrity_report().unwrap().join("\n");
    assert!(
        report.contains("tk-002") && report.contains("unblock"),
        "{}",
        report
    );
    assert!(report.contains("tk-050"), "{}", report);
    let _ = fs::remove_dir_all(&tmp);
}

// ---- Eval contract pinning (dev.to review by prpatel05) ----

fn write_check(store: &TaskStore, body: &str) {
    fs::write(Path::new(&store.config.vault_path).join("check.sh"), body).unwrap();
}

/// RED: an agent could edit the script its eval runs, after claiming, and get a green close.
#[test]
fn test_editing_the_eval_script_after_claim_blocks_sign_off() {
    if !git_available() {
        return;
    }
    let (store, tmp) = hardening_store("pin-script");
    write_check(&store, "exit 1\n");
    store
        .add_task("p1 work", None, vec![], Some("sh check.sh".into()), None)
        .unwrap();
    store.claim_task("1", "claude", None, false).unwrap();
    write_check(&store, "exit 0 # made it pass\n");
    let r = store
        .submit_for_review("1", "claude", None, None)
        .unwrap()
        .meta
        .receipt
        .unwrap();
    assert!(
        r.check_changed
            .as_deref()
            .is_some_and(|c| c.contains("check.sh")),
        "{:?}",
        r.check_changed
    );
    let err = store
        .complete_task_with("1", "ken", None, Approval::Interactive)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("changed since") && err.contains("--accept-changed-eval"),
        "{}",
        err
    );
    let done = store
        .complete_task_confirmed("1", "ken", None, Approval::Interactive, None, true)
        .unwrap();
    assert_eq!(done.meta.status, TaskStatus::Done);
    let _ = fs::remove_dir_all(&tmp);
}

/// RED: changing the eval command itself after claim only warned.
#[test]
fn test_changing_the_eval_command_after_claim_blocks_sign_off() {
    let (store, tmp) = hardening_store("pin-command");
    store
        .add_task("p1 work", None, vec![], Some("exit 0".into()), None)
        .unwrap();
    store.claim_task("1", "claude", None, false).unwrap();
    store
        .update_task(
            "1",
            None,
            None,
            None,
            None,
            vec![],
            Some("true".into()),
            None,
        )
        .unwrap();
    store.submit_for_review("1", "claude", None, None).unwrap();
    let err = store
        .complete_task_with("1", "ken", None, Approval::Interactive)
        .unwrap_err()
        .to_string();
    assert!(err.contains("changed since"), "{}", err);
    let _ = fs::remove_dir_all(&tmp);
}

/// RED: re-claiming must not reset the pinned check (edit, re-claim, launder).
#[test]
fn test_reclaiming_does_not_reset_the_pinned_check() {
    if !git_available() {
        return;
    }
    let (store, tmp) = hardening_store("pin-reclaim");
    write_check(&store, "exit 1\n");
    store
        .add_task("p1 work", None, vec![], Some("sh check.sh".into()), None)
        .unwrap();
    store.claim_task("1", "claude", None, false).unwrap();
    write_check(&store, "exit 0\n");
    store.claim_task("1", "claude", None, false).unwrap();
    let r = store
        .submit_for_review("1", "claude", None, None)
        .unwrap()
        .meta
        .receipt
        .unwrap();
    assert!(
        r.check_changed.is_some(),
        "re-claim laundered the script change"
    );
    let _ = fs::remove_dir_all(&tmp);
}

/// RED: `tk auto` approved a script name, so editing the script still auto-closed.
#[test]
fn test_auto_accept_skips_when_the_approved_script_changed() {
    if !git_available() {
        return;
    }
    let (store, tmp) = hardening_store("pin-auto");
    write_check(&store, "exit 0\n");
    store
        .add_task("p1 work", None, vec![], Some("sh check.sh".into()), None)
        .unwrap();
    store.approve_eval("1", Approval::Interactive).unwrap();
    store.claim_task("1", "claude", None, false).unwrap();
    write_check(&store, "true\n");
    let c = store.submit_for_review("1", "claude", None, None).unwrap();
    assert_eq!(
        c.meta.status,
        TaskStatus::PendingClose,
        "changed script must not auto-close"
    );
    let _ = fs::remove_dir_all(&tmp);
}

/// RED: outside git there was no snapshot, so a script edited after submit went unnoticed.
#[test]
fn test_script_edited_after_submit_is_refused_without_git_snapshot() {
    if !git_available() {
        return;
    }
    let (store, tmp) = hardening_store("pin-after-submit");
    write_check(&store, "exit 0\n");
    store
        .add_task("p1 work", None, vec![], Some("sh check.sh".into()), None)
        .unwrap();
    store.submit_for_review("1", "claude", None, None).unwrap();
    write_check(&store, "exit 0 # changed later\n");
    let err = store
        .complete_task_with("1", "ken", None, Approval::Interactive)
        .unwrap_err()
        .to_string();
    assert!(err.contains("check.sh"), "{}", err);
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_unchanged_check_closes_normally() {
    if !git_available() {
        return;
    }
    let (store, tmp) = hardening_store("pin-unchanged");
    write_check(&store, "exit 0\n");
    store
        .add_task("p1 work", None, vec![], Some("sh check.sh".into()), None)
        .unwrap();
    store.claim_task("1", "claude", None, false).unwrap();
    let r = store
        .submit_for_review("1", "claude", None, None)
        .unwrap()
        .meta
        .receipt
        .unwrap();
    assert!(r.check_changed.is_none());
    assert!(
        r.eval_inputs.contains_key("check.sh"),
        "{:?}",
        r.eval_inputs
    );
    let done = store
        .complete_task_with("1", "ken", None, Approval::Interactive)
        .unwrap();
    assert_eq!(done.meta.status, TaskStatus::Done);
    let _ = fs::remove_dir_all(&tmp);
}

/// RED (Sourcery PR #2, #2): `git hash-object` ignores the mode, so making the pinned
/// script executable after claim went unnoticed.
#[cfg(unix)]
#[test]
fn test_making_the_script_executable_after_claim_is_a_change() {
    use std::os::unix::fs::PermissionsExt;
    if !git_available() {
        return;
    }
    let (store, tmp) = hardening_store("pin-mode");
    let script = Path::new(&store.config.vault_path).join("check.sh");
    fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o644)).unwrap();
    store
        .add_task("p1 work", None, vec![], Some("./check.sh".into()), None)
        .unwrap();
    store.claim_task("1", "claude", None, false).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let r = store
        .submit_for_review("1", "claude", None, None)
        .unwrap()
        .meta
        .receipt
        .unwrap();
    assert!(
        r.check_changed
            .as_deref()
            .is_some_and(|c| c.contains("check.sh")),
        "{:?}",
        r.check_changed
    );
    let _ = fs::remove_dir_all(&tmp);
}

/// RED (Sourcery PR #2, #1): closing your own task directly after accepting a changed check
/// recorded a plain `interactive` approval, hiding the acceptance.
#[test]
fn test_direct_close_records_an_accepted_check_change() {
    let (store, tmp) = hardening_store("pin-direct");
    store
        .add_task("p1 mine", None, vec![], Some("exit 0".into()), None)
        .unwrap();
    store.claim_task("1", "ken", None, false).unwrap();
    store
        .update_task(
            "1",
            None,
            None,
            None,
            None,
            vec![],
            Some("true".into()),
            None,
        )
        .unwrap();
    let card = store.find_task("1").unwrap();
    assert!(
        store
            .check_change(&card)
            .is_some_and(|c| c.contains("eval"))
    );
    assert!(
        store
            .complete_task_with("1", "ken", None, Approval::Interactive)
            .is_err()
    );
    let done = store
        .complete_task_confirmed("1", "ken", None, Approval::Interactive, None, true)
        .unwrap();
    assert_eq!(
        done.meta.approval.as_deref(),
        Some("interactive (accepted changed check)")
    );
    let _ = fs::remove_dir_all(&tmp);
}

/// Issue #3: Pinned eval check also pins files a script sources or imports.
#[test]
fn test_editing_a_sourced_helper_script_after_claim_blocks_sign_off() {
    let (store, tmp) = hardening_store("pin-sourced");
    let scripts_dir = Path::new(&store.config.vault_path).join("scripts");
    fs::create_dir_all(&scripts_dir).unwrap();
    fs::write(scripts_dir.join("check.sh"), ". ./scripts/helper.sh\n").unwrap();
    fs::write(scripts_dir.join("helper.sh"), "exit 1\n").unwrap();

    store
        .add_task(
            "p1 work",
            None,
            vec![],
            Some("sh scripts/check.sh".into()),
            None,
        )
        .unwrap();
    let claimed = store.claim_task("1", "claude", None, false).unwrap();
    assert!(
        claimed
            .meta
            .claimed_eval_inputs
            .contains_key("scripts/check.sh")
    );
    assert!(
        claimed
            .meta
            .claimed_eval_inputs
            .contains_key("scripts/helper.sh")
    );

    // Agent edits the sourced helper script without touching check.sh.
    fs::write(scripts_dir.join("helper.sh"), "exit 0\n").unwrap();
    let r = store
        .submit_for_review("1", "claude", None, None)
        .unwrap()
        .meta
        .receipt
        .unwrap();
    assert!(
        r.check_changed
            .as_deref()
            .is_some_and(|c| c.contains("scripts/helper.sh")),
        "expected helper.sh change to be flagged, got: {:?}",
        r.check_changed
    );
    let err = store
        .complete_task_with("1", "ken", None, Approval::Interactive)
        .unwrap_err()
        .to_string();
    assert!(err.contains("scripts/helper.sh"), "{}", err);
    let _ = fs::remove_dir_all(&tmp);
}

/// Issue #4: Eval input pinning is computed in-process and works without `git` on PATH.
#[test]
fn test_eval_inputs_pinned_without_git() {
    let (store, tmp) = hardening_store("pin-nogit");
    write_check(&store, "exit 0\n");
    let inputs1 = store.eval_inputs(Some("sh check.sh"));
    assert!(inputs1.contains_key("check.sh"));

    write_check(&store, "exit 1\n");
    let inputs2 = store.eval_inputs(Some("sh check.sh"));
    assert_ne!(inputs1.get("check.sh"), inputs2.get("check.sh"));
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_archive_many_and_lockfile_cleanup() {
    let (store, tmp) = hardening_store("archive-many-locks");
    for title in ["p1 Task one", "p1 Task two", "p1 Task three"] {
        store.add_task(title, None, vec![], None, None).unwrap();
    }
    for id in ["1", "2", "3"] {
        store.submit_for_review(id, "ken", None, None).unwrap();
        store
            .complete_task_with(id, "ken", None, Approval::Interactive)
            .unwrap();
    }

    // Verify CardLock unlinked its `.lock` file automatically on drop.
    let lingering_locks: Vec<_> = fs::read_dir(&store.tasks_path)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.ends_with(".lock")))
        .collect();
    assert!(
        lingering_locks.is_empty(),
        "expected CardLock drop to unlink .lock files, found: {:?}",
        lingering_locks
    );

    // Seed a legacy orphan `.tk-002-task-two.lock` to verify sweep_orphan_locks cleans it up.
    let legacy_lock = store.tasks_path.join(".tk-002-task-two.lock");
    fs::write(&legacy_lock, b"").unwrap();

    let archived = store
        .archive_many(&["2".to_string(), "3".to_string()])
        .unwrap();
    assert_eq!(archived, vec!["tk-002".to_string(), "tk-003".to_string()]);
    assert!(
        !legacy_lock.exists(),
        "expected legacy lock file to be swept on archive"
    );

    // Prune reaches both live `done` cards (tk-001) and archived cards (tk-002).
    let tk1_path = PathBuf::from(store.find_task("1").unwrap().path);
    let tk2_path = store.tasks_path.join("archive").join(
        PathBuf::from(store.find_archived("2").unwrap().path)
            .file_name()
            .unwrap(),
    );
    let pruned = store
        .prune_many(&["1".to_string(), "2".to_string()])
        .unwrap();
    assert_eq!(pruned, vec!["tk-001".to_string(), "tk-002".to_string()]);
    assert!(!tk1_path.exists());
    assert!(!tk2_path.exists(), "archived card must be pruned");
    assert!(!CardLock::lock_path_for(&tk1_path).exists());
    assert!(
        store.find_archived("3").is_some(),
        "unlisted archived card must survive"
    );

    // Pruned cards leave the store consistent, and their IDs are never reused.
    assert!(store.integrity_report().unwrap().is_empty());
    assert_eq!(
        store
            .add_task("p1 Task four", None, vec![], None, None)
            .unwrap()
            .meta
            .id,
        "tk-004"
    );

    // Bare prune removes every remaining done/archived card, including tk-003 in archive/.
    assert_eq!(store.prune_many(&[]).unwrap(), vec!["tk-003".to_string()]);
    let _ = fs::remove_dir_all(&tmp);
}

fn events_text(store: &TaskStore) -> String {
    fs::read_to_string(store.tasks_path.join(".events.jsonl")).unwrap_or_default()
}

#[cfg(unix)]
#[test]
fn test_eval_dry_run_reports_without_running() {
    let (store, tmp) = hardening_store("dry-run-clean");
    write_check(&store, "touch ran.marker\nexit 0\n");
    store
        .add_task("p1 work", None, vec![], Some("sh check.sh".into()), None)
        .unwrap();
    let before_events = events_text(&store);
    let before_card = fs::read_to_string(store.find_task("1").unwrap().path).unwrap();

    let report = store.dry_run_eval("1").unwrap();

    assert!(report.ok(), "unexpected problems: {:?}", report.problems);
    assert!(
        report.notes.iter().any(|n| n.contains("check.sh")),
        "pinned script not reported: {:?}",
        report.notes
    );
    assert!(
        !tmp.join("ran.marker").exists(),
        "dry-run executed the eval"
    );
    assert_eq!(
        events_text(&store),
        before_events,
        "dry-run logged an event"
    );
    let after_card = fs::read_to_string(store.find_task("1").unwrap().path).unwrap();
    assert_eq!(after_card, before_card, "dry-run wrote the card");
    let _ = fs::remove_dir_all(&tmp);
}

#[cfg(unix)]
#[test]
fn test_eval_dry_run_flags_bad_gates() {
    let (store, tmp) = hardening_store("dry-run-bad");
    // 1: absolute cd target and named script both missing
    store
        .add_task(
            "p1 missing paths",
            None,
            vec![],
            Some("cd /nonexistent-tkmd-dry-run-dir && sh missing.sh".into()),
            None,
        )
        .unwrap();
    // 2: the command itself does not parse
    store
        .add_task("p1 bad syntax", None, vec![], Some("if then".into()), None)
        .unwrap();
    // 3: the named script does not parse
    write_check(&store, "if then fi\n");
    store
        .add_task(
            "p1 bad script",
            None,
            vec![],
            Some("sh check.sh".into()),
            None,
        )
        .unwrap();
    // 4: unknown program
    store
        .add_task(
            "p1 unknown program",
            None,
            vec![],
            Some("tkmd-no-such-program-xyz --run".into()),
            None,
        )
        .unwrap();

    let r1 = store.dry_run_eval("1").unwrap();
    assert!(
        r1.problems
            .iter()
            .any(|p| p.contains("cd /nonexistent-tkmd-dry-run-dir"))
    );
    assert!(r1.problems.iter().any(|p| p.contains("missing.sh")));
    let r2 = store.dry_run_eval("2").unwrap();
    assert!(
        r2.problems.iter().any(|p| p.contains("does not parse")),
        "{:?}",
        r2.problems
    );
    let r3 = store.dry_run_eval("3").unwrap();
    assert!(
        r3.problems
            .iter()
            .any(|p| p.contains("check.sh") && p.contains("does not parse")),
        "{:?}",
        r3.problems
    );
    let r4 = store.dry_run_eval("4").unwrap();
    assert!(
        r4.problems
            .iter()
            .any(|p| p.contains("tkmd-no-such-program-xyz")),
        "{:?}",
        r4.problems
    );
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_eval_dry_run_without_eval_follows_require_eval() {
    let (mut store, tmp) = hardening_store("dry-run-no-eval");
    store
        .add_task("p1 no check", None, vec![], None, None)
        .unwrap();
    let lenient = store.dry_run_eval("1").unwrap();
    assert!(lenient.ok());
    assert!(lenient.notes.iter().any(|n| n.contains("no eval")));
    store.config.policy.require_eval = true;
    let strict = store.dry_run_eval("1").unwrap();
    assert!(!strict.ok());
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_details_replaces_spec_and_keeps_sections() {
    let (store, tmp) = hardening_store("update-details");
    store
        .add_task(
            "p1 spec task",
            Some("old spec text"),
            vec!["item one".into()],
            Some("exit 0".into()),
            None,
        )
        .unwrap();
    let card = store
        .update_task(
            "1",
            None,
            None,
            None,
            None,
            vec![],
            None,
            Some("new spec\n\nsecond paragraph".into()),
        )
        .unwrap();
    assert!(card.body.contains("new spec\n\nsecond paragraph"));
    assert!(!card.body.contains("old spec text"));
    assert!(
        card.body.starts_with("# spec task\n\nnew spec"),
        "{}",
        card.body
    );
    assert!(
        card.body
            .contains("## Definition of Done (DoD)\n- [ ] item one")
    );
    assert!(card.body.contains("## Automated Eval"));
    assert!(card.body.contains("details replaced"));
    let saved = store.find_task("1").unwrap();
    assert_eq!(saved.body.trim_end(), card.body.trim_end());

    let cleared = store
        .update_task(
            "1",
            None,
            None,
            None,
            None,
            vec![],
            None,
            Some(String::new()),
        )
        .unwrap();
    assert!(!cleared.body.contains("new spec"));
    let saved = store.find_task("1").unwrap();
    assert_eq!(saved.body.trim_end(), cleared.body.trim_end());
    assert!(
        cleared
            .body
            .starts_with("# spec task\n\n## Definition of Done"),
        "{}",
        cleared.body
    );
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_update_details_invalidates_pending_review() {
    let (store, tmp) = hardening_store("update-details-review");
    store
        .add_task(
            "p1 reviewed",
            Some("spec"),
            vec![],
            Some("exit 0".into()),
            None,
        )
        .unwrap();
    store.claim_task("1", "claude", None, false).unwrap();
    store.submit_for_review("1", "claude", None, None).unwrap();
    let card = store
        .update_task(
            "1",
            None,
            None,
            None,
            None,
            vec![],
            None,
            Some("changed spec".into()),
        )
        .unwrap();
    assert_eq!(card.meta.status, TaskStatus::InProgress);
    assert!(card.meta.receipt.is_none());
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_replace_details_without_sections() {
    assert_eq!(replace_details("# T\n\nold\n", "new"), "# T\n\nnew\n");
    assert_eq!(replace_details("# T\n", "new"), "# T\n\nnew\n");
    assert_eq!(replace_details("# T\n\nold\n", ""), "# T\n");
}

#[test]
fn test_assign_on_claim_leaves_new_tasks_unassigned() {
    let (mut store, tmp) = hardening_store("assign-on-claim");
    // Default policy: new tasks belong to the configured owner, as before.
    let owned = store
        .add_task("p1 default owner", None, vec![], None, None)
        .unwrap();
    assert_eq!(owned.meta.owner, "ken");

    store.config.policy.assign_on_claim = true;
    let open = store
        .add_task("p1 nobody yet", None, vec![], None, None)
        .unwrap();
    assert_eq!(open.meta.owner, "");
    let created = store.read_events().unwrap();
    let created = created
        .iter()
        .rfind(|e| e.id == open.meta.id && e.action == "created")
        .unwrap();
    assert_eq!(created.actor, "ken", "creation event lost its actor");
    assert!(open.body.contains("Created unassigned"));
    let explicit = store
        .add_task("p1 for codex @codex", None, vec![], None, None)
        .unwrap();
    assert_eq!(explicit.meta.owner, "codex");

    let claimed = store
        .claim_task(&open.meta.id, "claude", None, false)
        .unwrap();
    assert_eq!(claimed.meta.owner, "claude");
    let _ = fs::remove_dir_all(&tmp);
}

#[cfg(unix)]
#[test]
fn test_eval_dry_run_checks_inside_named_script() {
    let (store, tmp) = hardening_store("dry-run-in-script");
    write_check(
        &store,
        "#!/bin/sh\nhelper() { true; }\nset -e\ncd /nonexistent-tkmd-script-dir && helper\n",
    );
    store
        .add_task(
            "p1 scripted",
            None,
            vec![],
            Some("sh check.sh".into()),
            None,
        )
        .unwrap();
    let report = store.dry_run_eval("1").unwrap();
    assert!(
        report
            .problems
            .iter()
            .any(|p| p.contains("in `check.sh`") && p.contains("/nonexistent-tkmd-script-dir")),
        "{:?}",
        report.problems
    );
    assert!(
        !report.problems.iter().any(|p| p.contains("helper")),
        "script-defined function flagged: {:?}",
        report.problems
    );
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_command_segments_keep_redirections() {
    let segs =
        crate::store::eval::command_segments_for_test("make > log 2>&1 && echo hi >&2; x &> out");
    let expected: Vec<Vec<String>> = [
        vec!["make", ">", "log", "2>&1"],
        vec!["echo", "hi", ">&2"],
        vec!["x", "&>", "out"],
    ]
    .iter()
    .map(|seg| seg.iter().map(|w| w.to_string()).collect())
    .collect();
    assert_eq!(segs, expected);
}
