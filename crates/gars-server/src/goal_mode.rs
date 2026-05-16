//! Goal-driven driver. Loops `run_task` until the wall-clock budget is
//! exhausted or the user stops the run. The "don't stop early" guidance
//! is loaded from `goal_sop.md` (referenced by `modes/builtin/goal.toml`),
//! not hard-coded here; the driver just provides the nudging *control*.

use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::Local;
use gars_core::{TaskOutcome, TaskRunOpts, run_task};
use gars_llm::build_client;
use gars_memory::GarsPaths;
use gars_skills::{load_mode, mode_hint};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GoalState {
    pub run_id: String,
    pub objective: String,
    pub budget_seconds: u64,
    pub started_at: String,
    #[serde(default)]
    pub turns_used: u32,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub stopped: bool,
    #[serde(default)]
    pub last_reply: Option<String>,
    /// Number of consecutive `Done` outcomes the inner LLM has produced
    /// without using a tool. If this hits `MAX_CONSECUTIVE_DONE` the
    /// driver stops even if budget remains — protects against a
    /// "model says done, runner nudges, model says done again" tight
    /// loop that would otherwise burn the entire budget in seconds.
    #[serde(default)]
    pub consecutive_done: u32,
}

/// Hard ceiling on "model declared completion but budget remains so we
/// nudged it back" rounds. Tuned for v0.7: 3 means we tolerate one or two
/// genuine "I'm done, you nudged me, here's more detail" cycles but
/// stop before a runaway.
const MAX_CONSECUTIVE_DONE: u32 = 3;

pub fn run_dir(paths: &GarsPaths, run_id: &str) -> PathBuf {
    paths.runs.join(run_id)
}

pub fn state_path(paths: &GarsPaths, run_id: &str) -> PathBuf {
    run_dir(paths, run_id).join("goal_state.json")
}

pub fn load(paths: &GarsPaths, run_id: &str) -> Option<GoalState> {
    serde_json::from_str(&fs::read_to_string(state_path(paths, run_id)).ok()?).ok()
}

pub fn save(paths: &GarsPaths, state: &GoalState) -> Result<()> {
    let dir = run_dir(paths, &state.run_id);
    fs::create_dir_all(&dir)?;
    fs::write(
        state_path(paths, &state.run_id),
        serde_json::to_string_pretty(state)?,
    )?;
    Ok(())
}

pub fn list(paths: &GarsPaths) -> Vec<GoalState> {
    let Ok(rd) = fs::read_dir(&paths.runs) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        if let Some(name) = p.file_name().and_then(|s| s.to_str())
            && let Some(g) = load(paths, name)
        {
            out.push(g);
        }
    }
    out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    out
}

pub fn stop(paths: &GarsPaths, run_id: &str) -> Result<()> {
    let mut g = load(paths, run_id).context("goal not found")?;
    g.stopped = true;
    g.status = "stopped".to_string();
    save(paths, &g)?;
    Ok(())
}

#[derive(Clone, Debug, Deserialize)]
pub struct GoalCreate {
    pub objective: String,
    #[serde(default = "default_budget")]
    pub budget_seconds: u64,
    pub llm: Option<String>,
}

fn default_budget() -> u64 {
    1800
}

pub async fn spawn(state: Arc<AppState>, create: GoalCreate) -> Result<GoalState> {
    let run_id = Uuid::new_v4().simple().to_string();
    let goal = GoalState {
        run_id: run_id.clone(),
        objective: create.objective.clone(),
        budget_seconds: create.budget_seconds.max(60),
        started_at: Local::now().to_rfc3339(),
        turns_used: 0,
        status: "running".to_string(),
        stopped: false,
        last_reply: None,
        consecutive_done: 0,
    };
    save(&state.paths, &goal)?;
    let state_for_task = state.clone();
    let llm = create.llm.clone();
    tokio::spawn(async move {
        if let Err(err) = drive(state_for_task, run_id, llm).await {
            tracing::warn!("goal_mode driver: {err:#}");
        }
    });
    Ok(goal)
}

async fn drive(state: Arc<AppState>, run_id: String, llm: Option<String>) -> Result<()> {
    let started = Instant::now();
    let mut input = match load(&state.paths, &run_id) {
        Some(g) => g.objective,
        None => return Ok(()),
    };
    // Load the "goal" mode once for budget defaults + SOP hint. Per v0.7
    // SOP loading policy we inject only a *hint* listing which SOPs are
    // relevant; the LLM fetches bodies on demand via `skill_show`.
    let mode_def = load_mode(&state.paths, "goal").unwrap_or_else(|| gars_skills::ModeDef {
        key: "goal".into(),
        label: "Goal".into(),
        description: String::new(),
        runner_kind: "goal".into(),
        sop_keys: vec![],
        allowed_tools: None,
        budget_secs: None,
        max_turns: None,
        source: "synthetic".into(),
    });
    let hint = mode_hint(&state.paths, &mode_def);
    let sop_contents = if hint.is_empty() { vec![] } else { vec![hint] };

    loop {
        let Some(mut goal) = load(&state.paths, &run_id) else {
            return Ok(());
        };
        if goal.stopped {
            return Ok(());
        }
        let elapsed = started.elapsed().as_secs();
        let remaining = goal.budget_seconds.saturating_sub(elapsed);
        if remaining == 0 {
            goal.status = "budget_exhausted".to_string();
            save(&state.paths, &goal)?;
            return Ok(());
        }
        let deadline = Instant::now() + Duration::from_secs(remaining);
        let cfg = state.config.read().await.clone();
        let selected = llm
            .as_deref()
            .or(cfg.default_llm.as_deref())
            .unwrap_or("primary");
        let client = build_client(&cfg.llm, selected)?;
        let registry = crate::registry(&cfg);
        let system_prompt_base = crate::build_system_prompt(&state.paths, &cfg)?;
        let opts = TaskRunOpts {
            prompt: input.clone(),
            system_prompt_base,
            sop_contents: sop_contents.clone(),
            allowed_tools: mode_def
                .allowed_tools
                .as_ref()
                .map(|v| v.iter().cloned().collect()),
            max_turns: mode_def.max_turns.map(|t| t as usize).unwrap_or(70),
            context_char_budget: cfg.context_char_budget.unwrap_or(180_000),
            deadline: Some(deadline),
            cwd: state.paths.tmp.clone(),
            gars_home: state.paths.home.clone(),
            verbose: true,
        };
        let outcome = run_task(client, registry, opts, |_| {}).await?;

        goal.turns_used += 1;
        let content = outcome.reply().to_string();
        goal.last_reply = Some(content.clone());
        save(&state.paths, &goal)?;

        // Append running transcript so users can audit progress.
        let transcript_path = run_dir(&state.paths, &run_id).join("reply.txt");
        use std::io::Write;
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&transcript_path)
        {
            let _ = writeln!(
                f,
                "[{}] turn={} elapsed_s={}",
                Local::now().to_rfc3339(),
                goal.turns_used,
                started.elapsed().as_secs()
            );
            let _ = writeln!(f, "{content}");
            let _ = writeln!(f, "---");
        }

        let elapsed = started.elapsed().as_secs();
        let remaining = goal.budget_seconds.saturating_sub(elapsed);
        match outcome {
            TaskOutcome::BudgetExhausted { .. } => {
                goal.status = "budget_exhausted".to_string();
                save(&state.paths, &goal)?;
                return Ok(());
            }
            TaskOutcome::Exited { .. } => {
                goal.status = "interrupted".to_string();
                save(&state.paths, &goal)?;
                return Ok(());
            }
            TaskOutcome::MaxTurns { .. } => {
                // Carry on if there's still wall-clock budget left, but
                // reset the Done counter — MaxTurns is unrelated.
                goal.consecutive_done = 0;
                if remaining == 0 {
                    goal.status = "budget_exhausted".to_string();
                    save(&state.paths, &goal)?;
                    return Ok(());
                }
            }
            TaskOutcome::Done { .. } => {
                goal.consecutive_done = goal.consecutive_done.saturating_add(1);
                // Death-loop guard: if the LLM keeps declaring completion
                // on every nudge, accept that signal and stop even if
                // budget remains. Otherwise we'd burn the whole budget in
                // a tight "done, nudge, done, nudge" loop.
                if goal.consecutive_done >= MAX_CONSECUTIVE_DONE {
                    goal.status = "done_early".to_string();
                    save(&state.paths, &goal)?;
                    tracing::info!(
                        "goal {} stopped after {} consecutive Done outcomes ({}s of {}s budget left)",
                        goal.run_id,
                        goal.consecutive_done,
                        remaining,
                        goal.budget_seconds
                    );
                    return Ok(());
                }
                // Otherwise: if budget is almost gone (< 25% remaining),
                // accept and stop. Otherwise loop with a nudge.
                let quarter = goal.budget_seconds / 4;
                if remaining < quarter {
                    goal.status = "done".to_string();
                    save(&state.paths, &goal)?;
                    return Ok(());
                }
            }
        }

        // Build nudge prompt for next iteration. The "what to do when nudged"
        // guidance is in goal_sop.md; here we just hand back control with
        // remaining budget and the last reply as anchor.
        input = format!(
            "Goal: {objective}\nRemaining budget: {remaining}s of {total}s.\nLast reply preview: {preview}\n\nContinue per goal_sop.md.",
            objective = goal.objective,
            total = goal.budget_seconds,
            preview = preview(&content)
        );

        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn preview(s: &str) -> String {
    if s.len() <= 600 {
        s.to_string()
    } else {
        format!("{}…", &s[..600])
    }
}
