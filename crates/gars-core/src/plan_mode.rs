//! Plan mode file format.
//!
//! Plans live at `~/.gars/plans/<run_id>/plan.md`. v0.0.3 stores step
//! status as the raw marker string (e.g. `[ ]`, `[D]`, `[FIX]`) — markdown
//! is the source of truth. The runtime just shuttles the file; SOPs that
//! read it decide what each marker means.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanStep {
    pub idx: usize,
    pub title: String,
    /// Raw marker, e.g. `"[ ]"`, `"[✓]"`, `"[D]"`, `"[FIX]"`. Empty = pending.
    pub marker: String,
    pub note: Option<String>,
}

impl PlanStep {
    pub fn is_done(&self) -> bool {
        matches!(self.marker.as_str(), "[✓]" | "[x]" | "[X]")
    }
    pub fn is_failed(&self) -> bool {
        matches!(self.marker.as_str(), "[✗]" | "[!]")
    }
    pub fn is_pending(&self) -> bool {
        self.marker.is_empty() || self.marker == "[ ]"
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanFile {
    pub path: PathBuf,
    pub title: String,
    pub steps: Vec<PlanStep>,
}

impl PlanFile {
    pub fn open_or_create(path: impl Into<PathBuf>, title: &str) -> Result<Self> {
        let path = path.into();
        if path.exists() {
            return Self::load(&path);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let plan = Self {
            path,
            title: title.to_string(),
            steps: Vec::new(),
        };
        plan.save()?;
        Ok(plan)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let content =
            fs::read_to_string(path).with_context(|| format!("read plan {}", path.display()))?;
        Ok(parse(&content, path))
    }

    pub fn set_steps(&mut self, titles: &[String]) -> Result<()> {
        self.steps = titles
            .iter()
            .enumerate()
            .map(|(i, t)| PlanStep {
                idx: i + 1,
                title: t.clone(),
                marker: "[ ]".to_string(),
                note: None,
            })
            .collect();
        self.save()
    }

    /// Mark a step with a raw marker or a friendly alias. `"done"` /
    /// `"delegate"` etc. are accepted and rewritten to the canonical
    /// marker string.
    pub fn mark(&mut self, idx: usize, marker: &str, note: Option<String>) -> Result<()> {
        if let Some(step) = self.steps.iter_mut().find(|s| s.idx == idx) {
            step.marker = normalize_marker(marker);
            step.note = note;
            self.save()
        } else {
            anyhow::bail!("step {idx} not found")
        }
    }

    pub fn status_summary(&self) -> (usize, usize, usize) {
        let mut done = 0;
        let mut failed = 0;
        for step in &self.steps {
            if step.is_done() {
                done += 1;
            } else if step.is_failed() {
                failed += 1;
            }
        }
        (done, failed, self.steps.len())
    }

    pub fn render(&self) -> String {
        let mut out = format!("# Plan: {}\n\n", self.title);
        for step in &self.steps {
            out.push_str(&format!("- {} {}. {}", step.marker, step.idx, step.title));
            if let Some(note) = &step.note
                && !note.trim().is_empty()
            {
                out.push_str(&format!("\n    note: {}", note));
            }
            out.push('\n');
        }
        out
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, self.render())?;
        Ok(())
    }
}

/// Permissive: accept the marker itself, the snake_case status word, or
/// a one-letter alias. Falls through to the raw input (trimmed) for any
/// custom marker the user wants to use.
fn normalize_marker(s: &str) -> String {
    let v = s.trim();
    let lower = v.to_ascii_lowercase();
    match lower.as_str() {
        "" | "[ ]" | "pending" | "todo" | "open" => "[ ]".into(),
        "[✓]" | "[x]" | "x" | "done" | "complete" | "completed" => "[✓]".into(),
        "[✗]" | "[!]" | "failed" | "fail" => "[✗]".into(),
        "[d]" | "d" | "delegate" => "[D]".into(),
        "[p]" | "p" | "parallel" => "[P]".into(),
        "[?]" | "?" | "question" | "conditional" => "[?]".into(),
        "[fix]" | "fix" | "remediation" => "[FIX]".into(),
        "[skip]" | "skip" | "skipped" => "[SKIP]".into(),
        _ => v.to_string(),
    }
}

fn parse(content: &str, path: &Path) -> PlanFile {
    let mut title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("plan")
        .to_string();
    let mut steps = Vec::new();
    let mut current: Option<PlanStep> = None;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("# Plan:") {
            title = rest.trim().to_string();
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("- ") {
            if let Some(step) = current.take() {
                steps.push(step);
            }
            let Some((marker, after)) = split_marker(rest) else {
                continue;
            };
            let after = after.trim_start();
            let mut split = after.splitn(2, '.');
            let idx_str = split.next().unwrap_or("").trim();
            let title_str = split.next().unwrap_or(after).trim().to_string();
            let idx = idx_str.parse::<usize>().unwrap_or(steps.len() + 1);
            current = Some(PlanStep {
                idx,
                title: title_str,
                marker,
                note: None,
            });
        } else if let Some(stripped) = trimmed.strip_prefix("note:")
            && let Some(step) = current.as_mut()
        {
            step.note = Some(stripped.trim().to_string());
        }
    }
    if let Some(step) = current.take() {
        steps.push(step);
    }
    PlanFile {
        path: path.to_path_buf(),
        title,
        steps,
    }
}

/// Take whatever `[...]` token leads `rest`. Returns `(marker, remainder)`.
fn split_marker(rest: &str) -> Option<(String, &str)> {
    if !rest.starts_with('[') {
        return None;
    }
    let end = rest.find(']')?;
    Some((rest[..=end].to_string(), &rest[end + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.md");
        let mut plan = PlanFile::open_or_create(&path, "demo").unwrap();
        plan.set_steps(&["one".into(), "two".into(), "three".into()])
            .unwrap();
        plan.mark(2, "done", Some("ok".into())).unwrap();
        let loaded = PlanFile::load(&path).unwrap();
        assert_eq!(loaded.steps.len(), 3);
        assert!(loaded.steps[1].is_done());
        assert_eq!(loaded.steps[1].note.as_deref(), Some("ok"));
        let (done, failed, total) = loaded.status_summary();
        assert_eq!((done, failed, total), (1, 0, 3));
    }

    #[test]
    fn keeps_arbitrary_markers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.md");
        let mut plan = PlanFile::open_or_create(&path, "demo").unwrap();
        plan.set_steps(&["a".into(), "b".into()]).unwrap();
        plan.mark(1, "[D]", None).unwrap();
        plan.mark(2, "[FIX]", None).unwrap();
        let loaded = PlanFile::load(&path).unwrap();
        assert_eq!(loaded.steps[0].marker, "[D]");
        assert_eq!(loaded.steps[1].marker, "[FIX]");
    }

    #[test]
    fn permissive_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.md");
        let mut plan = PlanFile::open_or_create(&path, "demo").unwrap();
        plan.set_steps(&["a".into()]).unwrap();
        plan.mark(1, "delegate", None).unwrap();
        assert_eq!(plan.steps[0].marker, "[D]");
        plan.mark(1, "skip", None).unwrap();
        assert_eq!(plan.steps[0].marker, "[SKIP]");
    }
}
