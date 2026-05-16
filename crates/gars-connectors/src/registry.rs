use std::{collections::BTreeMap, sync::Arc};

use anyhow::Result;
use serde_json::Value;
use tokio::task::JoinHandle;

use crate::{Connector, ConnectorContext, ConnectorState, FeishuConnector, TelegramConnector};

#[derive(Default)]
pub struct ConnectorRegistry {
    items: BTreeMap<String, Arc<dyn Connector>>,
    tasks: BTreeMap<String, JoinHandle<()>>,
    states: BTreeMap<String, ConnectorState>,
}

impl ConnectorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn list(&self) -> Vec<ConnectorState> {
        self.states.values().cloned().collect()
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Connector>> {
        self.items.get(id).cloned()
    }

    pub fn upsert_state(&mut self, state: ConnectorState) {
        self.states.insert(state.id.clone(), state);
    }

    pub fn shutdown_all(&mut self) {
        for (_, task) in std::mem::take(&mut self.tasks) {
            task.abort();
        }
    }

    pub async fn start_from_config(
        &mut self,
        ctx_base: &ConnectorContext,
        connectors_cfg: &Value,
    ) -> Result<()> {
        let table = connectors_cfg.as_object().cloned().unwrap_or_default();
        for (id, cfg) in table {
            let enabled = cfg.get("enable").and_then(Value::as_bool).unwrap_or(false);
            let mut state = ConnectorState {
                id: id.clone(),
                enabled,
                status: if enabled { "starting" } else { "disabled" }.to_string(),
                last_error: None,
                last_ping: None,
            };
            if !enabled {
                self.states.insert(id.clone(), state);
                continue;
            }
            let connector: Arc<dyn Connector> = match id.as_str() {
                "telegram" => Arc::new(TelegramConnector::new(cfg.clone())?),
                "feishu" => Arc::new(FeishuConnector::new(cfg.clone())?),
                other => {
                    // gars only implements Telegram + Feishu. Any other
                    // `[connectors.xxx]` segment is tolerated (so old
                    // configs keep parsing) but logged + skipped.
                    tracing::info!(
                        "connector '{other}' is not supported (only Telegram + Feishu); ignoring"
                    );
                    state.status = "unknown_connector".to_string();
                    state.last_error = Some(format!("no implementation for {other}"));
                    self.states.insert(id.clone(), state);
                    continue;
                }
            };
            let task_ctx = ConnectorContext {
                config: cfg.clone(),
                ..ctx_base.clone()
            };
            let id_for_task = id.clone();
            let connector_for_task = connector.clone();
            let handle = tokio::spawn(async move {
                if let Err(err) = connector_for_task.run(task_ctx).await {
                    tracing::warn!("connector {} exited: {err:#}", id_for_task);
                }
            });
            state.status = "running".to_string();
            self.items.insert(id.clone(), connector);
            self.tasks.insert(id.clone(), handle);
            self.states.insert(id, state);
        }
        Ok(())
    }
}
