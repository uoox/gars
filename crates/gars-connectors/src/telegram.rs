use std::{collections::HashSet, sync::Arc, time::Duration};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{Value, json};

use crate::common::{
    Attachment, ChatTarget, Connector, ConnectorCaps, ConnectorContext, InboundEvent,
    OutboundMessage, UserInfo,
};

pub struct TelegramConnector {
    token: String,
    allow_chats: HashSet<String>,
    http: Client,
    api_base: String,
}

impl TelegramConnector {
    pub fn new(cfg: Value) -> Result<Self> {
        let token = cfg
            .get("token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                cfg.get("token_env")
                    .and_then(Value::as_str)
                    .and_then(|env| std::env::var(env).ok())
            })
            .or_else(|| std::env::var("TG_BOT_TOKEN").ok())
            .ok_or_else(|| anyhow!("telegram.token missing"))?;
        let allow_chats: HashSet<String> = cfg
            .get("allow_chats")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        v.as_str()
                            .map(str::to_string)
                            .or_else(|| v.as_i64().map(|n| n.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let api_base = cfg
            .get("api_base")
            .and_then(Value::as_str)
            .unwrap_or("https://api.telegram.org")
            .trim_end_matches('/')
            .to_string();
        let http = Client::builder().timeout(Duration::from_secs(60)).build()?;
        Ok(Self {
            token,
            allow_chats,
            http,
            api_base,
        })
    }

    fn endpoint(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.api_base, self.token, method)
    }

    async fn long_poll(&self, ctx: &ConnectorContext) -> Result<()> {
        let mut offset: i64 = 0;
        loop {
            let url = self.endpoint("getUpdates");
            let resp = self
                .http
                .get(&url)
                .query(&[
                    ("timeout", "30".to_string()),
                    ("offset", offset.to_string()),
                    ("allowed_updates", "[\"message\"]".to_string()),
                ])
                .send()
                .await;
            let resp = match resp {
                Ok(r) => match r.error_for_status() {
                    Ok(r) => r,
                    Err(err) => {
                        tracing::warn!("telegram getUpdates: {err}");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                },
                Err(err) => {
                    tracing::warn!("telegram getUpdates: {err}");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };
            let body: Value = match resp.json().await {
                Ok(v) => v,
                Err(err) => {
                    tracing::warn!("telegram parse: {err}");
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    continue;
                }
            };
            let Some(arr) = body.get("result").and_then(Value::as_array) else {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            };
            for update in arr {
                if let Some(id) = update.get("update_id").and_then(Value::as_i64) {
                    offset = id + 1;
                }
                if let Err(err) = self.handle_update(update, ctx).await {
                    tracing::warn!("telegram handle: {err}");
                }
            }
        }
    }

    async fn handle_update(&self, update: &Value, ctx: &ConnectorContext) -> Result<()> {
        let Some(message) = update.get("message") else {
            return Ok(());
        };
        let chat_id = message
            .pointer("/chat/id")
            .and_then(|v| {
                v.as_i64()
                    .map(|n| n.to_string())
                    .or_else(|| v.as_str().map(str::to_string))
            })
            .unwrap_or_default();
        if !self.allow_chats.is_empty() && !self.allow_chats.contains(&chat_id) {
            tracing::info!("telegram chat {chat_id} not whitelisted, ignoring");
            return Ok(());
        }
        let text = message
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let user_id = message
            .pointer("/from/id")
            .and_then(|v| v.as_i64().map(|n| n.to_string()))
            .unwrap_or_default();
        let user_name = message
            .pointer("/from/first_name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let chat = ChatTarget {
            chat_id: chat_id.clone(),
            thread_id: None,
            reply_to: message
                .get("message_id")
                .and_then(|v| v.as_i64().map(|n| n.to_string())),
        };
        let user = UserInfo {
            id: user_id,
            name: user_name,
            is_admin: false,
        };
        let event = if let Some(rest) = text.strip_prefix('/') {
            let mut parts = rest.splitn(2, ' ');
            let command = parts.next().unwrap_or("").to_string();
            let args = parts.next().unwrap_or("").to_string();
            InboundEvent::Command {
                connector: "telegram".to_string(),
                chat,
                command,
                args,
                user,
            }
        } else {
            InboundEvent::Message {
                connector: "telegram".to_string(),
                chat,
                user,
                text,
                attachments: Vec::new(),
            }
        };
        ctx.event_bus.send(event).await.ok();
        Ok(())
    }
}

#[async_trait]
impl Connector for TelegramConnector {
    fn id(&self) -> &str {
        "telegram"
    }

    fn capabilities(&self) -> ConnectorCaps {
        ConnectorCaps {
            text: true,
            markdown: true,
            image: true,
            file: true,
            stream_edit: true,
        }
    }

    async fn run(self: Arc<Self>, ctx: ConnectorContext) -> Result<()> {
        self.long_poll(&ctx).await
    }

    async fn send(&self, target: &ChatTarget, msg: &OutboundMessage) -> Result<()> {
        let mut payload = json!({
            "chat_id": target.chat_id,
            "text": if msg.text.is_empty() { " " } else { msg.text.as_str() },
        });
        if let Some(reply) = &target.reply_to {
            payload["reply_to_message_id"] = json!(reply.parse::<i64>().unwrap_or(0));
        }
        if msg.markdown {
            payload["parse_mode"] = json!("Markdown");
        }
        let url = self.endpoint("sendMessage");
        let resp = self.http.post(&url).json(&payload).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("telegram send {status}: {text}"));
        }
        for attachment in &msg.attachments {
            if attachment.kind == "image"
                && let Some(url_str) = &attachment.url
            {
                let _ = self
                    .http
                    .post(self.endpoint("sendPhoto"))
                    .json(&json!({"chat_id": target.chat_id, "photo": url_str}))
                    .send()
                    .await;
            }
        }
        let _ = Attachment::default();
        Ok(())
    }
}
