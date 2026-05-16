use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc::Sender;

use gars_memory::GarsPaths;
use gars_store::Store;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ChatTarget {
    pub chat_id: String,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub reply_to: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UserInfo {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub is_admin: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Attachment {
    pub kind: String,
    pub url: Option<String>,
    pub bytes_base64: Option<String>,
    pub mime: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OutboundMessage {
    pub text: String,
    #[serde(default)]
    pub markdown: bool,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InboundEvent {
    Message {
        connector: String,
        chat: ChatTarget,
        user: UserInfo,
        text: String,
        #[serde(default)]
        attachments: Vec<Attachment>,
    },
    Command {
        connector: String,
        chat: ChatTarget,
        command: String,
        args: String,
        user: UserInfo,
    },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConnectorCaps {
    pub text: bool,
    pub markdown: bool,
    pub image: bool,
    pub file: bool,
    pub stream_edit: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConnectorState {
    pub id: String,
    pub enabled: bool,
    pub status: String,
    pub last_error: Option<String>,
    pub last_ping: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnectorEvent {
    pub connector: String,
    pub kind: String,
    pub payload: Value,
}

#[derive(Clone)]
pub struct ConnectorContext {
    pub paths: GarsPaths,
    pub store: Store,
    pub event_bus: Sender<InboundEvent>,
    pub admin_token: String,
    pub rest_base: String,
    pub config: Value,
}

/// Headers + raw body passed to `verify_webhook` for HMAC / signature checks.
///
/// Headers are *normalized to lowercase keys* by the server-side webhook
/// dispatcher so connectors don't have to care about HTTP casing.
pub struct WebhookRequest<'a> {
    pub headers: &'a std::collections::HashMap<String, String>,
    pub body: &'a [u8],
}

#[async_trait]
pub trait Connector: Send + Sync + 'static {
    fn id(&self) -> &str;
    fn capabilities(&self) -> ConnectorCaps;
    async fn run(self: Arc<Self>, ctx: ConnectorContext) -> Result<()>;
    async fn send(&self, target: &ChatTarget, msg: &OutboundMessage) -> Result<()>;

    /// Verify an inbound webhook request. Default implementation rejects with
    /// "not supported" so platforms that don't sign webhooks (Telegram via
    /// long-poll) don't accidentally accept arbitrary POSTs. Platforms that
    /// *do* sign (Feishu via X-Lark-Signature) override this.
    fn verify_webhook(&self, _req: &WebhookRequest<'_>) -> Result<()> {
        Err(anyhow::anyhow!(
            "connector '{}' does not support inbound webhooks",
            self.id()
        ))
    }
}
