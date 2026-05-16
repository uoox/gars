use anyhow::{Result, anyhow};
use async_trait::async_trait;
use gars_core::{StepOutcome, Tool, ToolContext, ToolSpec};
use gars_memory::GarsPaths;
use gars_skills::{SearchOptions, parse_skill_file, search_local, skills_dir};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;

use crate::arg_str;

pub struct SkillSearchTool;

#[async_trait]
impl Tool for SkillSearchTool {
    fn name(&self) -> &'static str {
        "skill_search"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Search local skill / SOP catalog with BM25 ranking. Use before tackling unfamiliar tasks.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "top_k": {"type": "integer", "default": 5},
                    "category": {"type": "string"}
                },
                "required": ["query"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let query = arg_str(&args, "query").ok_or_else(|| anyhow!("query required"))?;
        let top_k = args.get("top_k").and_then(Value::as_u64).unwrap_or(5) as usize;
        let category = arg_str(&args, "category");
        let paths = GarsPaths::resolve(Some(ctx.gars_home.clone()))?;
        let root = skills_dir(&paths);
        let hits = search_local(
            &query,
            &root,
            SearchOptions {
                top_k,
                category,
                autonomous_only: false,
            },
        );
        let hits_json: Vec<Value> = hits
            .into_iter()
            .map(|h| {
                json!({
                    "key": h.skill.key,
                    "name": h.skill.name,
                    "score": h.score,
                    "summary": h.skill.one_line_summary,
                    "path": h.skill.path,
                    "category": h.skill.category,
                    "tags": h.skill.tags,
                })
            })
            .collect();
        Ok(StepOutcome::next(
            json!({"status": "success", "hits": hits_json}),
            ctx.anchor_prompt(),
        ))
    }
}

pub struct SkillShowTool;

#[async_trait]
impl Tool for SkillShowTool {
    fn name(&self) -> &'static str {
        "skill_show"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Read a skill / SOP markdown file by key (preferred) or path.",
            json!({
                "type": "object",
                "properties": {
                    "key": {"type": "string"},
                    "path": {"type": "string"}
                }
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let paths = GarsPaths::resolve(Some(ctx.gars_home.clone()))?;
        let root = skills_dir(&paths);
        let path = if let Some(k) = arg_str(&args, "key") {
            find_skill_by_key(&root, &k).ok_or_else(|| anyhow!("skill with key '{k}' not found"))?
        } else if let Some(p) = arg_str(&args, "path") {
            ctx.resolve_path(&p)
        } else {
            return Err(anyhow!("provide key or path"));
        };
        let skill = parse_skill_file(&path)?;
        let body = fs::read_to_string(&path)?;
        Ok(StepOutcome::next(
            json!({
                "status": "success",
                "key": skill.key,
                "name": skill.name,
                "category": skill.category,
                "tags": skill.tags,
                "body": body,
            }),
            ctx.anchor_prompt(),
        ))
    }
}

fn find_skill_by_key(root: &std::path::Path, key: &str) -> Option<PathBuf> {
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if let Ok(s) = parse_skill_file(p)
            && s.key.eq_ignore_ascii_case(key)
        {
            return Some(p.to_path_buf());
        }
    }
    None
}

pub struct SkillImportTool;

#[async_trait]
impl Tool for SkillImportTool {
    fn name(&self) -> &'static str {
        "skill_import"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Import a skill from a local path. Pass `url` to download via reqwest (handled by REST API endpoint).",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "key": {"type": "string"}
                }
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let source =
            ctx.resolve_path(&arg_str(&args, "path").ok_or_else(|| anyhow!("path required"))?);
        let content = fs::read_to_string(&source)?;
        let _skill = parse_skill_file(&source)?;
        let paths = GarsPaths::resolve(Some(ctx.gars_home.clone()))?;
        let target_dir = skills_dir(&paths).join("imported");
        fs::create_dir_all(&target_dir)?;
        let filename = source
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("skill_{}.md", chrono::Utc::now().timestamp()));
        let dest = target_dir.join(filename);
        fs::write(&dest, content)?;
        Ok(StepOutcome::next(
            json!({"status": "success", "imported": dest}),
            ctx.anchor_prompt(),
        ))
    }
}
