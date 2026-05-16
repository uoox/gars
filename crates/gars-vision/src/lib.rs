//! Vision API client + OCR utilities.
//!
//! Local OCR ships via an external `ocrs` runtime if installed (we look for the
//! `ocrs` binary on PATH; users can run `pip install ocrs-cli` or download the
//! pre-built binary). If OCR is unavailable, callers fall back to a vision API.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::process::Command;

pub use gars_llm::SessionConfig;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct VisionConfig {
    pub backend: Option<String>, // "anthropic" | "openai"
    pub model: Option<String>,
    pub api_base: Option<String>,
    pub api_key_env: Option<String>,
    pub api_key: Option<String>,
    pub ocrs_binary: Option<String>,    // path to ocrs CLI
    pub ocrs_model_dir: Option<String>, // dir containing detection.rten + recognition.rten
}

pub async fn vision_describe(cfg: &VisionConfig, image: &Path, prompt: &str) -> Result<String> {
    let backend = cfg.backend.as_deref().unwrap_or("anthropic");
    let api_key = if let Some(k) = &cfg.api_key {
        k.clone()
    } else if let Some(env) = &cfg.api_key_env {
        std::env::var(env).with_context(|| format!("env {env} missing"))?
    } else {
        String::new()
    };
    let bytes = fs::read(image).with_context(|| format!("read {}", image.display()))?;
    let mime = guess_mime(image);
    let b64 = B64.encode(&bytes);
    let http = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    match backend {
        "anthropic" => {
            let model = cfg
                .model
                .clone()
                .unwrap_or_else(|| "claude-sonnet-4-5".to_string());
            let api_base = cfg
                .api_base
                .clone()
                .unwrap_or_else(|| "https://api.anthropic.com".to_string());
            let payload = json!({
                "model": model,
                "max_tokens": 1024,
                "messages": [{
                    "role": "user",
                    "content": [
                        {"type": "image", "source": {"type": "base64", "media_type": mime, "data": b64}},
                        {"type": "text", "text": prompt},
                    ]
                }]
            });
            let resp = http
                .post(format!("{}/v1/messages", api_base.trim_end_matches('/')))
                .header("anthropic-version", "2023-06-01")
                .header("x-api-key", &api_key)
                .json(&payload)
                .send()
                .await?
                .error_for_status()?
                .json::<Value>()
                .await?;
            let text = resp
                .pointer("/content/0/text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Ok(text)
        }
        _ => {
            // OpenAI Vision-style payload
            let model = cfg
                .model
                .clone()
                .unwrap_or_else(|| "gpt-4o-mini".to_string());
            let api_base = cfg
                .api_base
                .clone()
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            let url = format!("data:{};base64,{}", mime, b64);
            let payload = json!({
                "model": model,
                "messages": [{
                    "role": "user",
                    "content": [
                        {"type": "text", "text": prompt},
                        {"type": "image_url", "image_url": {"url": url}},
                    ]
                }]
            });
            let resp = http
                .post(format!(
                    "{}/chat/completions",
                    api_base.trim_end_matches('/')
                ))
                .bearer_auth(&api_key)
                .json(&payload)
                .send()
                .await?
                .error_for_status()?
                .json::<Value>()
                .await?;
            let text = resp
                .pointer("/choices/0/message/content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Ok(text)
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OcrResult {
    pub text: String,
    pub lines: Vec<String>,
    pub source: String,
}

pub async fn ocr_image(cfg: &VisionConfig, image: &Path) -> Result<OcrResult> {
    let binary = cfg
        .ocrs_binary
        .clone()
        .unwrap_or_else(|| "ocrs".to_string());
    let path_str = image.to_string_lossy().to_string();
    let mut cmd = Command::new(&binary);
    cmd.arg(&path_str)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = &cfg.ocrs_model_dir {
        cmd.env("OCRS_MODEL_DIR", dir);
    }
    let output = match cmd.output().await {
        Ok(o) => o,
        Err(err) => {
            return Err(anyhow!(
                "ocrs binary unavailable ({err}); install with `pip install ocrs-cli` or set vision.ocrs_binary"
            ));
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("ocrs failed: {stderr}"));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let lines: Vec<String> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    Ok(OcrResult {
        text: stdout.trim().to_string(),
        lines,
        source: "ocrs".to_string(),
    })
}

pub fn ocr_diagnostic(cfg: &VisionConfig) -> Result<String> {
    let binary = cfg
        .ocrs_binary
        .clone()
        .unwrap_or_else(|| "ocrs".to_string());
    Ok(format!(
        "expected ocrs CLI at '{binary}'; pass an image to `ocr_image` to verify"
    ))
}

fn guess_mime(path: &Path) -> String {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png".into(),
        Some("jpg") | Some("jpeg") => "image/jpeg".into(),
        Some("gif") => "image/gif".into(),
        Some("webp") => "image/webp".into(),
        _ => "image/png".into(),
    }
}

pub fn locate_default_ocrs_model(home: &Path) -> PathBuf {
    home.join("models").join("ocrs")
}
