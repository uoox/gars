//! L4 session archival + retrieval index.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{NaiveDateTime, Utc};
use gars_memory::GarsPaths;
use gars_store::{L4Hit, L4IndexEntry, Store};
use regex::Regex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use walkdir::WalkDir;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ArchiveConfig {
    #[serde(default = "default_auto")]
    pub auto: bool,
    #[serde(default = "default_idle_secs")]
    pub idle_secs: u64,
    #[serde(default = "default_min_bytes")]
    pub min_bytes: usize,
}

fn default_auto() -> bool {
    true
}
fn default_idle_secs() -> u64 {
    1800
}
fn default_min_bytes() -> usize {
    4600
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompressStats {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub original: usize,
    pub compressed: usize,
    pub skipped: bool,
    pub reason: Option<String>,
}

pub fn incoming_dir(paths: &GarsPaths) -> PathBuf {
    paths.l4_raw_sessions.join("incoming")
}

pub fn archived_dir(paths: &GarsPaths) -> PathBuf {
    paths.l4_raw_sessions.join("archived")
}

pub fn ensure_dirs(paths: &GarsPaths) -> Result<()> {
    fs::create_dir_all(incoming_dir(paths))?;
    fs::create_dir_all(archived_dir(paths))?;
    Ok(())
}

pub fn compress_session(src: &Path, dst_dir: &Path, min_bytes: usize) -> Result<CompressStats> {
    let content = fs::read_to_string(src).with_context(|| format!("read {}", src.display()))?;
    let original = content.len();
    let compressed = compress_text(&content);
    if compressed.len() < min_bytes {
        return Ok(CompressStats {
            source: src.to_path_buf(),
            destination: PathBuf::new(),
            original,
            compressed: compressed.len(),
            skipped: true,
            reason: Some(format!(
                "below minimum {} bytes after compression",
                min_bytes
            )),
        });
    }
    let stem = compute_stem(&compressed, src);
    fs::create_dir_all(dst_dir)?;
    let dst = dst_dir.join(format!("{stem}.txt"));
    fs::write(&dst, &compressed)?;
    Ok(CompressStats {
        source: src.to_path_buf(),
        destination: dst,
        original,
        compressed: compressed.len(),
        skipped: false,
        reason: None,
    })
}

fn compress_text(content: &str) -> String {
    // Strip frontmatter system prompt blocks (looking for === SYSTEM === or
    // explicit Markdown system fences). Keep user/assistant turns.
    let mut out = String::with_capacity(content.len());
    let mut skip_block = false;
    let mut last_role = "";
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("=== SYSTEM") || trimmed.starts_with("[SYSTEM]") {
            skip_block = true;
            last_role = "system";
            continue;
        }
        if trimmed.starts_with("=== Prompt")
            || trimmed.starts_with("[USER]")
            || trimmed.starts_with("=== USER")
        {
            skip_block = false;
            last_role = "user";
            out.push_str(trimmed);
            out.push('\n');
            continue;
        }
        if trimmed.starts_with("=== Response")
            || trimmed.starts_with("[Agent]")
            || trimmed.starts_with("=== ASSISTANT")
        {
            skip_block = false;
            last_role = "assistant";
            out.push_str(trimmed);
            out.push('\n');
            continue;
        }
        if skip_block {
            continue;
        }
        // Drop verbose tool-result blocks (>2k chars) but keep summaries.
        if trimmed.starts_with("<tool_result>") && trimmed.len() > 2000 {
            out.push_str("<tool_result>... (truncated) ...</tool_result>\n");
            continue;
        }
        if last_role == "assistant" && trimmed.is_empty() {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn compute_stem(content: &str, fallback: &Path) -> String {
    let re = Regex::new(r"(\d{4})[-/](\d{2})[-/](\d{2})[ T_](\d{2})[:](\d{2})").unwrap();
    let mut times: Vec<NaiveDateTime> = re
        .captures_iter(content)
        .filter_map(|c| {
            let s = format!("{}-{}-{} {}:{}", &c[1], &c[2], &c[3], &c[4], &c[5]);
            NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M").ok()
        })
        .collect();
    times.sort();
    if let (Some(first), Some(last)) = (times.first(), times.last()) {
        format!("{}-{}", first.format("%m%d_%H%M"), last.format("%m%d_%H%M"))
    } else {
        let stamp = Utc::now().format("%m%d_%H%M");
        let stem = fallback
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("session");
        format!("{stamp}_{stem}")
    }
}

pub fn run_idle_pass(
    paths: &GarsPaths,
    store: &Store,
    cfg: &ArchiveConfig,
) -> Result<Vec<CompressStats>> {
    ensure_dirs(paths)?;
    let mut stats_vec = Vec::new();
    let mut sources: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(incoming_dir(paths)).into_iter().flatten() {
        let p = entry.path();
        if p.is_file()
            && p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e == "md" || e == "txt" || e == "log")
                .unwrap_or(false)
        {
            sources.push(p.to_path_buf());
        }
    }
    for src in sources {
        let stats = compress_session(&src, &archived_dir(paths), cfg.min_bytes)?;
        if !stats.skipped {
            let summary = summarize(&fs::read_to_string(&stats.destination).unwrap_or_default());
            let entry = L4IndexEntry {
                id: Uuid::new_v4().to_string(),
                path: stats.destination.display().to_string(),
                summary,
                created_at: Utc::now().to_rfc3339(),
            };
            store.l4_upsert(&entry)?;
            // Move source out of incoming to a processed sibling location.
            let processed = incoming_dir(paths).join("_processed");
            fs::create_dir_all(&processed).ok();
            let dest = processed.join(
                src.file_name()
                    .unwrap_or_else(|| std::ffi::OsStr::new("session")),
            );
            let _ = fs::rename(&src, dest);
        }
        stats_vec.push(stats);
    }
    Ok(stats_vec)
}

fn summarize(content: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in content.lines() {
        if line.starts_with("[USER]") || line.starts_with("=== Prompt") {
            out.push(line);
            if out.len() >= 6 {
                break;
            }
        }
    }
    if out.is_empty() {
        let first: String = content.lines().take(4).collect::<Vec<_>>().join(" \n ");
        return first.chars().take(400).collect();
    }
    out.join(" | ")
}

pub fn search(store: &Store, query: &str, k: usize) -> Result<Vec<L4Hit>> {
    store.l4_search(query, k)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn compresses_strips_system_block() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("sess.md");
        let mut f = std::fs::File::create(&src).unwrap();
        writeln!(f, "[SYSTEM]\nshould-be-stripped\n=== Prompt 2026-05-15 03:01\nhello\n=== Response 2026-05-15 03:02\nworld").unwrap();
        let out_dir = dir.path().join("archived");
        let stats = compress_session(&src, &out_dir, 0).unwrap();
        let body = fs::read_to_string(&stats.destination).unwrap();
        assert!(!body.contains("should-be-stripped"));
        assert!(body.contains("hello"));
        assert!(stats.destination.display().to_string().contains("0515"));
    }
}
