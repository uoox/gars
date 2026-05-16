use std::fs;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use gars_core::{PlanFile, StepOutcome, Tool, ToolContext, ToolSpec};
use gars_memory::GarsPaths;
use gars_skills::plans_dir;
use serde_json::{Value, json};

use crate::arg_str;

pub struct PlanCreateTool;

#[async_trait]
impl Tool for PlanCreateTool {
    fn name(&self) -> &'static str {
        "plan_create"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Create or open ~/.gars/plans/<id>/plan.md and seed it with steps. Use Plan Mode for >=3 dependent steps.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "title": {"type": "string"},
                    "steps": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["id", "steps"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let id = arg_str(&args, "id").ok_or_else(|| anyhow!("id required"))?;
        let title = arg_str(&args, "title").unwrap_or_else(|| id.clone());
        let steps: Vec<String> = args
            .get("steps")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if steps.is_empty() {
            return Err(anyhow!("at least one step is required"));
        }
        let paths = GarsPaths::resolve(Some(ctx.gars_home.clone()))?;
        let dir = plans_dir(&paths).join(&id);
        fs::create_dir_all(&dir)?;
        let plan_path = dir.join("plan.md");
        let mut plan = PlanFile::open_or_create(&plan_path, &title)?;
        plan.set_steps(&steps)?;
        Ok(StepOutcome::next(
            json!({
                "status": "success",
                "plan_path": plan.path,
                "steps": plan.steps.len(),
            }),
            ctx.anchor_prompt(),
        ))
    }
}

pub struct PlanMarkTool;

#[async_trait]
impl Tool for PlanMarkTool {
    fn name(&self) -> &'static str {
        "plan_mark"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Mark a plan step done/failed and optionally attach a note.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "idx": {"type": "integer"},
                    "status": {"type": "string", "enum": ["done", "failed", "pending"]},
                    "note": {"type": "string"}
                },
                "required": ["id", "idx", "status"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let id = arg_str(&args, "id").ok_or_else(|| anyhow!("id required"))?;
        let idx = args
            .get("idx")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("idx required"))? as usize;
        let status = arg_str(&args, "status").unwrap_or_else(|| "done".to_string());
        let note = arg_str(&args, "note");
        let paths = GarsPaths::resolve(Some(ctx.gars_home.clone()))?;
        let plan_path = plans_dir(&paths).join(&id).join("plan.md");
        let mut plan = PlanFile::load(&plan_path)?;
        plan.mark(idx, &status, note)?;
        let (done, failed, total) = plan.status_summary();
        Ok(StepOutcome::next(
            json!({
                "status": "success",
                "done": done,
                "failed": failed,
                "total": total,
            }),
            ctx.anchor_prompt(),
        ))
    }
}

pub struct PlanStatusTool;

#[async_trait]
impl Tool for PlanStatusTool {
    fn name(&self) -> &'static str {
        "plan_status"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Read the current plan and return its steps + status summary.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"}
                },
                "required": ["id"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let id = arg_str(&args, "id").ok_or_else(|| anyhow!("id required"))?;
        let paths = GarsPaths::resolve(Some(ctx.gars_home.clone()))?;
        let plan_path = plans_dir(&paths).join(&id).join("plan.md");
        if !plan_path.exists() {
            return Err(anyhow!("plan {} not found at {}", id, plan_path.display()));
        }
        let plan = PlanFile::load(&plan_path)?;
        let (done, failed, total) = plan.status_summary();
        Ok(StepOutcome::next(
            json!({
                "status": "success",
                "title": plan.title,
                "path": plan.path,
                "done": done,
                "failed": failed,
                "total": total,
                "steps": plan.steps,
            }),
            ctx.anchor_prompt(),
        ))
    }
}
