use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAGIC: &[u8] = b"gars-keychain-v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeychainEntry {
    pub name: String,
    pub created_at: String,
    pub size: usize,
    pub mask: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KeychainFile {
    entries: std::collections::BTreeMap<String, EncryptedEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncryptedEntry {
    pub blob_b64: String,
    pub created_at: String,
    pub mask: String,
    pub size: usize,
}

pub struct Keychain {
    path: PathBuf,
    file: KeychainFile,
    key: [u8; 32],
}

impl Keychain {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let file = if path.exists() {
            let content = fs::read_to_string(&path)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            KeychainFile::default()
        };
        Ok(Self {
            path,
            file,
            key: derive_key(),
        })
    }

    pub fn list(&self) -> Vec<KeychainEntry> {
        self.file
            .entries
            .iter()
            .map(|(name, entry)| KeychainEntry {
                name: name.clone(),
                created_at: entry.created_at.clone(),
                size: entry.size,
                mask: entry.mask.clone(),
            })
            .collect()
    }

    pub fn set(&mut self, name: &str, value: &[u8]) -> Result<()> {
        let blob = xor(&self.key, value);
        let entry = EncryptedEntry {
            blob_b64: B64.encode(blob),
            created_at: chrono::Local::now().to_rfc3339(),
            mask: mask_value(value),
            size: value.len(),
        };
        self.file.entries.insert(name.to_string(), entry);
        self.save()
    }

    pub fn set_from_file(&mut self, name: &str, path: &Path) -> Result<()> {
        let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        self.set(name, &bytes)
    }

    pub fn get(&self, name: &str) -> Result<Vec<u8>> {
        let entry = self
            .file
            .entries
            .get(name)
            .ok_or_else(|| anyhow!("keychain entry {name} not found"))?;
        let blob = B64.decode(entry.blob_b64.as_bytes())?;
        Ok(xor(&self.key, &blob))
    }

    pub fn delete(&mut self, name: &str) -> Result<()> {
        self.file.entries.remove(name);
        self.save()
    }

    fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, serde_json::to_string_pretty(&self.file)?)?;
        Ok(())
    }
}

fn derive_key() -> [u8; 32] {
    let user = std::env::var("USER").unwrap_or_default();
    let host = hostname();
    let mut hasher = Sha256::new();
    hasher.update(MAGIC);
    hasher.update(user.as_bytes());
    hasher.update(host.as_bytes());
    let out = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&out);
    key
}

fn hostname() -> String {
    if let Ok(out) = std::process::Command::new("hostname").output()
        && let Ok(s) = String::from_utf8(out.stdout)
    {
        return s.trim().to_string();
    }
    std::env::var("HOSTNAME").unwrap_or_else(|_| "localhost".to_string())
}

fn xor(key: &[u8; 32], data: &[u8]) -> Vec<u8> {
    data.iter()
        .enumerate()
        .map(|(i, b)| b ^ key[i % key.len()])
        .collect()
}

fn mask_value(value: &[u8]) -> String {
    if value.is_empty() {
        return "(empty)".to_string();
    }
    let s = String::from_utf8_lossy(value);
    if s.chars().count() <= 12 {
        return format!("{}…", s.chars().take(2).collect::<String>());
    }
    let head: String = s.chars().take(6).collect();
    let tail: String = s.chars().rev().take(6).collect();
    let tail: String = tail.chars().rev().collect();
    format!("{head}…{tail} ({})", value.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut kc = Keychain::open(dir.path().join("kc.json")).unwrap();
        kc.set("api_key", b"sk-test-value-1234567890").unwrap();
        let v = kc.get("api_key").unwrap();
        assert_eq!(v, b"sk-test-value-1234567890");
        let list = kc.list();
        assert_eq!(list.len(), 1);
        assert!(list[0].mask.contains('…'));
    }
}
