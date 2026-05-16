//! Plan mode file format.
//!
//! Plans live at `~/.gars/plans/<run_id>/plan.md` and use an 8-state step
//! marker set inherited from upstream `lsdefine/GenericAgent`:
//!
//! | Marker  | Status     | Meaning                                                |
//! | ------- | ---------- | ------------------------------------------------------ |
//! | `[ ]`   | Pending    | Not yet started                                        |
//! | `[✓]`   | Done       | Completed; one-line result in `note`                   |
//! | `[D]`   | Delegate   | Delegate to a subagent (big reads / scraping / loops)  |
//! | `[P]`   | Parallel   | Parallel execution slot (Map mode)                     |
//! | `[?]`   | Question   | Conditional branch — agent decides which path to take  |
//! | `[FIX]` | Fix        | Remediation step inserted after a verification failure |
//! | `[SKIP]`| Skip       | Skipped because a dependency failed                    |
//! | `[✗]`   | Failed     | Failed, not retried                                    |
//!
//! `note: ...` lines under a step add context; verification SOPs require
//! `note: VERDICT: PASS|FAIL|PARTIAL` on `[✓]` steps emitted by the
//! `verify` subagent.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Done,
    Failed,
    Delegate,
    Parallel,
    Question,
    Fix,
    Skip,
}

impl StepStatus {
    pub fn marker(self) -> &'static str {
        match self {
            Self::Pending => "[ ]",
            Self::Done => "[✓]",
            Self::Failed => "[✗]",
            Self::Delegate => "[D]",
            Self::Parallel => "[P]",
            Self::Question => "[?]",
            Self::Fix => "[FIX]",
            Self::Skip => "[SKIP]",
        }
    }

    /// Parse a free-form status string from REST / CLI clients. Accepts the
    /// marker itself (`"[D]"`), the snake_case enum value (`"delegate"`),
    /// and a few permissive aliases (`"d"`, `"done"`, `"failed"`, `"x"`, etc).
    pub fn parse(s: &str) -> Option<Self> {
        let v = s.trim().to_ascii_lowercase();
        match v.as_str() {
            "" | "[ ]" | "pending" | "todo" | "open" => Some(Self::Pending),
            "[✓]" | "[x]" | "x" | "done" | "complete" | "completed" => Some(Self::Done),
            "[✗]" | "[!]" | "failed" | "fail" => Some(Self::Failed),
            "[d]" | "d" | "delegate" => Some(Self::Delegate),
            "[p]" | "p" | "parallel" => Some(Self::Parallel),
            "[?]" | "?" | "question" | "conditional" => Some(Self::Question),
            "[fix]" | "fix" | "remediation" => Some(Self::Fix),
            "[skip]" | "skip" | "skipped" => Some(Self::Skip),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanStep {
    pub idx: usize,
    pub title: String,
    pub status: StepStatus,
    pub note: Option<String>,
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
            .map(|(idx, title)| PlanStep {
                idx: idx + 1,
                title: title.clone(),
                status: StepStatus::Pending,
                note: None,
            })
            .collect();
        self.save()
    }

    pub fn mark(&mut self, idx: usize, status: StepStatus, note: Option<String>) -> Result<()> {
        if let Some(step) = self.steps.iter_mut().find(|s| s.idx == idx) {
            step.status = status;
            step.note = note;
            self.save()
        } else {
            anyhow::bail!("step {idx} not found")
        }
    }

    /// Total tally of `(done, failed, total)`. Kept for back-compat with
    /// existing REST callers; deeper breakdowns can iterate `self.steps`.
    pub fn status_summary(&self) -> (usize, usize, usize) {
        let mut done = 0;
        let mut failed = 0;
        for step in &self.steps {
            match step.status {
                StepStatus::Done => done += 1,
                StepStatus::Failed => failed += 1,
                _ => {}
            }
        }
        (done, failed, self.steps.len())
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("# Plan: {}\n\n", self.title));
        for step in &self.steps {
            out.push_str(&format!(
                "- {} {}. {}",
                step.status.marker(),
                step.idx,
                step.title
            ));
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
            let Some((status, after)) = strip_marker(rest) else {
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
                status,
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

/// Try to consume one of the 8 step markers off the start of `rest`. The
/// multi-letter markers (`[FIX]`, `[SKIP]`) must be tried before the
/// single-letter ones so `[FIX]` isn't matched as `[F]` etc.
fn strip_marker(rest: &str) -> Option<(StepStatus, &str)> {
    const TABLE: &[(&str, StepStatus)] = &[
        ("[ ]", StepStatus::Pending),
        ("[✓]", StepStatus::Done),
        ("[x]", StepStatus::Done),
        ("[X]", StepStatus::Done),
        ("[✗]", StepStatus::Failed),
        ("[!]", StepStatus::Failed),
        ("[FIX]", StepStatus::Fix),
        ("[SKIP]", StepStatus::Skip),
        ("[D]", StepStatus::Delegate),
        ("[P]", StepStatus::Parallel),
        ("[?]", StepStatus::Question),
    ];
    for (marker, status) in TABLE {
        if let Some(stripped) = rest.strip_prefix(marker) {
            return Some((*status, stripped));
        }
    }
    None
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
        plan.mark(2, StepStatus::Done, Some("ok".into())).unwrap();
        let loaded = PlanFile::load(&path).unwrap();
        assert_eq!(loaded.steps.len(), 3);
        assert_eq!(loaded.steps[1].status, StepStatus::Done);
        assert_eq!(loaded.steps[1].note.as_deref(), Some("ok"));
        let (done, failed, total) = loaded.status_summary();
        assert_eq!((done, failed, total), (1, 0, 3));
    }

    #[test]
    fn round_trip_all_eight_markers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.md");
        let mut plan = PlanFile::open_or_create(&path, "demo").unwrap();
        plan.set_steps(&["a", "b", "c", "d", "e", "f", "g", "h"].map(String::from))
            .unwrap();
        plan.mark(1, StepStatus::Pending, None).unwrap();
        plan.mark(2, StepStatus::Done, None).unwrap();
        plan.mark(3, StepStatus::Failed, None).unwrap();
        plan.mark(4, StepStatus::Delegate, None).unwrap();
        plan.mark(5, StepStatus::Parallel, None).unwrap();
        plan.mark(6, StepStatus::Question, None).unwrap();
        plan.mark(7, StepStatus::Fix, None).unwrap();
        plan.mark(8, StepStatus::Skip, None).unwrap();
        let loaded = PlanFile::load(&path).unwrap();
        let kinds: Vec<StepStatus> = loaded.steps.iter().map(|s| s.status).collect();
        assert_eq!(
            kinds,
            vec![
                StepStatus::Pending,
                StepStatus::Done,
                StepStatus::Failed,
                StepStatus::Delegate,
                StepStatus::Parallel,
                StepStatus::Question,
                StepStatus::Fix,
                StepStatus::Skip,
            ]
        );
    }

    #[test]
    fn parse_status_string_is_permissive() {
        assert_eq!(StepStatus::parse("[D]"), Some(StepStatus::Delegate));
        assert_eq!(StepStatus::parse("delegate"), Some(StepStatus::Delegate));
        assert_eq!(StepStatus::parse("SKIP"), Some(StepStatus::Skip));
        assert_eq!(StepStatus::parse("[fix]"), Some(StepStatus::Fix));
        assert_eq!(StepStatus::parse("done"), Some(StepStatus::Done));
        assert_eq!(StepStatus::parse("nonsense"), None);
    }
}
