//! Skill / SOP catalog, BM25 search, subagent definitions.

mod agents;
mod assets;
mod market;
mod modes;
mod plan_scan;
mod remote;
mod search;
mod skill;

pub use agents::{AgentDefinition, AgentRegistry};
pub use assets::{
    InitSummary, Manifest, embedded_agents, embedded_modes, embedded_sops, init_user_skills,
    load_manifest,
};
pub use market::{
    MarketClient, MarketDetail, MarketItem, MarketListPage, MarketQuery, parse_list_html,
};
pub use modes::{
    ModeDef, delete_local_mode, load_all_modes, load_mode, load_sop_bodies, mode_hint, modes_dir,
    resolve_mode, save_local_mode,
};
pub use plan_scan::{PlanSummary, scan_plans};
pub use remote::{RemoteClient, RemoteSearchResult};
pub use search::{SearchOptions, SkillHit, rank as rank_local, search_local};
pub use skill::{SkillForm, SkillIndex, SkillSource, parse_skill_file, scan_skills_dir};

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use gars_memory::GarsPaths;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillsConfig {
    /// Remote skill-search API base. Empty string disables the remote backend.
    #[serde(default = "default_remote")]
    pub remote: String,
    /// Env var holding the optional API key.
    #[serde(default = "default_remote_key_env")]
    pub remote_key_env: String,
    /// HTTP timeout for the remote backend.
    #[serde(default = "default_remote_timeout")]
    pub remote_timeout_secs: u64,
    /// Sophub marketplace base URL.
    #[serde(default = "default_market")]
    pub market: String,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            remote: default_remote(),
            remote_key_env: default_remote_key_env(),
            remote_timeout_secs: default_remote_timeout(),
            market: default_market(),
        }
    }
}

fn default_remote() -> String {
    "http://www.fudankw.cn:58787".into()
}
fn default_remote_key_env() -> String {
    "SKILL_SEARCH_KEY".into()
}
fn default_remote_timeout() -> u64 {
    8
}
fn default_market() -> String {
    "https://fudankw.cn".into()
}

impl SkillsConfig {
    pub fn remote_base(&self) -> Option<&str> {
        let modern = self.remote.trim();
        if modern.is_empty() {
            None
        } else {
            Some(modern)
        }
    }
}

/// Unified search hit returned to REST callers and CLI. Mirrors the upstream
/// `SearchResult` schema; locally-ranked hits fill `quality` with a neutral
/// 0.5 and tag themselves via `match_reasons = ["local_bm25"]`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UnifiedHit {
    pub skill: SkillIndex,
    pub final_score: f64,
    pub relevance: f64,
    pub quality: f64,
    pub match_reasons: Vec<String>,
    pub warnings: Vec<String>,
    pub source: String,
}

impl From<RemoteSearchResult> for UnifiedHit {
    fn from(r: RemoteSearchResult) -> Self {
        Self {
            skill: r.skill,
            final_score: r.final_score,
            relevance: r.relevance,
            quality: r.quality,
            match_reasons: r.match_reasons,
            warnings: r.warnings,
            source: "remote".into(),
        }
    }
}

impl From<SkillHit> for UnifiedHit {
    fn from(h: SkillHit) -> Self {
        Self {
            skill: h.skill,
            final_score: h.score,
            relevance: h.score,
            quality: 0.5,
            match_reasons: vec!["local_bm25".into()],
            warnings: vec![],
            source: "local_bm25".into(),
        }
    }
}

/// Search the local + (optionally) remote catalogs.
///
/// - If `cfg.remote_base()` is set, hit it first.
/// - On remote 4xx / 5xx / timeout / connection error, fall back to local BM25
///   and stamp `warnings = ["remote_unavailable: ..."]`.
/// - If the remote is empty, go straight to local.
pub async fn unified_search(
    cfg: &SkillsConfig,
    paths: &GarsPaths,
    query: &str,
    category: Option<&str>,
    top_k: usize,
) -> Vec<UnifiedHit> {
    if query.trim().is_empty() {
        // No query: just list everything from the local catalog.
        let root = skills_dir(paths);
        return scan_skills_dir(&root)
            .into_iter()
            .map(|skill| UnifiedHit {
                skill,
                final_score: 0.0,
                relevance: 0.0,
                quality: 0.5,
                match_reasons: vec!["catalog".into()],
                warnings: vec![],
                source: "local_catalog".into(),
            })
            .collect();
    }

    if let Some(base) = cfg.remote_base() {
        let key = std::env::var(&cfg.remote_key_env)
            .ok()
            .filter(|s| !s.is_empty());
        let timeout = Duration::from_secs(cfg.remote_timeout_secs.max(1));
        match RemoteClient::new(base, key, timeout) {
            Ok(client) => match client.search(query, category, top_k).await {
                Ok(results) => return results.into_iter().map(UnifiedHit::from).collect(),
                Err(err) => {
                    tracing::warn!("skill-search remote failed, falling back: {err}");
                    let mut fallback = local_unified(paths, query, category, top_k);
                    if let Some(first) = fallback.first_mut() {
                        first.warnings.push(format!("remote_unavailable: {err}"));
                    }
                    return fallback;
                }
            },
            Err(err) => {
                tracing::warn!("skill-search remote client init failed: {err}");
            }
        }
    }
    local_unified(paths, query, category, top_k)
}

fn local_unified(
    paths: &GarsPaths,
    query: &str,
    category: Option<&str>,
    top_k: usize,
) -> Vec<UnifiedHit> {
    let root = skills_dir(paths);
    let hits = search_local(
        query,
        &root,
        SearchOptions {
            top_k: top_k.max(1),
            category: category.map(str::to_string),
            autonomous_only: false,
        },
    );
    hits.into_iter().map(UnifiedHit::from).collect()
}

pub fn skills_dir(paths: &GarsPaths) -> PathBuf {
    paths.home.join("skills")
}

pub fn agents_dir(paths: &GarsPaths) -> PathBuf {
    paths.home.join("agents")
}

pub fn plans_dir(paths: &GarsPaths) -> PathBuf {
    paths.home.join("plans")
}

pub fn ensure_user_dirs(paths: &GarsPaths) -> Result<()> {
    for dir in [skills_dir(paths), agents_dir(paths), plans_dir(paths)] {
        std::fs::create_dir_all(dir)?;
    }
    Ok(())
}
