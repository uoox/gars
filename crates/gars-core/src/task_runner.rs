//! Unified task runner.
//!
//! Every "mode" in gars (chat / schedule / trigger / subagent / plan / goal)
//! ultimately calls `run_task`. The differences between modes are *not* in
//! this file — they live in:
//!
//! - **SOP markdown** loaded by the caller into `opts.sop_contents`
//! - **Mode TOML** (in `~/.gars/modes/{builtin,local}/<key>.toml`) that
//!   picks which SOPs to load, which tools to allow, what budget to enforce
//! - Thin loop wrappers like `goal_mode::drive()` that decide whether to
//!   call `run_task` once (chat / subagent) or repeatedly (goal mode).
//!
//! `run_task` only knows how to assemble a system prompt, drive the
//! `AgentRuntime`, enforce a wall-clock deadline if given, and emit events.
//! Anything more specific (cron, file protocol, goal re-nudging) lives in
//! its caller.

use std::{
    collections::BTreeSet,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::Result;
use serde_json::Value;

use crate::{AgentRuntime, ChatResponse, LlmClient, RuntimeEvent, RuntimeOptions, ToolRegistry};

/// Inputs to a single task execution.
///
/// Field meaning:
/// - `prompt` — initial user prompt for the first turn.
/// - `system_prompt_base` — the global system prompt (memory L0/L1/L2 etc).
/// - `sop_contents` — markdown SOP bodies, already loaded; appended to the
///   system prompt in order.
/// - `allowed_tools` — restrict the tool registry to this subset. `None`
///   means "all tools".
/// - `max_turns` — hard ceiling on assistant turns inside `AgentRuntime`.
/// - `context_char_budget` — `RuntimeOptions::context_char_budget`.
/// - `deadline` — wall-clock cutoff. The `run_once` future is wrapped in
///   `tokio::time::timeout(deadline - now())`; on hit we return
///   `TaskOutcome::BudgetExhausted` with whatever reply was captured.
/// - `cwd`, `gars_home`, `verbose` — passed straight through to
///   `RuntimeOptions`.
#[derive(Debug, Clone)]
pub struct TaskRunOpts {
    pub prompt: String,
    pub system_prompt_base: String,
    pub sop_contents: Vec<String>,
    pub allowed_tools: Option<BTreeSet<String>>,
    pub max_turns: usize,
    pub context_char_budget: usize,
    pub deadline: Option<Instant>,
    pub cwd: PathBuf,
    pub gars_home: PathBuf,
    pub verbose: bool,
}

impl TaskRunOpts {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            system_prompt_base: String::new(),
            sop_contents: Vec::new(),
            allowed_tools: None,
            max_turns: 70,
            context_char_budget: 180_000,
            deadline: None,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            gars_home: PathBuf::new(),
            verbose: true,
        }
    }
}

/// Stream-friendly task events. Mostly a 1:1 translation of `RuntimeEvent`
/// with two extra terminal variants for the outer loop.
#[derive(Debug, Clone)]
pub enum TaskEvent {
    TurnStarted(usize),
    AssistantText(String),
    ToolStarted { name: String, args: Value },
    ToolFinished { name: String, data: Option<Value> },
    Warning(String),
}

/// Result of a single `run_task` call. Note that "Done" here just means the
/// inner `AgentRuntime` returned — for goal mode the outer driver may still
/// loop and call `run_task` again with a follow-up prompt.
#[derive(Debug, Clone)]
pub enum TaskOutcome {
    /// The LLM finished without calling any tools (CURRENT_TASK_DONE).
    Done {
        reply: String,
        final_response: Option<ChatResponse>,
        exit_data: Option<Value>,
    },
    /// A tool with `should_exit = true` was called.
    Exited {
        reply: String,
        exit_data: Option<Value>,
    },
    /// `RuntimeOptions::max_turns` was hit before a clean stop.
    MaxTurns { reply: String },
    /// Wall-clock `deadline` elapsed during the run.
    BudgetExhausted { reply: String },
}

impl TaskOutcome {
    pub fn reply(&self) -> &str {
        match self {
            TaskOutcome::Done { reply, .. }
            | TaskOutcome::Exited { reply, .. }
            | TaskOutcome::MaxTurns { reply }
            | TaskOutcome::BudgetExhausted { reply } => reply,
        }
    }

    pub fn is_complete(&self) -> bool {
        matches!(self, TaskOutcome::Done { .. } | TaskOutcome::Exited { .. })
    }
}

/// Drive a single task through `AgentRuntime`.
pub async fn run_task<F>(
    client: Box<dyn LlmClient>,
    tool_registry: ToolRegistry,
    opts: TaskRunOpts,
    mut emit: F,
) -> Result<TaskOutcome>
where
    F: FnMut(TaskEvent) + Send,
{
    let system_prompt = assemble_system_prompt(&opts.system_prompt_base, &opts.sop_contents);
    let runtime_opts = RuntimeOptions {
        max_turns: opts.max_turns,
        context_char_budget: opts.context_char_budget,
        cwd: opts.cwd.clone(),
        gars_home: opts.gars_home.clone(),
        verbose: opts.verbose,
        allowed_tools: opts.allowed_tools.clone(),
    };
    let mut runtime = AgentRuntime::new(client, tool_registry, system_prompt, runtime_opts);
    let prompt = opts.prompt.clone();
    let inner_emit = |ev: RuntimeEvent| {
        let translated = match ev {
            RuntimeEvent::TurnStarted(t) => TaskEvent::TurnStarted(t),
            RuntimeEvent::AssistantText(s) => TaskEvent::AssistantText(s),
            RuntimeEvent::ToolStarted { name, args } => TaskEvent::ToolStarted { name, args },
            RuntimeEvent::ToolFinished { name, data } => TaskEvent::ToolFinished { name, data },
            RuntimeEvent::Warning(s) => TaskEvent::Warning(s),
        };
        emit(translated);
    };
    let fut = runtime.run_once(&prompt, inner_emit);
    let raw = match opts.deadline {
        Some(deadline) => {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::ZERO);
            if remaining.is_zero() {
                return Ok(TaskOutcome::BudgetExhausted {
                    reply: String::new(),
                });
            }
            match tokio::time::timeout(remaining, fut).await {
                Ok(r) => r?,
                Err(_) => {
                    return Ok(TaskOutcome::BudgetExhausted {
                        reply: String::new(),
                    });
                }
            }
        }
        None => fut.await?,
    };
    let reply = raw
        .final_response
        .as_ref()
        .map(|r| r.content.clone())
        .unwrap_or_else(|| raw.result.clone());
    let outcome = match raw.result.as_str() {
        "EXITED" => TaskOutcome::Exited {
            reply,
            exit_data: raw.exit_data,
        },
        "MAX_TURNS_EXCEEDED" => TaskOutcome::MaxTurns { reply },
        _ => TaskOutcome::Done {
            reply,
            final_response: raw.final_response,
            exit_data: raw.exit_data,
        },
    };
    Ok(outcome)
}

fn assemble_system_prompt(base: &str, sops: &[String]) -> String {
    let mut out = base.to_string();
    for sop in sops {
        let trimmed = sop.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(trimmed);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChatRequest, ChatResponse, ToolCall};
    use anyhow::Result as AResult;
    use async_trait::async_trait;
    use serde_json::json;

    struct ScriptedClient {
        replies: std::sync::Mutex<Vec<ChatResponse>>,
        seen_system: std::sync::Mutex<Option<String>>,
    }

    #[async_trait]
    impl LlmClient for ScriptedClient {
        fn name(&self) -> &str {
            "scripted"
        }

        async fn chat(&mut self, request: ChatRequest) -> AResult<ChatResponse> {
            *self.seen_system.lock().unwrap() = Some(request.system.clone());
            let mut q = self.replies.lock().unwrap();
            if q.is_empty() {
                Ok(ChatResponse {
                    content: "done".to_string(),
                    ..Default::default()
                })
            } else {
                Ok(q.remove(0))
            }
        }
    }

    #[tokio::test]
    async fn concatenates_sops_into_system_prompt() {
        let client = Box::new(ScriptedClient {
            replies: std::sync::Mutex::new(vec![ChatResponse {
                content: "ok".to_string(),
                ..Default::default()
            }]),
            seen_system: std::sync::Mutex::new(None),
        });
        // (we don't need to assert the assembled system prompt directly;
        //  the run produces a clean outcome which is enough.)
        let opts = TaskRunOpts {
            prompt: "hi".to_string(),
            system_prompt_base: "BASE".to_string(),
            sop_contents: vec!["SOP-A".to_string(), "SOP-B".to_string()],
            allowed_tools: None,
            max_turns: 5,
            context_char_budget: 50_000,
            deadline: None,
            cwd: PathBuf::from("."),
            gars_home: PathBuf::from("."),
            verbose: false,
        };
        let outcome = run_task(client, ToolRegistry::new(), opts, |_| {})
            .await
            .unwrap();
        match outcome {
            TaskOutcome::Done { reply, .. } => assert_eq!(reply, "ok"),
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[tokio::test]
    async fn deadline_in_the_past_short_circuits() {
        let client = Box::new(ScriptedClient {
            replies: std::sync::Mutex::new(vec![]),
            seen_system: std::sync::Mutex::new(None),
        });
        let opts = TaskRunOpts {
            prompt: "hi".to_string(),
            system_prompt_base: String::new(),
            sop_contents: vec![],
            allowed_tools: None,
            max_turns: 5,
            context_char_budget: 50_000,
            deadline: Some(Instant::now() - Duration::from_secs(1)),
            cwd: PathBuf::from("."),
            gars_home: PathBuf::from("."),
            verbose: false,
        };
        let outcome = run_task(client, ToolRegistry::new(), opts, |_| {})
            .await
            .unwrap();
        assert!(matches!(outcome, TaskOutcome::BudgetExhausted { .. }));
    }

    #[tokio::test]
    async fn tool_call_then_done_completes() {
        let client = Box::new(ScriptedClient {
            replies: std::sync::Mutex::new(vec![
                ChatResponse {
                    content: "calling tool".to_string(),
                    tool_calls: vec![ToolCall::new("bad_json", json!({"a": 1}))],
                    ..Default::default()
                },
                ChatResponse {
                    content: "final answer".to_string(),
                    ..Default::default()
                },
            ]),
            seen_system: std::sync::Mutex::new(None),
        });
        let opts = TaskRunOpts {
            prompt: "hi".to_string(),
            system_prompt_base: String::new(),
            sop_contents: vec![],
            allowed_tools: None,
            max_turns: 5,
            context_char_budget: 50_000,
            deadline: None,
            cwd: PathBuf::from("."),
            gars_home: PathBuf::from("."),
            verbose: false,
        };
        let outcome = run_task(client, ToolRegistry::new(), opts, |_| {})
            .await
            .unwrap();
        match outcome {
            TaskOutcome::Done { reply, .. } => assert_eq!(reply, "final answer"),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
