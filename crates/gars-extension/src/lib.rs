//! Browser extension bridge.
//!
//! Architecture:
//! - `gars-server` exposes a WebSocket endpoint `/v1/extension`.
//! - The browser extension (Chrome/Edge MV3) connects with the user's admin
//!   token. Each connection is registered with `ExtensionRegistry::attach`,
//!   which returns a per-connection RPC client.
//! - Tools (`web_scan`, `web_execute_js`) ask the registry for the most
//!   recently connected extension and issue RPC calls like `scan_page`,
//!   `execute_js`, `screenshot`, `click`, `type`, `navigate`, `list_tabs`.
//!
//! Wire protocol (JSON):
//!   gars → ext: {"op": "scan_page", "id": "<uuid>", "params": { ... }}
//!   ext → gars: {"id": "<uuid>", "ok": true,  "data": { ... }}
//!   ext → gars: {"id": "<uuid>", "ok": false, "error": "..."}
//!
//! The registry is `Clone` (cheap Arc-wrapping) so it can be shared between
//! the server and the tool layer without ceremony.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExtensionHello {
    pub browser: Option<String>,
    pub version: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExtensionStatus {
    pub id: String,
    pub browser: Option<String>,
    pub version: Option<String>,
    pub connected_at: String,
    pub pending: usize,
}

#[derive(Debug, Serialize)]
struct OutboundFrame<'a> {
    op: &'a str,
    id: &'a str,
    params: &'a Value,
}

pub struct ExtensionHandle {
    pub id: String,
    pub browser: Option<String>,
    pub version: Option<String>,
    pub connected_at: chrono::DateTime<chrono::Local>,
    tx: mpsc::Sender<String>,
    pending: Mutex<HashMap<String, oneshot::Sender<RpcResult>>>,
}

#[derive(Clone, Debug)]
pub enum RpcResult {
    Ok(Value),
    Err(String),
}

impl ExtensionHandle {
    pub fn status(&self) -> ExtensionStatus {
        ExtensionStatus {
            id: self.id.clone(),
            browser: self.browser.clone(),
            version: self.version.clone(),
            connected_at: self.connected_at.to_rfc3339(),
            pending: 0,
        }
    }

    /// Call an extension operation. Returns `Err` if the extension is not
    /// connected, the request times out, or the extension reported an error.
    pub async fn call(
        self: &Arc<Self>,
        op: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let id = Uuid::new_v4().simple().to_string();
        let frame = OutboundFrame {
            op,
            id: &id,
            params: &params,
        };
        let frame_json = serde_json::to_string(&frame)?;
        let (resolve_tx, resolve_rx) = oneshot::channel::<RpcResult>();
        self.pending.lock().await.insert(id.clone(), resolve_tx);
        if self.tx.send(frame_json).await.is_err() {
            self.pending.lock().await.remove(&id);
            return Err(anyhow!("extension {} disconnected", self.id));
        }
        match tokio::time::timeout(timeout, resolve_rx).await {
            Ok(Ok(RpcResult::Ok(value))) => Ok(value),
            Ok(Ok(RpcResult::Err(err))) => Err(anyhow!(err)),
            Ok(Err(_)) => Err(anyhow!("extension reply channel dropped")),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(anyhow!("extension {} timed out on {op}", self.id))
            }
        }
    }

    /// Handle an inbound message from the extension. Returns true if it
    /// matched a pending request (so the caller can stop further parsing).
    pub async fn handle_inbound(&self, body: &str) -> bool {
        let Ok(value): std::result::Result<Value, _> = serde_json::from_str(body) else {
            return false;
        };
        let Some(id) = value.get("id").and_then(Value::as_str) else {
            return false;
        };
        let mut pending = self.pending.lock().await;
        if let Some(resolver) = pending.remove(id) {
            let result = if value.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                RpcResult::Ok(value.get("data").cloned().unwrap_or(Value::Null))
            } else {
                RpcResult::Err(
                    value
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("extension reported error")
                        .to_string(),
                )
            };
            let _ = resolver.send(result);
            true
        } else {
            false
        }
    }

    /// Force all pending requests to fail (e.g. on disconnect).
    pub async fn drain_pending(&self, reason: &str) {
        let mut pending = self.pending.lock().await;
        for (_, resolver) in pending.drain() {
            let _ = resolver.send(RpcResult::Err(reason.to_string()));
        }
    }
}

#[derive(Clone, Default)]
pub struct ExtensionRegistry {
    inner: Arc<RegistryInner>,
}

#[derive(Default)]
struct RegistryInner {
    items: RwLock<Vec<Arc<ExtensionHandle>>>,
    counter: AtomicU64,
}

impl ExtensionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn attach(
        &self,
        hello: ExtensionHello,
        tx: mpsc::Sender<String>,
    ) -> Arc<ExtensionHandle> {
        let id = format!("ext-{}", self.inner.counter.fetch_add(1, Ordering::Relaxed));
        let handle = Arc::new(ExtensionHandle {
            id,
            browser: hello.browser,
            version: hello.version,
            connected_at: chrono::Local::now(),
            tx,
            pending: Mutex::new(HashMap::new()),
        });
        self.inner.items.write().await.push(handle.clone());
        handle
    }

    pub async fn detach(&self, id: &str) {
        let mut items = self.inner.items.write().await;
        if let Some(idx) = items.iter().position(|h| h.id == id) {
            let h = items.remove(idx);
            tokio::spawn(async move {
                h.drain_pending("extension disconnected").await;
            });
        }
    }

    pub async fn current(&self) -> Option<Arc<ExtensionHandle>> {
        let items = self.inner.items.read().await;
        items.last().cloned()
    }

    pub async fn list(&self) -> Vec<ExtensionStatus> {
        let items = self.inner.items.read().await;
        let mut out = Vec::with_capacity(items.len());
        for h in items.iter() {
            let mut status = h.status();
            status.pending = h.pending.lock().await.len();
            out.push(status);
        }
        out
    }

    pub async fn is_connected(&self) -> bool {
        !self.inner.items.read().await.is_empty()
    }
}

/// Helper used by tools: send a payload + collect ack, return Value.
pub async fn ext_call(registry: &ExtensionRegistry, op: &str, params: Value) -> Result<Value> {
    let handle = registry
        .current()
        .await
        .ok_or_else(|| anyhow!("no browser extension connected"))?;
    handle.call(op, params, Duration::from_secs(45)).await
}

#[allow(dead_code)]
fn _force_json_dep() -> Value {
    json!(null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handle_inbound_resolves_pending() {
        let (tx, mut rx) = mpsc::channel::<String>(8);
        let handle = Arc::new(ExtensionHandle {
            id: "ext-test".into(),
            browser: None,
            version: None,
            connected_at: chrono::Local::now(),
            tx,
            pending: Mutex::new(HashMap::new()),
        });
        let handle2 = handle.clone();
        let call_task = tokio::spawn(async move {
            handle2
                .call("ping", json!({}), Duration::from_secs(1))
                .await
        });
        let frame = rx.recv().await.unwrap();
        let id = serde_json::from_str::<Value>(&frame).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let reply = json!({"id": id, "ok": true, "data": {"pong": true}}).to_string();
        assert!(handle.handle_inbound(&reply).await);
        let result = call_task.await.unwrap().unwrap();
        assert_eq!(result["pong"], true);
    }

    #[tokio::test]
    async fn registry_tracks_attach_detach() {
        let reg = ExtensionRegistry::new();
        let (tx, _rx) = mpsc::channel::<String>(8);
        let h = reg
            .attach(
                ExtensionHello {
                    browser: Some("chrome".into()),
                    version: Some("0.4.0".into()),
                },
                tx,
            )
            .await;
        assert!(reg.is_connected().await);
        reg.detach(&h.id).await;
        assert!(!reg.is_connected().await);
    }
}
