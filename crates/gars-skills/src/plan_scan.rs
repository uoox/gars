use std::fs;
use std::path::PathBuf;

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use gars_memory::GarsPaths;

use crate::plans_dir;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanSummary {
    pub id: String,
    pub path: PathBuf,
    pub title: String,
    pub status: String,
    pub total: usize,
    pub done: usize,
    pub failed: usize,
    pub created_at: String,
    pub updated_at: String,
}

pub fn scan_plans(paths: &GarsPaths) -> Vec<PlanSummary> {
    let root = plans_dir(paths);
    let Ok(rd) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let plan_dir = entry.path();
        if !plan_dir.is_dir() {
            continue;
        }
        let id = match plan_dir.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let plan_path = plan_dir.join("plan.md");
        if !plan_path.exists() {
            continue;
        }
        let body = match fs::read_to_string(&plan_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let (title, done, failed, total) = summarize(&body);
        let meta = fs::metadata(&plan_path).ok();
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
        let status = if total == 0 {
            "empty".to_string()
        } else if failed > 0 {
            "failed".to_string()
        } else if done == total {
            "done".to_string()
        } else {
            "active".to_string()
        };
        out.push(PlanSummary {
            id,
            path: plan_path,
            title,
            status,
            total,
            done,
            failed,
            created_at,
            updated_at,
        });
    }
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    out
}

fn summarize(body: &str) -> (String, usize, usize, usize) {
    let mut title = String::new();
    let mut done = 0;
    let mut failed = 0;
    let mut total = 0;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("# Plan:") {
            title = rest.trim().to_string();
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("- ") {
            if rest.starts_with("[ ]") {
                total += 1;
            } else if rest.starts_with("[✓]") || rest.starts_with("[x]") || rest.starts_with("[X]")
            {
                total += 1;
                done += 1;
            } else if rest.starts_with("[✗]") || rest.starts_with("[!]") {
                total += 1;
                failed += 1;
            }
        }
    }
    (title, done, failed, total)
}

fn rfc3339(t: std::time::SystemTime) -> String {
    let dt: DateTime<Local> = t.into();
    dt.to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn scans_plans_dir() {
        let dir = tempfile::tempdir().unwrap();
        let paths = GarsPaths::resolve(Some(dir.path().to_path_buf())).unwrap();
        paths.ensure().unwrap();
        let plan_dir = plans_dir(&paths).join("alpha");
        fs::create_dir_all(&plan_dir).unwrap();
        let mut f = fs::File::create(plan_dir.join("plan.md")).unwrap();
        writeln!(f, "# Plan: Alpha\n\n- [✓] 1. one\n- [ ] 2. two\n").unwrap();
        drop(f);
        let plans = scan_plans(&paths);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].id, "alpha");
        assert_eq!(plans[0].title, "Alpha");
        assert_eq!(plans[0].done, 1);
        assert_eq!(plans[0].total, 2);
        assert_eq!(plans[0].status, "active");
    }
}
