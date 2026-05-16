//! Embedded SOP / agent assets and the `builtin/` `local/` namespace policy.
//!
//! Rules:
//! - `~/.gars/skills/builtin/` and `~/.gars/agents/builtin/` are gars-owned.
//!   Every service start overwrites the contents with the assets compiled into
//!   this binary, and refreshes `.manifest.json`.
//! - `~/.gars/skills/local/` and `~/.gars/agents/local/` are user-owned. gars
//!   reads them but never writes to them.
//! - `~/.gars/skills/imported/` is the destination for `skill_import`, treated
//!   as a sibling of `local/` for read purposes.
//! - On first startup after v0.4, any existing flat layout
//!   (`~/.gars/skills/*.md`) is migrated: files matching a builtin key go
//!   into `builtin/` and everything else goes into `local/`. A
//!   `MIGRATION_v0.4.log` is appended so users can see what moved.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use gars_memory::GarsPaths;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{agents_dir, skills_dir};
// v0.0.3: the "mode" concept was removed; modes/ is no longer shipped or
// refreshed. SOPs (skills/) and agent definitions (agents/) are the only
// embedded asset categories.

const MANIFEST_VERSION: u32 = 1;
const GARS_VERSION: &str = env!("CARGO_PKG_VERSION");

const SOPS: &[(&str, &str)] = &[
    ("plan_sop.md", include_str!("../assets/sop/plan_sop.md")),
    (
        "subagent_sop.md",
        include_str!("../assets/sop/subagent_sop.md"),
    ),
    (
        "supervisor_sop.md",
        include_str!("../assets/sop/supervisor_sop.md"),
    ),
    ("verify_sop.md", include_str!("../assets/sop/verify_sop.md")),
    ("vision_sop.md", include_str!("../assets/sop/vision_sop.md")),
    ("adb_sop.md", include_str!("../assets/sop/adb_sop.md")),
    (
        "keychain_sop.md",
        include_str!("../assets/sop/keychain_sop.md"),
    ),
    ("input_sop.md", include_str!("../assets/sop/input_sop.md")),
    (
        "skill_search_sop.md",
        include_str!("../assets/sop/skill_search_sop.md"),
    ),
    (
        "scheduled_task_sop.md",
        include_str!("../assets/sop/scheduled_task_sop.md"),
    ),
    (
        "autonomous_sop.md",
        include_str!("../assets/sop/autonomous_sop.md"),
    ),
    (
        "code_review_principles.md",
        include_str!("../assets/sop/code_review_principles.md"),
    ),
    (
        "github_contribution_sop.md",
        include_str!("../assets/sop/github_contribution_sop.md"),
    ),
    ("goal_sop.md", include_str!("../assets/sop/goal_sop.md")),
    (
        "trigger_sop.md",
        include_str!("../assets/sop/trigger_sop.md"),
    ),
];

const AGENTS: &[(&str, &str)] = &[
    (
        "verifier.toml",
        include_str!("../assets/agents/verifier.toml"),
    ),
    (
        "explorer.toml",
        include_str!("../assets/agents/explorer.toml"),
    ),
    (
        "reviewer.toml",
        include_str!("../assets/agents/reviewer.toml"),
    ),
];

pub fn embedded_sops() -> &'static [(&'static str, &'static str)] {
    SOPS
}

pub fn embedded_agents() -> &'static [(&'static str, &'static str)] {
    AGENTS
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub manifest_version: u32,
    pub gars_version: String,
    pub written_at: String,
    pub files: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct InitSummary {
    pub builtin_skills: Vec<PathBuf>,
    pub builtin_agents: Vec<PathBuf>,
    pub migrated_to_builtin: Vec<PathBuf>,
    pub migrated_to_local: Vec<PathBuf>,
}

pub fn init_user_skills(paths: &GarsPaths) -> Result<InitSummary> {
    crate::ensure_user_dirs(paths)?;

    let skills_root = skills_dir(paths);
    let agents_root = agents_dir(paths);

    for sub in ["builtin", "local", "imported"] {
        fs::create_dir_all(skills_root.join(sub))?;
    }
    for sub in ["builtin", "local"] {
        fs::create_dir_all(agents_root.join(sub))?;
    }

    let mut summary = InitSummary::default();

    // One-time migration from v0.3 flat layout
    migrate_flat(&skills_root, SOPS, &mut summary, paths)?;
    migrate_flat(&agents_root, AGENTS, &mut summary, paths)?;

    // Refresh builtin/ with embedded assets
    let skill_manifest = refresh_builtin(&skills_root.join("builtin"), SOPS)?;
    for (name, _) in SOPS {
        summary
            .builtin_skills
            .push(skills_root.join("builtin").join(name));
    }
    write_manifest(
        &skills_root.join("builtin").join(".manifest.json"),
        skill_manifest,
    )?;

    let agent_manifest = refresh_builtin(&agents_root.join("builtin"), AGENTS)?;
    for (name, _) in AGENTS {
        summary
            .builtin_agents
            .push(agents_root.join("builtin").join(name));
    }
    write_manifest(
        &agents_root.join("builtin").join(".manifest.json"),
        agent_manifest,
    )?;

    Ok(summary)
}

fn refresh_builtin(dir: &Path, items: &[(&str, &str)]) -> Result<Manifest> {
    fs::create_dir_all(dir)?;
    let mut files = BTreeMap::new();
    let shipped: std::collections::HashSet<&str> = items.iter().map(|(n, _)| *n).collect();

    // Write/overwrite each shipped file
    for (name, content) in items {
        let path = dir.join(name);
        fs::write(&path, content).with_context(|| format!("write {}", path.display()))?;
        files.insert(name.to_string(), sha256_hex(content.as_bytes()));
    }

    // Remove stale builtin files that are no longer shipped (excluding manifest)
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if name == ".manifest.json" {
                continue;
            }
            if !shipped.contains(name) {
                let _ = fs::remove_file(&p);
            }
        }
    }

    Ok(Manifest {
        manifest_version: MANIFEST_VERSION,
        gars_version: GARS_VERSION.to_string(),
        written_at: chrono::Local::now().to_rfc3339(),
        files,
    })
}

fn write_manifest(path: &Path, manifest: Manifest) -> Result<()> {
    fs::write(path, serde_json::to_string_pretty(&manifest)?)?;
    Ok(())
}

pub fn load_manifest(path: &Path) -> Option<Manifest> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

fn migrate_flat(
    root: &Path,
    shipped: &[(&str, &str)],
    summary: &mut InitSummary,
    paths: &GarsPaths,
) -> Result<()> {
    let Ok(rd) = fs::read_dir(root) else {
        return Ok(());
    };
    let shipped_names: std::collections::HashSet<&str> = shipped.iter().map(|(n, _)| *n).collect();
    let mut log_lines: Vec<String> = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let dest_subdir = if shipped_names.contains(name) {
            "builtin"
        } else {
            "local"
        };
        let dest = root.join(dest_subdir).join(name);
        fs::create_dir_all(dest.parent().unwrap())?;
        if dest.exists() {
            let _ = fs::remove_file(&path);
            log_lines.push(format!(
                "{}\tremoved-duplicate\tfrom={}",
                name,
                path.display()
            ));
            continue;
        }
        fs::rename(&path, &dest).or_else(|_| fs::copy(&path, &dest).map(|_| ()))?;
        let _ = fs::remove_file(&path);
        if dest_subdir == "builtin" {
            summary.migrated_to_builtin.push(dest.clone());
        } else {
            summary.migrated_to_local.push(dest.clone());
        }
        log_lines.push(format!(
            "{}\tmoved\tto={}\t({} layout)",
            name,
            dest.display(),
            dest_subdir
        ));
    }
    if !log_lines.is_empty() {
        let log_path = paths.home.join("MIGRATION_v0.4.log");
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        writeln!(
            f,
            "[{}] migration in {}",
            chrono::Local::now().to_rfc3339(),
            root.display()
        )?;
        for line in log_lines {
            writeln!(f, "  {line}")?;
        }
    }
    Ok(())
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let bytes = hasher.finalize();
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn refresh_writes_builtin_and_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let paths = GarsPaths::resolve(Some(dir.path().to_path_buf())).unwrap();
        paths.ensure().unwrap();
        let summary = init_user_skills(&paths).unwrap();
        assert!(!summary.builtin_skills.is_empty());
        assert!(skills_dir(&paths).join("builtin/plan_sop.md").exists());
        assert!(skills_dir(&paths).join("builtin/.manifest.json").exists());
        assert!(agents_dir(&paths).join("builtin/verifier.toml").exists());
    }

    #[test]
    fn migrates_v03_flat_layout() {
        let dir = tempfile::tempdir().unwrap();
        let paths = GarsPaths::resolve(Some(dir.path().to_path_buf())).unwrap();
        paths.ensure().unwrap();
        // Simulate a v0.3 flat layout
        let skills = skills_dir(&paths);
        fs::create_dir_all(&skills).unwrap();
        {
            let mut f = fs::File::create(skills.join("plan_sop.md")).unwrap();
            writeln!(f, "old plan").unwrap();
        }
        {
            let mut f = fs::File::create(skills.join("user_custom_sop.md")).unwrap();
            writeln!(f, "my custom").unwrap();
        }
        let summary = init_user_skills(&paths).unwrap();
        // builtin/plan_sop.md should be the EMBEDDED content, not the old "old plan"
        let plan = fs::read_to_string(skills.join("builtin/plan_sop.md")).unwrap();
        assert!(plan.contains("Plan Mode SOP"));
        // user_custom_sop.md should be in local/
        assert!(skills.join("local/user_custom_sop.md").exists());
        assert!(!summary.migrated_to_local.is_empty());
        assert!(paths.home.join("MIGRATION_v0.4.log").exists());
    }

    #[test]
    fn rerunning_does_not_touch_local() {
        let dir = tempfile::tempdir().unwrap();
        let paths = GarsPaths::resolve(Some(dir.path().to_path_buf())).unwrap();
        paths.ensure().unwrap();
        init_user_skills(&paths).unwrap();
        let local_file = skills_dir(&paths).join("local/my.md");
        fs::write(&local_file, "user content").unwrap();
        init_user_skills(&paths).unwrap();
        assert_eq!(fs::read_to_string(&local_file).unwrap(), "user content");
    }
}
