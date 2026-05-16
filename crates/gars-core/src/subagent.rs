use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubagentSpec {
    pub agent: String,
    pub input: String,
    #[serde(default)]
    pub verbose: bool,
    #[serde(default)]
    pub parallel: bool,
    #[serde(default)]
    pub key_info: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubagentHandle {
    pub run_id: String,
    pub agent: String,
    pub workdir: PathBuf,
}

impl SubagentHandle {
    pub fn input_path(&self) -> PathBuf {
        self.workdir.join("input.txt")
    }
    pub fn output_path(&self) -> PathBuf {
        self.workdir.join("output.txt")
    }
    pub fn reply_path(&self) -> PathBuf {
        self.workdir.join("reply.txt")
    }
    pub fn stop_path(&self) -> PathBuf {
        self.workdir.join("_stop")
    }
    pub fn keyinfo_path(&self) -> PathBuf {
        self.workdir.join("_keyinfo")
    }
    pub fn intervene_path(&self) -> PathBuf {
        self.workdir.join("_intervene")
    }
    /// Optional structured context (absolute paths to files / prior plans /
    /// "key info" pointers). Read by multi-step subagents so they can pick
    /// up where they left off. Upstream `subagent.md` documents this slot
    /// as required for multi-step tasks.
    pub fn context_path(&self) -> PathBuf {
        self.workdir.join("context.json")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubagentSnapshot {
    pub run_id: String,
    pub agent: String,
    pub status: SubagentStatus,
    pub reply: Option<String>,
    pub output_preview: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStatus {
    Running,
    Replied,
    Stopped,
    TimedOut,
}

pub fn allocate_workdir(tasks_root: &Path, agent: &str) -> Result<SubagentHandle> {
    let run_id = Uuid::new_v4().simple().to_string();
    let workdir = tasks_root.join(&run_id).join(agent);
    fs::create_dir_all(&workdir).with_context(|| format!("mkdir {}", workdir.display()))?;
    Ok(SubagentHandle {
        run_id,
        agent: agent.to_string(),
        workdir,
    })
}

pub fn write_input(handle: &SubagentHandle, input: &str, key_info: Option<&str>) -> Result<()> {
    fs::write(handle.input_path(), input)?;
    if let Some(info) = key_info {
        fs::write(handle.keyinfo_path(), info)?;
    }
    Ok(())
}

/// Write the optional `context.json` slot. Callers pass a JSON value (object,
/// usually). Persists pretty-printed for human auditing.
pub fn write_context(handle: &SubagentHandle, context: &serde_json::Value) -> Result<()> {
    let pretty = serde_json::to_string_pretty(context)?;
    fs::write(handle.context_path(), pretty)?;
    Ok(())
}

/// Read `context.json` back if present. Returns `Ok(None)` when missing
/// (single-round subagents may legitimately not need it).
pub fn read_context(handle: &SubagentHandle) -> Result<Option<serde_json::Value>> {
    let path = handle.context_path();
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)?;
    Ok(Some(serde_json::from_str(&raw)?))
}

/// Marker written to `output.txt` after a round's last chunk. Read by the
/// upstream subagent file protocol; gars subagent_runner emits this after
/// the final write so multi-reader clients can split rounds cleanly.
pub const ROUND_END_MARKER: &str = "[ROUND END]";

/// Append the [ROUND END] marker to output.txt. Idempotent in the sense that
/// emitting two ROUND ENDs in a row is harmless (each marks a round).
pub fn append_round_end(handle: &SubagentHandle) -> Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(handle.output_path())?;
    writeln!(file, "{ROUND_END_MARKER}")?;
    Ok(())
}

pub fn append_output(handle: &SubagentHandle, text: &str) -> Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(handle.output_path())?;
    writeln!(file, "[{}] {}", Utc::now().to_rfc3339(), text)?;
    Ok(())
}

pub fn write_reply(handle: &SubagentHandle, text: &str) -> Result<()> {
    fs::write(handle.reply_path(), text)?;
    Ok(())
}

pub fn intervene(handle: &SubagentHandle, message: &str) -> Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(handle.intervene_path())?;
    writeln!(file, "[{}] {}", Utc::now().to_rfc3339(), message)?;
    Ok(())
}

pub fn stop(handle: &SubagentHandle, reason: &str) -> Result<()> {
    fs::write(handle.stop_path(), reason)?;
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubagentRun {
    pub run_id: String,
    pub agent: String,
    pub workdir: PathBuf,
    pub status: SubagentStatus,
    pub created_at: String,
    pub updated_at: String,
    pub last_reply: Option<String>,
}

pub fn scan_runs(tasks_root: &std::path::Path) -> Vec<SubagentRun> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(tasks_root) else {
        return out;
    };
    for entry in rd.flatten() {
        let run_dir = entry.path();
        if !run_dir.is_dir() {
            continue;
        }
        let run_id = match run_dir.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let Ok(inner) = std::fs::read_dir(&run_dir) else {
            continue;
        };
        for agent_entry in inner.flatten() {
            let agent_dir = agent_entry.path();
            if !agent_dir.is_dir() {
                continue;
            }
            let agent = match agent_dir.file_name().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let handle = SubagentHandle {
                run_id: run_id.clone(),
                agent: agent.clone(),
                workdir: agent_dir.clone(),
            };
            let last_reply = std::fs::read_to_string(handle.reply_path()).ok();
            let status = if last_reply.is_some() {
                SubagentStatus::Replied
            } else if handle.stop_path().exists() {
                SubagentStatus::Stopped
            } else if elapsed(&agent_dir).unwrap_or(Duration::ZERO) > Duration::from_secs(60 * 10) {
                SubagentStatus::TimedOut
            } else {
                SubagentStatus::Running
            };
            let meta = std::fs::metadata(&agent_dir).ok();
            let created_at = meta
                .as_ref()
                .and_then(|m| m.created().ok())
                .map(rfc3339)
                .unwrap_or_default();
            let updated_at = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .map(rfc3339)
                .unwrap_or_default();
            out.push(SubagentRun {
                run_id: run_id.clone(),
                agent,
                workdir: agent_dir,
                status,
                created_at,
                updated_at,
                last_reply,
            });
        }
    }
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    out
}

pub fn load_run(tasks_root: &std::path::Path, run_id: &str) -> Option<SubagentRun> {
    let run_dir = tasks_root.join(run_id);
    let inner = std::fs::read_dir(&run_dir).ok()?;
    for entry in inner.flatten() {
        let agent_dir = entry.path();
        if !agent_dir.is_dir() {
            continue;
        }
        let agent = agent_dir.file_name()?.to_str()?.to_string();
        let handle = SubagentHandle {
            run_id: run_id.to_string(),
            agent: agent.clone(),
            workdir: agent_dir.clone(),
        };
        let last_reply = std::fs::read_to_string(handle.reply_path()).ok();
        let status = if last_reply.is_some() {
            SubagentStatus::Replied
        } else if handle.stop_path().exists() {
            SubagentStatus::Stopped
        } else {
            SubagentStatus::Running
        };
        let meta = std::fs::metadata(&agent_dir).ok();
        let created_at = meta
            .as_ref()
            .and_then(|m| m.created().ok())
            .map(rfc3339)
            .unwrap_or_default();
        let updated_at = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .map(rfc3339)
            .unwrap_or_default();
        return Some(SubagentRun {
            run_id: run_id.to_string(),
            agent,
            workdir: agent_dir,
            status,
            created_at,
            updated_at,
            last_reply,
        });
    }
    None
}

fn rfc3339(t: std::time::SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Local> = t.into();
    dt.to_rfc3339()
}

pub fn snapshot(handle: &SubagentHandle, timeout: Duration) -> Result<SubagentSnapshot> {
    let reply_path = handle.reply_path();
    let reply = if reply_path.exists() {
        Some(fs::read_to_string(&reply_path).unwrap_or_default())
    } else {
        None
    };
    let output = fs::read_to_string(handle.output_path()).unwrap_or_default();
    let preview = output
        .lines()
        .rev()
        .take(40)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    let status = if reply.is_some() {
        SubagentStatus::Replied
    } else if handle.stop_path().exists() {
        SubagentStatus::Stopped
    } else if elapsed(&handle.workdir).unwrap_or(Duration::ZERO) > timeout {
        SubagentStatus::TimedOut
    } else {
        SubagentStatus::Running
    };
    Ok(SubagentSnapshot {
        run_id: handle.run_id.clone(),
        agent: handle.agent.clone(),
        status,
        reply,
        output_preview: preview,
    })
}

fn elapsed(dir: &Path) -> Option<Duration> {
    let meta = fs::metadata(dir).ok()?;
    meta.modified().ok().and_then(|m| m.elapsed().ok())
}
