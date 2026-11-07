use crate::store::TaskStore;
use anyhow::Result;
use serde_json::{Value, json};
use std::io::{self, BufRead, BufReader, Read, Write};

/// Default `task_list` page size: enough to pick work, small enough for an agent context.
const DEFAULT_LIST_LIMIT: usize = 100;

/// Runs a Model Context Protocol (MCP) JSON-RPC 2.0 server over stdio.
/// Supports BOTH newline-delimited JSON-RPC and LSP-style `Content-Length:` framed JSON-RPC
/// so every agent (Claude Code, Codex, Pi, OpenCode, Goose, Gemini, Cursor) works out of the box.
pub fn run_mcp_server(store: &TaskStore) -> Result<()> {
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut stdout = io::stdout();

    loop {
        let mut first_line = String::new();
        let bytes_read = reader.read_line(&mut first_line)?;
        if bytes_read == 0 {
            break; // EOF
        }

        let trimmed = first_line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Auto-detect LSP Content-Length framing vs newline-delimited JSON
        let (payload, use_content_length) =
            if let Some(len_str) = trimmed.strip_prefix("Content-Length:") {
                let len: usize = len_str.trim().parse().unwrap_or(0);
                // Consume remaining headers until empty line
                loop {
                    let mut header_line = String::new();
                    if reader.read_line(&mut header_line)? == 0 || header_line.trim().is_empty() {
                        break;
                    }
                }
                let mut buf = vec![0u8; len];
                reader.read_exact(&mut buf)?;
                (String::from_utf8_lossy(&buf).to_string(), true)
            } else {
                (trimmed.to_string(), false)
            };

        let Some(response) = handle_message(store, &payload) else {
            continue;
        };

        let out_str = serde_json::to_string(&response)?;
        if use_content_length {
            write!(
                stdout,
                "Content-Length: {}\r\n\r\n{}",
                out_str.len(),
                out_str
            )?;
        } else {
            writeln!(stdout, "{}", out_str)?;
        }
        stdout.flush()?;
    }

    Ok(())
}

/// Handle one JSON-RPC message. Returns the response to send, or `None` for notifications.
/// Malformed JSON gets a -32700 parse error and unknown methods a -32601 error, so a client
/// never waits on a request the server silently dropped.
fn handle_message(store: &TaskStore, payload: &str) -> Option<Value> {
    let req: Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(e) => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32700, "message": format!("Parse error: {}", e) }
            }));
        }
    };

    if !req.is_object() {
        return Some(json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": -32600, "message": "Invalid Request: expected a JSON-RPC object" }
        }));
    }
    // Notifications (no id) never get a response.
    let id = req.get("id").cloned()?;
    if !req.get("method").is_some_and(|m| m.is_string()) {
        return Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32600, "message": "Invalid Request: `method` must be a string" }
        }));
    }
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or_else(|| json!({}));

    let response = match method {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "tkmd",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        }),
        "ping" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {}
        }),
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "tools": [
                    {
                        "name": "task_list",
                        "description": "List tasks across local project repository and/or central global vault with optional status, owner, or scope filter. Returns {tasks, total, next_offset}: compact summaries (no body), 100 per page by default. Use task_get for one full card.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "detail": { "type": "string", "enum": ["summary", "full"], "description": "summary (default): id, title, status, owner, priority, project, due, blockers, eval state, auto-accept, resume. full: complete cards including body." },
                                "limit": { "type": "integer", "minimum": 1, "description": "Page size (default 100, sorted by id)" },
                                "offset": { "type": "integer", "minimum": 0, "description": "Skip this many tasks; pass the previous page's next_offset" },
                                "owner": { "type": "string", "description": "Filter by owner (e.g. ken, hermes, pi, codex)" },
                                "status": { "type": "string", "description": "Filter by status (inbox, ready, in-progress, pending-close, blocked, done)" },
                                "scope": { "type": "string", "enum": ["local", "global", "all"], "description": "Filter by task scope: 'local' (repository project backlog), 'global' (central vault backlog), or 'all' (merged)" }
                            }
                        }
                    },
                    {
                        "name": "task_get",
                        "description": "Get one task as a full card (frontmatter, body, execution log, sessions, review receipt).",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "Task ID (e.g. 7 or tk-007)" }
                            },
                            "required": ["id"]
                        }
                    },
                    {
                        "name": "task_add",
                        "description": "Create a new task with todo.txt shorthand, Definition of Done (dod), optional shell eval command, and optional agent session_id.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "shorthand": { "type": "string", "description": "todo.txt formatted string" },
                                "details": { "type": "string", "description": "Optional markdown description" },
                                "dod": { "type": "array", "items": { "type": "string" }, "description": "Definition of Done checklist items" },
                                "eval": { "type": "string", "description": "Shell command to verify completion (exit 0 = pass)" },
                                "session_id": { "type": "string", "description": "Agent session ID or full resume command (e.g. '8y9380573586' or 'claude -r 8y9380573586')" }
                            },
                            "required": ["shorthand"]
                        }
                    },
                    {
                        "name": "task_claim",
                        "description": "Claim a task, attach your agent session_id for easy resume, and transition status to in-progress. Refuses tasks another owner is working on, tasks under review, and closed tasks unless force is true.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "Task ID (e.g. tk-001)" },
                                "owner": { "type": "string", "description": "Agent or human claiming the task (e.g. claude, codex, hermes, pi)" },
                                "session_id": { "type": "string", "description": "Agent session ID or resume command (e.g. '8y9380573586' or 'claude -r 8y9380573586')" },
                                "force": { "type": "boolean", "description": "Take over a task another owner is working on, or pull it back from review. Recorded in the log." }
                            },
                            "required": ["id", "owner"]
                        }
                    },
                    {
                        "name": "task_eval",
                        "description": "Run the automated verification (eval) command for a task and record pass/fail.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "Task ID (e.g. tk-001)" },
                                "actor": { "type": "string", "description": "Agent running the eval" }
                            },
                            "required": ["id"]
                        }
                    },
                    {
                        "name": "task_submit_review",
                        "description": "Run eval and transition task to pending-close for Ken's final sign-off.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "Task ID (e.g. tk-001)" },
                                "actor": { "type": "string", "description": "Agent submitting the task" },
                                "note": { "type": "string", "description": "Summary of work or [[wikilink]] to artifact" },
                                "session_id": { "type": "string", "description": "Agent session ID or resume command (e.g. '8y9380573586' or 'claude -r 8y9380573586')" }
                            },
                            "required": ["id"]
                        }
                    },
                    {
                        "name": "task_note",
                        "description": "Append a note to a task's execution log, e.g. to explain why the work differs from the spec. Does not change the spec, DoD, eval, owner or status. Shown to Ken at sign-off.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "Task ID (e.g. tk-001)" },
                                "note": { "type": "string", "description": "Note text (max 2000 characters)" },
                                "actor": { "type": "string", "description": "Agent writing the note" },
                                "session_id": { "type": "string", "description": "Agent session ID or resume command" }
                            },
                            "required": ["id", "note"]
                        }
                    },
                    {
                        "name": "task_complete",
                        "description": "Finish your work on a task. Agents cannot mark tasks done: this runs eval and submits to pending-close for human sign-off. It fails if the task is already awaiting sign-off.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "Task ID (e.g. tk-001)" },
                                "actor": { "type": "string", "description": "Who completed the task" },
                                "note": { "type": "string", "description": "Optional summary or [[wikilink]] to output artifact" }
                            },
                            "required": ["id"]
                        }
                    },
                    {
                        "name": "task_cpm",
                        "description": "Calculate Critical Path Method (CPM) metrics (ES, EF, LS, LF, Buffer, project duration, critical path) for active tasks without diagrams.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {}
                        }
                    }
                ]
            }
        }),
        "tools/call" => {
            let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let tool_res = handle_tool_call(store, tool_name, &args);
            match tool_res {
                Ok(text) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{ "type": "text", "text": text }]
                    }
                }),
                Err(err) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "isError": true,
                        "content": [{ "type": "text", "text": format!("Error: {}", err) }]
                    }
                }),
            }
        }
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("Method not found: {}", method) }
        }),
    };
    Some(response)
}

/// What an agent needs to pick and route work, without the card body. Empty fields are omitted.
fn card_summary(c: &crate::model::TaskCard) -> Value {
    let m = &c.meta;
    let mut v = json!({
        "id": m.id,
        "title": m.title,
        "status": m.status.as_str(),
        "owner": m.owner,
        "priority": m.priority,
    });
    let obj = v.as_object_mut().expect("summary is an object");
    if let Some(p) = &m.project {
        obj.insert("project".into(), json!(p));
    }
    if let Some(d) = &m.due {
        obj.insert("due".into(), json!(d));
    }
    if !m.blocked_by.is_empty() {
        obj.insert("blocked_by".into(), json!(m.blocked_by));
    }
    if let Some(e) = &m.eval_status {
        obj.insert("eval_status".into(), json!(e));
    }
    if m.approved_eval.is_some() {
        obj.insert("auto_accept".into(), json!(true));
    }
    if let Some(r) = m.resume_command() {
        obj.insert("resume".into(), json!(r));
    }
    v
}

fn handle_tool_call(store: &TaskStore, name: &str, args: &Value) -> Result<String> {
    match name {
        "task_list" => {
            let owner_filter = args.get("owner").and_then(|v| v.as_str());
            let status_filter = args.get("status").and_then(|v| v.as_str());
            let scope_filter = args.get("scope").and_then(|v| v.as_str());
            let cards = store.list_all()?;
            let filtered: Vec<_> = cards
                .into_iter()
                .filter(|c| {
                    let owner_ok =
                        owner_filter.is_none_or(|o| c.meta.owner.eq_ignore_ascii_case(o));
                    let status_ok = status_filter
                        .is_none_or(|s| c.meta.status.as_str().eq_ignore_ascii_case(s));
                    let scope_ok = scope_filter.is_none_or(|s| match s.to_lowercase().as_str() {
                        "local" => c.scope == crate::model::CardScope::Local,
                        "global" => c.scope == crate::model::CardScope::Global,
                        _ => true,
                    });
                    owner_ok && status_ok && scope_ok
                })
                .collect();
            let total = filtered.len();
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .map_or(DEFAULT_LIST_LIMIT, |n| n as usize);
            if limit == 0 {
                anyhow::bail!("limit must be at least 1");
            }
            let page: Vec<_> = filtered.into_iter().skip(offset).take(limit).collect();
            let next_offset = (offset + page.len() < total).then_some(offset + page.len());
            let tasks: Vec<Value> = if args.get("detail").and_then(|v| v.as_str()) == Some("full") {
                page.iter()
                    .map(serde_json::to_value)
                    .collect::<serde_json::Result<_>>()?
            } else {
                page.iter().map(card_summary).collect()
            };
            Ok(serde_json::to_string(&json!({
                "tasks": tasks,
                "total": total,
                "next_offset": next_offset
            }))?)
        }
        "task_get" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
            Ok(serde_json::to_string_pretty(&store.find_task(id)?)?)
        }
        "task_add" => {
            let shorthand = args.get("shorthand").and_then(|v| v.as_str()).unwrap_or("");
            let details = args.get("details").and_then(|v| v.as_str());
            let dod = args
                .get("dod")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let eval = args
                .get("eval")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let session_id = args
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let card = store.add_task(shorthand, details, dod, eval, session_id)?;
            Ok(serde_json::to_string_pretty(&card)?)
        }
        "task_claim" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let owner = args
                .get("owner")
                .and_then(|v| v.as_str())
                .unwrap_or("agent");
            let session_id = args
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
            let card = store.claim_task(id, owner, session_id, force)?;
            Ok(serde_json::to_string_pretty(&card)?)
        }
        "task_eval" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let actor = args
                .get("actor")
                .and_then(|v| v.as_str())
                .unwrap_or("agent");
            let (card, passed, output) = store.run_eval(id, actor)?;
            Ok(serde_json::to_string_pretty(&json!({
                "id": card.meta.id,
                "passed": passed,
                "output": output,
                "card": card
            }))?)
        }
        "task_submit_review" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let actor = args
                .get("actor")
                .and_then(|v| v.as_str())
                .unwrap_or("agent");
            let note = args.get("note").and_then(|v| v.as_str());
            let session_id = args
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let card = store.submit_for_review(id, actor, note, session_id)?;
            Ok(serde_json::to_string_pretty(&card)?)
        }
        "task_note" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let note = args.get("note").and_then(|v| v.as_str()).unwrap_or("");
            let actor = args
                .get("actor")
                .and_then(|v| v.as_str())
                .unwrap_or("agent");
            let session_id = args.get("session_id").and_then(|v| v.as_str());
            let card = store.add_note(id, actor, note, session_id)?;
            Ok(serde_json::to_string_pretty(&card)?)
        }
        "task_complete" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let actor = args
                .get("actor")
                .and_then(|v| v.as_str())
                .unwrap_or("agent");
            let note = args.get("note").and_then(|v| v.as_str());
            let card = store.complete_task(id, actor, note)?;
            Ok(serde_json::to_string_pretty(&card)?)
        }
        "task_cpm" => {
            let cards = store.list_all()?;
            let cpm = crate::analytics::compute_cpm(&cards);
            Ok(serde_json::to_string_pretty(&cpm)?)
        }
        _ => anyhow::bail!("Unknown MCP tool: {}", name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CardScope;
    use crate::store::{AppConfig, ScopeSelection, TaskStore};
    use std::fs;

    fn setup_test_store(name: &str) -> (TaskStore, std::path::PathBuf) {
        let tmp =
            std::env::temp_dir().join(format!("tkmd-mcp-test-{}-{}", name, std::process::id()));
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
            policy: crate::store::Policy::default(),
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

    #[test]
    fn test_mcp_tool_lifecycle() {
        let (store, tmp) = setup_test_store("lifecycle");

        // 1. Add task via MCP
        let add_args = json!({
            "shorthand": "p0 Test MCP tool integration +ci dur:1h",
            "details": "Integration test for MCP",
            "dod": ["Item 1", "Item 2"],
            "eval": "exit 0",
            "session_id": "test-agent-sess-123"
        });
        let add_res = handle_tool_call(&store, "task_add", &add_args).expect("task_add failed");
        let added: Value = serde_json::from_str(&add_res).unwrap();
        assert_eq!(added["id"], "tk-001");
        assert_eq!(added["priority"], "P0");
        assert_eq!(added["status"], "ready");

        // 2. Claim task via MCP
        let claim_args = json!({
            "id": "1",
            "owner": "claude",
            "session_id": "claude -r 999"
        });
        let claim_res =
            handle_tool_call(&store, "task_claim", &claim_args).expect("task_claim failed");
        let claimed: Value = serde_json::from_str(&claim_res).unwrap();
        assert_eq!(claimed["status"], "in-progress");
        assert_eq!(claimed["owner"], "claude");

        // 3. Eval task via MCP
        let eval_args = json!({
            "id": "1",
            "actor": "claude"
        });
        let eval_res = handle_tool_call(&store, "task_eval", &eval_args).expect("task_eval failed");
        let eval_val: Value = serde_json::from_str(&eval_res).unwrap();
        assert_eq!(eval_val["passed"], true);

        // 4. Submit review via MCP
        let rev_args = json!({
            "id": "1",
            "actor": "claude",
            "note": "All checks verified"
        });
        let rev_res = handle_tool_call(&store, "task_submit_review", &rev_args)
            .expect("task_submit_review failed");
        let reviewed: Value = serde_json::from_str(&rev_res).unwrap();
        assert_eq!(reviewed["status"], "pending-close");

        // 5. MCP cannot sign off, even when it claims to be the owner
        let comp_args = json!({
            "id": "1",
            "actor": "ken",
            "note": "Signed off"
        });
        let comp_err = handle_tool_call(&store, "task_complete", &comp_args).unwrap_err();
        assert!(comp_err.to_string().contains("awaiting human sign-off"));
        assert_eq!(
            store.find_task("1").unwrap().meta.status,
            crate::model::TaskStatus::PendingClose
        );

        // 6. CPM calculation via MCP
        let cpm_res = handle_tool_call(&store, "task_cpm", &json!({})).expect("task_cpm failed");
        let cpm_val: Value = serde_json::from_str(&cpm_res).unwrap();
        assert_eq!(cpm_val["project_duration_seconds"], 3600);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_mcp_task_note() {
        let (store, tmp) = setup_test_store("task-note");
        handle_tool_call(
            &store,
            "task_add",
            &json!({ "shorthand": "p1 noted via MCP", "eval": "exit 0" }),
        )
        .expect("task_add failed");
        handle_tool_call(
            &store,
            "task_note",
            &json!({ "id": "tk-001", "note": "built X instead of Y", "actor": "codex" }),
        )
        .expect("task_note failed");
        let card = store.find_task("1").unwrap();
        assert!(card.body.contains("Note by `codex`: built X instead of Y"));
        assert_eq!(card.meta.eval.as_deref(), Some("exit 0"));
        assert!(handle_tool_call(&store, "task_note", &json!({ "id": "tk-001" })).is_err());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_mcp_unknown_tool() {
        let (store, tmp) = setup_test_store("unknown");
        let res = handle_tool_call(&store, "non_existent_tool", &json!({}));
        assert!(res.is_err());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_task_list_returns_compact_summaries_by_default() {
        let (store, tmp) = setup_test_store("summary");
        for i in 0..5 {
            store
                .add_task(
                    &format!("p1 Summary task {} +proj", i),
                    Some(
                        "A long body that agents should not pay for in a listing. "
                            .repeat(10)
                            .as_str(),
                    ),
                    vec!["one".into(), "two".into()],
                    Some("exit 0".into()),
                    None,
                )
                .unwrap();
        }
        let res = handle_tool_call(&store, "task_list", &json!({})).unwrap();
        let list: Value = serde_json::from_str(&res).unwrap();
        let items = list["tasks"].as_array().unwrap();
        assert_eq!(items.len(), 5);
        for item in items {
            assert!(
                item.get("body").is_none(),
                "summary must not include the body: {}",
                item
            );
            assert!(item.get("sessions").is_none());
            for key in ["id", "title", "status", "owner", "priority"] {
                assert!(item.get(key).is_some(), "summary missing {}: {}", key, item);
            }
        }
        let per_card = serde_json::to_string(&list["tasks"]).unwrap().len() / items.len();
        assert!(per_card < 400, "summary is {} bytes per card", per_card);

        let full = handle_tool_call(&store, "task_list", &json!({"detail": "full"})).unwrap();
        let full: Value = serde_json::from_str(&full).unwrap();
        assert!(
            full["tasks"][0].get("body").is_some(),
            "detail=full must include the body"
        );
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_task_get_returns_one_full_card() {
        let (store, tmp) = setup_test_store("get");
        store
            .add_task("p1 Fetch me", Some("the details"), vec![], None, None)
            .unwrap();
        let res = handle_tool_call(&store, "task_get", &json!({"id": "1"})).unwrap();
        let card: Value = serde_json::from_str(&res).unwrap();
        assert_eq!(card["id"], "tk-001");
        assert!(card["body"].as_str().unwrap().contains("the details"));
        assert!(handle_tool_call(&store, "task_get", &json!({"id": "99"})).is_err());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_task_list_limit() {
        let (store, tmp) = setup_test_store("limit");
        for i in 0..4 {
            store
                .add_task(&format!("p1 t{}", i), None, vec![], None, None)
                .unwrap();
        }
        let res = handle_tool_call(&store, "task_list", &json!({"limit": 2})).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&res).unwrap()["tasks"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let _ = fs::remove_dir_all(&tmp);
    }

    /// RED (Codex F9): malformed JSON got no response and unknown methods returned success.
    #[test]
    fn test_protocol_errors_are_reported() {
        let (store, tmp) = setup_test_store("protocol");
        let bad = handle_message(&store, "{not json").expect("parse errors need a response");
        assert_eq!(bad["error"]["code"], -32700);
        assert!(bad["id"].is_null());
        let unknown =
            handle_message(&store, r#"{"jsonrpc":"2.0","id":7,"method":"no/such"}"#).unwrap();
        assert_eq!(unknown["error"]["code"], -32601);
        assert_eq!(unknown["id"], 7);
        assert!(
            handle_message(
                &store,
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
            )
            .is_none()
        );
        let _ = fs::remove_dir_all(&tmp);
    }

    /// RED (Codex F8): an unbounded default list cost megabytes on large stores.
    #[test]
    fn test_task_list_is_paged_by_default() {
        let (store, tmp) = setup_test_store("paged");
        for i in 0..130 {
            store
                .add_task(&format!("p1 t{}", i), None, vec![], None, None)
                .unwrap();
        }
        let page: Value =
            serde_json::from_str(&handle_tool_call(&store, "task_list", &json!({})).unwrap())
                .unwrap();
        assert_eq!(page["total"], 130);
        assert_eq!(page["tasks"].as_array().unwrap().len(), 100);
        assert_eq!(page["next_offset"], 100);
        let rest: Value = serde_json::from_str(
            &handle_tool_call(&store, "task_list", &json!({"offset": 100})).unwrap(),
        )
        .unwrap();
        assert_eq!(rest["tasks"].as_array().unwrap().len(), 30);
        assert!(rest["next_offset"].is_null());
        let _ = fs::remove_dir_all(&tmp);
    }

    /// RED (Sourcery #24, #25): limit 0 paged forever; structurally invalid requests got no error.
    #[test]
    fn test_invalid_paging_and_requests_are_errors() {
        let (store, tmp) = setup_test_store("invalid");
        store.add_task("p1 a", None, vec![], None, None).unwrap();
        assert!(handle_tool_call(&store, "task_list", &json!({"limit": 0})).is_err());
        let no_method = handle_message(&store, r#"{"jsonrpc":"2.0","id":3}"#).unwrap();
        assert_eq!(no_method["error"]["code"], -32600);
        assert_eq!(no_method["id"], 3);
        let not_object = handle_message(&store, "[1,2]").unwrap();
        assert_eq!(not_object["error"]["code"], -32600);
        let _ = fs::remove_dir_all(&tmp);
    }
}
