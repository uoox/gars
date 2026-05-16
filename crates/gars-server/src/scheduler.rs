//! Schedule runner. Reads `~/.gars/schedules/*.toml`, evaluates cron triggers,
//! and runs the configured prompt through the agent runtime when due. Output
//! is written to `~/.gars/schedules/done/{YYYY-MM-DD_HHMM}_{id}.md` so users
//! can audit runs without scraping logs.
//!
//! v0.0.3: no "mode" concept. The runner just executes the prompt through
//! `run_task`. If the prompt needs SOP guidance the LLM fetches it itself
//! via `skill_show`.

use std::{
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use cron::Schedule;
use gars_core::{TaskRunOpts, run_task};
use gars_llm::build_client;
use gars_memory::GarsPaths;
use serde::{Deserialize, Serialize};

use crate::AppState;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScheduledTask {
    pub id: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub cron: String,
    pub prompt: String,
    #[serde(default)]
    pub llm: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

fn default_enabled() -> bool {
    true
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ScheduledState {
    pub last_run: Option<String>,
    pub last_status: Option<String>,
    pub last_report: Option<String>,
    pub runs: u64,
    pub errors: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScheduleHealth {
    pub id: String,
    pub status: String,
    pub last_run: Option<String>,
    pub next_run: Option<String>,
    pub runs: u64,
    pub errors: u64,
}

pub fn schedules_dir(paths: &GarsPaths) -> PathBuf {
    paths.schedules.clone()
}

pub fn done_dir(paths: &GarsPaths) -> PathBuf {
    paths.schedules.join("done")
}

pub fn list_tasks(paths: &GarsPaths) -> Vec<ScheduledTask> {
    let Ok(rd) = fs::read_dir(schedules_dir(paths)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        match load_task(&p) {
            Ok(t) => out.push(t),
            Err(err) => tracing::warn!("schedule {}: {err}", p.display()),
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

pub fn load_task(path: &Path) -> Result<ScheduledTask> {
    let content = fs::read_to_string(path)?;
    let mut t: ScheduledTask = toml::from_str(&content)?;
    if t.id.is_empty() {
        t.id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("task")
            .to_string();
    }
    Ok(t)
}

pub fn save_task(paths: &GarsPaths, task: &ScheduledTask) -> Result<PathBuf> {
    fs::create_dir_all(schedules_dir(paths))?;
    let path = schedules_dir(paths).join(format!("{}.toml", task.id));
    fs::write(&path, toml::to_string_pretty(task)?)?;
    Ok(path)
}

pub fn delete_task(paths: &GarsPaths, id: &str) -> Result<()> {
    let _ = fs::remove_file(schedules_dir(paths).join(format!("{id}.toml")));
    let _ = fs::remove_file(schedules_dir(paths).join(format!("{id}.state.json")));
    Ok(())
}

pub fn load_state(paths: &GarsPaths, id: &str) -> ScheduledState {
    fs::read_to_string(schedules_dir(paths).join(format!("{id}.state.json")))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_state(paths: &GarsPaths, id: &str, state: &ScheduledState) -> Result<()> {
    fs::write(
        schedules_dir(paths).join(format!("{id}.state.json")),
        serde_json::to_string_pretty(state)?,
    )?;
    Ok(())
}

pub fn health(task: &ScheduledTask, state: &ScheduledState) -> ScheduleHealth {
    let next_run = Schedule::from_str(&task.cron)
        .ok()
        .and_then(|s| s.upcoming(Local).next())
        .map(|dt| dt.to_rfc3339());
    let status = if !task.enabled {
        "disabled"
    } else if state.runs == 0 {
        "never_run"
    } else if state.errors > 0 && state.last_status.as_deref() != Some("ok") {
        "error"
    } else {
        "healthy"
    };
    ScheduleHealth {
        id: task.id.clone(),
        status: status.to_string(),
        last_run: state.last_run.clone(),
        next_run,
        runs: state.runs,
        errors: state.errors,
    }
}

pub fn spawn_scheduler(state: Arc<AppState>) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        loop {
            if let Err(err) = tick(&state).await {
                tracing::warn!("scheduler tick: {err:#}");
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
}

async fn tick(state: &AppState) -> Result<()> {
    let tasks = list_tasks(&state.paths);
    let now = Local::now();
    for task in tasks {
        if !task.enabled {
            continue;
        }
        let mut task_state = load_state(&state.paths, &task.id);
        let schedule = match Schedule::from_str(&task.cron) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!("schedule {} invalid cron: {err}", task.id);
                continue;
            }
        };
        let last_run: Option<DateTime<Local>> = task_state
            .last_run
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Local));
        let anchor = last_run.unwrap_or_else(|| now - chrono::Duration::seconds(60));
        let due = matches!(schedule.after(&anchor).next(), Some(fire) if fire <= now);
        if !due {
            continue;
        }
        run_task_now(state, &task, &mut task_state).await?;
    }
    Ok(())
}

pub async fn run_task_now(
    state: &AppState,
    task: &ScheduledTask,
    task_state: &mut ScheduledState,
) -> Result<()> {
    tracing::info!("scheduler running task {}", task.id);
    let cfg = state.config.read().await.clone();
    let selected = task
        .llm
        .as_deref()
        .or(cfg.default_llm.as_deref())
        .unwrap_or("primary");
    let client = build_client(&cfg.llm, selected).context("build llm client")?;
    let opts = TaskRunOpts {
        prompt: task.prompt.clone(),
        system_prompt_base: crate::build_system_prompt(&state.paths, &cfg)?,
        sop_contents: vec![],
        allowed_tools: None,
        max_turns: 70,
        context_char_budget: cfg.context_char_budget.unwrap_or(180_000),
        deadline: None,
        cwd: state.paths.tmp.clone(),
        gars_home: state.paths.home.clone(),
        verbose: false,
    };
    let outcome = run_task(client, crate::registry(&cfg), opts, |_| {}).await;
    let stamp = Local::now().format("%Y-%m-%d_%H%M");
    let report_dir = done_dir(&state.paths);
    fs::create_dir_all(&report_dir)?;
    let report_path = report_dir.join(format!("{}_{}.md", stamp, sanitize(&task.id)));
    let now_iso = Local::now().to_rfc3339();
    task_state.runs += 1;
    task_state.last_run = Some(now_iso.clone());
    let (body, status) = match outcome {
        Ok(o) => {
            let kind = match &o {
                gars_core::TaskOutcome::Done { .. } => "done",
                gars_core::TaskOutcome::Exited { .. } => "exited",
                gars_core::TaskOutcome::MaxTurns { .. } => "max_turns",
                gars_core::TaskOutcome::BudgetExhausted { .. } => "budget_exhausted",
            };
            (
                format!(
                    "# Scheduled run: {id}\n\nTime: {time}\nPrompt: {prompt}\n\n## Result ({kind})\n\n{content}\n",
                    id = task.id,
                    time = now_iso,
                    prompt = task.prompt,
                    content = o.reply(),
                ),
                "ok",
            )
        }
        Err(err) => {
            task_state.errors += 1;
            (
                format!(
                    "# Scheduled run: {id}\n\nTime: {time}\nPrompt: {prompt}\n\nERROR: {err:#}\n",
                    id = task.id,
                    time = now_iso,
                    prompt = task.prompt
                ),
                "error",
            )
        }
    };
    fs::write(&report_path, body)?;
    task_state.last_status = Some(status.to_string());
    task_state.last_report = Some(report_path.display().to_string());
    save_state(&state.paths, &task.id, task_state)?;
    let _ = state.event_bus.send(crate::BusEvent {
        topic: "schedule".to_string(),
        payload: serde_json::json!({
            "id": task.id,
            "status": status,
            "report": report_path.display().to_string(),
        }),
    });
    Ok(())
}

fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
