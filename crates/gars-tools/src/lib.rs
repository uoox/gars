use std::{
    collections::VecDeque,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use gars_cdp::{BrowserConfig, execute_js, list_tabs, scan_page};
use gars_core::{StepOutcome, Tool, ToolContext, ToolRegistry, ToolSpec, smart_truncate};
use gars_extension::{ExtensionRegistry, ext_call};
use gars_memory::{GarsPaths, global_memory_prompt, record_memory_access};
use gars_vision::VisionConfig;
use serde_json::{Value, json};
use tempfile::Builder;
use tokio::process::Command;

mod archive_tools;
mod osctl_tools;
mod plan_tools;
mod skill_tools;
mod subagent_tools;
mod vision_tools;

pub use vision_tools::VisionToolOptions;

#[derive(Clone, Default)]
pub struct BuiltinToolsOptions {
    pub browser: BrowserConfig,
    pub vision: VisionConfig,
    pub extensions: Option<ExtensionRegistry>,
}

pub fn register_builtin_tools(registry: &mut ToolRegistry, options: BuiltinToolsOptions) {
    registry.register(CodeRunTool);
    registry.register(FileReadTool);
    registry.register(FilePatchTool);
    registry.register(FileWriteTool);
    registry.register(WebScanTool {
        browser: options.browser.clone(),
        extensions: options.extensions.clone(),
    });
    registry.register(WebExecuteJsTool {
        browser: options.browser,
        extensions: options.extensions.clone(),
    });
    registry.register(UpdateWorkingCheckpointTool);
    registry.register(AskUserTool);
    registry.register(StartLongTermUpdateTool);

    // Skill tools
    registry.register(skill_tools::SkillSearchTool);
    registry.register(skill_tools::SkillShowTool);
    registry.register(skill_tools::SkillImportTool);

    // Plan tools
    registry.register(plan_tools::PlanCreateTool);
    registry.register(plan_tools::PlanMarkTool);
    registry.register(plan_tools::PlanStatusTool);

    // Subagent tools
    registry.register(subagent_tools::SubagentDispatchTool);
    registry.register(subagent_tools::SubagentStatusTool);
    registry.register(subagent_tools::SubagentIntervenetool);

    // Archive tools
    registry.register(archive_tools::ArchiveRunTool);
    registry.register(archive_tools::ArchiveSearchTool);

    // Vision tools
    registry.register(vision_tools::ImageDescribeTool {
        config: options.vision.clone(),
    });
    registry.register(vision_tools::OcrImageTool {
        config: options.vision.clone(),
    });

    // OS controls
    registry.register(osctl_tools::AdbUiTool);
    registry.register(osctl_tools::AdbTapTool);
    registry.register(osctl_tools::AdbSwipeTool);
    registry.register(osctl_tools::AdbTextTool);
    registry.register(osctl_tools::AdbDevicesTool);
    registry.register(osctl_tools::KeychainSetTool);
    registry.register(osctl_tools::KeychainUseTool);
    registry.register(osctl_tools::KeychainListTool);
    registry.register(osctl_tools::InputActTool);
}

pub(crate) fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

struct CodeRunTool;

#[async_trait]
impl Tool for CodeRunTool {
    fn name(&self) -> &'static str {
        "code_run"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Execute a small script or shell command. Prefer shell/python only when a tool probe is needed; do not inline bulk data.",
            json!({
                "type": "object",
                "properties": {
                    "script": {"type": "string"},
                    "type": {"type": "string", "enum": ["python", "bash", "sh", "shell", "powershell"], "default": "bash"},
                    "timeout": {"type": "integer", "default": 60},
                    "cwd": {"type": "string"}
                },
                "required": ["script"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let script = arg_str(&args, "script").ok_or_else(|| anyhow!("script is required"))?;
        let kind = arg_str(&args, "type").unwrap_or_else(|| "bash".to_string());
        let timeout = args.get("timeout").and_then(Value::as_u64).unwrap_or(60);
        let cwd = arg_str(&args, "cwd")
            .map(|p| ctx.resolve_path(&p))
            .unwrap_or_else(|| ctx.cwd.clone());
        fs::create_dir_all(ctx.gars_home.join("tmp"))?;

        let (program, command_args, temp_path) = match kind.as_str() {
            "python" | "py" => {
                let mut file = Builder::new()
                    .suffix(".gars.py")
                    .tempfile_in(ctx.gars_home.join("tmp"))?;
                file.write_all(script.as_bytes())?;
                let (_f, path) = file.keep()?;
                (
                    "python3".to_string(),
                    vec![
                        "-X".to_string(),
                        "utf8".to_string(),
                        "-u".to_string(),
                        path.display().to_string(),
                    ],
                    Some(path),
                )
            }
            "powershell" | "pwsh" => (
                "pwsh".to_string(),
                vec!["-NoProfile".to_string(), "-Command".to_string(), script],
                None,
            ),
            _ => ("bash".to_string(), vec!["-lc".to_string(), script], None),
        };

        let output = tokio::time::timeout(
            Duration::from_secs(timeout),
            Command::new(&program)
                .args(&command_args)
                .current_dir(&cwd)
                .output(),
        )
        .await;

        if let Some(path) = temp_path {
            let _ = fs::remove_file(path);
        }

        let result = match output {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                json!({
                    "status": if output.status.success() { "success" } else { "error" },
                    "exit_code": output.status.code(),
                    "stdout": smart_truncate(stdout, 10000 / ctx.tool_count.max(1)),
                    "stderr": smart_truncate(stderr, 4000 / ctx.tool_count.max(1)),
                })
            }
            Ok(Err(err)) => json!({ "status": "error", "msg": err.to_string() }),
            Err(_) => json!({ "status": "error", "msg": format!("timeout after {timeout}s") }),
        };
        Ok(StepOutcome::next(result, ctx.anchor_prompt()))
    }
}

struct FileReadTool;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &'static str {
        "file_read"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Read file content with line numbers, paging, and optional case-insensitive keyword context.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "start": {"type": "integer", "default": 1},
                    "count": {"type": "integer", "default": 200},
                    "keyword": {"type": "string"},
                    "show_linenos": {"type": "boolean", "default": true}
                },
                "required": ["path"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let path =
            ctx.resolve_path(&arg_str(&args, "path").ok_or_else(|| anyhow!("path is required"))?);
        let start = args
            .get("start")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1) as usize;
        let count = args
            .get("count")
            .and_then(Value::as_u64)
            .unwrap_or(200)
            .max(1) as usize;
        let keyword = arg_str(&args, "keyword").map(|s| s.to_lowercase());
        let show_linenos = args
            .get("show_linenos")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let content =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let result = read_lines(&content, start, count, keyword.as_deref(), show_linenos);
        if path.starts_with(ctx.gars_home.join("memory"))
            || path.to_string_lossy().contains("memory")
        {
            let paths = GarsPaths::resolve(Some(ctx.gars_home.clone()))?;
            let _ = record_memory_access(&paths, &path);
        }
        Ok(StepOutcome::next(
            smart_truncate(result, 15000 / ctx.tool_count.max(1)),
            ctx.anchor_prompt(),
        ))
    }
}

fn read_lines(
    content: &str,
    start: usize,
    count: usize,
    keyword: Option<&str>,
    show_linenos: bool,
) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let mut begin = start.saturating_sub(1).min(total);
    if let Some(keyword) = keyword
        && let Some(idx) = lines
            .iter()
            .enumerate()
            .skip(begin)
            .find(|(_, line)| line.to_lowercase().contains(keyword))
            .map(|(idx, _)| idx)
    {
        begin = idx.saturating_sub(count / 3);
    }
    let end = (begin + count).min(total);
    let mut out = String::new();
    if show_linenos {
        out.push_str(&format!(
            "[FILE] {total} lines{} \n",
            if end < total {
                format!(" | PARTIAL showing {}", end - begin)
            } else {
                String::new()
            }
        ));
    }
    for (idx, line) in lines[begin..end].iter().enumerate() {
        let line_no = begin + idx + 1;
        let line = smart_truncate(line, 8000);
        if show_linenos {
            out.push_str(&format!("{line_no}|{line}\n"));
        } else {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

struct FilePatchTool;

#[async_trait]
impl Tool for FilePatchTool {
    fn name(&self) -> &'static str {
        "file_patch"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Replace one unique old_content block with new_content. Exact match required; read first on failure.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old_content": {"type": "string"},
                    "new_content": {"type": "string"}
                },
                "required": ["path", "old_content", "new_content"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let path =
            ctx.resolve_path(&arg_str(&args, "path").ok_or_else(|| anyhow!("path is required"))?);
        let old_content =
            arg_str(&args, "old_content").ok_or_else(|| anyhow!("old_content is required"))?;
        let new_content =
            expand_file_refs(&arg_str(&args, "new_content").unwrap_or_default(), &ctx.cwd)?;
        let full = fs::read_to_string(&path)?;
        let count = full.matches(&old_content).count();
        let result = match count {
            0 => {
                json!({"status": "error", "msg": "old_content not found; file_read first and patch a smaller exact block"})
            }
            1 => {
                fs::write(&path, full.replacen(&old_content, &new_content, 1))?;
                json!({"status": "success", "msg": "file patched"})
            }
            n => {
                json!({"status": "error", "msg": format!("{n} matches; old_content must be unique")})
            }
        };
        Ok(StepOutcome::next(result, ctx.anchor_prompt()))
    }
}

struct FileWriteTool;

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &'static str {
        "file_write"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Create, overwrite, append, or prepend a file. Use file_patch for precise edits.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"},
                    "mode": {"type": "string", "enum": ["overwrite", "append", "prepend"], "default": "overwrite"}
                },
                "required": ["path", "content"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let path =
            ctx.resolve_path(&arg_str(&args, "path").ok_or_else(|| anyhow!("path is required"))?);
        let content = expand_file_refs(
            &arg_str(&args, "content").ok_or_else(|| anyhow!("content is required"))?,
            &ctx.cwd,
        )?;
        let mode = arg_str(&args, "mode").unwrap_or_else(|| "overwrite".to_string());
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        match mode.as_str() {
            "append" => {
                use std::fs::OpenOptions;
                let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
                f.write_all(content.as_bytes())?;
            }
            "prepend" => {
                let old = fs::read_to_string(&path).unwrap_or_default();
                fs::write(&path, content.clone() + &old)?;
            }
            _ => fs::write(&path, &content)?,
        }
        Ok(StepOutcome::next(
            json!({"status": "success", "written_bytes": content.len()}),
            ctx.anchor_prompt(),
        ))
    }
}

struct WebScanTool {
    browser: BrowserConfig,
    extensions: Option<ExtensionRegistry>,
}

#[async_trait]
impl Tool for WebScanTool {
    fn name(&self) -> &'static str {
        "web_scan"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Get simplified visible page content and tab metadata. Prefers a connected browser extension; falls back to Chrome DevTools Protocol when no extension is connected.",
            json!({
                "type": "object",
                "properties": {
                    "tabs_only": {"type": "boolean"},
                    "switch_tab_id": {"type": "string"},
                    "text_only": {"type": "boolean"},
                    "max_len": {"type": "integer", "default": 35000}
                }
            }),
        )
    }

    async fn execute(&self, args: Value, _ctx: &mut ToolContext) -> Result<StepOutcome> {
        let tabs_only = args
            .get("tabs_only")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // Prefer extension when connected.
        if let Some(reg) = &self.extensions
            && reg.is_connected().await
        {
            let value = if tabs_only {
                ext_call(reg, "list_tabs", json!({})).await?
            } else {
                ext_call(
                    reg,
                    "scan_page",
                    json!({
                        "tab_id": args.get("switch_tab_id").cloned().unwrap_or(json!("active")),
                        "text_only": args.get("text_only").and_then(Value::as_bool).unwrap_or(false),
                        "max_len": args.get("max_len").and_then(Value::as_u64).unwrap_or(35000),
                    }),
                )
                .await?
            };
            return Ok(StepOutcome::next(
                json!({"status": "success", "source": "extension", "data": value}),
                "\n",
            ));
        }
        if tabs_only {
            let tabs = list_tabs(&self.browser).await?;
            return Ok(StepOutcome::next(
                json!({"status": "success", "source": "cdp", "tabs": tabs}),
                "\n",
            ));
        }
        let tab = arg_str(&args, "switch_tab_id");
        let text_only = args
            .get("text_only")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let max_len = args.get("max_len").and_then(Value::as_u64).unwrap_or(35000) as usize;
        let result = scan_page(&self.browser, tab.as_deref(), text_only, max_len).await?;
        Ok(StepOutcome::next(result, "\n"))
    }
}

struct WebExecuteJsTool {
    browser: BrowserConfig,
    extensions: Option<ExtensionRegistry>,
}

#[async_trait]
impl Tool for WebExecuteJsTool {
    fn name(&self) -> &'static str {
        "web_execute_js"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Execute JavaScript in a browser tab. Prefers a connected browser extension; falls back to Chrome DevTools Protocol when no extension is connected.",
            json!({
                "type": "object",
                "properties": {
                    "script": {"type": "string"},
                    "switch_tab_id": {"type": "string"},
                    "save_to_file": {"type": "string"}
                },
                "required": ["script"]
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let mut script = arg_str(&args, "script").ok_or_else(|| anyhow!("script is required"))?;
        let possible_path = ctx.resolve_path(script.trim());
        if possible_path.is_file() {
            script = fs::read_to_string(possible_path)?;
        }
        let tab = arg_str(&args, "switch_tab_id");
        let mut result = if let Some(reg) = &self.extensions
            && reg.is_connected().await
        {
            let value = ext_call(
                reg,
                "execute_js",
                json!({
                    "tab_id": tab.clone().unwrap_or_else(|| "active".to_string()),
                    "script": script,
                }),
            )
            .await?;
            json!({"source": "extension", "js_return": value})
        } else {
            serde_json::to_value(execute_js(&self.browser, tab.as_deref(), &script).await?)?
        };
        if let Some(save_to_file) = arg_str(&args, "save_to_file") {
            let path = ctx.resolve_path(&save_to_file);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let content = result
                .get("js_return")
                .map(Value::to_string)
                .unwrap_or_default();
            fs::write(&path, &content)?;
            result["saved_to"] = json!(path.display().to_string());
            result["js_return"] = json!(smart_truncate(content, 300));
        }
        Ok(StepOutcome::next(result, ctx.anchor_prompt()))
    }
}

struct UpdateWorkingCheckpointTool;

#[async_trait]
impl Tool for UpdateWorkingCheckpointTool {
    fn name(&self) -> &'static str {
        "update_working_checkpoint"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Update the short-term working checkpoint injected each turn. Use in early/mid long tasks.",
            json!({
                "type": "object",
                "properties": {
                    "key_info": {"type": "string"},
                    "related_sop": {"type": "string"}
                }
            }),
        )
    }

    async fn execute(&self, args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        if let Some(info) = arg_str(&args, "key_info") {
            ctx.working.key_info = Some(info);
            ctx.working.passed_sessions = 0;
        }
        if let Some(sop) = arg_str(&args, "related_sop") {
            ctx.working.related_sop = Some(sop);
        }
        Ok(StepOutcome::next(
            json!({"status": "success", "msg": "working checkpoint updated"}),
            ctx.anchor_prompt(),
        ))
    }
}

struct AskUserTool;

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &'static str {
        "ask_user"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Interrupt the task to ask the user for a decision or missing information.",
            json!({
                "type": "object",
                "properties": {
                    "question": {"type": "string"},
                    "candidates": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["question"]
            }),
        )
    }

    async fn execute(&self, args: Value, _ctx: &mut ToolContext) -> Result<StepOutcome> {
        Ok(StepOutcome::exit(json!({
            "status": "INTERRUPT",
            "intent": "HUMAN_INTERVENTION",
            "data": {
                "question": arg_str(&args, "question").unwrap_or_else(|| "请提供输入：".to_string()),
                "candidates": args.get("candidates").cloned().unwrap_or_else(|| json!([])),
            }
        })))
    }
}

struct StartLongTermUpdateTool;

#[async_trait]
impl Tool for StartLongTermUpdateTool {
    fn name(&self) -> &'static str {
        "start_long_term_update"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::function(
            self.name(),
            "Start distilling action-verified facts, preferences, and hard-won SOPs into long-term memory.",
            json!({"type": "object", "properties": {}}),
        )
    }

    async fn execute(&self, _args: Value, ctx: &mut ToolContext) -> Result<StepOutcome> {
        let paths = GarsPaths::resolve(Some(ctx.gars_home.clone()))?;
        paths.ensure()?;
        let l0 = fs::read_to_string(paths.memory.join("memory_management_sop.md"))?;
        let prompt = format!(
            "### [总结提炼经验]\n只提取未来可复用且经工具验证成功的信息。禁止写入猜测、临时状态、密钥、通用常识。先读取 L0，再最小化 patch L1/L2/L3。\n\n{}",
            global_memory_prompt(&paths)?
        );
        Ok(StepOutcome::next(
            json!({ "status": "success", "L0": l0 }),
            prompt,
        ))
    }
}

fn expand_file_refs(content: &str, base: &Path) -> Result<String> {
    let mut out = String::new();
    let mut rest = content;
    while let Some(start) = rest.find("{{file:") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "{{file:".len()..];
        let end = after
            .find("}}")
            .ok_or_else(|| anyhow!("unterminated file ref"))?;
        let spec = &after[..end];
        let parts: Vec<&str> = spec.rsplitn(3, ':').collect();
        if parts.len() != 3 {
            return Err(anyhow!("file ref must be {{file:path:start:end}}"));
        }
        let end_line = parts[0].parse::<usize>()?;
        let start_line = parts[1].parse::<usize>()?;
        let path = PathBuf::from(parts[2]);
        let path = if path.is_absolute() {
            path
        } else {
            base.join(path)
        };
        let file = fs::read_to_string(&path)?;
        let lines: Vec<&str> = file.lines().collect();
        if start_line == 0 || end_line < start_line || end_line > lines.len() {
            return Err(anyhow!(
                "file ref line range out of bounds: {}",
                path.display()
            ));
        }
        out.push_str(&lines[start_line - 1..end_line].join("\n"));
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

#[allow(dead_code)]
fn tail_lines(content: &str, count: usize) -> Vec<String> {
    let mut buf = VecDeque::with_capacity(count);
    for line in content.lines() {
        if buf.len() == count {
            buf.pop_front();
        }
        buf.push_back(line.to_string());
    }
    buf.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_requires_unique_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "x\nx\n").unwrap();
        let count = fs::read_to_string(&path).unwrap().matches("x").count();
        assert_eq!(count, 2);
    }

    #[test]
    fn expands_file_refs() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let s = expand_file_refs("{{file:a.txt:2:3}}", dir.path()).unwrap();
        assert_eq!(s, "two\nthree");
    }
}
