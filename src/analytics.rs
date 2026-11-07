use crate::ansi::Ansi;
use crate::model::{TaskCard, TaskStatus};
use crate::store::TaskStore;
use anyhow::Result;
use chrono::{DateTime, FixedOffset, Local};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpmTaskSchedule {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(rename = "duration_seconds", alias = "duration")]
    pub duration_seconds: u64,
    pub duration_display: String,
    pub duration_days: u32,
    #[serde(rename = "early_start", alias = "es")]
    pub early_start: u64,
    #[serde(rename = "early_finish", alias = "ef")]
    pub early_finish: u64,
    #[serde(rename = "late_start", alias = "ls")]
    pub late_start: u64,
    #[serde(rename = "late_finish", alias = "lf")]
    pub late_finish: u64,
    #[serde(rename = "buffer_seconds", alias = "buffer")]
    pub buffer_seconds: u64,
    pub buffer_display: String,
    pub buffer_days: u32,
    pub is_critical: bool,
    pub status: String,
    pub owner: String,
    pub blocked_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpmAnalysis {
    #[serde(rename = "project_duration_seconds", alias = "project_duration")]
    pub project_duration_seconds: u64,
    pub project_duration_display: String,
    pub project_duration_days: u32,
    pub critical_path: Vec<String>,
    pub summary_short: String,
    pub summary_long: String,
    pub schedule: Vec<CpmTaskSchedule>,
}

const RESET: Ansi = Ansi("\x1b[0m");
const BOLD: Ansi = Ansi("\x1b[1m");
const BOLD_WHITE: Ansi = Ansi("\x1b[1;97m");
const DIM: Ansi = Ansi("\x1b[2m");
const RED: Ansi = Ansi("\x1b[38;2;224;108;117m");
const ORANGE: Ansi = Ansi("\x1b[38;2;254;128;25m");
const AQUA: Ansi = Ansi("\x1b[38;2;86;182;194m");
const GREEN: Ansi = Ansi("\x1b[38;2;152;195;121m");
const PURPLE: Ansi = Ansi("\x1b[38;2;198;120;221m");

pub fn format_duration_secs(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        format!("{}s", s)
    } else if s < 3600 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else if s < 86400 {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d {}h", s / 86400, (s % 86400) / 3600)
    }
}

pub fn parse_duration_seconds(raw: &str) -> Option<u64> {
    let s = raw.trim().to_lowercase();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_suffix('s') {
        rest.parse::<u64>().ok()
    } else if let Some(rest) = s.strip_suffix('m') {
        rest.parse::<f64>().ok().map(|m| (m * 60.0).round() as u64)
    } else if let Some(rest) = s.strip_suffix('h') {
        rest.parse::<f64>()
            .ok()
            .map(|h| (h * 3600.0).round() as u64)
    } else if let Some(rest) = s.strip_suffix('d') {
        rest.parse::<f64>()
            .ok()
            .map(|d| (d * 86400.0).round() as u64)
    } else if let Some(rest) = s.strip_suffix('w') {
        rest.parse::<f64>()
            .ok()
            .map(|w| (w * 604800.0).round() as u64)
    } else if let Ok(n) = s.parse::<u64>() {
        if n <= 365 { Some(n * 86400) } else { Some(n) }
    } else {
        None
    }
}

pub fn format_duration_concise(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        let m = secs / 60;
        let s = secs % 60;
        if s == 0 {
            format!("{}m", m)
        } else {
            format!("{}m {:02}s", m, s)
        }
    } else if secs < 86400 {
        let h = secs / 3600;
        let rem = secs % 3600;
        let m = rem / 60;
        if m == 0 {
            format!("{}h", h)
        } else {
            format!("{}h {:02}m", h, m)
        }
    } else {
        let d = secs / 86400;
        let rem = secs % 86400;
        let h = rem / 3600;
        if h == 0 {
            format!("{}d", d)
        } else {
            format!("{}d {}h", d, h)
        }
    }
}

fn truncate_vis(s: &str, max_chars: usize) -> String {
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

/// Render `tk history [id]` (alias `tk h`) — chronological event timeline with step deltas.
pub fn print_history(
    store: &TaskStore,
    id_filter: Option<&str>,
    limit: usize,
    term_w: usize,
) -> Result<()> {
    let events = store.read_events()?;
    let norm_filter = id_filter.map(|q| {
        if let Ok(n) = q.parse::<u32>() {
            format!("tk-{:03}", n)
        } else {
            q.to_lowercase()
        }
    });

    // Track previous timestamp per task to compute step duration (+5m 04s)
    let mut prev_ts_by_task: HashMap<String, DateTime<FixedOffset>> = HashMap::new();
    let mut rows = Vec::new();

    for ev in events {
        let parsed_ts = DateTime::parse_from_rfc3339(&ev.timestamp).ok();
        let delta_str = if let Some(ts) = parsed_ts {
            let d = prev_ts_by_task
                .get(&ev.id)
                .map(|prev| format!("+{}", format_duration_secs((ts - *prev).num_seconds())))
                .unwrap_or_else(|| "start".to_string());
            prev_ts_by_task.insert(ev.id.clone(), ts);
            d
        } else {
            "-".to_string()
        };

        if let Some(ref f) = norm_filter
            && !ev.id.to_lowercase().contains(f)
        {
            continue;
        }
        rows.push((ev, parsed_ts, delta_str));
    }

    if rows.is_empty() {
        println!("{DIM}  No history events recorded yet.{RESET}");
        return Ok(());
    }

    let start = rows.len().saturating_sub(limit);
    let slice = &rows[start..];

    println!(
        "\n{BOLD}📜 TASK EVENT HISTORY{RESET} {DIM}(showing {} events from tasks/.events.jsonl){RESET}",
        slice.len()
    );

    // Fixed columns: TIME (11) + ID (6) + ACTION (13) + ACTOR (8) + DELTA (8) + borders (19) = 65
    let title_w = term_w.saturating_sub(66).max(12);
    let title_bar = "─".repeat(title_w + 2);
    let title_hdr_pad = " ".repeat(title_w.saturating_sub(5));

    println!(
        "{DIM}╭─────────────┬────────┬───────────────┬──────────┬──────────┬{}╮{RESET}",
        title_bar
    );
    println!(
        "{DIM}│{RESET} {BOLD}TIME{RESET}        {DIM}│{RESET} {BOLD}ID{RESET}     {DIM}│{RESET} {BOLD}ACTION{RESET}        {DIM}│{RESET} {BOLD}ACTOR{RESET}    {DIM}│{RESET} {BOLD}DELTA{RESET}    {DIM}│{RESET} {BOLD}TITLE{RESET}{} {DIM}│{RESET}",
        title_hdr_pad
    );
    println!(
        "{DIM}├─────────────┼────────┼───────────────┼──────────┼──────────┼{}┤{RESET}",
        title_bar
    );

    for (ev, parsed_ts, delta) in slice {
        let time_short = parsed_ts
            .map(|t| t.format("%m-%d %H:%M").to_string())
            .unwrap_or_else(|| truncate_vis(&ev.timestamp, 11));

        let action_colored = match ev.action.as_str() {
            "created" => format!("{AQUA}+ created    {RESET}"),
            "claimed" => format!("{BOLD}{ORANGE}▶ claimed    {RESET}"),
            "eval" => {
                if ev.eval_passed == Some(true) {
                    format!("{GREEN}✔ eval PASS  {RESET}")
                } else {
                    format!("{BOLD}{RED}✖ eval FAIL  {RESET}")
                }
            }
            "pending-close" => format!("{BOLD}{PURPLE}◐ review     {RESET}"),
            "completed" => format!("{BOLD}{GREEN}✓ completed  {RESET}"),
            "note" => format!("{AQUA}✎ note       {RESET}"),
            other => format!("{:<13}", truncate_vis(other, 13)),
        };

        let actor_str = format!("@{:<7}", truncate_vis(&ev.actor, 7));
        let actor_colored = if ev.actor.eq_ignore_ascii_case("ken") {
            format!("{BOLD}{GREEN}{actor_str}{RESET}")
        } else {
            format!("{PURPLE}{actor_str}{RESET}")
        };

        let t = truncate_vis(&ev.title, title_w);
        let pad = " ".repeat(title_w.saturating_sub(t.chars().count()));

        println!(
            "{DIM}│{RESET} {DIM}{:<11}{RESET} {DIM}│{RESET} {BOLD}{:<6}{RESET} {DIM}│{RESET} {} {DIM}│{RESET} {} {DIM}│{RESET} {:<8} {DIM}│{RESET} {}{} {DIM}│{RESET}",
            time_short,
            ev.id,
            action_colored,
            actor_colored,
            truncate_vis(delta, 8),
            t,
            pad
        );
    }

    println!(
        "{DIM}╰─────────────┴────────┴───────────────┴──────────┴──────────┴{}╯{RESET}",
        title_bar
    );
    Ok(())
}

/// Render `tk stats` (alias `tk st`) — Queueing metrics, durations, splits, eval yield, and insights.
pub fn print_stats(store: &TaskStore, term_w: usize) -> Result<()> {
    let cards = store.list_all()?;
    let events = store.read_events()?;
    let now = Local::now().fixed_offset();

    let total = cards.len();
    if total == 0 {
        println!("{DIM}  No tasks found to compute statistics.{RESET}");
        return Ok(());
    }

    // 1. Status counts
    let mut status_counts: HashMap<&str, usize> = HashMap::new();
    let mut owner_counts: HashMap<String, usize> = HashMap::new();
    let mut cat_counts: HashMap<String, usize> = HashMap::new();
    let mut with_eval = 0usize;
    let mut with_dod = 0usize;
    let mut agent_without_session = 0usize;

    for c in &cards {
        *status_counts.entry(c.meta.status.as_str()).or_insert(0) += 1;
        *owner_counts.entry(c.meta.owner.to_lowercase()).or_insert(0) += 1;

        let cat = c
            .meta
            .project
            .clone()
            .or_else(|| c.meta.topic.first().map(|t| format!("#{}", t)))
            .unwrap_or_else(|| "uncategorized".to_string());
        *cat_counts.entry(cat).or_insert(0) += 1;

        if c.meta.eval.is_some() {
            with_eval += 1;
        }
        if !c.meta.dod.is_empty() {
            with_dod += 1;
        }
        if !c
            .meta
            .owner
            .eq_ignore_ascii_case(&store.config.default_owner)
            && c.meta.session_id.is_none()
        {
            agent_without_session += 1;
        }
    }

    // 2. Compute Durations, Wait Times & Completion Metrics from `.events.jsonl`
    let mut created_at: HashMap<String, DateTime<FixedOffset>> = HashMap::new();
    let mut claimed_at: HashMap<String, DateTime<FixedOffset>> = HashMap::new();
    let mut review_at: HashMap<String, DateTime<FixedOffset>> = HashMap::new();

    let mut queue_waits: Vec<i64> = Vec::new(); // created -> claimed
    let mut active_durations: Vec<i64> = Vec::new(); // claimed -> pending-close (or done)
    let mut review_waits: Vec<i64> = Vec::new(); // pending-close -> done (or now)
    let mut completion_times: Vec<i64> = Vec::new(); // created/claimed -> completed

    let mut eval_runs = 0usize;
    let mut eval_passes = 0usize;

    for ev in &events {
        let Some(ts) = DateTime::parse_from_rfc3339(&ev.timestamp).ok() else {
            continue;
        };
        match ev.action.as_str() {
            "created" => {
                created_at.entry(ev.id.clone()).or_insert(ts);
            }
            "claimed" => {
                claimed_at.insert(ev.id.clone(), ts);
                if let Some(c_ts) = created_at.get(&ev.id) {
                    queue_waits.push((ts - *c_ts).num_seconds().max(0));
                }
            }
            "eval" => {
                eval_runs += 1;
                if ev.eval_passed == Some(true) {
                    eval_passes += 1;
                }
            }
            "pending-close" => {
                review_at.insert(ev.id.clone(), ts);
                if let Some(cl_ts) = claimed_at.get(&ev.id).or_else(|| created_at.get(&ev.id)) {
                    active_durations.push((ts - *cl_ts).num_seconds().max(0));
                }
            }
            "completed" => {
                if let Some(rev_ts) = review_at.get(&ev.id) {
                    review_waits.push((ts - *rev_ts).num_seconds().max(0));
                } else if let Some(cl_ts) = claimed_at.get(&ev.id) {
                    active_durations.push((ts - *cl_ts).num_seconds().max(0));
                }
                // Lead Time / MTTC: from created_at (or claimed_at) to completed
                if let Some(c_ts) = created_at.get(&ev.id).or_else(|| claimed_at.get(&ev.id)) {
                    completion_times.push((ts - *c_ts).num_seconds().max(0));
                }
            }
            _ => {}
        }
    }

    // Include currently waiting `pending-close` tasks in Sign-Off Wait Time
    for c in &cards {
        if c.meta.status == TaskStatus::PendingClose
            && let Some(rev_ts) = review_at.get(&c.meta.id)
        {
            review_waits.push((now - *rev_ts).num_seconds().max(0));
        }
    }

    let avg = |v: &[i64]| -> Option<i64> {
        if v.is_empty() {
            None
        } else {
            Some(v.iter().sum::<i64>() / (v.len() as i64))
        }
    };

    let percentile = |v: &[i64], p: f64| -> Option<i64> {
        if v.is_empty() {
            return None;
        }
        let mut sorted = v.to_vec();
        sorted.sort_unstable();
        let idx = (((sorted.len() as f64) * p / 100.0).ceil() as usize).saturating_sub(1);
        sorted.get(idx.min(sorted.len() - 1)).copied()
    };

    let tpm90 = |v: &[i64]| -> Option<i64> {
        if v.is_empty() {
            return None;
        }
        let mut sorted = v.to_vec();
        sorted.sort_unstable();
        let start_idx = ((sorted.len() as f64) * 0.9).floor() as usize;
        let tail = &sorted[start_idx.min(sorted.len() - 1)..];
        if tail.is_empty() {
            None
        } else {
            Some(tail.iter().sum::<i64>() / (tail.len() as i64))
        }
    };

    let avg_queue_wait = avg(&queue_waits)
        .map(format_duration_secs)
        .unwrap_or_else(|| "n/a".to_string());
    let avg_active_dur = avg(&active_durations)
        .map(format_duration_secs)
        .unwrap_or_else(|| "n/a".to_string());
    let avg_review_wait = avg(&review_waits)
        .map(format_duration_secs)
        .unwrap_or_else(|| "n/a".to_string());

    // Mean, P50, P90, and TPM90 execution durations
    let active_p50 = percentile(&active_durations, 50.0)
        .map(format_duration_secs)
        .unwrap_or_else(|| "n/a".to_string());
    let active_p90 = percentile(&active_durations, 90.0)
        .map(format_duration_secs)
        .unwrap_or_else(|| "n/a".to_string());
    let active_tpm90 = tpm90(&active_durations)
        .map(format_duration_secs)
        .unwrap_or_else(|| "n/a".to_string());

    // Mean Time to Complete (MTTC / Lead Time)
    let mttc_mean = avg(&completion_times)
        .map(format_duration_secs)
        .unwrap_or_else(|| "n/a (0 closed)".to_string());
    let mttc_p50 = percentile(&completion_times, 50.0)
        .map(format_duration_secs)
        .unwrap_or_else(|| "n/a".to_string());
    let mttc_p90 = percentile(&completion_times, 90.0)
        .map(format_duration_secs)
        .unwrap_or_else(|| "n/a".to_string());
    let mttc_tpm90 = tpm90(&completion_times)
        .map(format_duration_secs)
        .unwrap_or_else(|| "n/a".to_string());

    let rdy = *status_counts.get("ready").unwrap_or(&0);
    let wip = *status_counts.get("in-progress").unwrap_or(&0);
    let rev = *status_counts.get("pending-close").unwrap_or(&0);
    let blk = *status_counts.get("blocked").unwrap_or(&0);
    let don = *status_counts.get("done").unwrap_or(&0);

    let active_wip = wip + rev; // Active WIP (L)

    // Little's Law calculation: L = λ * W
    // Average Cycle Time (W) = Active Execution Duration + Sign-Off Wait Time
    let mean_w_secs = match (avg(&active_durations), avg(&review_waits)) {
        (Some(act), Some(rev_w)) => Some(act + rev_w),
        (Some(act), None) => Some(act),
        (None, Some(rev_w)) => Some(rev_w),
        (None, None) => None,
    };
    let littles_law_str = if let Some(w_secs) = mean_w_secs {
        if w_secs > 0 {
            let throughput_per_day = (active_wip as f64) / (w_secs as f64 / 86400.0);
            let throughput_per_hour = (active_wip as f64) / (w_secs as f64 / 3600.0);
            format!(
                "L = {active_wip} WIP · W = {} cycle time → λ = {:.2} tasks/day ({:.2} tasks/hr)",
                format_duration_secs(w_secs),
                throughput_per_day,
                throughput_per_hour
            )
        } else {
            format!("L = {active_wip} WIP · W ≈ 0s")
        }
    } else {
        format!("L = {active_wip} WIP · awaiting timing samples")
    };

    let eval_rate = if eval_runs > 0 {
        format!(
            "{:.0}% ({}/{} passed)",
            (eval_passes as f64 / eval_runs as f64) * 100.0,
            eval_passes,
            eval_runs
        )
    } else {
        "n/a (0 runs)".to_string()
    };

    let bar_w = term_w.saturating_sub(10).clamp(20, 60);
    let divider = "─".repeat(term_w.saturating_sub(4).clamp(30, 90));

    println!("\n{BOLD}📊 TKMD TELEMETRY & HARNESS ANALYTICS{RESET}");
    println!("{DIM}{divider}{RESET}");

    // Section 1: Counts & Queue State
    println!(
        "{BOLD}1. Queue & Status Breakdown{RESET} {DIM}(Total: {total} · Active WIP L = {active_wip}){RESET}"
    );
    println!(
        "   {AQUA}○ ready:{RESET} {BOLD}{rdy}{RESET}   {ORANGE}● in-progress:{RESET} {BOLD}{wip}{RESET}   {PURPLE}◐ pending-close:{RESET} {BOLD}{rev}{RESET}   {RED}✖ blocked:{RESET} {BOLD}{blk}{RESET}   {GREEN}✓ done:{RESET} {BOLD}{don}{RESET}"
    );

    // Section 2: Durations & Wait Times (Little's Law: L = λW)
    println!("\n{BOLD}2. Flow & Queueing Timings (Little's Law: L = λW · Wall-Clock){RESET}");
    println!(
        "   • {DIM}Little's Law Model:{RESET}         {BOLD}{AQUA}{}{RESET}",
        littles_law_str
    );
    println!(
        "   • {BOLD}Queue Wait{RESET} (`ready` → `claimed`):         mean {AQUA}{avg_queue_wait}{RESET}"
    );
    println!(
        "   • {BOLD}Execution Duration{RESET} (`claimed` → `review`): mean {ORANGE}{avg_active_dur}{RESET} {DIM}· P50: {active_p50} · P90: {active_p90} · TPM90: {active_tpm90}{RESET}"
    );
    println!(
        "   • {BOLD}Sign-Off Wait{RESET} (`pending-close` age):      mean {PURPLE}{avg_review_wait}{RESET}"
    );
    println!(
        "   • {BOLD}Mean Time to Complete (MTTC){RESET}:        mean {GREEN}{mttc_mean}{RESET} {DIM}· P50: {mttc_p50} · P90: {mttc_p90} · TPM90: {mttc_tpm90}{RESET}"
    );

    // Section 3: Human vs. Agent Split
    println!("\n{BOLD}3. Human vs. AI Agent Split{RESET}");
    let mut owners_vec: Vec<_> = owner_counts.into_iter().collect();
    owners_vec.sort_by_key(|a| std::cmp::Reverse(a.1));
    for (owner, cnt) in &owners_vec {
        let pct = (*cnt as f64 / total as f64) * 100.0;
        let filled = ((*cnt as f64 / total as f64) * (bar_w as f64 / 2.0)).round() as usize;
        let bar = "█".repeat(filled.max(1));
        let color = if owner.eq_ignore_ascii_case(&store.config.default_owner) {
            GREEN
        } else {
            PURPLE
        };
        println!(
            "   {color}@{:<10}{RESET} {:>2} ({:>3.0}%)  {color}{bar}{RESET}",
            owner, cnt, pct
        );
    }

    // Section 4: Category / Project Split
    println!("\n{BOLD}4. Category & Project Split{RESET}");
    let mut cats_vec: Vec<_> = cat_counts.into_iter().collect();
    cats_vec.sort_by_key(|a| std::cmp::Reverse(a.1));
    for (cat, cnt) in &cats_vec {
        let pct = (*cnt as f64 / total as f64) * 100.0;
        println!("   {AQUA}{:<20}{RESET} {:>2} ({:>3.0}%)", cat, cnt, pct);
    }

    // Section 5: Verification & Eval Yield
    println!("\n{BOLD}5. Verification & Eval Gate Yield{RESET}");
    println!("   • {BOLD}Eval Success Rate:{RESET}   {GREEN}{eval_rate}{RESET}");
    println!(
        "   • {BOLD}Automated Eval Gate:{RESET} {}/{} tasks ({:.0}%) have `--eval` shell verification",
        with_eval,
        total,
        (with_eval as f64 / total as f64) * 100.0
    );
    println!(
        "   • {BOLD}DoD Spec Coverage:{RESET}   {}/{} tasks ({:.0}%) have `--dod` acceptance checklists",
        with_dod,
        total,
        (with_dod as f64 / total as f64) * 100.0
    );

    // Section 6: Insights & Opportunities
    println!("\n{BOLD}6. 💡 Actionable Insights & Opportunities{RESET}");
    if rev > 0 {
        println!(
            "   {PURPLE}🔍 Sign-Off Bottleneck:{RESET} {BOLD}{rev}{RESET} task(s) are waiting in `pending-close` (avg wait: {avg_review_wait}). Run `tk d <id>` to close."
        );
    }
    if with_eval < total {
        println!(
            "   {ORANGE}⚡ Harness Opportunity:{RESET} {BOLD}{}{RESET} of {total} task(s) lack an `--eval` command. Add executable shell checks so agents self-verify before review.",
            total - with_eval
        );
    }
    if agent_without_session > 0 {
        println!(
            "   {AQUA}🔗 Session Traceability:{RESET} {BOLD}{agent_without_session}{RESET} agent task(s) have no `session_id`. Pass `-s <session_id>` so `tk u <id>` can resume them."
        );
    }
    if rev == 0 && with_eval == total && agent_without_session == 0 {
        println!(
            "   {GREEN}✨ All tasks have automated evals, session IDs, and zero sign-off backlog.{RESET}"
        );
    }
    println!("{DIM}{divider}{RESET}");
    Ok(())
}

/// Interactive (`fzf --ansi` + `bat` preview) or inline Rust fuzzy search (`tk f [query]`).
pub fn run_fuzzy_finder(
    store: &TaskStore,
    query: &[String],
    interactive: bool,
    edit: bool,
    open: bool,
    print_table: fn(&TaskStore, &[TaskCard]),
) -> Result<()> {
    let q_str = query.join(" ");

    // If the user gave a query without `-i`, `-e`, or `-o`, show matching cards directly in the responsive table!
    if !q_str.trim().is_empty() && !interactive && !edit && !open {
        let matches = store.fuzzy_search(&q_str)?;
        if matches.is_empty() {
            println!("{DIM}  No tasks matched fuzzy query: '{q_str}'{RESET}");
        } else {
            println!(
                "\n{BOLD}🔎 Fuzzy Search:{RESET} `{AQUA}{q_str}{RESET}` ({} match{})",
                matches.len(),
                if matches.len() == 1 { "" } else { "es" }
            );
            print_table(store, &matches);
        }
        return Ok(());
    }

    // Interactive mode via `fzf` + `bat` preview (if `fzf` is installed)
    let cards = store.fuzzy_search(&q_str)?;
    if cards.is_empty() {
        println!("{DIM}  No tasks found.{RESET}");
        return Ok(());
    }

    let mut fzf_input = String::new();
    for c in &cards {
        let proj = c.meta.project.as_deref().unwrap_or("");
        fzf_input.push_str(&format!(
            "{:<6} │ {:<2} │ {:<13} │ @{:<8} │ {} {} \t{}\n",
            c.meta.id,
            c.meta.priority,
            c.meta.status.as_str(),
            c.meta.owner,
            c.meta.title,
            proj,
            c.path
        ));
    }

    let child = Command::new("fzf")
        .args([
            "--ansi",
            "--delimiter=\t",
            "--with-nth=1",
            "--prompt=🔎 tk > ",
            "--height=65%",
            "--layout=reverse",
            "--border=rounded",
            "--preview=bat --color=always --style=numbers {2} 2>/dev/null || cat {2}",
            "--preview-window=right:55%:wrap",
            &format!("--query={}", q_str),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();

    match child {
        Ok(mut proc) => {
            if let Some(mut stdin) = proc.stdin.take() {
                let _ = stdin.write_all(fzf_input.as_bytes());
            }
            let out = proc.wait_with_output()?;
            if !out.status.success() {
                return Ok(());
            }
            let selected = String::from_utf8_lossy(&out.stdout);
            let Some(id_token) = selected.split_whitespace().next() else {
                return Ok(());
            };
            let card = store.find_task(id_token)?;
            if edit {
                store.open_in_editor(std::path::Path::new(&card.path))?;
            } else if open {
                Command::new("tk").args(["open", &card.meta.id]).status()?;
            } else {
                Command::new("tk").args(["show", &card.meta.id]).status()?;
            }
        }
        Err(_) => {
            // Fallback if `fzf` is not installed
            print_table(store, &cards);
        }
    }

    Ok(())
}

pub fn compute_cpm(all_cards: &[TaskCard]) -> CpmAnalysis {
    let mut node_ids = std::collections::BTreeSet::new();
    for c in all_cards {
        node_ids.insert(c.meta.id.clone());
        for bid in &c.meta.blocked_by {
            node_ids.insert(bid.clone());
        }
    }

    let mut card_map = std::collections::HashMap::new();
    for c in all_cards {
        if node_ids.contains(&c.meta.id) {
            card_map.insert(c.meta.id.clone(), c.clone());
        }
    }

    let mut duration_map = std::collections::HashMap::new();
    for (id, c) in &card_map {
        let secs = if let Some(ref dur_str) = c.meta.duration {
            parse_duration_seconds(dur_str).unwrap_or(86400)
        } else if let (Some(due_str), Ok(c_date)) = (
            &c.meta.due,
            chrono::NaiveDate::parse_from_str(&c.meta.created, "%Y-%m-%d"),
        ) {
            if let Ok(d_date) = chrono::NaiveDate::parse_from_str(due_str, "%Y-%m-%d") {
                ((d_date - c_date).num_days().max(1) as u64) * 86400
            } else {
                86400
            }
        } else {
            86400
        };
        duration_map.insert(id.clone(), secs);
    }

    let mut preds: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut succs: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for id in &node_ids {
        preds.entry(id.clone()).or_default();
        succs.entry(id.clone()).or_default();
    }
    for (id, c) in &card_map {
        for bid in &c.meta.blocked_by {
            if node_ids.contains(bid) {
                preds.entry(id.clone()).or_default().push(bid.clone());
                succs.entry(bid.clone()).or_default().push(id.clone());
            }
        }
    }

    let mut in_degree: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for id in &node_ids {
        in_degree.insert(id.clone(), preds.get(id).map_or(0, |p| p.len()));
    }
    let mut queue: std::collections::VecDeque<String> = in_degree
        .iter()
        .filter(|(_, deg)| **deg == 0)
        .map(|(id, _)| id.clone())
        .collect();

    let mut topo_order = Vec::new();
    while let Some(u) = queue.pop_front() {
        topo_order.push(u.clone());
        if let Some(downstream) = succs.get(&u) {
            for v in downstream {
                if let Some(deg) = in_degree.get_mut(v) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(v.clone());
                    }
                }
            }
        }
    }

    if topo_order.len() < node_ids.len() {
        for id in &node_ids {
            if !topo_order.contains(id) {
                topo_order.push(id.clone());
            }
        }
    }

    let mut es_map = std::collections::HashMap::new();
    let mut ef_map = std::collections::HashMap::new();
    for u in &topo_order {
        let d = *duration_map.get(u).unwrap_or(&86400);
        let max_pred_ef = preds
            .get(u)
            .and_then(|p_list| p_list.iter().filter_map(|p| ef_map.get(p)).copied().max())
            .unwrap_or(0);
        es_map.insert(u.clone(), max_pred_ef);
        ef_map.insert(u.clone(), max_pred_ef + d);
    }

    let total_project_duration_secs = ef_map.values().copied().max().unwrap_or(0);

    let mut lf_map = std::collections::HashMap::new();
    let mut ls_map = std::collections::HashMap::new();
    for u in topo_order.iter().rev() {
        let d = *duration_map.get(u).unwrap_or(&86400);
        let min_succ_ls = succs
            .get(u)
            .and_then(|s_list| s_list.iter().filter_map(|s| ls_map.get(s)).copied().min())
            .unwrap_or(total_project_duration_secs);
        lf_map.insert(u.clone(), min_succ_ls);
        ls_map.insert(u.clone(), min_succ_ls.saturating_sub(d));
    }

    let mut schedule = Vec::new();
    for id in &topo_order {
        let es = *es_map.get(id).unwrap_or(&0);
        let ef = *ef_map.get(id).unwrap_or(&0);
        let ls = *ls_map.get(id).unwrap_or(&0);
        let lf = *lf_map.get(id).unwrap_or(&0);
        let d = *duration_map.get(id).unwrap_or(&86400);
        let buffer = ls.saturating_sub(es);
        let is_critical = buffer == 0;
        let d_days = (d / 86400).max(1) as u32;
        let buffer_days = (buffer / 86400) as u32;

        let (title, status, owner, project, blocked_by) = if let Some(c) = card_map.get(id) {
            (
                c.meta.title.clone(),
                c.meta.status.as_str().to_string(),
                c.meta.owner.clone(),
                c.meta.project.clone(),
                c.meta.blocked_by.clone(),
            )
        } else {
            (
                id.clone(),
                "unknown".to_string(),
                "unknown".to_string(),
                None,
                vec![],
            )
        };

        schedule.push(CpmTaskSchedule {
            id: id.clone(),
            title,
            project,
            duration_seconds: d,
            duration_display: format_duration_concise(d),
            duration_days: d_days,
            early_start: es,
            early_finish: ef,
            late_start: ls,
            late_finish: lf,
            buffer_seconds: buffer,
            buffer_display: format_duration_concise(buffer),
            buffer_days,
            is_critical,
            status,
            owner,
            blocked_by,
        });
    }

    let critical_path: Vec<String> = schedule
        .iter()
        .filter(|t| t.is_critical)
        .map(|t| t.id.clone())
        .collect();

    let project_duration_display = format_duration_concise(total_project_duration_secs);
    let project_duration_days = (total_project_duration_secs / 86400).max(1) as u32;

    let summary_short =
        generate_cpm_summary_short(&project_duration_display, &critical_path, &schedule);
    let summary_long =
        generate_cpm_summary_long(&project_duration_display, &critical_path, &schedule);

    CpmAnalysis {
        project_duration_seconds: total_project_duration_secs,
        project_duration_display,
        project_duration_days,
        critical_path,
        summary_short,
        summary_long,
        schedule,
    }
}

pub fn generate_cpm_summary_short(
    duration: &str,
    critical_path: &[String],
    schedule: &[CpmTaskSchedule],
) -> String {
    if schedule.is_empty() {
        return "No tasks recorded. The project schedule is empty.".to_string();
    }
    let total_tasks = schedule.len();
    let crit_count = schedule.iter().filter(|t| t.is_critical).count();
    let blocked_tasks: Vec<_> = schedule.iter().filter(|t| t.status == "blocked").collect();

    let path_str = if critical_path.is_empty() {
        "None".to_string()
    } else {
        critical_path.join(" ➔ ")
    };

    let mut summary = format!(
        "Project duration: {}. Critical path: {}. {} of {} tasks are critical with zero buffer.",
        duration, path_str, crit_count, total_tasks
    );

    if !blocked_tasks.is_empty() {
        let blocked_names: Vec<String> = blocked_tasks
            .iter()
            .map(|t| {
                if t.blocked_by.is_empty() {
                    t.id.clone()
                } else {
                    format!("{} (blocked by {})", t.id, t.blocked_by.join(", "))
                }
            })
            .collect();
        summary.push_str(&format!(" Blocked tasks: {}.", blocked_names.join("; ")));
    }

    summary
}

pub fn generate_cpm_summary_long(
    duration: &str,
    critical_path: &[String],
    schedule: &[CpmTaskSchedule],
) -> String {
    if schedule.is_empty() {
        return "No tasks recorded. The project schedule is empty.".to_string();
    }

    let mut s = String::new();
    s.push_str(&format!("{DIM}================================================================================{RESET}\n"));
    s.push_str(&format!(
        "{BOLD_WHITE}CRITICAL PATH METHOD (CPM) PROJECT SCHEDULE ANALYSIS{RESET}\n"
    ));
    s.push_str(&format!("{DIM}================================================================================{RESET}\n\n"));

    let total = schedule.len();
    let critical_tasks: Vec<&CpmTaskSchedule> = schedule.iter().filter(|t| t.is_critical).collect();
    let float_tasks: Vec<&CpmTaskSchedule> = schedule.iter().filter(|t| !t.is_critical).collect();
    let blocked_tasks: Vec<&CpmTaskSchedule> =
        schedule.iter().filter(|t| t.status == "blocked").collect();

    s.push_str(&format!("{BOLD_WHITE}1. EXECUTIVE SUMMARY{RESET}\n"));
    s.push_str(&format!(
        "- Total Project Duration: {BOLD}{}{RESET}\n",
        duration
    ));
    s.push_str(&format!("- Total Active Tasks: {}\n", total));
    s.push_str(&format!(
        "- Critical Path Tasks: {BOLD}{RED}{}{RESET} ({:.0}% of project)\n",
        critical_tasks.len(),
        (critical_tasks.len() as f64 / total as f64) * 100.0
    ));
    s.push_str(&format!(
        "- Non-Critical Tasks with Float/Buffer: {}\n\n",
        float_tasks.len()
    ));

    s.push_str(&format!(
        "{BOLD_WHITE}2. CRITICAL PATH EXECUTION CHAIN (ZERO FLOAT){RESET}\n"
    ));
    s.push_str("Any delay to these tasks directly delays project completion:\n");
    if critical_tasks.is_empty() {
        s.push_str("- None detected.\n");
    } else {
        for t in &critical_tasks {
            s.push_str(&format!(
                "- {BOLD}{AQUA}{}{RESET} [{}] \"{}\"\n  Dur: {} | ES: {} · EF: {} | Owner: @{}\n",
                t.id,
                t.status,
                t.title,
                t.duration_display,
                format_duration_concise(t.early_start),
                format_duration_concise(t.early_finish),
                t.owner
            ));
        }
        s.push_str(&format!(
            "\nSequence: {BOLD}{RED}{}{RESET}\n\n",
            critical_path.join(" ➔ ")
        ));
    }

    s.push_str(&format!(
        "{BOLD_WHITE}3. BOTTLENECK & DEPENDENCY ANALYSIS{RESET}\n"
    ));
    if blocked_tasks.is_empty() {
        s.push_str("- No tasks are currently in blocked status.\n\n");
    } else {
        for t in &blocked_tasks {
            s.push_str(&format!(
                "- {BOLD}{RED}{}{RESET} is BLOCKED. Requires {} to complete first.\n",
                t.id,
                t.blocked_by.join(" and ")
            ));
        }
        s.push('\n');
    }

    s.push_str(&format!(
        "{BOLD_WHITE}4. SCHEDULE BUFFERS (FLOAT / BUFFER){RESET}\n"
    ));
    if float_tasks.is_empty() {
        s.push_str("- No tasks have buffer. All tasks operate on the critical edge.\n\n");
    } else {
        for t in &float_tasks {
            s.push_str(&format!(
                "- {BOLD}{AQUA}{}{RESET}: \"{}\"\n  Buffer: {GREEN}{}{RESET} | ES: {} · LS: {}\n",
                t.id,
                t.title,
                t.buffer_display,
                format_duration_concise(t.early_start),
                format_duration_concise(t.late_start)
            ));
        }
        s.push('\n');
    }

    s.push_str(&format!(
        "{BOLD_WHITE}5. ACTION PLAN FOR AGENTS AND HUMANS{RESET}\n"
    ));
    let mut step = 1;
    let pending_close_tasks: Vec<_> = schedule
        .iter()
        .filter(|t| t.status == "pending-close")
        .collect();
    if !pending_close_tasks.is_empty() {
        let p_ids: Vec<_> = pending_close_tasks
            .iter()
            .map(|t| format!("{BOLD}{AQUA}{}{RESET} (@{})", t.id, t.owner))
            .collect();
        s.push_str(&format!(
            "{}. Human sign-off: review and close tasks in pending-close: {}.\n",
            step,
            p_ids.join(", ")
        ));
        step += 1;
    }
    if !blocked_tasks.is_empty() {
        s.push_str(&format!(
            "{}. Cascade Unblock: Closing blocker tasks will promote {} from blocked to ready.\n",
            step,
            blocked_tasks
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        step += 1;
    }
    let in_progress_tasks: Vec<_> = schedule
        .iter()
        .filter(|t| t.status == "in-progress")
        .collect();
    if !in_progress_tasks.is_empty() {
        let ip_ids: Vec<_> = in_progress_tasks
            .iter()
            .map(|t| format!("{BOLD}{AQUA}{}{RESET} (@{})", t.id, t.owner))
            .collect();
        s.push_str(&format!(
            "{}. Active Work: Focus execution on in-progress tasks: {}.\n",
            step,
            ip_ids.join(", ")
        ));
    }
    s.push_str(&format!("{DIM}================================================================================{RESET}\n"));

    s
}

pub fn colorize_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut i = 0;

    while i < n {
        let ch = chars[i];
        if ch == '"' {
            let start = i;
            i += 1;
            while i < n {
                if chars[i] == '\\' {
                    i += 2; // skip escape sequence
                } else if chars[i] == '"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            let str_val: String = chars[start..i].iter().collect();

            // Look ahead to see if ':' follows (ignoring whitespace)
            let mut j = i;
            while j < n && chars[j].is_whitespace() {
                j += 1;
            }
            let is_key = j < n && chars[j] == ':';

            if is_key {
                out.push_str("\x1b[1;36m"); // Bold Cyan for keys
                out.push_str(&str_val);
                out.push_str("\x1b[0m");
            } else {
                out.push_str("\x1b[38;2;152;195;121m"); // Green for string values
                out.push_str(&str_val);
                out.push_str("\x1b[0m");
            }
        } else if ch == '-' || ch.is_ascii_digit() {
            let start = i;
            while i < n
                && (chars[i].is_ascii_digit()
                    || chars[i] == '.'
                    || chars[i] == 'e'
                    || chars[i] == 'E'
                    || chars[i] == '-'
                    || chars[i] == '+')
            {
                i += 1;
            }
            let num_val: String = chars[start..i].iter().collect();
            out.push_str("\x1b[38;2;209;154;102m"); // Orange/Yellow for numbers
            out.push_str(&num_val);
            out.push_str("\x1b[0m");
        } else if ch.is_ascii_alphabetic() {
            let start = i;
            while i < n && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            match word.as_str() {
                "true" | "false" => {
                    out.push_str("\x1b[38;2;198;120;221m"); // Purple for booleans
                    out.push_str(&word);
                    out.push_str("\x1b[0m");
                }
                "null" => {
                    out.push_str("\x1b[2m"); // Dim for null
                    out.push_str(&word);
                    out.push_str("\x1b[0m");
                }
                _ => out.push_str(&word),
            }
        } else {
            out.push(ch);
            i += 1;
        }
    }
    out
}

pub fn print_json_value<T: serde::Serialize>(val: &T, force_color: bool) -> anyhow::Result<()> {
    use std::io::IsTerminal;
    let json_str = serde_json::to_string_pretty(val)?;
    let should_color =
        (std::io::stdout().is_terminal() || force_color) && std::env::var("NO_COLOR").is_err();
    if should_color {
        crate::ansi::set_enabled(true);
        println!("{}", colorize_json(&json_str));
    } else {
        println!("{}", json_str);
    }
    Ok(())
}

pub fn render_terminal_gantt(analysis: &CpmAnalysis, term_w: usize) {
    if analysis.schedule.is_empty() {
        println!("\n{BOLD_WHITE}🎯 TIMELINE & EXECUTION SCHEDULE (TERMINAL GANTT){RESET}");
        println!("  {DIM}No tasks recorded.{RESET}\n");
        return;
    }

    println!("\n{BOLD_WHITE}🎯 TIMELINE & EXECUTION SCHEDULE (TERMINAL GANTT){RESET}");
    println!("{DIM}Horizontal bars calculated from CPM early-start and duration offsets.{RESET}\n");

    let total_secs = analysis.project_duration_seconds.max(1) as f64;

    // Layout dimensions based on term_w
    let (label_w, bar_w, show_title) = if term_w >= 105 {
        let bar = (term_w.saturating_sub(55)).max(25).min(65);
        (22, bar, true)
    } else if term_w >= 75 {
        let bar = (term_w.saturating_sub(36)).max(20).min(45);
        (18, bar, false)
    } else {
        let bar = (term_w.saturating_sub(22)).max(15);
        (12, bar, false)
    };

    // Time Axis Ruler
    let t0_str = "0s";
    let t_mid_secs = (total_secs / 2.0).round() as u64;
    let t_mid_str = format_duration_concise(t_mid_secs);
    let t_end_str = &analysis.project_duration_display;

    let mid_pos = bar_w / 2;
    let mut ruler_chars = vec![' '; bar_w];

    for (idx, ch) in t0_str.chars().enumerate() {
        if idx < bar_w {
            ruler_chars[idx] = ch;
        }
    }
    let mid_start = mid_pos.saturating_sub(t_mid_str.len() / 2);
    if mid_start > t0_str.len() + 1
        && mid_start + t_mid_str.len() + 1 < bar_w.saturating_sub(t_end_str.len())
    {
        for (idx, ch) in t_mid_str.chars().enumerate() {
            if mid_start + idx < bar_w {
                ruler_chars[mid_start + idx] = ch;
            }
        }
    }
    let end_start = bar_w.saturating_sub(t_end_str.len());
    if end_start > mid_start + t_mid_str.len() {
        for (idx, ch) in t_end_str.chars().enumerate() {
            if end_start + idx < bar_w {
                ruler_chars[end_start + idx] = ch;
            }
        }
    }
    let ruler_str: String = ruler_chars.into_iter().collect();

    // Header line
    let hdr_label = if label_w >= 22 {
        format!(
            "{BOLD_WHITE}{:<7} {:<7} {:<6}{RESET}",
            "ID", "Owner", "Status"
        )
    } else if label_w >= 18 {
        format!("{BOLD_WHITE}{:<7} {:<10}{RESET}", "ID", "Status")
    } else {
        format!("{BOLD_WHITE}{:<12}{RESET}", "Task")
    };

    println!("  {} │ {DIM}{}{RESET}", hdr_label, ruler_str);

    // Divider line
    let div_left = "─".repeat(label_w);
    let mut div_bar_chars = vec!['─'; bar_w];
    if mid_pos < bar_w {
        div_bar_chars[mid_pos] = '┬';
    }
    let div_bar: String = div_bar_chars.into_iter().collect();
    println!("  {DIM}{}┼{}┤{RESET}", div_left, div_bar);

    for t in &analysis.schedule {
        let start_ratio = (t.early_start as f64 / total_secs).clamp(0.0, 1.0);
        let finish_ratio = (t.early_finish as f64 / total_secs).clamp(0.0, 1.0);
        let late_finish_ratio = (t.late_finish as f64 / total_secs).clamp(0.0, 1.0);

        let start_col = ((start_ratio * bar_w as f64).round() as usize).min(bar_w);
        let mut finish_col = ((finish_ratio * bar_w as f64).round() as usize).min(bar_w);
        if finish_col <= start_col && bar_w > 0 {
            finish_col = (start_col + 1).min(bar_w);
        }
        let late_finish_col = ((late_finish_ratio * bar_w as f64).round() as usize)
            .max(finish_col)
            .min(bar_w);

        let (bar_color, is_crit) = if t.is_critical {
            if t.status == "pending-close" {
                (format!("{BOLD}{PURPLE}"), true)
            } else {
                (format!("{BOLD}{RED}"), true)
            }
        } else {
            match t.status.as_str() {
                "done" | "archived" => (GREEN.to_string(), false),
                "pending-close" => (PURPLE.to_string(), false),
                "in-progress" => (format!("{BOLD}{ORANGE}"), false),
                "blocked" => (RED.to_string(), false),
                _ => (AQUA.to_string(), false),
            }
        };

        let mut bar_render = String::new();
        for col in 0..bar_w {
            if col >= start_col && col < finish_col {
                bar_render.push_str(&format!("{}█{RESET}", bar_color));
            } else if col >= finish_col && col < late_finish_col {
                bar_render.push_str(&format!("{DIM}░{RESET}"));
            } else {
                bar_render.push(' ');
            }
        }

        let status_short = match t.status.as_str() {
            "pending-close" => "◐ rev",
            "in-progress" => "● act",
            "blocked" => "✖ blk",
            "ready" => "○ rdy",
            "done" => "✓ don",
            "inbox" => "◌ inb",
            _ => "·    ",
        };

        let row_label = if label_w >= 22 {
            let owner_trunc = if t.owner.len() > 6 {
                &t.owner[..6]
            } else {
                &t.owner
            };
            format!(
                "{BOLD}{:<7}{RESET} {DIM}@{:<6}{RESET} {:<6}",
                t.id, owner_trunc, status_short
            )
        } else if label_w >= 18 {
            format!("{BOLD}{:<7}{RESET} {:<10}", t.id, status_short)
        } else {
            format!("{BOLD}{:<12}{RESET}", t.id)
        };

        let dur_info = if is_crit {
            format!(" {BOLD}{RED}{}{RESET}", t.duration_display)
        } else if t.buffer_seconds > 0 {
            format!(
                " {} {DIM}(+{}){RESET}",
                t.duration_display, t.buffer_display
            )
        } else {
            format!(" {}", t.duration_display)
        };

        let title_info = if show_title {
            let max_t = term_w.saturating_sub(label_w + bar_w + dur_info.len() + 10);
            if max_t >= 8 {
                let mut title_sub = t.title.clone();
                if title_sub.chars().count() > max_t {
                    title_sub = title_sub
                        .chars()
                        .take(max_t.saturating_sub(1))
                        .collect::<String>()
                        + "…";
                }
                format!(" {DIM}\"{}\"{RESET}", title_sub)
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        println!(
            "  {} {DIM}│{RESET} {}{}{}",
            row_label, bar_render, dur_info, title_info
        );
    }

    let mut div_bottom_chars = vec!['─'; bar_w];
    if mid_pos < bar_w {
        div_bottom_chars[mid_pos] = '┴';
    }
    let div_bottom: String = div_bottom_chars.into_iter().collect();
    println!("  {DIM}{}┴{}┘{RESET}", div_left, div_bottom);

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
    println!(
        "  {DIM}Legend: {BOLD}{RED}█{RESET} {DIM}Critical {BOLD}{PURPLE}█{RESET} {DIM}Review {BOLD}{ORANGE}█{RESET} {DIM}Active {AQUA}█{RESET} {DIM}Ready {GREEN}█{RESET} {DIM}Done {DIM}░ Buffer{RESET}\n"
    );
}

pub fn render_terminal_aon(analysis: &CpmAnalysis, term_w: usize, project_filter: Option<&str>) {
    let mut schedule: Vec<&CpmTaskSchedule> = analysis.schedule.iter().collect();

    if let Some(pf) = project_filter {
        let pf_lower = pf.trim().to_lowercase();
        if pf_lower == "global" || pf_lower == "none" {
            schedule.retain(|t| t.project.is_none());
        } else {
            schedule.retain(|t| {
                t.project
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&pf_lower)
            });
        }
    }

    if schedule.is_empty() {
        println!("\n{BOLD_WHITE}🕸️  ACTIVITY-ON-NODE (AON) EXECUTION NETWORK{RESET}");
        if let Some(pf) = project_filter {
            println!("  {DIM}No tasks found matching project: {AQUA}{pf}{RESET}\n");
        } else {
            println!("  {DIM}No tasks recorded.{RESET}\n");
        }
        return;
    }

    println!("\n{BOLD_WHITE}🕸️  ACTIVITY-ON-NODE (AON) EXECUTION NETWORK{RESET}");
    println!("{DIM}Precedence nodes, topological execution waves, and critical path edges.{RESET}");

    if let Some(pf) = project_filter {
        println!("  {AQUA}Scope:{RESET} {BOLD}Project: +{}{RESET}\n", pf);
    } else {
        println!("  {DIM}Scope: All Projects (Global Workspace){RESET}\n");
    }

    let mut es_times: Vec<u64> = schedule.iter().map(|t| t.early_start).collect();
    es_times.sort_unstable();
    es_times.dedup();

    let box_w = term_w.saturating_sub(6).min(66).max(44);
    let inner_w = box_w.saturating_sub(4);

    let sched_map: HashMap<&str, &CpmTaskSchedule> = analysis
        .schedule
        .iter()
        .map(|s| (s.id.as_str(), s))
        .collect();

    for (stage_idx, &stage_es) in es_times.iter().enumerate() {
        let stage_tasks: Vec<&&CpmTaskSchedule> = schedule
            .iter()
            .filter(|t| t.early_start == stage_es)
            .collect();
        let es_display = format_duration_concise(stage_es);

        println!(
            "  {BOLD_WHITE}STAGE {} · Offset t = {}{RESET} {DIM}({} task{}){RESET}",
            stage_idx + 1,
            es_display,
            stage_tasks.len(),
            if stage_tasks.len() == 1 { "" } else { "s" }
        );
        println!("  {DIM}────────────────────────────────────────────────────────────{RESET}");

        for t in &stage_tasks {
            let cond_badge = if t.is_critical {
                if t.status == "pending-close" {
                    format!("{BOLD}{PURPLE}[CRITICAL · REVIEW]{RESET}")
                } else {
                    format!("{BOLD}{RED}[CRITICAL]{RESET}")
                }
            } else if t.buffer_seconds > 0 {
                format!("{DIM}[FLOAT +{}]{RESET}", t.buffer_display)
            } else {
                format!("{DIM}[FLOAT 0s]{RESET}")
            };

            let cond_plain_len = if t.is_critical {
                if t.status == "pending-close" { 19 } else { 10 }
            } else if t.buffer_seconds > 0 {
                9 + t.buffer_display.chars().count()
            } else {
                10
            };

            let (proj_display, proj_plain_len) = if let Some(p) = &t.project {
                let clean_p = p.trim_matches('[').trim_matches(']');
                (
                    format!("{PURPLE}+{}{RESET}", clean_p),
                    clean_p.chars().count() + 1,
                )
            } else {
                (format!("{DIM}[global]{RESET}"), 8)
            };

            // Box top
            println!("  {DIM}┌{}┐{RESET}", "─".repeat(box_w.saturating_sub(2)));

            // Line 1: ID, Project, Condition
            let line1_visible = t.id.chars().count() + 2 + proj_plain_len + cond_plain_len;
            let pad1 = inner_w.saturating_sub(line1_visible);
            println!(
                "  {DIM}│{RESET} {BOLD}{}{RESET} {} {}{}{} {DIM}│{RESET}",
                t.id,
                proj_display,
                " ".repeat(pad1),
                cond_badge,
                RESET
            );

            // Line 2: Title
            let mut title_sub = t.title.clone();
            if title_sub.chars().count() > inner_w {
                title_sub = title_sub
                    .chars()
                    .take(inner_w.saturating_sub(1))
                    .collect::<String>()
                    + "…";
            }
            let pad_title = inner_w.saturating_sub(title_sub.chars().count());
            println!(
                "  {DIM}│{RESET} {}{}{} {DIM}│{RESET}",
                title_sub,
                " ".repeat(pad_title),
                RESET
            );

            // Line 3: Metrics (Dur, ES, EF, Buffer, Owner)
            let es_str = format_duration_concise(t.early_start);
            let ef_str = format_duration_concise(t.early_finish);
            let metrics_str = format!(
                "d: {} │ ES: {} · EF: {} │ Buf: {} │ @{}",
                t.duration_display, es_str, ef_str, t.buffer_display, t.owner
            );
            let mut metrics_sub = metrics_str.clone();
            if metrics_sub.chars().count() > inner_w {
                metrics_sub = metrics_sub
                    .chars()
                    .take(inner_w.saturating_sub(1))
                    .collect::<String>()
                    + "…";
            }
            let pad_metrics = inner_w.saturating_sub(metrics_sub.chars().count());
            println!(
                "  {DIM}│{RESET} {}{}{} {DIM}│{RESET}",
                metrics_sub,
                " ".repeat(pad_metrics),
                RESET
            );

            // Line 4: Prerequisites if any
            if !t.blocked_by.is_empty() {
                let reqs = format!("Needs: {}", t.blocked_by.join(", "));
                let mut reqs_sub = reqs.clone();
                if reqs_sub.chars().count() > inner_w {
                    reqs_sub = reqs_sub
                        .chars()
                        .take(inner_w.saturating_sub(1))
                        .collect::<String>()
                        + "…";
                }
                let pad_reqs = inner_w.saturating_sub(reqs_sub.chars().count());
                println!(
                    "  {DIM}│{RESET} {RED}{}{RESET}{} {DIM}│{RESET}",
                    reqs_sub,
                    " ".repeat(pad_reqs)
                );
            }

            // Box bottom
            println!("  {DIM}└{}┘{RESET}", "─".repeat(box_w.saturating_sub(2)));
        }

        // Inter-stage arrows
        if stage_idx + 1 < es_times.len() {
            let next_es = es_times[stage_idx + 1];
            let has_crit_handoff = schedule.iter().any(|pred| {
                pred.early_start == stage_es
                    && pred.is_critical
                    && schedule.iter().any(|succ| {
                        succ.early_start == next_es
                            && succ.is_critical
                            && succ.blocked_by.iter().any(|b| b == &pred.id)
                    })
            });

            if has_crit_handoff {
                println!("       {BOLD}{RED}║║ (Critical Precedence Hand-Off){RESET}");
                println!("       {BOLD}{RED}▼▼{RESET}");
            } else {
                println!("       {DIM}││ (Precedence Hand-Off){RESET}");
                println!("       {DIM}▼▼{RESET}");
            }
        }
    }

    println!("\n  {BOLD_WHITE}🔗 DIRECT PRECEDENCE & CRITICAL EDGES:{RESET}");
    let mut edge_count = 0;
    for t in &schedule {
        for bid in &t.blocked_by {
            edge_count += 1;
            let is_crit_edge = if let Some(pred) = sched_map.get(bid.as_str()) {
                pred.is_critical && t.is_critical && pred.early_finish == t.early_start
            } else {
                false
            };

            let pred_proj = sched_map
                .get(bid.as_str())
                .and_then(|p| p.project.as_deref())
                .map(|p| format!("({})", p.trim_matches('[').trim_matches(']')))
                .unwrap_or_else(|| "(global)".to_string());
            let succ_proj = t
                .project
                .as_deref()
                .map(|p| format!("({})", p.trim_matches('[').trim_matches(']')))
                .unwrap_or_else(|| "(global)".to_string());

            if is_crit_edge {
                println!(
                    "    {BOLD}{RED}{} {} ══> {} {} (0s buffer · CRITICAL PATH){RESET}",
                    bid, pred_proj, t.id, succ_proj
                );
            } else {
                println!(
                    "    {DIM}{} {} ──> {} {} (+{} buffer){RESET}",
                    bid, pred_proj, t.id, succ_proj, t.buffer_display
                );
            }
        }
    }

    if edge_count == 0 {
        println!(
            "    {DIM}No inter-task dependencies recorded (all tasks in this scope are unconstrained).{RESET}"
        );
    }

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
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{TaskCard, TaskFrontmatter, TaskStatus};

    fn make_test_card(
        id: &str,
        title: &str,
        created: &str,
        due: Option<&str>,
        blocked_by: Vec<&str>,
    ) -> TaskCard {
        TaskCard {
            meta: TaskFrontmatter {
                id: id.to_string(),
                title: title.to_string(),
                kind: "task".to_string(),
                status: TaskStatus::Ready,
                priority: "P1".to_string(),
                owner: "ken".to_string(),
                topic: vec![],
                tags: vec![],
                project: None,
                due: due.map(|s| s.to_string()),
                duration: None,
                session_id: None,
                blocked_by: blocked_by.into_iter().map(|s| s.to_string()).collect(),
                blocks: vec![],
                sessions: vec![],
                dod: vec![],
                eval: None,
                eval_status: None,
                claimed_eval: None,
                receipt: None,
                approval: None,
                approved_eval: None,
                claimed_eval_inputs: Default::default(),
                check_pinned_at: None,
                approved_eval_inputs: Default::default(),
                created: created.to_string(),
                updated: created.to_string(),
                _organized: true,
            },
            body: String::new(),
            path: format!("/path/to/{}.md", id),
            scope: crate::model::CardScope::Global,
        }
    }

    fn make_timed_card(id: &str, title: &str, duration: &str, blocked_by: Vec<&str>) -> TaskCard {
        TaskCard {
            meta: TaskFrontmatter {
                id: id.to_string(),
                title: title.to_string(),
                kind: "task".to_string(),
                status: TaskStatus::Ready,
                priority: "P1".to_string(),
                owner: "hermes".to_string(),
                topic: vec![],
                tags: vec![],
                project: None,
                due: None,
                duration: Some(duration.to_string()),
                session_id: None,
                blocked_by: blocked_by.into_iter().map(|s| s.to_string()).collect(),
                blocks: vec![],
                sessions: vec![],
                dod: vec![],
                eval: None,
                eval_status: None,
                claimed_eval: None,
                receipt: None,
                approval: None,
                approved_eval: None,
                claimed_eval_inputs: Default::default(),
                check_pinned_at: None,
                approved_eval_inputs: Default::default(),
                created: "2026-10-18".to_string(),
                updated: "2026-10-18".to_string(),
                _organized: true,
            },
            body: String::new(),
            path: format!("/path/to/{}.md", id),
            scope: crate::model::CardScope::Global,
        }
    }

    #[test]
    fn test_compute_cpm_linear_chain() {
        let cards = vec![
            make_test_card("tk-001", "Task 1", "2026-10-01", Some("2026-10-03"), vec![]), // 2 days
            make_test_card(
                "tk-002",
                "Task 2",
                "2026-10-03",
                Some("2026-10-06"),
                vec!["tk-001"],
            ), // 3 days
        ];
        let analysis = compute_cpm(&cards);
        assert_eq!(analysis.project_duration_days, 5);
        assert_eq!(analysis.critical_path, vec!["tk-001", "tk-002"]);
        for t in &analysis.schedule {
            assert_eq!(t.buffer_days, 0);
            assert!(t.is_critical);
        }
    }

    #[test]
    fn test_compute_cpm_sub_day_durations() {
        // Agent tasks: tk-001 (15m) -> tk-002 (30m) = 45m
        // tk-003 (20m) parallel, floats by 25m
        let cards = vec![
            make_timed_card("tk-001", "Run benchmark", "15m", vec![]),
            make_timed_card("tk-002", "Analyze results", "30m", vec!["tk-001"]),
            make_timed_card("tk-003", "Scrape doc links", "20m", vec![]),
        ];
        let analysis = compute_cpm(&cards);
        assert_eq!(analysis.project_duration_seconds, 2700); // 45m
        assert_eq!(analysis.project_duration_display, "45m");
        assert_eq!(analysis.critical_path, vec!["tk-001", "tk-002"]);

        let t3 = analysis.schedule.iter().find(|t| t.id == "tk-003").unwrap();
        assert_eq!(t3.duration_seconds, 1200); // 20m
        assert_eq!(t3.buffer_seconds, 1500); // 25m buffer
        assert_eq!(t3.buffer_display, "25m");
        assert!(!t3.is_critical);
    }

    #[test]
    fn test_compute_cpm_buffer_calculation() {
        // Path A: tk-001 (1d) -> tk-003 (2d) = 3d
        // Path B: tk-002 (5d) -> tk-003 (2d) = 7d
        // Critical path must be tk-002 -> tk-003 (7d duration)
        // tk-001 must have schedule buffer of 4d
        let cards = vec![
            make_test_card(
                "tk-001",
                "Short Task",
                "2026-10-01",
                Some("2026-10-02"),
                vec![],
            ), // 1d
            make_test_card(
                "tk-002",
                "Long Task",
                "2026-10-01",
                Some("2026-10-06"),
                vec![],
            ), // 5d
            make_test_card(
                "tk-003",
                "Final Task",
                "2026-10-06",
                Some("2026-10-08"),
                vec!["tk-001", "tk-002"],
            ), // 2d
        ];
        let analysis = compute_cpm(&cards);
        assert_eq!(analysis.project_duration_days, 7);
        assert_eq!(analysis.critical_path, vec!["tk-002", "tk-003"]);

        let t1 = analysis.schedule.iter().find(|t| t.id == "tk-001").unwrap();
        assert_eq!(t1.duration_days, 1);
        assert_eq!(t1.early_start, 0);
        assert_eq!(t1.early_finish, 86400);
        assert_eq!(t1.late_start, 345600);
        assert_eq!(t1.late_finish, 432000);
        assert_eq!(t1.buffer_days, 4);
        assert_eq!(t1.buffer_seconds, 345600);
        assert!(!t1.is_critical);

        let t2 = analysis.schedule.iter().find(|t| t.id == "tk-002").unwrap();
        assert_eq!(t2.duration_days, 5);
        assert_eq!(t2.buffer_days, 0);
        assert!(t2.is_critical);

        let t3 = analysis.schedule.iter().find(|t| t.id == "tk-003").unwrap();
        assert_eq!(t3.duration_days, 2);
        assert_eq!(t3.buffer_days, 0);
        assert!(t3.is_critical);

        let json_str = serde_json::to_string(&t1).unwrap();
        assert!(json_str.contains("\"early_start\":0"));
        assert!(json_str.contains("\"early_finish\":86400"));
        assert!(json_str.contains("\"late_start\":345600"));
        assert!(json_str.contains("\"late_finish\":432000"));
        assert!(json_str.contains("\"buffer_seconds\":345600"));
    }

    #[test]
    fn test_cpm_summaries() {
        let cards = vec![
            make_test_card(
                "tk-001",
                "Short Task",
                "2026-10-01",
                Some("2026-10-02"),
                vec![],
            ),
            make_test_card(
                "tk-002",
                "Long Task",
                "2026-10-01",
                Some("2026-10-06"),
                vec![],
            ),
            make_test_card(
                "tk-003",
                "Final Task",
                "2026-10-06",
                Some("2026-10-08"),
                vec!["tk-001", "tk-002"],
            ),
        ];
        let analysis = compute_cpm(&cards);
        assert!(analysis.summary_short.contains("Project duration: 7d"));
        assert!(analysis.summary_short.contains("tk-002 ➔ tk-003"));
        assert!(
            analysis
                .summary_long
                .contains("CRITICAL PATH METHOD (CPM) PROJECT SCHEDULE ANALYSIS")
        );
        assert!(analysis.summary_long.contains("Total Project Duration:"));
        assert!(analysis.summary_long.contains("7d"));
        assert!(
            analysis
                .summary_long
                .contains("SCHEDULE BUFFERS (FLOAT / BUFFER)")
        );
    }

    #[test]
    fn test_colorize_json() {
        let raw = r#"{"project_duration_days": 2, "is_critical": true, "owner": "ken"}"#;
        let colored = colorize_json(raw);
        assert!(colored.contains("\x1b[1;36m\"project_duration_days\"\x1b[0m"));
        assert!(colored.contains("\x1b[38;2;209;154;102m2\x1b[0m"));
        assert!(colored.contains("\x1b[38;2;198;120;221mtrue\x1b[0m"));
        assert!(colored.contains("\x1b[38;2;152;195;121m\"ken\"\x1b[0m"));
    }
}
