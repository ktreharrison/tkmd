pub mod config;
pub mod doctor;
pub mod eval;
pub mod lock;
pub mod receipt;
#[cfg(test)]
mod tests;

pub use config::{
    AppConfig, Approval, Policy, ScopeSelection, detect_editor, detect_git_root,
    detect_installed_editors, detect_local_store, detect_obsidian_vault,
};
pub use lock::CardLock;

use crate::model::{
    CardScope, TaskCard, TaskFrontmatter, TaskStatus, agent_continue_cmd, agent_resume_cmd,
    is_human, set_human_owner,
};
use crate::todotxt::parse_todotxt;
use anyhow::{Context, Result, bail};
use chrono::{Datelike, Local};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct TaskStore {
    pub config: AppConfig,
    pub tasks_path: PathBuf,
    pub daily_path: PathBuf,
    pub scope: CardScope,
    pub peer: Option<Box<TaskStore>>,
    pub active_scope: ScopeSelection,
    pub uninitialized_git_repo: Option<PathBuf>,
    pub is_uninitialized_local: bool,
}

impl TaskStore {
    pub fn config_file_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let modern = PathBuf::from(&home).join(".config/tkmd/config.toml");
        if modern.is_file() {
            return modern;
        }
        let legacy = PathBuf::from(&home).join(".config/taskmd/config.toml");
        if legacy.is_file() {
            return legacy;
        }
        modern
    }

    pub fn load_global_store() -> Result<Self> {
        let cfg_path = Self::config_file_path();

        let config: AppConfig = if cfg_path.exists() {
            let raw = fs::read_to_string(&cfg_path)
                .with_context(|| format!("Failed reading {:?}", cfg_path))?;
            toml::from_str(&raw).with_context(|| format!("Invalid tkmd config {:?}", cfg_path))?
        } else {
            let def = AppConfig::default();
            if let Some(parent) = cfg_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(&cfg_path, toml::to_string_pretty(&def).unwrap_or_default());
            def
        };

        let mut config = config;
        set_human_owner(&config.default_owner);
        // Check for runtime environment variable override (TASKMD_DIR or TASKMD_PATH)
        if let Ok(env_path) = std::env::var("TASKMD_DIR").or_else(|_| std::env::var("TASKMD_PATH"))
        {
            config.vault_path = env_path;
            config.tasks_dir = String::new();
        }

        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let expanded_vault = if config.vault_path.starts_with("~/") {
            format!("{}/{}", home, &config.vault_path[2..])
        } else {
            config.vault_path.clone()
        };

        let vault = PathBuf::from(&expanded_vault);
        let tasks_path = if config.tasks_dir.is_empty() {
            vault.clone()
        } else {
            vault.join(&config.tasks_dir)
        };
        let daily_path = if config.daily_dir.is_empty() {
            vault.clone()
        } else {
            vault.join(&config.daily_dir)
        };
        fs::create_dir_all(&tasks_path)?;

        Ok(Self {
            config,
            tasks_path,
            daily_path,
            scope: CardScope::Global,
            peer: None,
            active_scope: ScopeSelection::Global,
            uninitialized_git_repo: None,
            is_uninitialized_local: false,
        })
    }

    pub fn load_local_store(root: &Path) -> Result<Self> {
        let user = std::env::var("USER").unwrap_or_else(|_| "owner".to_string());
        let cfg_file = if root.join(".tkmd.toml").is_file() {
            Some(root.join(".tkmd.toml"))
        } else if root.join("tkmd.toml").is_file() {
            Some(root.join("tkmd.toml"))
        } else if root.join(".tkmd/config.toml").is_file() {
            Some(root.join(".tkmd/config.toml"))
        } else if root.join(".taskmd.toml").is_file() {
            Some(root.join(".taskmd.toml"))
        } else if root.join("taskmd.toml").is_file() {
            Some(root.join("taskmd.toml"))
        } else if root.join(".taskmd/config.toml").is_file() {
            Some(root.join(".taskmd/config.toml"))
        } else {
            None
        };

        let config: AppConfig = if let Some(p) = cfg_file {
            let raw = fs::read_to_string(&p)
                .with_context(|| format!("Failed reading local config {:?}", p))?;
            toml::from_str(&raw).with_context(|| format!("Invalid tkmd config {:?}", p))?
        } else {
            AppConfig {
                mode: "cli".to_string(),
                vault_path: root.to_string_lossy().to_string(),
                tasks_dir: "tasks".to_string(),
                base_file: String::new(),
                daily_dir: String::new(),
                default_owner: user,
                obsidian_vault_name: None,
                editor: Some(detect_editor()),
                policy: Policy::default(),
            }
        };

        set_human_owner(&config.default_owner);
        let root_path = PathBuf::from(&config.vault_path);
        let tasks_path = if config.tasks_dir.is_empty() {
            root_path.clone()
        } else {
            root_path.join(&config.tasks_dir)
        };
        let daily_path = if config.daily_dir.is_empty() {
            root_path.clone()
        } else {
            root_path.join(&config.daily_dir)
        };
        Ok(Self {
            config,
            tasks_path,
            daily_path,
            scope: CardScope::Local,
            peer: None,
            active_scope: ScopeSelection::Local,
            uninitialized_git_repo: None,
            is_uninitialized_local: false,
        })
    }

    pub fn load_with_scope(scope_sel: ScopeSelection) -> Result<Self> {
        let global = Self::load_global_store()?;
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let global_vault = PathBuf::from(&global.config.vault_path);
        let local_root = detect_local_store(&cwd, &global_vault);

        match scope_sel {
            ScopeSelection::Global => {
                let mut store = global;
                store.active_scope = ScopeSelection::Global;
                Ok(store)
            }
            ScopeSelection::Local => {
                let is_uninit = local_root.is_none();
                let root = if let Some(r) = local_root {
                    r
                } else if let Some(gr) = detect_git_root(&cwd) {
                    gr
                } else {
                    cwd.clone()
                };
                let mut local = Self::load_local_store(&root)?;
                local.peer = Some(Box::new(global));
                local.active_scope = ScopeSelection::Local;
                local.is_uninitialized_local = is_uninit;
                Ok(local)
            }
            ScopeSelection::All => {
                if let Some(r) = local_root.or_else(|| detect_git_root(&cwd)) {
                    let mut local = Self::load_local_store(&r)?;
                    local.peer = Some(Box::new(global));
                    local.active_scope = ScopeSelection::All;
                    Ok(local)
                } else {
                    let mut store = global;
                    store.active_scope = ScopeSelection::All;
                    Ok(store)
                }
            }
            ScopeSelection::Auto => {
                if let Some(r) = local_root {
                    let mut local = Self::load_local_store(&r)?;
                    local.peer = Some(Box::new(global));
                    local.active_scope = ScopeSelection::Auto;
                    Ok(local)
                } else {
                    let mut store = global;
                    store.active_scope = ScopeSelection::Auto;
                    store.uninitialized_git_repo = detect_git_root(&cwd);
                    Ok(store)
                }
            }
        }
    }

    #[allow(dead_code)]
    pub fn load() -> Result<Self> {
        Self::load_with_scope(ScopeSelection::Auto)
    }

    pub fn open_in_editor(&self, target: &Path) -> Result<()> {
        let ed = self
            .config
            .editor
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(detect_editor);
        let parts: Vec<&str> = ed.split_whitespace().collect();
        if let Some((bin, args)) = parts.split_first() {
            Command::new(bin).args(args).arg(target).status()?;
        }
        Ok(())
    }

    /// Initialize either an Obsidian Vault setup (recommended) or a Standalone CLI setup.
    pub fn init_workspace(
        mode: &str,
        target_path: &str,
        owner: &str,
        base_file: &str,
        daily_dir: &str,
        editor: Option<String>,
        is_local: bool,
    ) -> Result<(AppConfig, PathBuf)> {
        let expanded = if let Some(stripped) = target_path.strip_prefix("~/") {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            format!("{}/{}", home, stripped)
        } else {
            target_path.to_string()
        };
        let root = PathBuf::from(&expanded);
        let tasks_dir = "tasks".to_string();
        fs::create_dir_all(root.join(&tasks_dir))?;

        let vault_name = if mode == "obsidian" {
            root.file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        } else {
            None
        };

        if mode == "obsidian" {
            // 1. Ensure bases/tasks.base exists
            let base_path = root.join(base_file);
            if let Some(parent) = base_path.parent() {
                fs::create_dir_all(parent)?;
            }
            if !base_path.exists() {
                let base_yaml = format!(
                    r#"filters:
  and:
    - file.inFolder("tasks")
    - type == "task"
views:
  - type: cards
    name: Kanban Board
    groupBy:
      property: status
      direction: ASC
    order:
      - file.name
      - priority
      - owner
      - session_id
      - sessions
      - eval_status
      - project
      - due
    sort:
      - property: priority
        direction: ASC
  - type: table
    name: Pending Close (Sign-Off Queue)
    filters:
      and:
        - status == "pending-close"
    order:
      - id
      - file.name
      - owner
      - session_id
      - sessions
      - eval_status
      - dod
      - updated
    sort:
      - property: priority
        direction: ASC
  - type: table
    name: {owner}'s Queue
    filters:
      and:
        - owner == "{owner}"
        - status != "done"
    order:
      - id
      - file.name
      - priority
      - status
      - dod
      - due
    sort:
      - property: priority
        direction: ASC
  - type: table
    name: Agent Queues
    filters:
      and:
        - owner != "{owner}"
        - status != "done"
    order:
      - id
      - file.name
      - owner
      - session_id
      - sessions
      - priority
      - status
      - eval_status
      - eval
    sort:
      - property: priority
        direction: ASC
  - type: table
    name: Completed Tasks
    filters:
      and:
        - status == "done"
    order:
      - id
      - file.name
      - owner
      - eval_status
      - updated
    sort:
      - property: updated
        direction: DESC
"#,
                    owner = owner
                );
                fs::write(&base_path, base_yaml)?;
            }

            // 2. Safely merge property types into .obsidian/types.json if .obsidian exists
            let obs_dir = root.join(".obsidian");
            if obs_dir.exists() {
                let types_path = obs_dir.join("types.json");
                let mut root_json: serde_json::Value = if types_path.exists() {
                    let raw = fs::read_to_string(&types_path).unwrap_or_else(|_| "{}".to_string());
                    serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({"types": {}}))
                } else {
                    serde_json::json!({"types": {}})
                };
                if root_json.get("types").is_none() {
                    root_json["types"] = serde_json::json!({});
                }
                if let Some(types_obj) = root_json["types"].as_object_mut() {
                    let defaults = [
                        ("id", "text"),
                        ("status", "text"),
                        ("priority", "text"),
                        ("owner", "text"),
                        ("session_id", "text"),
                        ("project", "text"),
                        ("due", "date"),
                        ("dod", "multitext"),
                        ("eval", "text"),
                        ("eval_status", "text"),
                    ];
                    for (k, v) in defaults {
                        types_obj
                            .entry(k.to_string())
                            .or_insert_with(|| serde_json::Value::String(v.to_string()));
                    }
                }
                let _ = fs::write(&types_path, serde_json::to_string_pretty(&root_json)?);
            }
        }

        let cfg = AppConfig {
            mode: mode.to_string(),
            vault_path: expanded,
            tasks_dir,
            base_file: if mode == "obsidian" {
                base_file.to_string()
            } else {
                String::new()
            },
            daily_dir: if mode == "obsidian" {
                daily_dir.to_string()
            } else {
                String::new()
            },
            default_owner: owner.to_string(),
            obsidian_vault_name: vault_name,
            editor: editor.or_else(|| Some(detect_editor())),
            policy: Policy::default(),
        };

        let cfg_path = if is_local {
            root.join(".tkmd.toml")
        } else {
            Self::config_file_path()
        };
        if let Some(parent) = cfg_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&cfg_path, toml::to_string_pretty(&cfg)?)?;

        Ok((cfg, cfg_path))
    }

    pub fn list_store_cards(&self) -> Result<Vec<TaskCard>> {
        let mut cards = Vec::new();
        if !self.tasks_path.exists() {
            return Ok(cards);
        }
        for entry in fs::read_dir(&self.tasks_path)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if fname.starts_with('.') || fname.eq_ignore_ascii_case("readme.md") {
                continue;
            }
            match Self::read_card(&path) {
                Ok(mut card) => {
                    card.scope = self.scope;
                    cards.push(card);
                }
                Err(e) => warn_unreadable_card(&path, &e),
            }
        }
        cards.sort_by(|a, b| a.meta.id.cmp(&b.meta.id));
        Ok(cards)
    }

    pub fn list_all(&self) -> Result<Vec<TaskCard>> {
        let mut cards = self.list_store_cards()?;
        if self.active_scope == ScopeSelection::All
            && let Some(ref peer) = self.peer
        {
            let mut peer_cards = peer.list_store_cards()?;
            cards.append(&mut peer_cards);
            cards.sort_by(|a, b| a.meta.id.cmp(&b.meta.id));
        }
        Ok(cards)
    }

    pub fn read_card(path: &Path) -> Result<TaskCard> {
        let raw = fs::read_to_string(path)?;
        if !raw.starts_with("---\n") && !raw.starts_with("---\r\n") {
            bail!("Missing YAML frontmatter in {:?}", path);
        }
        let end_idx = raw[4..]
            .find("\n---")
            .context("Unclosed YAML frontmatter")?
            + 4;
        let yaml_str = &raw[4..end_idx];
        let body = raw[end_idx + 4..]
            .trim_start_matches(['\r', '\n'])
            .to_string();

        let meta: TaskFrontmatter = serde_yaml::from_str(yaml_str)
            .with_context(|| format!("Invalid YAML in {:?}", path))?;

        Ok(TaskCard {
            meta,
            body,
            path: path.to_string_lossy().to_string(),
            scope: CardScope::default(),
        })
    }

    /// Acquire an advisory exclusive lock on a task card to serialize concurrent agent updates.
    pub fn lock_card(path: &Path) -> Result<CardLock> {
        CardLock::acquire_exclusive(path)
    }

    pub fn write_card(&self, card: &TaskCard) -> Result<()> {
        let mut c = card.clone();
        Self::sync_card_sessions(&mut c);
        let yaml = serde_yaml::to_string(&c.meta)?;
        let content = format!("---\n{}---\n\n{}\n", yaml, c.body.trim());

        // POSIX atomic write: write to sibling temp file, then atomic rename
        let target_path = Path::new(&c.path);
        let parent = target_path.parent().unwrap_or_else(|| Path::new("."));
        let file_stem = target_path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("card");
        let tmp_path = parent.join(format!(".{}.tmp.{}", file_stem, std::process::id()));

        fs::write(&tmp_path, content)?;
        fs::rename(&tmp_path, target_path)?;
        Ok(())
    }

    /// Record or update an agent session entry on the card (`meta.session_id` + `meta.sessions`).
    pub fn record_session_on_card(card: &mut TaskCard, actor: &str, sid: Option<&str>) {
        let clean_actor = actor.trim().trim_start_matches('@').to_lowercase();
        let clean_sid = sid.map(str::trim).filter(|s| !s.is_empty());

        if let Some(s) = clean_sid {
            card.meta.session_id = Some(s.to_string());
        }

        // Determine which agent CLI owns this session
        let agent = if !is_human(&clean_actor) && clean_actor != "agent" {
            clean_actor
        } else if let Some(s) = clean_sid
            && let Some(first_word) = s.split_whitespace().next()
            && matches!(
                first_word.to_lowercase().as_str(),
                "agy" | "claude" | "codex" | "hermes" | "opencode" | "gemini" | "pi"
            )
        {
            first_word.to_lowercase()
        } else if !is_human(&card.meta.owner) && !card.meta.owner.eq_ignore_ascii_case("agent") {
            card.meta.owner.to_lowercase()
        } else {
            return;
        };

        let entry_str = if let Some(s) = clean_sid {
            format!("{}: {}", agent, s)
        } else {
            format!("{}: latest", agent)
        };

        // Update existing entry for the same agent or append a new agent session entry
        let agent_prefix = format!("{}:", agent);
        let prefix_latest = format!("{}: latest", agent);
        if let Some(existing) = card.meta.sessions.iter_mut().find(|e| {
            *e == &entry_str
                || (clean_sid.is_some() && (*e == &prefix_latest || e.starts_with(&agent_prefix)))
        }) {
            if clean_sid.is_some() || *existing == prefix_latest {
                *existing = entry_str;
            }
        } else if (!card
            .meta
            .sessions
            .iter()
            .any(|e| e.starts_with(&agent_prefix))
            || clean_sid.is_some())
            && !card.meta.sessions.contains(&entry_str)
        {
            card.meta.sessions.push(entry_str);
        }
    }

    /// Synchronize `meta.sessions` and the `## Agent Sessions` Markdown section inside `card.body`
    /// so a human viewing the `.md` file in Obsidian or `$EDITOR` always sees copy-pasteable
    /// `resume` and `continue` commands across every agent that touched the task.
    pub fn sync_card_sessions(card: &mut TaskCard) {
        let owner_copy = card.meta.owner.clone();
        let sid_copy = card.meta.session_id.clone();
        let sid_already_assigned = sid_copy.as_deref().is_some_and(|s| {
            card.meta.sessions.iter().any(|entry| {
                entry
                    .split_once(':')
                    .is_some_and(|(_, es)| es.trim() == s.trim())
            })
        });
        if !sid_already_assigned {
            Self::record_session_on_card(card, &owner_copy, sid_copy.as_deref());
        } else {
            Self::record_session_on_card(card, &owner_copy, None);
        }

        if card.meta.sessions.is_empty() {
            return;
        }

        let mut md_lines = Vec::new();
        md_lines.push("## Agent Sessions".to_string());
        for entry in &card.meta.sessions {
            let (agent, raw_sid) = entry
                .split_once(':')
                .map(|(a, s)| (a.trim(), s.trim()))
                .unwrap_or((card.meta.owner.as_str(), entry.as_str()));

            let resume_opt = agent_resume_cmd(agent, Some(raw_sid));
            let cont_opt = agent_continue_cmd(agent);

            match (resume_opt, cont_opt) {
                (Some(res), Some(cont)) if res != cont => {
                    md_lines.push(format!(
                        "- **@{}** (`{}`) — resume: `{}` · continue: `{}`",
                        agent, raw_sid, res, cont
                    ));
                }
                (Some(res), _) => {
                    md_lines.push(format!(
                        "- **@{}** (`{}`) — continue: `{}`",
                        agent, raw_sid, res
                    ));
                }
                _ => {
                    md_lines.push(format!("- **@{}**: `{}`", agent, raw_sid));
                }
            }
        }
        let section_block = md_lines.join("\n");

        if let Some(start_idx) = card.body.find("## Agent Sessions") {
            let after_header = &card.body[start_idx + "## Agent Sessions".len()..];
            if let Some(next_sec_rel) = after_header.find("\n## ") {
                let end_idx = start_idx + "## Agent Sessions".len() + next_sec_rel;
                card.body = format!(
                    "{}{}\n{}",
                    &card.body[..start_idx],
                    section_block,
                    &card.body[end_idx..]
                );
            } else {
                card.body = format!("{}{}\n", &card.body[..start_idx], section_block);
            }
        } else if let Some(exec_idx) = card.body.find("\n## Execution Log") {
            card.body = format!(
                "{}\n\n{}\n\n{}",
                card.body[..exec_idx].trim_end(),
                section_block,
                &card.body[exec_idx + 1..]
            );
        } else {
            card.body = format!("{}\n\n{}\n", card.body.trim_end(), section_block);
        }
    }

    /// Next free id. Counts live cards, archived cards and every id in the event log, so a
    /// deleted or archived task's id is never handed to a new task.
    pub fn next_id(&self) -> Result<String> {
        let num = |id: &str| id.strip_prefix("tk-").and_then(|n| n.parse::<u32>().ok());
        // File names carry the id (`tk-NNN-slug.md`), so no card needs to be parsed.
        let mut max_num = Self::max_id_in_dir(&self.tasks_path)
            .max(Self::max_id_in_dir(&self.tasks_path.join("archive")));
        if self.active_scope == ScopeSelection::All
            && let Some(peer) = &self.peer
        {
            max_num = max_num.max(Self::max_id_in_dir(&peer.tasks_path));
        }
        if let Ok(raw) = fs::read_to_string(self.tasks_path.join(".events.jsonl")) {
            for line in raw.lines() {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line)
                    && let Some(n) = v.get("id").and_then(|i| i.as_str()).and_then(num)
                {
                    max_num = max_num.max(n);
                }
            }
        }
        Ok(format!("tk-{:03}", max_num + 1))
    }

    /// Auto-detect agent session ID from common environment variables if not explicitly passed.
    pub fn detect_env_session_id() -> Option<String> {
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
            if let Ok(val) = std::env::var(var) {
                let t = val.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
        None
    }

    pub fn add_task(
        &self,
        raw_input: &str,
        details: Option<&str>,
        dod: Vec<String>,
        eval: Option<String>,
        session_id: Option<String>,
    ) -> Result<TaskCard> {
        let mut parsed = parse_todotxt(raw_input, &self.config.default_owner);
        if self.config.policy.assign_on_claim && parse_todotxt(raw_input, "").owner.is_empty() {
            parsed.owner = String::new();
        }
        if parsed.title.is_empty() {
            bail!("Task title cannot be empty");
        }
        // Serialize id allocation across processes: without this, racing agents reuse ids.
        fs::create_dir_all(&self.tasks_path)?;
        let _store_lock = Self::lock_card(&self.tasks_path)?;
        let id = self.next_id()?;
        let slug = slugify(&parsed.title);
        let filename = format!("{}-{}.md", id, slug);
        let full_path = self.tasks_path.join(&filename);
        let today = Local::now().format("%Y-%m-%d").to_string();

        let topic = parsed
            .tags
            .iter()
            .filter(|t| *t != "task")
            .cloned()
            .collect::<Vec<_>>();

        let eval_status = if eval.is_some() {
            Some("pending".to_string())
        } else {
            None
        };

        let resolved_session = session_id
            .filter(|s| !s.trim().is_empty())
            .or(parsed.session_id)
            .or_else(Self::detect_env_session_id);

        let initial_status = if !parsed.blocked_by.is_empty() {
            TaskStatus::Blocked
        } else {
            TaskStatus::Ready
        };

        let meta = TaskFrontmatter {
            id: id.clone(),
            title: parsed.title.clone(),
            kind: "task".to_string(),
            status: initial_status,
            priority: parsed.priority,
            owner: parsed.owner.clone(),
            topic,
            tags: parsed.tags,
            project: parsed.project,
            due: parsed.due,
            duration: parsed.duration,
            session_id: resolved_session,
            blocked_by: parsed.blocked_by,
            blocks: Vec::new(),
            sessions: Vec::new(),
            dod: dod.clone(),
            eval: eval.clone(),
            eval_status,
            claimed_eval: None,
            receipt: None,
            approval: None,
            approved_eval: None,
            claimed_eval_inputs: Default::default(),
            check_pinned_at: None,
            approved_eval_inputs: Default::default(),
            created: today.clone(),
            updated: today,
            _organized: true,
        };

        let mut sections = format!("# {}\n", meta.title);
        if let Some(d) = details
            && !d.trim().is_empty()
        {
            sections.push_str(&format!("\n{}\n", d.trim()));
        }
        if !dod.is_empty() {
            sections.push_str("\n## Definition of Done (DoD)\n");
            for item in &dod {
                sections.push_str(&format!("- [ ] {}\n", item));
            }
        }
        if let Some(ev) = &eval {
            sections.push_str(&format!("\n## Automated Eval\n```bash\n{}\n```\n", ev));
        }
        let session_note = meta
            .resume_command()
            .map(|cmd| format!(" (resume: `{}`)", cmd))
            .unwrap_or_default();
        let assignment = if meta.owner.is_empty() {
            "unassigned (the first claim assigns it)".to_string()
        } else {
            format!("and assigned to `{}`", meta.owner)
        };
        sections.push_str(&format!(
            "\n## Execution Log\n- Created {}{}.",
            assignment, session_note
        ));

        let mut card = TaskCard {
            meta,
            body: sections,
            path: full_path.to_string_lossy().to_string(),
            scope: self.scope,
        };
        Self::sync_card_sessions(&mut card);
        self.write_card(&card)?;
        self.log_event(
            &card.meta.id,
            "created",
            &card.meta.owner,
            card.meta.session_id.as_deref(),
            None,
            "ready",
            None,
            &card.meta.title,
        )?;
        Ok(card)
    }

    /// Card files are named `<id>-<slug>.md`, so an id lookup reads one directory listing and
    /// parses one file instead of every card in the store.
    fn card_by_id_fast(&self, id: &str) -> Option<TaskCard> {
        let prefix = format!("{}-", id.to_lowercase());
        let exact = format!("{}.md", id.to_lowercase());
        for entry in fs::read_dir(&self.tasks_path).ok()?.flatten() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if !(name.ends_with(".md") && (name.starts_with(&prefix) || name == exact)) {
                continue;
            }
            if let Ok(mut card) = Self::read_card(&entry.path())
                && card.meta.id.eq_ignore_ascii_case(id)
            {
                card.scope = self.scope;
                return Some(card);
            }
        }
        None
    }

    /// An archived card (`tasks/archive/<id>-*.md`) here or in the peer store.
    fn find_archived(&self, id: &str) -> Option<TaskCard> {
        let id = normalize_id(id)?;
        let prefix = format!("{}-", id);
        let mut dirs = vec![self.tasks_path.join("archive")];
        if let Some(peer) = &self.peer {
            dirs.push(peer.tasks_path.join("archive"));
        }
        for dir in dirs {
            for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().to_lowercase();
                if name.starts_with(&prefix)
                    && name.ends_with(".md")
                    && let Ok(card) = Self::read_card(&entry.path())
                    && card.meta.id.eq_ignore_ascii_case(&id)
                {
                    return Some(card);
                }
            }
        }
        None
    }

    /// Status of a blocker by id: live card, then archive. `None` means no card exists.
    fn blocker_status(&self, id: &str) -> Option<TaskStatus> {
        self.find_task(id)
            .ok()
            .map(|c| c.meta.status)
            .or_else(|| self.find_archived(id).map(|_| TaskStatus::Archived))
    }

    /// Highest `tk-NNN` number among card file names in `dir` (no card parsing).
    fn max_id_in_dir(dir: &Path) -> u32 {
        let Ok(entries) = fs::read_dir(dir) else {
            return 0;
        };
        entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let id = name.splitn(3, '-').take(2).collect::<Vec<_>>().join("-");
                id.strip_prefix("tk-")
                    .and_then(|n| n.trim_end_matches(".md").parse::<u32>().ok())
            })
            .max()
            .unwrap_or(0)
    }

    /// Resolve a task by id (`7`, `tk-7`, `tk-007`) or by a unique fragment of its file name.
    /// Ids match exactly. A name fragment matching several cards is an error listing them,
    /// so `tk d smoke` can never close the wrong task.
    pub fn find_task(&self, query: &str) -> Result<TaskCard> {
        let q = query.trim().to_lowercase();
        let digits = q.strip_prefix("tk-").unwrap_or(&q);
        let id_query = digits.parse::<u32>().ok().map(|n| format!("tk-{:03}", n));

        if let Some(id) = &id_query
            && let Some(card) = self.card_by_id_fast(id)
        {
            return Ok(card);
        }
        let cards = self.list_store_cards()?;
        if let Some(id) = &id_query {
            // Slow path: a card whose file name no longer starts with its id.
            if let Some(card) = cards
                .into_iter()
                .find(|c| c.meta.id.eq_ignore_ascii_case(id))
            {
                return Ok(card);
            }
        } else if !q.is_empty() {
            let mut matches: Vec<TaskCard> = cards
                .into_iter()
                .filter(|c| {
                    Path::new(&c.path)
                        .file_stem()
                        .and_then(|f| f.to_str())
                        .is_some_and(|f| f.to_lowercase().contains(&q))
                })
                .collect();
            match matches.len() {
                0 => {}
                1 => return Ok(matches.remove(0)),
                _ => bail!(
                    "'{}' matches {} tasks: {}. Use an id",
                    query,
                    matches.len(),
                    matches
                        .iter()
                        .map(|c| format!("{} ({})", c.meta.id, c.meta.title))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        }

        if (self.active_scope == ScopeSelection::Auto || self.active_scope == ScopeSelection::All)
            && let Some(ref peer) = self.peer
        {
            return peer.find_task(query);
        }

        bail!("Task '{}' not found", query)
    }

    /// Claim a task -> `in-progress`.
    /// Refuses tasks under review or closed, and tasks another owner is working on, unless `force`.
    pub fn claim_task(
        &self,
        query: &str,
        owner: &str,
        session_id: Option<String>,
        force: bool,
    ) -> Result<TaskCard> {
        // The graph lock keeps a claim from passing the blocker check while `tk dep` is adding
        // a new blocker to the same task.
        fs::create_dir_all(&self.tasks_path)?;
        let _graph_lock = Self::lock_card(&self.tasks_path)?;
        let found = self.find_task(query)?;
        let _lock = Self::lock_card(Path::new(&found.path))?;
        let mut card = Self::read_card(Path::new(&found.path))?;
        card.scope = found.scope;
        let prev = card.meta.status.as_str().to_string();
        let today = Local::now().format("%Y-%m-%d").to_string();
        let now_ts = Local::now().format("%Y-%m-%d %H:%M").to_string();
        let new_owner = owner.trim().trim_start_matches('@').to_lowercase();

        match card.meta.status {
            TaskStatus::Done | TaskStatus::Archived => {
                bail!("Cannot claim {}: it is already `{}`", card.meta.id, prev);
            }
            TaskStatus::PendingClose if !force => {
                bail!(
                    "Cannot claim {}: it is awaiting sign-off (`pending-close`). Use --force to take it back into work",
                    card.meta.id
                );
            }
            TaskStatus::InProgress
                if !force && !card.meta.owner.eq_ignore_ascii_case(&new_owner) =>
            {
                bail!(
                    "Cannot claim {}: `{}` is working on it. Use --force to take it over",
                    card.meta.id,
                    card.meta.owner
                );
            }
            _ => {}
        }

        let open = self.open_blockers(&card)?;
        if !open.is_empty() {
            bail!(
                "Cannot claim {} — Blocked by open task(s): {}",
                card.meta.id,
                open.join(", ")
            );
        }

        let takeover = if force && !card.meta.owner.eq_ignore_ascii_case(&new_owner) {
            format!(
                " (forced takeover from `{}`, was `{}`)",
                card.meta.owner, prev
            )
        } else if force && prev == "pending-close" {
            " (forced take-back from review)".to_string()
        } else {
            String::new()
        };
        card.meta.status = TaskStatus::InProgress;
        card.meta.owner = new_owner;
        // A new claim starts a new attempt: any earlier review receipt no longer applies.
        card.meta.receipt = None;
        // Pin the check on a fresh claim only. Re-claiming work already in progress or under
        // review keeps the original pin, so "edit the script, re-claim" cannot launder a change.
        if !matches!(prev.as_str(), "in-progress" | "pending-close")
            || card.meta.check_pinned_at.is_none()
        {
            card.meta.claimed_eval = card.meta.eval.clone();
            card.meta.claimed_eval_inputs = self.eval_inputs(card.meta.eval.as_deref());
            card.meta.check_pinned_at = Some(Local::now().to_rfc3339());
        }
        let resolved_sid = session_id
            .filter(|s| !s.trim().is_empty())
            .or_else(Self::detect_env_session_id);
        Self::record_session_on_card(&mut card, owner, resolved_sid.as_deref());
        card.meta.updated = today;
        let session_note = card
            .meta
            .resume_command()
            .map(|cmd| format!(" — resume: `{}`", cmd))
            .unwrap_or_default();
        card.body = format!(
            "{}\n- `{}`: Claimed by `{}` (`in-progress`){}{}.",
            card.body.trim_end(),
            now_ts,
            owner,
            takeover,
            session_note
        );

        Self::sync_card_sessions(&mut card);
        self.write_card(&card)?;
        self.log_event(
            &card.meta.id,
            if takeover.is_empty() {
                "claimed"
            } else {
                "claimed-forced"
            },
            owner,
            card.meta.session_id.as_deref(),
            Some(&prev),
            "in-progress",
            None,
            &card.meta.title,
        )?;
        Ok(card)
    }

    /// Blockers in `card.blocked_by` that are not done or archived. A blocker whose card cannot
    /// be found stays blocking (reported as missing): unknown is not the same as done.
    pub(crate) fn open_blockers(&self, card: &TaskCard) -> Result<Vec<String>> {
        if card.meta.blocked_by.is_empty() {
            return Ok(Vec::new());
        }
        let mut open = Vec::new();
        for bid in &card.meta.blocked_by {
            let status = self.blocker_status(bid);
            match status {
                Some(TaskStatus::Done | TaskStatus::Archived) => {}
                Some(_) => open.push(bid.clone()),
                None => open.push(format!(
                    "{} (missing: its card was deleted; remove it from blocked_by)",
                    bid
                )),
            }
        }
        Ok(open)
    }

    /// The peer store that owns `card`, when it was found through this store's peer. Lifecycle
    /// transitions run in the owning store so its cascade and event log see them.
    pub(crate) fn owning_peer(&self, card: &TaskCard) -> Option<&TaskStore> {
        let peer = self.peer.as_deref()?;
        (card.scope != self.scope && Path::new(&card.path).starts_with(&peer.tasks_path))
            .then_some(peer)
    }

    /// Daily-note entry for a completed task. The completion is already committed, so a
    /// failure here is a warning, never an error that skips work after it.
    pub(crate) fn note_completion(&self, card: &TaskCard, note: Option<&str>) {
        if let Err(e) = self.log_to_daily_note(card, note) {
            eprintln!(
                "warning: {} is done, but the daily note was not updated: {:#}",
                card.meta.id, e
            );
        }
    }

    /// Promote `blocked` tasks whose blockers are now all closed. Locks each card it rewrites.
    pub(crate) fn cascade_unblock(
        &self,
        completed_id: &str,
        today: &str,
        now_ts: &str,
    ) -> Result<()> {
        let all = self.list_all()?;
        // One status snapshot for the whole cascade: re-reading every card per dependent is quadratic.
        let open: std::collections::HashSet<String> = all
            .iter()
            .filter(|c| !matches!(c.meta.status, TaskStatus::Done | TaskStatus::Archived))
            .map(|c| c.meta.id.to_lowercase())
            .filter(|id| !id.eq_ignore_ascii_case(completed_id))
            .collect();
        let known: std::collections::HashSet<String> =
            all.iter().map(|c| c.meta.id.to_lowercase()).collect();
        // A blocker with no card stays blocking (unless another store resolves it as closed).
        let still_blocking = |bid: &String| {
            let b = bid.to_lowercase();
            if b == completed_id.to_lowercase() {
                return false;
            }
            if known.contains(&b) {
                return open.contains(&b);
            }
            !matches!(
                self.blocker_status(bid),
                Some(TaskStatus::Done | TaskStatus::Archived)
            )
        };
        for other in all {
            if !other
                .meta
                .blocked_by
                .iter()
                .any(|b| b.eq_ignore_ascii_case(completed_id))
            {
                continue;
            }
            let _lock = Self::lock_card(Path::new(&other.path))?;
            let Ok(mut other) = Self::read_card(Path::new(&other.path)).map(|mut c| {
                c.scope = other.scope;
                c
            }) else {
                continue;
            };
            if other.meta.status != TaskStatus::Blocked {
                continue;
            }
            let remaining_blockers = other
                .meta
                .blocked_by
                .iter()
                .filter(|bid| still_blocking(bid))
                .count();

            if remaining_blockers == 0 {
                let other_prev = other.meta.status.as_str().to_string();
                other.meta.status = TaskStatus::Ready;
                other.meta.updated = today.to_string();
                other.body = format!(
                    "{}\n- `{}`: Auto-unblocked -> `ready` (blocker `{}` completed).",
                    other.body.trim_end(),
                    now_ts,
                    completed_id
                );
                if let Err(e) = self.write_card(&other) {
                    eprintln!(
                        "warning: could not unblock {}: {:#}. Run `tk doctor`",
                        other.meta.id, e
                    );
                    continue;
                }
                let _ = self.log_event(
                    &other.meta.id,
                    "unblocked",
                    "system",
                    other.meta.session_id.as_deref(),
                    Some(&other_prev),
                    "ready",
                    None,
                    &other.meta.title,
                );
            }
        }
        Ok(())
    }

    /// Update properties on a task card (title, priority, owner, due, dod, eval, details).
    /// `new_details` replaces the text between the `# Title` line and the first `## ` section;
    /// like a DoD change, new details invalidate a pending review.
    #[allow(clippy::too_many_arguments)]
    pub fn update_task(
        &self,
        query: &str,
        new_title: Option<String>,
        new_priority: Option<String>,
        new_owner: Option<String>,
        new_due: Option<String>,
        new_dod: Vec<String>,
        new_eval: Option<String>,
        new_details: Option<String>,
    ) -> Result<TaskCard> {
        let mut card = self.find_task(query)?;
        let _lock = Self::lock_card(Path::new(&card.path))?;
        let orig_scope = card.scope;
        card = Self::read_card(Path::new(&card.path))?;
        card.scope = orig_scope;
        let today = Local::now().format("%Y-%m-%d").to_string();
        let now_ts = Local::now().format("%Y-%m-%d %H:%M").to_string();

        let mut changes = Vec::new();

        if let Some(title) = new_title {
            let old_title = card.meta.title.clone();
            card.meta.title = title.clone();
            let old_h1 = format!("# {}", old_title);
            let new_h1 = format!("# {}", title);
            if card.body.contains(&old_h1) {
                card.body = card.body.replace(&old_h1, &new_h1);
            }
            changes.push(format!("title -> '{}'", title));
        }

        if let Some(p) = new_priority {
            let prio = match p.to_lowercase().as_str() {
                "p0" | "(a)" | "a" => "P0",
                "p1" | "(b)" | "b" => "P1",
                "p2" | "(c)" | "c" => "P2",
                _ => "P1",
            };
            card.meta.priority = prio.to_string();
            changes.push(format!("priority -> {}", prio));
        }

        if let Some(o) = new_owner {
            let clean_owner = o.trim().trim_start_matches('@').to_string();
            card.meta.owner = clean_owner.clone();
            changes.push(format!("owner -> @{}", clean_owner));
        }

        if let Some(d) = new_due {
            card.meta.due = Some(d.clone());
            changes.push(format!("due -> {}", d));
        }

        if !new_dod.is_empty() {
            card.meta.dod = new_dod.clone();
            let dod_lines: Vec<String> = new_dod
                .iter()
                .map(|item| format!("- [ ] {}", item))
                .collect();
            let new_dod_block = format!("## Definition of Done (DoD)\n{}", dod_lines.join("\n"));
            if let Some(dod_idx) = card.body.find("## Definition of Done (DoD)") {
                let after = &card.body[dod_idx + "## Definition of Done (DoD)".len()..];
                if let Some(next_h2) = after.find("\n## ") {
                    let end_idx = dod_idx + "## Definition of Done (DoD)".len() + next_h2;
                    card.body = format!(
                        "{}{}\n{}",
                        &card.body[..dod_idx],
                        new_dod_block,
                        &card.body[end_idx..]
                    );
                } else {
                    card.body = format!("{}{}\n", &card.body[..dod_idx], new_dod_block);
                }
            } else {
                card.body = format!("{}\n\n{}\n", card.body.trim_end(), new_dod_block);
            }
            changes.push(format!("updated {} DoD item(s)", new_dod.len()));
            if card.meta.status == TaskStatus::PendingClose {
                card.meta.status = TaskStatus::InProgress;
                card.meta.receipt = None;
                changes
                    .push("status -> in-progress (review invalidated by DoD change)".to_string());
            }
        }

        if let Some(e) = new_eval {
            card.meta.eval = Some(e.clone());
            card.meta.eval_status = None;
            let eval_block = format!("## Automated Eval\n```bash\n{}\n```", e);
            if let Some(eval_idx) = card.body.find("## Automated Eval") {
                let after = &card.body[eval_idx + "## Automated Eval".len()..];
                if let Some(next_h2) = after.find("\n## ") {
                    let end_idx = eval_idx + "## Automated Eval".len() + next_h2;
                    card.body = format!(
                        "{}{}\n{}",
                        &card.body[..eval_idx],
                        eval_block,
                        &card.body[end_idx..]
                    );
                } else {
                    card.body = format!("{}{}\n", &card.body[..eval_idx], eval_block);
                }
            } else {
                card.body = format!("{}\n\n{}\n", card.body.trim_end(), eval_block);
            }
            changes.push(format!("eval -> '{}'", e));
            if card.meta.approved_eval.as_deref().is_some_and(|a| a != e) {
                card.meta.approved_eval = None;
                changes.push("auto-accept revoked (eval changed)".to_string());
            }
            // Changing the check invalidates any review built on the old one.
            if card.meta.status == TaskStatus::PendingClose {
                card.meta.status = TaskStatus::InProgress;
                card.meta.receipt = None;
                changes
                    .push("status -> in-progress (review invalidated by eval change)".to_string());
            }
        }

        if let Some(details) = new_details {
            card.body = replace_details(&card.body, &details);
            changes.push("details replaced".to_string());
            if card.meta.status == TaskStatus::PendingClose {
                card.meta.status = TaskStatus::InProgress;
                card.meta.receipt = None;
                changes.push(
                    "status -> in-progress (review invalidated by details change)".to_string(),
                );
            }
        }

        if changes.is_empty() {
            bail!(
                "No updates specified. Use --title, --priority, --owner, --due, --dod, --eval, or --details"
            );
        }

        card.meta.updated = today;
        let change_summary = changes.join(", ");
        card.body = format!(
            "{}\n- `{}`: Updated properties: {}.",
            card.body.trim_end(),
            now_ts,
            change_summary
        );

        self.write_card(&card)?;
        Ok(card)
    }

    /// Add dependency: `dependent_id` will be blocked by `blocker_ids`.
    pub fn add_dependency(&self, dependent_id: &str, blocker_ids: &[String]) -> Result<TaskCard> {
        // One graph-wide lock: validation and every card write happen as one transaction, so
        // concurrent edges cannot each pass against the same old graph and form a cycle.
        fs::create_dir_all(&self.tasks_path)?;
        let _graph_lock = Self::lock_card(&self.tasks_path)?;
        let found = self.find_task(dependent_id)?;
        let today = Local::now().format("%Y-%m-%d").to_string();
        let now_ts = Local::now().format("%Y-%m-%d %H:%M").to_string();

        let mut blockers = Vec::new();
        for raw in blocker_ids {
            let blocker = self.find_task(raw)?;
            if blocker.meta.id.eq_ignore_ascii_case(&found.meta.id) {
                bail!("Task cannot depend on itself: {}", found.meta.id);
            }
            blockers.push(blocker);
        }

        // Refuse edges that would close a cycle: walk each blocker's own dependencies, loading
        // only the cards on the path (not the whole store).
        let mut deps_of: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        let target = found.meta.id.to_lowercase();
        for blocker in &blockers {
            let mut stack = vec![(
                blocker.meta.id.to_lowercase(),
                vec![blocker.meta.id.clone()],
            )];
            let mut seen = std::collections::HashSet::new();
            while let Some((node, path)) = stack.pop() {
                if node == target {
                    bail!(
                        "Dependency cycle: {} would wait on {}, which already waits on it ({} -> {})",
                        found.meta.id,
                        blocker.meta.id,
                        path.join(" -> "),
                        found.meta.id
                    );
                }
                if !seen.insert(node.clone()) {
                    continue;
                }
                let next = deps_of.entry(node.clone()).or_insert_with(|| {
                    self.find_task(&node)
                        .map(|c| c.meta.blocked_by.iter().map(|b| b.to_lowercase()).collect())
                        .unwrap_or_default()
                });
                for n in next.clone() {
                    let mut p = path.clone();
                    p.push(n.clone());
                    stack.push((n, p));
                }
            }
        }

        // Update each blocker's `blocks` array under its own lock (one card locked at a time).
        for blocker in &blockers {
            let _lock = Self::lock_card(Path::new(&blocker.path))?;
            let mut blocker_card = Self::read_card(Path::new(&blocker.path))?;
            blocker_card.scope = blocker.scope;
            if !blocker_card
                .meta
                .blocks
                .iter()
                .any(|b| b.eq_ignore_ascii_case(&found.meta.id))
            {
                blocker_card.meta.blocks.push(found.meta.id.clone());
                self.write_card(&blocker_card)?;
            }
        }

        let _lock = Self::lock_card(Path::new(&found.path))?;
        let mut dep_card = Self::read_card(Path::new(&found.path))?;
        dep_card.scope = found.scope;
        let mut added = Vec::new();
        for blocker in &blockers {
            if !dep_card
                .meta
                .blocked_by
                .iter()
                .any(|b| b.eq_ignore_ascii_case(&blocker.meta.id))
            {
                dep_card.meta.blocked_by.push(blocker.meta.id.clone());
                added.push(blocker.meta.id.clone());
            }
        }

        if !added.is_empty() {
            if dep_card.meta.status == TaskStatus::Ready {
                dep_card.meta.status = TaskStatus::Blocked;
            }
            dep_card.meta.updated = today;
            dep_card.body = format!(
                "{}\n- `{}`: Added dependency on: {}.",
                dep_card.body.trim_end(),
                now_ts,
                added.join(", ")
            );
            self.write_card(&dep_card)?;
        }

        Ok(dep_card)
    }

    fn log_to_daily_note(&self, card: &TaskCard, note: Option<&str>) -> Result<()> {
        if card.scope == CardScope::Global
            && self.scope != CardScope::Global
            && let Some(ref peer) = self.peer
        {
            return peer.log_to_daily_note(card, note);
        }
        if self.config.mode == "cli" || self.config.daily_dir.trim().is_empty() {
            return Ok(());
        }
        fs::create_dir_all(&self.daily_path)?;
        let now = Local::now();
        let today_str = now.format("%Y-%m-%d").to_string();
        let daily_file = self.daily_path.join(format!("{}.md", today_str));

        if !daily_file.exists() {
            let iso = now.iso_week();
            let day_name = now.format("%A").to_string();
            let template = format!(
                "---\ntype: daily\nstatus: active\ndate: {today}\nweek: {week:02}\ntags:\n  - daily-notes\n_organized: true\n---\n> [[W{week:02}-{year}]] · {day}\n\n## Notes\n\n\n---\n## Task List\n\n---\n## Earlier Notes\n\n>[!note]-\n> ```dataview\n> LIST\n> FROM \"journals/daily\"\n> WHERE date < this.date\n> SORT date DESC\n> LIMIT 5\n> ```\n\n---\n",
                today = today_str,
                week = iso.week(),
                year = iso.year(),
                day = day_name
            );
            fs::write(&daily_file, template)?;
        }

        let raw = fs::read_to_string(&daily_file)?;
        let stem = Path::new(&card.path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&card.meta.id);

        if raw.contains(stem) {
            return Ok(());
        }

        let extra = match note {
            Some(n) if !n.trim().is_empty() => format!(": {}", n.trim()),
            _ => String::new(),
        };
        let task_line = format!(
            "- [x] [[tasks/{stem}|{id}: {title}]] (`@{owner}`){extra} ✅ {today}",
            stem = stem,
            id = card.meta.id,
            title = card.meta.title,
            owner = card.meta.owner,
            extra = extra,
            today = today_str
        );

        let updated = if let Some(idx) = raw.find("## Task List") {
            let after_header = idx + "## Task List".len();
            let next_sep = raw[after_header..]
                .find("\n---")
                .map(|i| after_header + i)
                .unwrap_or(raw.len());
            let section = raw[after_header..next_sep].trim_end();
            format!(
                "{}{}\n{}\n\n{}",
                &raw[..after_header],
                if section.is_empty() { "\n" } else { section },
                task_line,
                raw[next_sep..].trim_start_matches('\n')
            )
        } else {
            format!("{}\n## Task List\n\n{}\n", raw.trim_end(), task_line)
        };

        fs::write(&daily_file, updated)?;
        Ok(())
    }

    /// Fuzzy search across all task cards (`id`, `title`, `owner`, `project`, `tags`, `dod`, `session_id`, `body`).
    pub fn fuzzy_search(&self, query: &str) -> Result<Vec<TaskCard>> {
        let cards = self.list_all()?;
        let q = query.trim();
        if q.is_empty() {
            return Ok(cards);
        }
        let mut scored: Vec<(i64, TaskCard)> = cards
            .into_iter()
            .filter_map(|c| {
                let searchable = format!(
                    "{} {} @{} {} {} {} {}",
                    c.meta.id,
                    c.meta.title,
                    c.meta.owner,
                    c.meta.project.as_deref().unwrap_or(""),
                    c.meta.tags.join(" "),
                    c.meta.session_id.as_deref().unwrap_or(""),
                    c.body
                );
                // Give a 2x bonus if the match occurs in the primary header (id/title/owner/project/tags)
                let header = format!(
                    "{} {} @{} {} {}",
                    c.meta.id,
                    c.meta.title,
                    c.meta.owner,
                    c.meta.project.as_deref().unwrap_or(""),
                    c.meta.tags.join(" ")
                );
                let score = fuzzy_score(&header, q).map(|s| s * 2 + 50).or_else(|| {
                    // In full markdown body, require exact substring match for each token
                    let body_lower = searchable.to_lowercase();
                    let mut body_score = 0i64;
                    for tok in q.split_whitespace() {
                        let pat = tok.to_lowercase();
                        {
                            let pos = body_lower.find(&pat)?;
                            body_score += 100 - (pos.min(80) as i64);
                        }
                    }
                    Some(body_score)
                })?;
                Some((score, c))
            })
            .collect();
        scored.sort_by_key(|a| std::cmp::Reverse(a.0));
        Ok(scored.into_iter().map(|(_, c)| c).collect())
    }

    /// Archive completed tasks: moves `done` tasks to `tasks/archive/` and sets `status: archived`.
    /// Preserves full `.events.jsonl` history so DuckDB / `tk st` metrics are 100% intact.
    #[allow(dead_code)]
    pub fn archive_tasks(&self, id_filter: Option<&str>) -> Result<Vec<String>> {
        let ids: Vec<String> = id_filter.into_iter().map(str::to_string).collect();
        self.archive_many(&ids)
    }

    /// Archive completed tasks matching `ids` (or all `done` tasks when `ids` is empty).
    pub fn archive_many(&self, ids: &[String]) -> Result<Vec<String>> {
        let archive_dir = self.tasks_path.join("archive");
        fs::create_dir_all(&archive_dir)?;

        let wanted: Vec<String> = ids
            .iter()
            .flat_map(|s| s.split(','))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|fid| normalize_id(fid).unwrap_or_else(|| fid.to_string()))
            .collect();

        let all_cards = self.list_all()?;
        let mut archived = Vec::new();

        for mut card in all_cards {
            if card.meta.status != TaskStatus::Done {
                continue;
            }
            if !wanted.is_empty() && !wanted.iter().any(|w| card.meta.id.eq_ignore_ascii_case(w)) {
                continue;
            }

            let old_path = PathBuf::from(&card.path);
            let old_lock = CardLock::lock_path_for(&old_path);
            let filename = old_path.file_name().context("Invalid card filename")?;
            let new_path = archive_dir.join(filename);

            card.meta.status = TaskStatus::Archived;
            card.path = new_path.to_string_lossy().to_string();
            let _ = fs::rename(&old_path, &new_path);
            let _ = fs::remove_file(&old_lock);
            self.write_card(&card)?;

            let _ = self.log_event(
                &card.meta.id,
                "archived",
                &self.config.default_owner,
                card.meta.session_id.as_deref(),
                Some("done"),
                "archived",
                None,
                &card.meta.title,
            );
            archived.push(card.meta.id);
        }

        self.sweep_orphan_locks();
        Ok(archived)
    }

    /// Prune completed/archived task notes: deletes `.md` cards from disk while keeping `.events.jsonl` database completely intact!
    #[allow(dead_code)]
    pub fn prune_tasks(&self, id_filter: Option<&str>) -> Result<Vec<String>> {
        let ids: Vec<String> = id_filter.into_iter().map(str::to_string).collect();
        self.prune_many(&ids)
    }

    /// Prune completed/archived task notes matching `ids` (or all completed/archived when `ids` is empty).
    pub fn prune_many(&self, ids: &[String]) -> Result<Vec<String>> {
        let wanted: Vec<String> = ids
            .iter()
            .flat_map(|s| s.split(','))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|fid| normalize_id(fid).unwrap_or_else(|| fid.to_string()))
            .collect();

        let mut all_cards = self.list_all()?;
        all_cards.extend(self.list_archived_cards());
        let mut pruned = Vec::new();

        for card in all_cards {
            if card.meta.status != TaskStatus::Done && card.meta.status != TaskStatus::Archived {
                continue;
            }
            if !wanted.is_empty() && !wanted.iter().any(|w| card.meta.id.eq_ignore_ascii_case(w)) {
                continue;
            }

            let path = PathBuf::from(&card.path);
            let lock_path = CardLock::lock_path_for(&path);
            if path.exists() {
                let _ = fs::remove_file(&path);
            }
            let _ = fs::remove_file(&lock_path);

            let _ = self.log_event(
                &card.meta.id,
                "pruned",
                &self.config.default_owner,
                card.meta.session_id.as_deref(),
                Some(card.meta.status.as_str()),
                "deleted",
                None,
                &card.meta.title,
            );
            pruned.push(card.meta.id);
        }

        self.sweep_orphan_locks();
        Ok(pruned)
    }

    /// Cards in `tasks/archive/` (and the peer's archive under `-A`). `list_all` covers only live cards.
    fn list_archived_cards(&self) -> Vec<TaskCard> {
        let mut dirs = vec![self.tasks_path.join("archive")];
        if self.active_scope == ScopeSelection::All
            && let Some(peer) = &self.peer
        {
            dirs.push(peer.tasks_path.join("archive"));
        }
        let mut cards = Vec::new();
        for dir in dirs {
            for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|x| x == "md")
                    && let Ok(card) = Self::read_card(&path)
                {
                    cards.push(card);
                }
            }
        }
        cards.sort_by(|a, b| a.meta.id.cmp(&b.meta.id));
        cards
    }

    /// Remove any legacy `.tk-*.lock` files in `tasks/` and `tasks/archive/` whose `.md` card is gone.
    pub fn sweep_orphan_locks(&self) {
        for dir in [self.tasks_path.clone(), self.tasks_path.join("archive")] {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                if let Some(stem) = name.strip_prefix('.').and_then(|s| s.strip_suffix(".lock"))
                    && stem.starts_with("tk-")
                    && !dir.join(format!("{stem}.md")).exists()
                {
                    let _ = fs::remove_file(&path);
                }
            }
        }
    }
}

/// Fast native Rust fuzzy-matcher (supports space-separated tokens, exact substring bonus,
/// word-boundary acronym jumps, and tight character proximity).
pub fn fuzzy_score(text: &str, query: &str) -> Option<i64> {
    let text_lower = text.to_lowercase();
    let mut total_score = 0i64;

    for token in query.split_whitespace() {
        let pat = token.to_lowercase();
        if pat.is_empty() {
            continue;
        }
        // Fast path: exact substring match gets a massive bonus
        if let Some(pos) = text_lower.find(&pat) {
            total_score += 200 - (pos.min(100) as i64);
            continue;
        }
        // Subsequence fuzzy match (e.g. "embscf" matches "EMBA Supply Chain formulas"):
        // Non-consecutive jumps must land on a word boundary or within 3 chars of the previous match.
        let t_chars: Vec<char> = text_lower.chars().collect();
        let p_chars: Vec<char> = pat.chars().collect();
        let mut t_idx = 0usize;
        let mut p_idx = 0usize;
        let mut prev_match_idx: Option<usize> = None;
        let mut token_score = 0i64;

        while t_idx < t_chars.len() && p_idx < p_chars.len() {
            if t_chars[t_idx] == p_chars[p_idx] {
                let is_word_boundary = t_idx == 0
                    || matches!(t_chars[t_idx - 1], ' ' | '-' | '_' | '/' | '[' | '@' | '+');
                let is_consecutive = prev_match_idx == Some(t_idx.saturating_sub(1));

                let allowed = if p_idx == 0 {
                    is_word_boundary
                } else {
                    is_consecutive || is_word_boundary
                };

                if allowed {
                    token_score += 10;
                    if is_consecutive {
                        token_score += 15;
                    }
                    if is_word_boundary {
                        token_score += 20;
                    }
                    prev_match_idx = Some(t_idx);
                    p_idx += 1;
                }
            }
            t_idx += 1;
        }
        if p_idx < p_chars.len() {
            return None; // Token did not match as tight/boundary subsequence
        }
        total_score += token_score;
    }

    Some(total_score)
}

/// Replace a card body's details: the text after the `# Title` line and before the first
/// `## ` section. Empty `details` removes the text. Sections and the log are kept as they are.
pub(crate) fn replace_details(body: &str, details: &str) -> String {
    let details = details.trim();
    let (head, rest) = match body
        .find("# ")
        .filter(|&i| i == 0 || body[..i].ends_with('\n'))
    {
        Some(h1) => {
            let line_end = body[h1..].find('\n').map_or(body.len(), |n| h1 + n);
            (&body[..line_end], &body[line_end..])
        }
        None => ("", body),
    };
    let tail = match rest.find("\n## ") {
        Some(i) => &rest[i + 1..],
        None if rest.trim_start().starts_with("## ") => rest.trim_start(),
        None => "",
    };
    let mut out = head.trim_end().to_string();
    if !details.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(details);
    }
    if !tail.is_empty() {
        out.push_str("\n\n");
        out.push_str(tail);
    } else {
        out.push('\n');
    }
    out
}

pub fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    trimmed
        .chars()
        .take(45)
        .collect::<String>()
        .trim_end_matches('-')
        .to_string()
}

/// `7`, `tk-7`, `TK-007` -> `tk-007`; `None` for anything that is not an id.
pub(crate) fn normalize_id(query: &str) -> Option<String> {
    let q = query.trim().to_lowercase();
    let digits = q.strip_prefix("tk-").unwrap_or(&q);
    digits.parse::<u32>().ok().map(|n| format!("tk-{:03}", n))
}

/// Report an unreadable card once per process on stderr (never stdout, which MCP uses).
fn warn_unreadable_card(path: &Path, err: &anyhow::Error) {
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    if seen
        .lock()
        .map(|mut s| s.insert(path.to_path_buf()))
        .unwrap_or(false)
    {
        let name = path.file_name().and_then(|f| f.to_str()).unwrap_or("?");
        eprintln!("warning: skipped unreadable card {}: {:#}", name, err);
    }
}
