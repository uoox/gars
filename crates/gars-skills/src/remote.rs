//! Remote skill search client targeting the lsdefine/GenericAgent
//! `skill_search` API (default `http://www.fudankw.cn:58787`). Contract is
//! derived from `memory/skill_search/skill_search/engine.py` in that repo:
//! `POST {base}/search` with `{query, env, top_k, category?}` returning
//! `{results: [{skill, relevance, quality, final_score, match_reasons, warnings}]}`.
//!
//! Note: per upstream SKILL.md, Chinese queries match poorly — callers should
//! pass English keywords.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::skill::SkillIndex;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteSearchResult {
    pub skill: SkillIndex,
    #[serde(default)]
    pub relevance: f64,
    #[serde(default)]
    pub quality: f64,
    #[serde(default)]
    pub final_score: f64,
    #[serde(default)]
    pub match_reasons: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct RemoteClient {
    api: String,
    key: Option<String>,
    timeout: Duration,
    http: Client,
}

impl RemoteClient {
    pub fn new(api: &str, key: Option<String>, timeout: Duration) -> Result<Self> {
        let http = Client::builder()
            .timeout(timeout)
            .build()
            .context("build skill-search http client")?;
        Ok(Self {
            api: api.trim_end_matches('/').to_string(),
            key,
            timeout,
            http,
        })
    }

    pub async fn search(
        &self,
        query: &str,
        category: Option<&str>,
        top_k: usize,
    ) -> Result<Vec<RemoteSearchResult>> {
        let mut body = json!({
            "query": query,
            "env": minimal_env(),
            "top_k": top_k,
        });
        if let Some(cat) = category
            && !cat.is_empty()
        {
            body["category"] = Value::String(cat.to_string());
        }
        let url = format!("{}/search", self.api);
        let mut req = self.http.post(&url).json(&body);
        if let Some(k) = &self.key
            && !k.is_empty()
        {
            req = req.header("X-Skill-Search-Key", k);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("POST {url} (timeout {}s)", self.timeout.as_secs().max(1)))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("skill-search {status}: {text}"));
        }
        let parsed: Value = resp.json().await.context("parse skill-search json")?;
        let arr = parsed
            .get("results")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("skill-search response missing 'results' array"))?;
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            let r: RemoteSearchResult =
                serde_json::from_value(item.clone()).context("decode SearchResult")?;
            out.push(r);
        }
        Ok(out)
    }
}

fn minimal_env() -> Value {
    json!({
        "os": std::env::consts::OS,
        "shell": std::env::var("SHELL").unwrap_or_else(|_| "".to_string()),
        "runtimes": ["rust"],
        "tools": Value::Array(vec![]),
        "model": {
            "tool_calling": true,
            "reasoning": true,
            "context_window": "long",
        }
    })
}
