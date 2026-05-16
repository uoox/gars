//! Subagent runner. Reads `input.txt`, runs one task through `run_task`,
//! and writes the final reply to `reply.txt`. The file protocol
//! (`input.txt` / `output.txt` / `reply.txt` / `_stop` / `_keyinfo` /
//! `_intervene`) is the canonical interface.
//!
//! v0.0.3: no "mode" concept. The subagent runs whatever prompt it gets;
//! SOP guidance is left to the agent definition's `system_prompt` and to
//! on-demand `skill_show` calls from the LLM.

use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use gars_core::{
    SubagentHandle, TaskEvent, TaskRunOpts, ToolRegistry, append_output, run_task, write_reply,
};
use gars_llm::RootConfig;
use gars_memory::GarsPaths;
use gars_skills::AgentDefinition;

pub async fn run_subagent(
    handle: SubagentHandle,
    agent_def: AgentDefinition,
    cfg: RootConfig,
    paths: GarsPaths,
    registry: ToolRegistry,
) -> Result<String> {
    let input = fs::read_to_string(handle.input_path())
        .with_context(|| format!("read {}", handle.input_path().display()))?;
    let selected = cfg.default_llm.as_deref().unwrap_or("primary");
    let client = gars_llm::build_client(&cfg.llm, selected)?;

    let allowed: std::collections::BTreeSet<String> =
        agent_def.allowed_tools.iter().cloned().collect();
    let allowed = if allowed.is_empty() {
        None
    } else {
        Some(allowed)
    };

    let opts = TaskRunOpts {
        prompt: input,
        system_prompt_base: agent_def.system_prompt.clone(),
        sop_contents: vec![],
        allowed_tools: allowed,
        max_turns: agent_def.max_turns,
        context_char_budget: agent_def.context_char_budget,
        deadline: None,
        cwd: paths.tmp.clone(),
        gars_home: paths.home.clone(),
        verbose: agent_def.verbose_default,
    };

    let handle_for_emit = handle.clone();
    let outcome = run_task(client, registry, opts, move |ev| {
        if let TaskEvent::AssistantText(text) = ev {
            let _ = append_output(&handle_for_emit, &text);
        }
    })
    .await?;

    let reply = outcome.reply().to_string();
    write_reply(&handle, &reply)?;
    Ok(reply)
}

pub fn workdir_for(paths: &GarsPaths, run_id: &str, agent: &str) -> PathBuf {
    paths.tasks.join(run_id).join(agent)
}
