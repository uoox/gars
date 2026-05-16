//! Chat platform connectors.
//!
//! gars only supports **Telegram** and **Feishu / Lark** as inbound /
//! outbound chat platforms. Unknown `[connectors.xxx]` segments are
//! tolerated (logged + ignored) so old configs keep parsing, but no
//! additional platforms are planned.

mod common;
mod feishu;
mod registry;
mod telegram;

pub use common::{
    Attachment, ChatTarget, Connector, ConnectorCaps, ConnectorContext, ConnectorEvent,
    ConnectorState, InboundEvent, OutboundMessage, UserInfo, WebhookRequest,
};
pub use feishu::FeishuConnector;
pub use registry::ConnectorRegistry;
pub use telegram::TelegramConnector;
