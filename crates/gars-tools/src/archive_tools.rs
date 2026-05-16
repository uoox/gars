use anyhow::Result;
use async_trait::async_trait;
use gars_archive::{ArchiveConfig, run_idle_pass, search};
use gars_core::{StepOutcome, Tool, ToolContext, ToolSpec};
use gars_memory::GarsPaths;
use gars_store::Store;
use serde_json::{Value, json};

use crate::arg_str;

pub struct ArchiveRunTool;

#[async_trait]
impl Tool for ArchiveRunTool {
    fn name(&self) -> &'static str {
        "archive_run"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Compress sessions in memory/L4_raw_sessions/incoming/ into archived/ and index them.",
            json!({
                "type": "object",
                "properties": {
                    "min_bytes": {"type": "integer", "default": 4600}
                }
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let paths = GarsPaths::resolve(Some(ctx.gars_home.clone()))?;
        let store = Store::new(paths.home.join("gars.db"));
        store.init()?;
        let cfg = ArchiveConfig {
            auto: false,
            idle_secs: 0,
            min_bytes: args
                .get("min_bytes")
                .and_then(Value::as_u64)
                .map(|v| v as usize)
                .unwrap_or(4600),
        };
        let stats = run_idle_pass(&paths, &store, &cfg)?;
        Ok(StepOutcome::next(
            json!({"status": "success", "stats": stats}),
            ctx.anchor_prompt(),
        ))
    }
}

pub struct ArchiveSearchTool;

#[async_trait]
impl Tool for ArchiveSearchTool {
    fn name(&self) -> &'static str {
        "archive_search"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Search the L4 archive index for past sessions matching the query.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "k": {"type": "integer", "default": 5}
                },
                "required": ["query"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let q = arg_str(&args, "query").unwrap_or_default();
        let k = args.get("k").and_then(Value::as_u64).unwrap_or(5) as usize;
        let paths = GarsPaths::resolve(Some(ctx.gars_home.clone()))?;
        let store = Store::new(paths.home.join("gars.db"));
        store.init()?;
        let hits = search(&store, &q, k)?;
        Ok(StepOutcome::next(
            json!({"status": "success", "hits": hits}),
            ctx.anchor_prompt(),
        ))
    }
}
