use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ToolSpec;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkingMemory {
    pub key_info: Option<String>,
    pub related_sop: Option<String>,
    pub passed_sessions: usize,
}

#[derive(Clone, Debug)]
pub struct ToolContext {
    pub gars_home: PathBuf,
    pub cwd: PathBuf,
    pub current_turn: usize,
    pub tool_index: usize,
    pub tool_count: usize,
    pub working: WorkingMemory,
    pub history_info: Vec<String>,
}

impl ToolContext {
    pub fn anchor_prompt(&self) -> String {
        let window = 30usize;
        let mut prompt = String::from("\n### [WORKING MEMORY]\n");
        if self.history_info.len() > window {
            prompt.push_str("<earlier_context>\n");
            prompt.push_str(&fold_earlier(
                &self.history_info[..self.history_info.len() - window],
            ));
            prompt.push_str("\n</earlier_context>\n");
        }
        prompt.push_str("<history>\n");
        let start = self.history_info.len().saturating_sub(window);
        prompt.push_str(&self.history_info[start..].join("\n"));
        prompt.push_str("\n</history>\n");
        prompt.push_str(&format!("Current turn: {}\n", self.current_turn));
        if let Some(key_info) = &self.working.key_info {
            prompt.push_str("\n<key_info>");
            prompt.push_str(key_info);
            prompt.push_str("</key_info>\n");
        }
        if let Some(sop) = &self.working.related_sop {
            prompt.push_str("\n有不清晰的地方请再次读取");
            prompt.push_str(sop);
            prompt.push('\n');
        }
        prompt
    }

    pub fn resolve_path(&self, path: &str) -> PathBuf {
        let p = PathBuf::from(path);
        if p.is_absolute() { p } else { self.cwd.join(p) }
    }
}

fn fold_earlier(lines: &[String]) -> String {
    let mut parts = Vec::new();
    let mut count = 0usize;
    let mut last = String::new();
    for line in lines {
        if line.starts_with("[USER]") {
            if count > 0 {
                parts.push(format!("{}（{} turns）", last, count));
            }
            parts.push(line.clone());
            count = 0;
            last.clear();
        } else {
            count += 1;
            last = if line.contains("直接回答") {
                "[Agent]".to_string()
            } else {
                line.clone()
            };
        }
    }
    if count > 0 {
        parts.push(format!("{}（{} turns）", last, count));
    }
    let start = parts.len().saturating_sub(100);
    parts[start..].join("\n")
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct StepOutcome {
    pub data: Option<Value>,
    pub next_prompt: Option<String>,
    pub should_exit: bool,
}

impl StepOutcome {
    pub fn done(data: impl Into<Value>) -> Self {
        Self {
            data: Some(data.into()),
            next_prompt: None,
            should_exit: false,
        }
    }

    pub fn next(data: impl Into<Value>, next_prompt: impl Into<String>) -> Self {
        Self {
            data: Some(data.into()),
            next_prompt: Some(next_prompt.into()),
            should_exit: false,
        }
    }

    pub fn exit(data: impl Into<Value>) -> Self {
        Self {
            data: Some(data.into()),
            next_prompt: None,
            should_exit: true,
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn spec(&self) -> ToolSpec;
    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome>;
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<T: Tool + 'static>(&mut self, tool: T) {
        self.tools.insert(tool.name().to_string(), Arc::new(tool));
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|tool| tool.spec()).collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    pub async fn execute(
        &self,
        name: &str,
        args: Value,
        ctx: &mut ToolContext,
    ) -> Result<StepOutcome> {
        let tool = self
            .tools
            .get(name)
            .cloned()
            .with_context(|| format!("Unknown tool: {name}"))?;
        tool.execute(args, ctx).await
    }
}
