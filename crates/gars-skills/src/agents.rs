use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use gars_memory::GarsPaths;

use crate::{agents_dir, assets::embedded_agents};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentDefinition {
    pub name: String,
    pub system_prompt: String,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default = "default_budget")]
    pub context_char_budget: usize,
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,
    #[serde(default)]
    pub verbose_default: bool,
    #[serde(skip)]
    pub source: Option<PathBuf>,
}

fn default_budget() -> usize {
    80_000
}

fn default_max_turns() -> usize {
    30
}

#[derive(Clone, Debug, Default)]
pub struct AgentRegistry {
    map: BTreeMap<String, AgentDefinition>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load_embedded() -> Self {
        let mut map = BTreeMap::new();
        for (name, content) in embedded_agents() {
            if let Ok(def) = toml::from_str::<AgentDefinition>(content) {
                map.insert(def.name.clone(), def);
            } else {
                tracing::warn!("failed to load embedded agent {name}");
            }
        }
        Self { map }
    }

    pub fn load_from_dir(dir: &Path) -> Result<Self> {
        let mut registry = Self::load_embedded();
        if !dir.exists() {
            return Ok(registry);
        }
        // v0.4: walk builtin/ then local/ so local overrides builtin.
        let mut candidates = Vec::new();
        let layered = dir.join("builtin").is_dir() || dir.join("local").is_dir();
        if layered {
            for sub in ["builtin", "local"] {
                let p = dir.join(sub);
                if p.is_dir() {
                    candidates.push(p);
                }
            }
        } else {
            candidates.push(dir.to_path_buf());
        }
        for d in candidates {
            for entry in fs::read_dir(&d)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                    continue;
                }
                let content = fs::read_to_string(&path)
                    .with_context(|| format!("read {}", path.display()))?;
                match toml::from_str::<AgentDefinition>(&content) {
                    Ok(mut def) => {
                        def.source = Some(path.clone());
                        registry.map.insert(def.name.clone(), def);
                    }
                    Err(err) => tracing::warn!("invalid agent {}: {}", path.display(), err),
                }
            }
        }
        Ok(registry)
    }

    pub fn load(paths: &GarsPaths) -> Result<Self> {
        Self::load_from_dir(&agents_dir(paths))
    }

    pub fn get(&self, name: &str) -> Option<&AgentDefinition> {
        self.map.get(name)
    }

    pub fn names(&self) -> Vec<String> {
        self.map.keys().cloned().collect()
    }

    pub fn list(&self) -> Vec<AgentDefinition> {
        self.map.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_loads() {
        let reg = AgentRegistry::load_embedded();
        assert!(reg.get("verifier").is_some());
    }
}
