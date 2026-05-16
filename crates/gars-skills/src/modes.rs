//! Task mode definitions.
//!
//! A *mode* is a TOML file that bundles together: which SOPs to load, which
//! tools are allowed, and which defaults (budget / max_turns / runner_kind)
//! to use. Live at `~/.gars/modes/{builtin,local}/<key>.toml`.
//!
//! `builtin/` is refreshed from binary-embedded assets on every service
//! start (see `assets::init_user_skills`); `local/` is user-owned and never
//! touched. Listings merge both, with `local` overriding `builtin` on a
//! key collision.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use gars_memory::GarsPaths;
use serde::{Deserialize, Serialize};

use crate::{scan_skills_dir, skills_dir};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ModeDef {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    /// Which underlying runner shape to use. One of:
    /// "chat", "schedule", "subagent", "plan", "goal".
    /// Front-ends pick the right REST endpoint from this field.
    pub runner_kind: String,
    #[serde(default)]
    pub sop_keys: Vec<String>,
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub budget_secs: Option<u64>,
    #[serde(default)]
    pub max_turns: Option<u32>,
    /// Populated at load time. "builtin" | "local" | "imported".
    #[serde(default = "default_source")]
    pub source: String,
}

fn default_source() -> String {
    "local".into()
}

pub fn modes_dir(paths: &GarsPaths) -> PathBuf {
    paths.home.join("modes")
}

/// Load every mode currently on disk. `local` overrides `builtin` on
/// key collision. Imported sources are returned alongside both.
pub fn load_all_modes(paths: &GarsPaths) -> Vec<ModeDef> {
    let root = modes_dir(paths);
    let mut by_key: BTreeMap<String, ModeDef> = BTreeMap::new();
    for sub in ["builtin", "local", "imported"] {
        let dir = root.join(sub);
        if !dir.exists() {
            continue;
        }
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) != Some("toml") {
                continue;
            }
            if p.file_name().and_then(|s| s.to_str()) == Some(".manifest.json") {
                continue;
            }
            match parse_mode_file(&p, sub) {
                Ok(mut m) => {
                    m.source = sub.to_string();
                    by_key.insert(m.key.clone(), m);
                }
                Err(err) => tracing::warn!("skip mode {}: {err}", p.display()),
            }
        }
    }
    by_key.into_values().collect()
}

pub fn load_mode(paths: &GarsPaths, key: &str) -> Option<ModeDef> {
    load_all_modes(paths).into_iter().find(|m| m.key == key)
}

/// Read the body markdown of each SOP referenced by `keys`. Missing keys are
/// silently skipped (with a warn log) so the runner never panics on a typo.
pub fn load_sop_bodies(paths: &GarsPaths, keys: &[String]) -> Vec<String> {
    if keys.is_empty() {
        return Vec::new();
    }
    let skills = scan_skills_dir(&skills_dir(paths));
    let mut by_key: std::collections::HashMap<String, &crate::skill::SkillIndex> =
        std::collections::HashMap::new();
    for s in &skills {
        by_key.entry(s.key.clone()).or_insert(s);
    }
    let mut out = Vec::with_capacity(keys.len());
    for key in keys {
        match by_key.get(key) {
            Some(skill) => match std::fs::read_to_string(&skill.path) {
                Ok(body) => out.push(body),
                Err(err) => tracing::warn!("read sop {}: {err}", skill.path.display()),
            },
            None => tracing::warn!("sop key '{key}' not found (mode references it)"),
        }
    }
    out
}

/// Resolve a mode key into (definition, sop bodies in declared order).
///
/// **v0.7 note**: callers should prefer `mode_hint` instead of consuming the
/// SOP bodies directly. Per the upstream "small core, big SOP" design, mode-
/// specific SOPs should be fetched on demand by the LLM (via the `skill_show`
/// tool), not dumped into the system prompt every turn. This function is kept
/// around for tooling that genuinely wants raw bodies, but the runners now
/// inject only a hint listing which SOP keys are relevant.
pub fn resolve_mode(paths: &GarsPaths, key: &str) -> Option<(ModeDef, Vec<String>)> {
    let m = load_mode(paths, key)?;
    let bodies = load_sop_bodies(paths, &m.sop_keys);
    Some((m, bodies))
}

/// Build a short system-prompt hint for a mode: the mode's label + description
/// plus a bulleted list of available SOP keys with one-line summaries. The LLM
/// reads this hint and decides whether to call `skill_show(key=...)` to fetch
/// the full body of any particular SOP — matching upstream's "L3 SOP is read
/// on demand, not stacked in context" pattern.
pub fn mode_hint(paths: &GarsPaths, mode: &ModeDef) -> String {
    let mut out = format!("## Active mode: {} (`{}`)\n", mode.label, mode.key);
    if !mode.description.trim().is_empty() {
        out.push_str(mode.description.trim());
        out.push('\n');
    }
    if !mode.sop_keys.is_empty() {
        out.push_str("\n## Available SOPs for this mode\n");
        out.push_str(
            "Call `skill_show(key=\"<key>\")` to fetch the full body of any SOP \
             below when you need its detailed guidance — do NOT assume the full \
             text is already in this prompt.\n\n",
        );
        let skills = crate::skill::scan_skills_dir(&crate::skills_dir(paths));
        let by_key: std::collections::HashMap<&str, &crate::skill::SkillIndex> =
            skills.iter().map(|s| (s.key.as_str(), s)).collect();
        for k in &mode.sop_keys {
            match by_key.get(k.as_str()) {
                Some(s) if !s.one_line_summary.is_empty() => {
                    out.push_str(&format!("- `{}` — {}\n", s.key, s.one_line_summary));
                }
                Some(s) => {
                    out.push_str(&format!("- `{}` — {}\n", s.key, s.name));
                }
                None => {
                    out.push_str(&format!(
                        "- `{}` — (referenced by mode but not currently in skills/)\n",
                        k
                    ));
                }
            }
        }
    }
    out
}

fn parse_mode_file(path: &Path, source: &str) -> Result<ModeDef> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut def: ModeDef =
        toml::from_str(&content).with_context(|| format!("parse {}", path.display()))?;
    def.source = source.to_string();
    if def.key.trim().is_empty() {
        // Fall back to file stem.
        def.key = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("mode")
            .to_string();
    }
    Ok(def)
}

/// Write a user mode to `local/<key>.toml`. Rejects keys colliding with
/// a builtin mode.
pub fn save_local_mode(paths: &GarsPaths, def: &ModeDef) -> Result<PathBuf> {
    let root = modes_dir(paths);
    let local = root.join("local");
    fs::create_dir_all(&local)?;
    let safe_key = sanitize_key(&def.key)?;
    let builtin_path = root.join("builtin").join(format!("{safe_key}.toml"));
    if builtin_path.exists() {
        return Err(anyhow!(
            "key '{safe_key}' collides with a builtin mode; pick a different key"
        ));
    }
    let target = local.join(format!("{safe_key}.toml"));
    let mut to_write = def.clone();
    to_write.source = "local".into();
    to_write.key = safe_key.clone();
    let text = toml::to_string_pretty(&to_write).context("encode mode toml")?;
    fs::write(&target, text).with_context(|| format!("write {}", target.display()))?;
    Ok(target)
}

/// Delete a local or imported mode. Builtin modes can never be deleted.
pub fn delete_local_mode(paths: &GarsPaths, key: &str) -> Result<()> {
    let root = modes_dir(paths);
    let safe_key = sanitize_key(key)?;
    let candidates = [
        root.join("local").join(format!("{safe_key}.toml")),
        root.join("imported").join(format!("{safe_key}.toml")),
    ];
    for c in &candidates {
        if c.exists() {
            fs::remove_file(c).with_context(|| format!("remove {}", c.display()))?;
            return Ok(());
        }
    }
    Err(anyhow!(
        "mode '{safe_key}' not found in local/ or imported/"
    ))
}

fn sanitize_key(key: &str) -> Result<String> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("mode key cannot be empty"));
    }
    if trimmed.len() > 64 {
        return Err(anyhow!("mode key too long: {trimmed}"));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(anyhow!(
            "mode key '{trimmed}' must be ASCII alphanumeric + _-"
        ));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_keys() {
        assert!(sanitize_key("").is_err());
        assert!(sanitize_key("../etc").is_err());
        assert!(sanitize_key("has space").is_err());
        assert!(sanitize_key("valid-key_1").is_ok());
    }
}
