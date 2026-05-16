use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SkillForm {
    #[default]
    Markdown,
    Tool,
    Recipe,
}

impl SkillForm {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "tool" => Self::Tool,
            "recipe" => Self::Recipe,
            _ => Self::Markdown,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SkillSource {
    #[default]
    Builtin,
    Local,
    Imported,
}

impl SkillSource {
    pub fn from_path(path: &Path) -> Self {
        for comp in path.components() {
            if let std::path::Component::Normal(s) = comp {
                match s.to_str() {
                    Some("builtin") => return Self::Builtin,
                    Some("local") => return Self::Local,
                    Some("imported") => return Self::Imported,
                    _ => {}
                }
            }
        }
        Self::Local
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SkillIndex {
    pub key: String,
    pub name: String,
    pub one_line_summary: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub form: SkillForm,
    #[serde(default)]
    pub autonomous_safe: bool,
    pub path: PathBuf,
    #[serde(default)]
    pub body_preview: String,
    #[serde(default)]
    pub source: SkillSource,
}

pub fn parse_skill_file(path: &Path) -> Result<SkillIndex> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("read skill {}", path.display()))?;
    let (frontmatter, body) = split_frontmatter(&content);
    let mut skill = if let Some(fm) = frontmatter {
        parse_yaml_lite(fm)?
    } else {
        SkillIndex::default()
    };
    skill.path = path.to_path_buf();
    skill.source = SkillSource::from_path(path);
    if skill.key.is_empty() {
        skill.key = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("skill")
            .to_string();
    }
    if skill.name.is_empty() {
        skill.name = skill.key.clone();
    }
    skill.body_preview = body.lines().take(40).collect::<Vec<_>>().join("\n");
    Ok(skill)
}

/// Scan a skills root directory honoring the v0.4 namespace layout.
///
/// When the root contains the new `builtin/` / `local/` / `imported/` subdirs,
/// the function walks them in order and lets `local`/`imported` override
/// `builtin` entries with the same key.
///
/// When called on a directory without that layout (e.g. a stand-alone scratch
/// dir from tests) it falls back to a recursive scan and returns every
/// markdown file it finds.
pub fn scan_skills_dir(root: &Path) -> Vec<SkillIndex> {
    let layered = root.join("builtin").is_dir()
        || root.join("local").is_dir()
        || root.join("imported").is_dir();
    if !layered {
        return scan_flat(root);
    }
    let mut by_key: std::collections::BTreeMap<String, SkillIndex> = Default::default();
    for sub in ["builtin", "local", "imported"] {
        let dir = root.join(sub);
        if !dir.exists() {
            continue;
        }
        for skill in scan_flat(&dir) {
            // later passes (local, imported) overwrite earlier (builtin) on same key
            by_key.insert(skill.key.clone(), skill);
        }
    }
    by_key.into_values().collect()
}

fn scan_flat(root: &Path) -> Vec<SkillIndex> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name.starts_with('.') {
            continue;
        }
        if p.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        match parse_skill_file(p) {
            Ok(skill) => out.push(skill),
            Err(err) => tracing::warn!("skip {}: {}", p.display(), err),
        }
    }
    out
}

fn split_frontmatter(text: &str) -> (Option<&str>, &str) {
    let trimmed = text.trim_start();
    if let Some(stripped) = trimmed.strip_prefix("---\n")
        && let Some(end) = stripped.find("\n---")
    {
        let fm = &stripped[..end];
        let mut body = &stripped[end + 4..];
        if body.starts_with('\n') {
            body = &body[1..];
        }
        return (Some(fm), body);
    }
    (None, text)
}

fn parse_yaml_lite(fm: &str) -> Result<SkillIndex> {
    let mut skill = SkillIndex::default();
    let mut current_key: Option<String> = None;
    for raw_line in fm.lines() {
        if raw_line.trim().is_empty() {
            continue;
        }
        if let Some(rest) = raw_line.strip_prefix("- ") {
            if let Some(key) = &current_key {
                let val = rest.trim().trim_matches('"').to_string();
                if key == "tags" {
                    skill.tags.push(val);
                }
            }
            continue;
        }
        let Some((key, value)) = raw_line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_string();
        let value = value.trim();
        current_key = Some(key.clone());
        if value.is_empty() {
            continue;
        }
        if value.starts_with('[') && value.ends_with(']') {
            let inner = &value[1..value.len() - 1];
            let items: Vec<String> = inner
                .split(',')
                .map(|s| s.trim().trim_matches('"').to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if key.as_str() == "tags" {
                skill.tags = items;
            }
            continue;
        }
        let value = value.trim_matches('"').to_string();
        match key.as_str() {
            "key" => skill.key = value,
            "name" => skill.name = value,
            "one_line_summary" => skill.one_line_summary = value,
            "description" => skill.description = value,
            "category" => skill.category = value,
            "form" => skill.form = SkillForm::parse(&value),
            "autonomous_safe" => skill.autonomous_safe = parse_bool(&value),
            _ => {}
        }
    }
    if skill.key.is_empty() && skill.name.is_empty() {
        return Err(anyhow!("frontmatter missing key/name"));
    }
    Ok(skill)
}

fn parse_bool(s: &str) -> bool {
    matches!(s.to_ascii_lowercase().as_str(), "true" | "yes" | "1" | "on")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("foo.md");
        std::fs::write(
            &path,
            "---\nkey: foo\nname: Foo\ntags: [a, b]\ncategory: x\n---\nbody",
        )
        .unwrap();
        let s = parse_skill_file(&path).unwrap();
        assert_eq!(s.key, "foo");
        assert_eq!(s.tags, vec!["a".to_string(), "b".to_string()]);
    }
}
