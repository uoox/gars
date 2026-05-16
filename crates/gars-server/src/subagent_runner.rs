//! Subagent runner. Reads the user input from `input.txt`, runs one task
//! through the unified `run_task`, and writes the final reply to `reply.txt`.
//!
//! The file protocol (`input.txt` / `output.txt` / `reply.txt` / `_stop` /
//! `_keyinfo` / `_intervene`) is preserved end-to-end — that's the canonical
//! interface. This file just glues it to `run_task`.

use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use gars_core::{
    SubagentHandle, TaskEvent, TaskRunOpts, ToolRegistry, append_output, append_round_end,
    run_task, write_reply,
};
use gars_llm::RootConfig;
use gars_memory::GarsPaths;
use gars_skills::{AgentDefinition, load_mode, mode_hint};

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

    // v0.7: inject a hint pointing at the subagent mode's SOPs rather than
    // dumping the bodies. The subagent can `skill_show("subagent_sop")` for
    // the file-protocol reference when it actually needs it.
    let mode_def = load_mode(&paths, "subagent").unwrap_or_else(|| gars_skills::ModeDef {
        key: "subagent".into(),
        label: "Subagent".into(),
        description: String::new(),
        runner_kind: "subagent".into(),
        sop_keys: vec!["subagent_sop".into()],
        allowed_tools: None,
        budget_secs: None,
        max_turns: None,
        source: "synthetic".into(),
    });
    let hint = mode_hint(&paths, &mode_def);
    let sop_contents = if hint.is_empty() { vec![] } else { vec![hint] };

    append_output(
        &handle,
        &format!("[subagent] start agent={}", agent_def.name),
    )?;

    let opts = TaskRunOpts {
        prompt: input,
        system_prompt_base: agent_def.system_prompt.clone(),
        sop_contents,
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
        let line = match ev {
            TaskEvent::TurnStarted(t) => format!("[turn] {t}"),
            TaskEvent::AssistantText(text) => format!("[assistant] {text}"),
            TaskEvent::ToolStarted { name, args } => format!("[tool:{name}] {args}"),
            TaskEvent::ToolFinished { name, data } => {
                let summary = data
                    .map(|d| d.to_string())
                    .map(|s| {
                        if s.len() > 600 {
                            format!("{}…", &s[..600])
                        } else {
                            s
                        }
                    })
                    .unwrap_or_default();
                format!("[done:{name}] {summary}")
            }
            TaskEvent::Warning(msg) => format!("[warn] {msg}"),
        };
        let _ = append_output(&handle_for_emit, &line);
    })
    .await?;

    let reply = outcome.reply().to_string();
    write_reply(&handle, &reply)?;
    // Mark this round complete in output.txt per upstream subagent.md
    // file-protocol convention. Multi-round drivers split on this marker.
    let _ = append_round_end(&handle);
    Ok(reply)
}

pub fn workdir_for(paths: &GarsPaths, run_id: &str, agent: &str) -> PathBuf {
    paths.tasks.join(run_id).join(agent)
}
