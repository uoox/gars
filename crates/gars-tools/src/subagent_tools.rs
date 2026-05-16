use std::time::Duration;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use gars_core::{
    StepOutcome, Tool, ToolContext, ToolSpec, allocate_workdir, intervene, snapshot, stop,
    write_input,
};
use gars_memory::GarsPaths;
use gars_skills::AgentRegistry;
use serde_json::{Value, json};

use crate::arg_str;

pub struct SubagentDispatchTool;

#[async_trait]
impl Tool for SubagentDispatchTool {
    fn name(&self) -> &'static str {
        "subagent_dispatch"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Allocate a subagent workdir, write input.txt, and return a run_id. The REST server's /v1/subagents endpoint drives the actual agent loop.",
            json!({
                "type": "object",
                "properties": {
                    "agent": {"type": "string"},
                    "input": {"type": "string"},
                    "verbose": {"type": "boolean"},
                    "parallel": {"type": "boolean"},
                    "key_info": {"type": "string"}
                },
                "required": ["agent", "input"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let agent = arg_str(&args, "agent").ok_or_else(|| anyhow!("agent required"))?;
        let input = arg_str(&args, "input").ok_or_else(|| anyhow!("input required"))?;
        let key_info = arg_str(&args, "key_info");
        let paths = GarsPaths::resolve(Some(ctx.gars_home.clone()))?;
        let registry = AgentRegistry::load(&paths)?;
        if registry.get(&agent).is_none() {
            return Err(anyhow!(
                "subagent '{agent}' not defined; available: {}",
                registry.names().join(", ")
            ));
        }
        let handle = allocate_workdir(&paths.tasks, &agent)?;
        write_input(&handle, &input, key_info.as_deref())?;
        Ok(StepOutcome::next(
            json!({
                "status": "queued",
                "run_id": handle.run_id,
                "agent": handle.agent,
                "workdir": handle.workdir,
                "hint": "subagent execution is driven by /v1/subagents/{run_id}:run on the REST server"
            }),
            ctx.anchor_prompt(),
        ))
    }
}

pub struct SubagentStatusTool;

#[async_trait]
impl Tool for SubagentStatusTool {
    fn name(&self) -> &'static str {
        "subagent_status"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Inspect a subagent workdir: returns reply (if present) and an output preview.",
            json!({
                "type": "object",
                "properties": {
                    "run_id": {"type": "string"},
                    "agent": {"type": "string"}
                },
                "required": ["run_id", "agent"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let run_id = arg_str(&args, "run_id").ok_or_else(|| anyhow!("run_id required"))?;
        let agent = arg_str(&args, "agent").ok_or_else(|| anyhow!("agent required"))?;
        let paths = GarsPaths::resolve(Some(ctx.gars_home.clone()))?;
        let workdir = paths.tasks.join(&run_id).join(&agent);
        let handle = gars_core::SubagentHandle {
            run_id,
            agent,
            workdir,
        };
        let snap = snapshot(&handle, Duration::from_secs(60 * 10))?;
        Ok(StepOutcome::next(
            serde_json::to_value(&snap)?,
            ctx.anchor_prompt(),
        ))
    }
}

pub struct SubagentIntervenetool;

#[async_trait]
impl Tool for SubagentIntervenetool {
    fn name(&self) -> &'static str {
        "subagent_intervene"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Append a guidance message to the subagent's _intervene file (or write _stop).",
            json!({
                "type": "object",
                "properties": {
                    "run_id": {"type": "string"},
                    "agent": {"type": "string"},
                    "message": {"type": "string"},
                    "stop": {"type": "boolean"}
                },
                "required": ["run_id", "agent"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let run_id = arg_str(&args, "run_id").ok_or_else(|| anyhow!("run_id required"))?;
        let agent = arg_str(&args, "agent").ok_or_else(|| anyhow!("agent required"))?;
        let message = arg_str(&args, "message").unwrap_or_default();
        let should_stop = args.get("stop").and_then(Value::as_bool).unwrap_or(false);
        let paths = GarsPaths::resolve(Some(ctx.gars_home.clone()))?;
        let workdir = paths.tasks.join(&run_id).join(&agent);
        let handle = gars_core::SubagentHandle {
            run_id,
            agent,
            workdir,
        };
        if should_stop {
            stop(&handle, &message)?;
            Ok(StepOutcome::next(
                json!({"status": "stopped"}),
                ctx.anchor_prompt(),
            ))
        } else {
            intervene(&handle, &message)?;
            Ok(StepOutcome::next(
                json!({"status": "intervened"}),
                ctx.anchor_prompt(),
            ))
        }
    }
}
