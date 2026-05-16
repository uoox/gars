use std::{sync::Arc, time::Duration};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use crate::common::{
    ChatTarget, Connector, ConnectorCaps, ConnectorContext, OutboundMessage, WebhookRequest,
};

pub struct FeishuConnector {
    app_id: String,
    app_secret: String,
    api_base: String,
    pub encrypt_key: Option<String>,
    pub verification_token: Option<String>,
    http: Client,
    tenant_token: RwLock<Option<(String, std::time::Instant)>>,
}

impl FeishuConnector {
    pub fn new(cfg: Value) -> Result<Self> {
        let app_id = cfg
            .get("app_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                cfg.get("app_id_env")
                    .and_then(Value::as_str)
                    .and_then(|env| std::env::var(env).ok())
            })
            .ok_or_else(|| anyhow!("feishu.app_id missing"))?;
        let app_secret = cfg
            .get("app_secret")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                cfg.get("app_secret_env")
                    .and_then(Value::as_str)
                    .and_then(|env| std::env::var(env).ok())
            })
            .ok_or_else(|| anyhow!("feishu.app_secret missing"))?;
        let api_base = cfg
            .get("api_base")
            .and_then(Value::as_str)
            .unwrap_or("https://open.feishu.cn/open-apis")
            .trim_end_matches('/')
            .to_string();
        let encrypt_key = cfg
            .get("encrypt_key")
            .and_then(Value::as_str)
            .map(str::to_string);
        let verification_token = cfg
            .get("verification_token")
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(Self {
            app_id,
            app_secret,
            api_base,
            encrypt_key,
            verification_token,
            http: Client::builder().timeout(Duration::from_secs(30)).build()?,
            tenant_token: RwLock::new(None),
        })
    }

    pub fn verification_token(&self) -> Option<&str> {
        self.verification_token.as_deref()
    }

    pub fn encrypt_key(&self) -> Option<&str> {
        self.encrypt_key.as_deref()
    }

    async fn ensure_token(&self) -> Result<String> {
        if let Some((token, fetched)) = self.tenant_token.read().await.clone()
            && fetched.elapsed() < Duration::from_secs(60 * 90)
        {
            return Ok(token);
        }
        let resp = self
            .http
            .post(format!(
                "{}/auth/v3/tenant_access_token/internal",
                self.api_base
            ))
            .json(&json!({"app_id": self.app_id, "app_secret": self.app_secret}))
            .send()
            .await?
            .json::<Value>()
            .await?;
        let code = resp.get("code").and_then(Value::as_i64).unwrap_or(-1);
        if code != 0 {
            return Err(anyhow!(
                "feishu tenant_token: code={code} msg={}",
                resp.get("msg").and_then(Value::as_str).unwrap_or("")
            ));
        }
        let token = resp
            .get("tenant_access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("feishu tenant_access_token missing"))?
            .to_string();
        *self.tenant_token.write().await = Some((token.clone(), std::time::Instant::now()));
        Ok(token)
    }

    /// Verify a webhook signature. Feishu uses sha256(timestamp + nonce + encrypt_key + body).
    pub fn verify_signature(
        &self,
        timestamp: &str,
        nonce: &str,
        body: &str,
        provided: &str,
    ) -> bool {
        let Some(encrypt_key) = &self.encrypt_key else {
            return true;
        };
        let mut hasher = Sha256::new();
        hasher.update(timestamp.as_bytes());
        hasher.update(nonce.as_bytes());
        hasher.update(encrypt_key.as_bytes());
        hasher.update(body.as_bytes());
        let computed = hex_lower(&hasher.finalize());
        computed.eq_ignore_ascii_case(provided)
    }
}

#[async_trait]
impl Connector for FeishuConnector {
    fn id(&self) -> &str {
        "feishu"
    }

    fn capabilities(&self) -> ConnectorCaps {
        ConnectorCaps {
            text: true,
            markdown: true,
            image: true,
            file: true,
            stream_edit: false,
        }
    }

    async fn run(self: Arc<Self>, _ctx: ConnectorContext) -> Result<()> {
        // Feishu uses webhook-based event subscription. Inbound events are dispatched
        // by the REST server's /v1/connectors/feishu/webhook handler. Keep the task
        // alive so the registry treats this connector as running.
        loop {
            tokio::time::sleep(Duration::from_secs(60 * 30)).await;
            // periodic token refresh
            if let Err(err) = self.ensure_token().await {
                tracing::warn!("feishu token refresh: {err}");
            }
        }
    }

    async fn send(&self, target: &ChatTarget, msg: &OutboundMessage) -> Result<()> {
        let token = self.ensure_token().await?;
        let content = json!({"text": msg.text}).to_string();
        let payload = json!({
            "receive_id": target.chat_id,
            "msg_type": "text",
            "content": content,
        });
        let receive_id_type = if target.chat_id.starts_with("oc_") {
            "chat_id"
        } else if target.chat_id.starts_with("ou_") {
            "open_id"
        } else {
            "user_id"
        };
        let resp = self
            .http
            .post(format!(
                "{}/im/v1/messages?receive_id_type={receive_id_type}",
                self.api_base
            ))
            .bearer_auth(&token)
            .json(&payload)
            .send()
            .await?
            .json::<Value>()
            .await?;
        let code = resp.get("code").and_then(Value::as_i64).unwrap_or(-1);
        if code != 0 {
            return Err(anyhow!("feishu send failed: code={code} resp={resp}"));
        }
        Ok(())
    }

    fn verify_webhook(&self, req: &WebhookRequest<'_>) -> Result<()> {
        // No encrypt_key configured = no signature configured at the platform
        // side either; we accept the request unverified. This matches what
        // verify_signature did before.
        let Some(encrypt_key) = &self.encrypt_key else {
            return Ok(());
        };
        let timestamp = req
            .headers
            .get("x-lark-request-timestamp")
            .map(String::as_str)
            .unwrap_or("");
        let nonce = req
            .headers
            .get("x-lark-request-nonce")
            .map(String::as_str)
            .unwrap_or("");
        let provided = req
            .headers
            .get("x-lark-signature")
            .map(String::as_str)
            .unwrap_or("");
        let mut hasher = Sha256::new();
        hasher.update(timestamp.as_bytes());
        hasher.update(nonce.as_bytes());
        hasher.update(encrypt_key.as_bytes());
        hasher.update(req.body);
        let computed = hex_lower(&hasher.finalize());
        if !computed.eq_ignore_ascii_case(provided) {
            return Err(anyhow!(
                "feishu webhook signature mismatch (provided='{}' computed='{}')",
                truncate_for_log(provided),
                truncate_for_log(&computed)
            ));
        }
        Ok(())
    }
}

fn truncate_for_log(s: &str) -> String {
    if s.len() <= 12 {
        s.to_string()
    } else {
        format!("{}…", &s[..12])
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{:02x}", b));
    }
    out
}
