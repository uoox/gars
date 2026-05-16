use std::{
    convert::Infallible,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{
        Path as AxumPath, Query, State,
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, Method, StatusCode},
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures_util::{Stream, StreamExt};
use gars_archive::{ArchiveConfig, run_idle_pass};
use gars_connectors::{ConnectorContext, ConnectorRegistry, InboundEvent, WebhookRequest};
use gars_core::{
    AgentRuntime, PlanFile, RuntimeEvent, RuntimeOptions, SubagentHandle, ToolContext,
    ToolRegistry, allocate_workdir, load_run, scan_runs, write_input,
};
use gars_extension::{ExtensionHello, ExtensionRegistry};
use gars_llm::{RootConfig, build_client, parse_root_config};
use gars_memory::{GarsPaths, ServiceState, global_memory_prompt};
use gars_skills::{
    AgentRegistry, MarketClient, MarketQuery, SkillsConfig, UnifiedHit, init_user_skills,
    plans_dir, scan_plans, skills_dir, unified_search,
};
use gars_store::{Store, TaskRecord};
use gars_tools::{BuiltinToolsOptions, register_builtin_tools};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    net::TcpListener,
    sync::{Mutex, RwLock, broadcast},
};
use tokio_stream::wrappers::IntervalStream;
use tower_http::{
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};

pub mod scheduler;
pub mod subagent_runner;

#[derive(Clone)]
pub struct ServerOptions {
    pub paths: GarsPaths,
    pub config_path: PathBuf,
}

#[derive(Clone)]
pub struct AppState {
    pub(crate) paths: GarsPaths,
    pub(crate) config_path: PathBuf,
    pub(crate) config: Arc<RwLock<RootConfig>>,
    pub(crate) store: Store,
    pub(crate) connectors: Arc<Mutex<ConnectorRegistry>>,
    pub(crate) extensions: ExtensionRegistry,
    pub(crate) event_bus: broadcast::Sender<BusEvent>,
    pub(crate) inbound_tx: tokio::sync::mpsc::Sender<InboundEvent>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BusEvent {
    pub topic: String,
    pub payload: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct ServerInfo {
    pub bind: String,
    pub port: u16,
    pub data_dir: String,
    pub config_path: String,
}

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub message: String,
    pub llm: Option<String>,
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub stream: bool,
}

#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub result: String,
    pub content: Option<String>,
    pub exit_data: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct TaskCreateResponse {
    pub task: TaskRecord,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ConfigPayload {
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct ToolCallPayload {
    #[serde(default)]
    pub args: Value,
    pub cwd: Option<PathBuf>,
}

pub async fn serve(options: ServerOptions) -> Result<()> {
    options.paths.ensure()?;
    let _ = init_user_skills(&options.paths);
    gars_archive::ensure_dirs(&options.paths).ok();
    let store = Store::new(options.paths.home.join("gars.db"));
    store.init()?;
    let config = load_config(&options.config_path)?;
    let bind = server_bind(&config);
    let port = server_port(&config);
    let addr: SocketAddr = format!("{bind}:{port}").parse()?;
    let (event_tx, _) = broadcast::channel::<BusEvent>(256);
    let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel::<InboundEvent>(64);
    let state = Arc::new(AppState {
        paths: options.paths,
        config_path: options.config_path,
        config: Arc::new(RwLock::new(config)),
        store,
        connectors: Arc::new(Mutex::new(ConnectorRegistry::new())),
        extensions: ExtensionRegistry::new(),
        event_bus: event_tx.clone(),
        inbound_tx: inbound_tx.clone(),
    });
    {
        let mut s = ServiceState::load(&state.paths.state);
        s.status = Some("running".into());
        s.started_at = Some(chrono::Local::now().to_rfc3339());
        s.save(&state.paths.state)?;
    }

    // Start connectors based on config.
    start_connectors(state.clone()).await?;

    // Spawn inbound dispatcher: turns InboundEvent into REST tasks.
    spawn_inbound_dispatcher(state.clone(), inbound_rx);

    // Spawn archive idle pass if enabled.
    spawn_archive_loop(state.clone());

    // Spawn scheduler.
    scheduler::spawn_scheduler(state.clone());

    let app = router(state.clone());
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    tracing::info!("gars REST API listening on http://{addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    {
        let mut s = ServiceState::load(&state.paths.state);
        s.status = Some("stopped".into());
        let _ = s.save(&state.paths.state);
    }
    state.connectors.lock().await.shutdown_all();
    Ok(())
}

async fn start_connectors(state: Arc<AppState>) -> Result<()> {
    let cfg = state.config.read().await.clone();
    let Some(connectors_cfg) = cfg.connectors.clone() else {
        return Ok(());
    };
    let admin_token = admin_token(&cfg).unwrap_or_default();
    let rest_base = format!("http://{}:{}", server_bind(&cfg), server_port(&cfg));
    let ctx_base = ConnectorContext {
        paths: state.paths.clone(),
        store: state.store.clone(),
        event_bus: state.inbound_tx.clone(),
        admin_token,
        rest_base,
        config: Value::Null,
    };
    let mut registry = state.connectors.lock().await;
    registry
        .start_from_config(&ctx_base, &connectors_cfg)
        .await?;
    Ok(())
}

fn spawn_inbound_dispatcher(
    state: Arc<AppState>,
    mut rx: tokio::sync::mpsc::Receiver<InboundEvent>,
) {
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let _ = state.event_bus.send(BusEvent {
                topic: "connector".to_string(),
                payload: serde_json::to_value(&event).unwrap_or(Value::Null),
            });
            if let Err(err) = dispatch_inbound(&state, event).await {
                tracing::warn!("inbound dispatch failed: {err}");
            }
        }
    });
}

async fn dispatch_inbound(state: &AppState, event: InboundEvent) -> Result<()> {
    let (input, connector_id, chat) = match event {
        InboundEvent::Message {
            connector,
            chat,
            text,
            ..
        } => (text, connector, chat),
        InboundEvent::Command {
            connector,
            chat,
            command,
            args,
            ..
        } => match command.as_str() {
            "status" => (
                "Return a one-line status summary of the gars service.".to_string(),
                connector,
                chat,
            ),
            "cancel" => return Ok(()),
            _ => (
                format!("/{command} {args}").trim().to_string(),
                connector,
                chat,
            ),
        },
    };
    let task = state.store.create_task(&input)?;
    let task_id = task.id.clone();
    let state2 = state.clone();
    let connector_id2 = connector_id.clone();
    let chat2 = chat.clone();
    tokio::spawn(async move {
        let _ = state2.store.set_task_status(&task_id, "running", None);
        let outcome =
            run_agent_with_state(&state2, input.clone(), None, None, Some(task_id.clone())).await;
        let reply_text = match outcome {
            Ok(o) => o
                .final_response
                .map(|r| r.content)
                .unwrap_or_else(|| o.result),
            Err(err) => format!("error: {err:#}"),
        };
        let _ = state2
            .store
            .set_task_status(&task_id, "done", Some(&reply_text));
        // Push reply back via connector.
        let registry = state2.connectors.lock().await;
        if let Some(connector) = registry.get(&connector_id2) {
            let msg = gars_connectors::OutboundMessage {
                text: reply_text,
                markdown: false,
                attachments: Vec::new(),
                extra: Default::default(),
            };
            if let Err(err) = connector.send(&chat2, &msg).await {
                tracing::warn!("connector {} send failed: {err}", connector_id2);
            }
        }
    });
    Ok(())
}

async fn run_agent_with_state(
    state: &AppState,
    input: String,
    llm: Option<String>,
    cwd: Option<PathBuf>,
    task_id: Option<String>,
) -> Result<gars_core::RuntimeOutcome> {
    let cfg = state.config.read().await.clone();
    let selected = llm
        .as_deref()
        .or(cfg.default_llm.as_deref())
        .unwrap_or("primary");
    let client = build_client(&cfg.llm, selected)?;
    let mut runtime = AgentRuntime::new(
        client,
        registry_with_extensions(&cfg, Some(state.extensions.clone())),
        build_system_prompt(&state.paths, &cfg)?,
        RuntimeOptions {
            gars_home: state.paths.home.clone(),
            cwd: cwd.unwrap_or_else(|| state.paths.tmp.clone()),
            context_char_budget: cfg.context_char_budget.unwrap_or(180_000),
            ..RuntimeOptions::default()
        },
    );
    let store = state.store.clone();
    let bus = state.event_bus.clone();
    let emit_task = task_id.clone();
    runtime
        .run_once(&input, move |event| {
            let payload = match &event {
                RuntimeEvent::TurnStarted(turn) => Some(("turn_started", json!({"turn": turn}))),
                RuntimeEvent::AssistantText(text) => {
                    Some(("assistant_text", json!({"text": text})))
                }
                RuntimeEvent::ToolStarted { name, args } => {
                    Some(("tool_started", json!({"name": name, "args": args})))
                }
                RuntimeEvent::ToolFinished { name, data } => {
                    Some(("tool_finished", json!({"name": name, "data": data})))
                }
                RuntimeEvent::Warning(msg) => Some(("warning", json!({"message": msg}))),
            };
            if let Some((event_type, payload)) = payload {
                if let Some(task_id) = &emit_task {
                    let _ = store.append_event(task_id, event_type, payload.clone());
                }
                let _ = bus.send(BusEvent {
                    topic: "task".to_string(),
                    payload: json!({"task_id": emit_task, "type": event_type, "payload": payload}),
                });
            }
        })
        .await
}

fn spawn_archive_loop(state: Arc<AppState>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let cfg_root = state.config.read().await.clone();
            let archive_cfg: ArchiveConfig = cfg_root
                .archive
                .clone()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            if !archive_cfg.auto {
                tokio::time::sleep(Duration::from_secs(archive_cfg.idle_secs.max(60))).await;
                continue;
            }
            match run_idle_pass(&state.paths, &state.store, &archive_cfg) {
                Ok(stats) => {
                    if !stats.is_empty() {
                        let _ = state.event_bus.send(BusEvent {
                            topic: "archive".to_string(),
                            payload: json!({"compressed": stats.len()}),
                        });
                    }
                }
                Err(err) => tracing::warn!("archive idle pass: {err}"),
            }
            tokio::time::sleep(Duration::from_secs(archive_cfg.idle_secs.max(60))).await;
        }
    });
}

pub fn load_config(path: &PathBuf) -> Result<RootConfig> {
    parse_root_config(
        &fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?,
    )
}

pub fn server_info(paths: &GarsPaths, config_path: &Path, cfg: &RootConfig) -> ServerInfo {
    ServerInfo {
        bind: server_bind(cfg),
        port: server_port(cfg),
        data_dir: paths.home.display().to_string(),
        config_path: config_path.display().to_string(),
    }
}

fn router(state: Arc<AppState>) -> Router {
    let v1 = Router::new()
        .route("/status", get(status))
        .route("/chat", post(chat))
        .route("/chat/stream", post(chat_stream_handler))
        .route("/tasks", get(list_tasks).post(create_task))
        .route("/tasks/{id}", get(get_task))
        .route("/tasks/{id}/events", get(task_events))
        .route("/config", get(get_config).put(put_config))
        .route("/tools", get(tools))
        .route("/tools/{name_call}", post(call_tool))
        .route("/memory/{layer}", get(get_memory).put(put_memory))
        .route("/skills", get(list_skills))
        .route("/skills/import", post(skills_import))
        .route("/skills/market", get(market_list))
        .route("/skills/market/install", post(market_install))
        .route("/skills/market/{id}", get(market_detail))
        .route("/skills/{key}", get(get_skill))
        .route("/agents", get(list_agents))
        .route("/connectors", get(list_connectors))
        .route("/connectors/{id}/reload", post(reload_connector))
        .route("/connectors/{id}/send", post(send_connector))
        .route("/connectors/feishu/webhook", post(feishu_webhook))
        .route("/connectors/telegram/webhook", post(telegram_webhook))
        .route("/archive/run", post(archive_run))
        .route("/archive/search", get(archive_search))
        .route("/archive/{id}", get(archive_get))
        .route("/plans", get(list_plans).post(create_plan))
        .route("/plans/{id}", get(get_plan).delete(delete_plan))
        .route("/plans/{id}/steps/{idx}/mark", post(mark_plan_step))
        .route("/subagents", get(list_subagents).post(spawn_subagent))
        .route("/subagents/{run_id}", get(get_subagent))
        .route("/subagents/{run_id}/run", post(run_subagent_route))
        .route("/subagents/{run_id}/intervene", post(intervene_subagent))
        .route("/subagents/{run_id}/stop", post(stop_subagent))
        .route("/schedules", get(list_schedules).post(upsert_schedule))
        .route("/schedules/{id}", get(get_schedule).delete(delete_schedule))
        .route("/schedules/{id}/trigger", post(trigger_schedule))
        .route("/schedules/{id}/health", get(schedule_health))
        .route("/extension", get(extension_ws))
        .route("/extension/state", get(extension_state))
        .route("/events", get(events_ws));

    Router::new()
        .route("/health", get(health))
        .route("/", get(redirect_to_ui))
        .route("/ui", get(redirect_to_ui))
        .route("/ui/", get(ui_root))
        .route("/ext/download/gars-extension.zip", get(ext_download))
        .nest("/v1", v1)
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
                .allow_headers(Any),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

// Embedded single-file Web UI. ~45 KB of HTML + CSS + vanilla JS; renders
// against the same /v1 REST API. CDN is used for markdown-it / highlight.js
// with a graceful fallback to <pre> when offline.
const UI_HTML: &str = include_str!("../assets/ui.html");

async fn ui_root() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        UI_HTML,
    )
}

async fn redirect_to_ui() -> impl IntoResponse {
    axum::response::Redirect::permanent("/ui/")
}

// Embedded extension zip. The bytes come from `extension/dist.zip` produced
// by the extension's `npm run build`; we ship it directly so users can
// download via `/ext/download/gars-extension.zip` without a web frontend.
const EXTENSION_ZIP: &[u8] = include_bytes!("../../../extension/dist.zip");

async fn ext_download() -> axum::response::Response {
    use axum::body::Body;
    use axum::http::{HeaderValue, header};
    let mut response = axum::response::Response::new(Body::from(EXTENSION_ZIP.to_vec()));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"gars-extension.zip\""),
    );
    response
}

async fn health() -> Json<Value> {
    Json(json!({"status": "ok", "service": "gars"}))
}

async fn status(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let cfg = state.config.read().await;
    let info = server_info(&state.paths, &state.config_path, &cfg);
    let st = ServiceState::load(&state.paths.state);
    Ok(Json(json!({
        "service": "gars",
        "status": st.status.unwrap_or_else(|| "running".to_string()),
        "started_at": st.started_at,
        "server": info,
        "db": state.store.path().display().to_string(),
    })))
}

async fn chat(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ChatRequest>,
) -> ApiResult<Json<ChatResponse>> {
    authorize(&state, &headers)?;
    let outcome = run_agent(state, req.message, req.llm, req.cwd, None).await?;
    Ok(Json(ChatResponse {
        result: outcome.result,
        content: outcome.final_response.map(|r| r.content),
        exit_data: outcome.exit_data,
    }))
}

async fn chat_stream_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ChatRequest>,
) -> ApiResult<Sse<impl Stream<Item = std::result::Result<Event, Infallible>>>> {
    authorize(&state, &headers)?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let state_for_task = state.clone();
    tokio::spawn(async move {
        let cfg = state_for_task.config.read().await.clone();
        let selected = req
            .llm
            .as_deref()
            .or(cfg.default_llm.as_deref())
            .unwrap_or("primary")
            .to_string();
        let client = match build_client(&cfg.llm, &selected) {
            Ok(c) => c,
            Err(err) => {
                let _ = tx.send(
                    Event::default()
                        .event("error")
                        .data(format!("client: {err:#}")),
                );
                return;
            }
        };
        let system_prompt = match build_system_prompt(&state_for_task.paths, &cfg) {
            Ok(s) => s,
            Err(err) => {
                let _ = tx.send(
                    Event::default()
                        .event("error")
                        .data(format!("system: {err:#}")),
                );
                return;
            }
        };
        let mut runtime = AgentRuntime::new(
            client,
            registry_with_extensions(&cfg, Some(state_for_task.extensions.clone())),
            system_prompt,
            RuntimeOptions {
                gars_home: state_for_task.paths.home.clone(),
                cwd: req.cwd.unwrap_or_else(|| state_for_task.paths.tmp.clone()),
                context_char_budget: cfg.context_char_budget.unwrap_or(180_000),
                ..RuntimeOptions::default()
            },
        );
        let tx_event = tx.clone();
        let tx_delta = tx.clone();
        let outcome = runtime
            .run_once_stream(
                &req.message,
                move |event| {
                    let payload = match event {
                        RuntimeEvent::TurnStarted(turn) => {
                            (Some("turn_started"), json!({"turn": turn}))
                        }
                        RuntimeEvent::AssistantText(text) => {
                            (Some("assistant_text"), json!({"text": text}))
                        }
                        RuntimeEvent::ToolStarted { name, args } => {
                            (Some("tool_started"), json!({"name": name, "args": args}))
                        }
                        RuntimeEvent::ToolFinished { name, data } => {
                            (Some("tool_finished"), json!({"name": name, "data": data}))
                        }
                        RuntimeEvent::Warning(msg) => (Some("warning"), json!({"message": msg})),
                    };
                    if let (Some(name), payload) = payload {
                        let _ =
                            tx_event.send(Event::default().event(name).data(payload.to_string()));
                    }
                },
                &mut move |delta: &str| {
                    let _ = tx_delta.send(Event::default().event("delta").data(delta));
                },
            )
            .await;
        match outcome {
            Ok(o) => {
                let _ = tx.send(
                    Event::default().event("done").data(
                        serde_json::to_string(&json!({
                            "result": o.result,
                            "content": o.final_response.map(|r| r.content),
                            "exit_data": o.exit_data,
                        }))
                        .unwrap_or_default(),
                    ),
                );
            }
            Err(err) => {
                let _ = tx.send(Event::default().event("error").data(format!("{err:#}")));
            }
        }
    });
    let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx).map(Ok::<_, Infallible>);
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

#[derive(Debug, Deserialize)]
pub struct ListTasksQuery {
    pub limit: Option<usize>,
}

async fn list_tasks(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<ListTasksQuery>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let limit = q.limit.unwrap_or(50).min(500);
    let tasks = state.store.list_tasks(limit)?;
    Ok(Json(json!({ "tasks": tasks })))
}

async fn create_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ChatRequest>,
) -> ApiResult<Json<TaskCreateResponse>> {
    authorize(&state, &headers)?;
    let task = state.store.create_task(&req.message)?;
    let task_id = task.id.clone();
    let bg_state = state.clone();
    tokio::spawn(async move {
        let _ = bg_state.store.set_task_status(&task_id, "running", None);
        let outcome = run_agent(
            bg_state.clone(),
            req.message,
            req.llm,
            req.cwd,
            Some(task_id.clone()),
        )
        .await;
        match outcome {
            Ok(outcome) => {
                let content = outcome
                    .final_response
                    .map(|r| r.content)
                    .unwrap_or_else(|| outcome.result.clone());
                let _ = bg_state
                    .store
                    .set_task_status(&task_id, "done", Some(&content));
            }
            Err(err) => {
                let msg = format!("{err:#}");
                let _ = bg_state
                    .store
                    .set_task_status(&task_id, "error", Some(&msg));
            }
        }
    });
    Ok(Json(TaskCreateResponse { task }))
}

async fn get_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let Some(task) = state.store.get_task(&id)? else {
        return Err(ApiError::not_found("task not found"));
    };
    Ok(Json(serde_json::to_value(task)?))
}

async fn task_events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> ApiResult<Sse<impl Stream<Item = std::result::Result<Event, Infallible>>>> {
    authorize(&state, &headers)?;
    let last = Arc::new(AtomicI64::new(
        query
            .get("after")
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0),
    ));
    let stream_state = state.clone();
    let stream_id = id.clone();
    let stream_last = last.clone();
    let stream =
        IntervalStream::new(tokio::time::interval(Duration::from_millis(750))).then(move |_| {
            let store = stream_state.store.clone();
            let task_id = stream_id.clone();
            let last = stream_last.clone();
            async move {
                let events = store
                    .task_events_after(&task_id, last.load(Ordering::Relaxed))
                    .unwrap_or_default();
                if let Some(max_id) = events.iter().map(|event| event.id).max() {
                    last.store(max_id, Ordering::Relaxed);
                }
                let payload = serde_json::to_string(&events).unwrap_or_else(|_| "[]".to_string());
                Ok(Event::default().event("events").data(payload))
            }
        });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn get_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<ConfigPayload>> {
    authorize(&state, &headers)?;
    Ok(Json(ConfigPayload {
        content: fs::read_to_string(&state.config_path)?,
    }))
}

async fn put_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<ConfigPayload>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let cfg = parse_root_config(&payload.content)?;
    fs::write(&state.config_path, payload.content)?;
    *state.config.write().await = cfg;
    Ok(Json(json!({"status": "ok"})))
}

async fn tools(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let cfg = state.config.read().await.clone();
    let registry = registry_with_extensions(&cfg, Some(state.extensions.clone()));
    Ok(Json(json!({"tools": registry.specs()})))
}

async fn call_tool(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(name_call): AxumPath<String>,
    Json(payload): Json<ToolCallPayload>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let cfg = state.config.read().await.clone();
    let registry = registry_with_extensions(&cfg, Some(state.extensions.clone()));
    let mut ctx = ToolContext {
        gars_home: state.paths.home.clone(),
        cwd: payload.cwd.unwrap_or_else(|| state.paths.tmp.clone()),
        current_turn: 0,
        tool_index: 0,
        tool_count: 1,
        working: Default::default(),
        history_info: Vec::new(),
    };
    let name = name_call
        .strip_suffix(":call")
        .ok_or_else(|| ApiError::not_found("tool call route must end with :call"))?;
    let outcome = registry.execute(name, payload.args, &mut ctx).await?;
    Ok(Json(serde_json::to_value(outcome)?))
}

async fn get_memory(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(layer): AxumPath<String>,
) -> ApiResult<Json<ConfigPayload>> {
    authorize(&state, &headers)?;
    let path = memory_path(&state.paths, &layer)?;
    Ok(Json(ConfigPayload {
        content: fs::read_to_string(path)?,
    }))
}

async fn put_memory(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(layer): AxumPath<String>,
    Json(payload): Json<ConfigPayload>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let path = memory_path(&state.paths, &layer)?;
    fs::write(path, payload.content)?;
    Ok(Json(json!({"status": "ok"})))
}

async fn run_agent(
    state: Arc<AppState>,
    input: String,
    llm: Option<String>,
    cwd: Option<PathBuf>,
    task_id: Option<String>,
) -> Result<gars_core::RuntimeOutcome> {
    run_agent_with_state(&state, input, llm, cwd, task_id).await
}

pub(crate) fn registry(cfg: &RootConfig) -> ToolRegistry {
    registry_with_extensions(cfg, None)
}

pub(crate) fn registry_with_extensions(
    cfg: &RootConfig,
    extensions: Option<ExtensionRegistry>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    register_builtin_tools(
        &mut registry,
        BuiltinToolsOptions {
            browser: browser_config(cfg),
            vision: vision_config(cfg),
            extensions,
        },
    );
    registry
}

fn vision_config(cfg: &RootConfig) -> gars_vision::VisionConfig {
    let v = cfg.vision.clone().unwrap_or_default();
    serde_json::from_value(v).unwrap_or_default()
}

pub(crate) fn build_system_prompt(paths: &GarsPaths, cfg: &RootConfig) -> Result<String> {
    let lang = cfg.language.as_deref().unwrap_or("zh");
    let base = if lang == "en" {
        "Role: Physical-Level Omnipotent Executor\nUse tools to probe before claiming. Reply in the user's language.\n"
    } else {
        "Role: Physical-Level Omnipotent Executor\n你拥有文件、脚本、浏览器和系统级工具。不要空猜，先用工具验证。用用户语言回复。\n"
    };
    Ok(format!(
        "{}\nToday: {}\n{}",
        base,
        chrono::Local::now().format("%Y-%m-%d %a"),
        global_memory_prompt(paths)?
    ))
}

fn browser_config(cfg: &RootConfig) -> gars_cdp::BrowserConfig {
    gars_cdp::BrowserConfig {
        host: cfg
            .browser
            .get("host")
            .and_then(Value::as_str)
            .unwrap_or("127.0.0.1")
            .to_string(),
        port: cfg
            .browser
            .get("port")
            .and_then(Value::as_i64)
            .unwrap_or(9222) as u16,
    }
}

fn server_bind(cfg: &RootConfig) -> String {
    cfg.server
        .get("bind")
        .and_then(Value::as_str)
        .unwrap_or("127.0.0.1")
        .to_string()
}

fn server_port(cfg: &RootConfig) -> u16 {
    cfg.server
        .get("port")
        .and_then(Value::as_i64)
        .unwrap_or(9221) as u16
}

fn admin_token(cfg: &RootConfig) -> Option<String> {
    cfg.server
        .get("admin_token")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(ToString::to_string)
}

fn authorize(state: &AppState, headers: &HeaderMap) -> ApiResult<()> {
    let cfg = state
        .config
        .try_read()
        .map_err(|_| ApiError::internal("config temporarily locked"))?;
    let Some(token) = admin_token(&cfg) else {
        return Ok(());
    };
    let header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if header == format!("Bearer {token}") {
        Ok(())
    } else {
        Err(ApiError::unauthorized())
    }
}

fn memory_path(paths: &GarsPaths, layer: &str) -> ApiResult<PathBuf> {
    let path = match layer {
        "l0" | "L0" => paths.memory.join("memory_management_sop.md"),
        "l1" | "L1" => paths.memory.join("global_mem_insight.txt"),
        "l2" | "L2" => paths.memory.join("global_mem.txt"),
        other => {
            if other.contains('/') || other.contains("..") {
                return Err(ApiError::bad_request("invalid memory layer"));
            }
            paths.memory.join(other)
        }
    };
    Ok(path)
}

// ===== New handlers (v0.3) =====

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: Option<String>,
    k: Option<usize>,
    category: Option<String>,
}

async fn list_skills(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<SearchQuery>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let cfg = load_skills_config(&state).await;
    let query = q.q.unwrap_or_default();
    let top_k = q.k.unwrap_or(20);
    let hits: Vec<UnifiedHit> =
        unified_search(&cfg, &state.paths, &query, q.category.as_deref(), top_k).await;
    Ok(Json(json!({"hits": hits})))
}

async fn load_skills_config(state: &AppState) -> SkillsConfig {
    let snapshot = state.config.read().await.clone();
    match snapshot.skills {
        Some(raw) => match serde_json::from_value::<SkillsConfig>(raw) {
            Ok(cfg) => cfg,
            Err(err) => {
                tracing::warn!("[skills] config parse failed: {err}; using defaults");
                SkillsConfig::default()
            }
        },
        None => SkillsConfig::default(),
    }
}

#[derive(Debug, Deserialize)]
struct MarketListQuery {
    source: Option<String>,
    level: Option<String>,
    q: Option<String>,
    page: Option<u32>,
}

async fn market_list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<MarketListQuery>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let cfg = load_skills_config(&state).await;
    let client = MarketClient::new(&cfg.market)
        .map_err(|e| ApiError::bad_request(format!("market client: {e}")))?;
    let page = client
        .list(&MarketQuery {
            source: q.source,
            level: q.level,
            q: q.q,
            page: q.page,
        })
        .await
        .map_err(|e| ApiError::bad_request(format!("market list: {e}")))?;
    Ok(Json(serde_json::to_value(&page).unwrap_or_default()))
}

async fn market_detail(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let cfg = load_skills_config(&state).await;
    let client = MarketClient::new(&cfg.market)
        .map_err(|e| ApiError::bad_request(format!("market client: {e}")))?;
    let detail = client
        .detail(&id)
        .await
        .map_err(|e| ApiError::bad_request(format!("market detail: {e}")))?;
    Ok(Json(serde_json::to_value(&detail).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct MarketInstallPayload {
    id: String,
    filename: Option<String>,
}

async fn market_install(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<MarketInstallPayload>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let cfg = load_skills_config(&state).await;
    let client = MarketClient::new(&cfg.market)
        .map_err(|e| ApiError::bad_request(format!("market client: {e}")))?;
    let markdown = client
        .download_markdown(&payload.id)
        .await
        .map_err(|e| ApiError::bad_request(format!("market download: {e}")))?;
    let dest_dir = skills_dir(&state.paths).join("imported");
    fs::create_dir_all(&dest_dir)?;
    let filename = payload
        .filename
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("sophub_{}.md", payload.id));
    let safe = filename.replace(['/', '\\'], "_");
    let target = dest_dir.join(&safe);
    fs::write(&target, &markdown)?;
    Ok(Json(json!({
        "ok": true,
        "path": target.to_string_lossy(),
        "bytes": markdown.len(),
    })))
}

async fn get_skill(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(key): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let root = skills_dir(&state.paths);
    for entry in walkdir_iter(&root) {
        if let Ok(s) = gars_skills::parse_skill_file(&entry)
            && s.key.eq_ignore_ascii_case(&key)
        {
            let body = fs::read_to_string(&entry).unwrap_or_default();
            return Ok(Json(json!({
                "skill": s,
                "body": body,
            })));
        }
    }
    Err(ApiError::not_found("skill not found"))
}

fn walkdir_iter(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root).into_iter().flatten() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.is_dir() {
            out.extend(walkdir_iter(&path));
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(path);
        }
    }
    out
}

#[derive(Debug, Deserialize)]
struct SkillImportPayload {
    url: Option<String>,
    path: Option<String>,
    content: Option<String>,
    filename: Option<String>,
}

async fn skills_import(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<SkillImportPayload>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let dest_dir = skills_dir(&state.paths).join("imported");
    fs::create_dir_all(&dest_dir)?;
    let (filename, body) = if let Some(content) = payload.content {
        (
            payload
                .filename
                .clone()
                .unwrap_or_else(|| format!("skill_{}.md", chrono::Utc::now().timestamp())),
            content,
        )
    } else if let Some(url) = payload.url {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|err| ApiError::internal(err.to_string()))?;
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|err| ApiError::bad_request(format!("fetch {url}: {err}")))?
            .error_for_status()
            .map_err(|err| ApiError::bad_request(err.to_string()))?;
        let text = resp
            .text()
            .await
            .map_err(|err| ApiError::internal(err.to_string()))?;
        let name = payload
            .filename
            .clone()
            .unwrap_or_else(|| url.split('/').next_back().unwrap_or("skill.md").to_string());
        (name, text)
    } else if let Some(path) = payload.path {
        let src = PathBuf::from(path);
        let body = fs::read_to_string(&src)?;
        (
            payload
                .filename
                .or_else(|| src.file_name().map(|s| s.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "skill.md".to_string()),
            body,
        )
    } else {
        return Err(ApiError::bad_request("provide url, path, or content"));
    };
    let dest = dest_dir.join(filename);
    fs::write(&dest, body)?;
    Ok(Json(json!({"status": "ok", "imported": dest})))
}

async fn list_agents(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let registry = AgentRegistry::load(&state.paths)?;
    Ok(Json(json!({"agents": registry.list()})))
}

async fn list_connectors(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let registry = state.connectors.lock().await;
    Ok(Json(json!({"connectors": registry.list()})))
}

async fn reload_connector(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let mut registry = state.connectors.lock().await;
    registry.shutdown_all();
    drop(registry);
    start_connectors(state.clone()).await?;
    Ok(Json(json!({"status": "ok", "reloaded": id})))
}

#[derive(Debug, Deserialize)]
struct ConnectorSendPayload {
    chat_id: String,
    text: String,
    #[serde(default)]
    markdown: bool,
    #[serde(default)]
    reply_to: Option<String>,
}

async fn send_connector(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
    Json(payload): Json<ConnectorSendPayload>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let registry = state.connectors.lock().await;
    let connector = registry
        .get(&id)
        .ok_or_else(|| ApiError::not_found("connector not running"))?;
    let target = gars_connectors::ChatTarget {
        chat_id: payload.chat_id,
        thread_id: None,
        reply_to: payload.reply_to,
    };
    let msg = gars_connectors::OutboundMessage {
        text: payload.text,
        markdown: payload.markdown,
        attachments: Vec::new(),
        extra: Default::default(),
    };
    connector
        .send(&target, &msg)
        .await
        .map_err(|err| ApiError::internal(err.to_string()))?;
    Ok(Json(json!({"status": "ok"})))
}

async fn feishu_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> ApiResult<Json<Value>> {
    let registry = state.connectors.lock().await;
    let connector_arc = registry
        .get("feishu")
        .ok_or_else(|| ApiError::not_found("feishu connector not running"))?;
    let value: Value =
        serde_json::from_str(&body).map_err(|err| ApiError::bad_request(err.to_string()))?;
    if let Some(challenge) = value.get("challenge").and_then(Value::as_str) {
        return Ok(Json(json!({"challenge": challenge})));
    }
    let webhook_headers = lower_header_map(&headers);
    let req = WebhookRequest {
        headers: &webhook_headers,
        body: body.as_bytes(),
    };
    if let Err(err) = connector_arc.verify_webhook(&req) {
        tracing::warn!("feishu webhook rejected: {err}");
        return Err(ApiError::unauthorized());
    }
    let chat_id = value
        .pointer("/event/message/chat_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let user_id = value
        .pointer("/event/sender/sender_id/user_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let content_str = value
        .pointer("/event/message/content")
        .and_then(Value::as_str)
        .unwrap_or("");
    let content_json: Value = serde_json::from_str(content_str).unwrap_or(Value::Null);
    let text = content_json
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let event = InboundEvent::Message {
        connector: "feishu".to_string(),
        chat: gars_connectors::ChatTarget {
            chat_id,
            thread_id: None,
            reply_to: None,
        },
        user: gars_connectors::UserInfo {
            id: user_id,
            name: String::new(),
            is_admin: false,
        },
        text,
        attachments: Vec::new(),
    };
    let _ = state.inbound_tx.send(event).await;
    Ok(Json(json!({"status": "ok"})))
}

fn lower_header_map(headers: &HeaderMap) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::with_capacity(headers.len());
    for (k, v) in headers.iter() {
        if let Ok(s) = v.to_str() {
            out.insert(k.as_str().to_ascii_lowercase(), s.to_string());
        }
    }
    out
}

async fn telegram_webhook(
    State(state): State<Arc<AppState>>,
    _headers: HeaderMap,
    Json(update): Json<Value>,
) -> ApiResult<Json<Value>> {
    // Telegram webhook is optional; long-poll is the default. We accept either.
    let message = update
        .get("message")
        .cloned()
        .or_else(|| update.get("edited_message").cloned())
        .unwrap_or(Value::Null);
    if message.is_null() {
        return Ok(Json(json!({"status": "ignored"})));
    }
    let chat_id = message
        .pointer("/chat/id")
        .and_then(|v| {
            v.as_i64()
                .map(|n| n.to_string())
                .or_else(|| v.as_str().map(str::to_string))
        })
        .unwrap_or_default();
    let user_id = message
        .pointer("/from/id")
        .and_then(|v| v.as_i64().map(|n| n.to_string()))
        .unwrap_or_default();
    let text = message
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let event = if let Some(rest) = text.strip_prefix('/') {
        let mut parts = rest.splitn(2, ' ');
        InboundEvent::Command {
            connector: "telegram".to_string(),
            chat: gars_connectors::ChatTarget {
                chat_id,
                thread_id: None,
                reply_to: None,
            },
            command: parts.next().unwrap_or("").to_string(),
            args: parts.next().unwrap_or("").to_string(),
            user: gars_connectors::UserInfo {
                id: user_id,
                name: String::new(),
                is_admin: false,
            },
        }
    } else {
        InboundEvent::Message {
            connector: "telegram".to_string(),
            chat: gars_connectors::ChatTarget {
                chat_id,
                thread_id: None,
                reply_to: None,
            },
            user: gars_connectors::UserInfo {
                id: user_id,
                name: String::new(),
                is_admin: false,
            },
            text,
            attachments: Vec::new(),
        }
    };
    let _ = state.inbound_tx.send(event).await;
    Ok(Json(json!({"status": "ok"})))
}

async fn archive_run(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let cfg = state.config.read().await.clone();
    let archive_cfg: ArchiveConfig = cfg
        .archive
        .clone()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let stats = run_idle_pass(&state.paths, &state.store, &archive_cfg)?;
    Ok(Json(json!({"status": "ok", "stats": stats})))
}

async fn archive_search(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<SearchQuery>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let query = q.q.unwrap_or_default();
    let k = q.k.unwrap_or(10);
    let hits = state.store.l4_search(&query, k)?;
    Ok(Json(json!({"hits": hits})))
}

async fn archive_get(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let entry = state
        .store
        .l4_get(&id)?
        .ok_or_else(|| ApiError::not_found("archive entry not found"))?;
    let content = fs::read_to_string(&entry.path).unwrap_or_default();
    Ok(Json(json!({"entry": entry, "content": content})))
}

#[derive(Debug, Deserialize)]
struct PlanCreatePayload {
    id: String,
    title: Option<String>,
    steps: Vec<String>,
}

async fn list_plans(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let plans = scan_plans(&state.paths);
    Ok(Json(json!({"plans": plans})))
}

async fn create_plan(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<PlanCreatePayload>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let dir = plans_dir(&state.paths).join(&payload.id);
    fs::create_dir_all(&dir)?;
    let plan_path = dir.join("plan.md");
    let mut plan =
        PlanFile::open_or_create(&plan_path, payload.title.as_deref().unwrap_or(&payload.id))?;
    plan.set_steps(&payload.steps)?;
    Ok(Json(json!({"status": "ok", "plan": plan})))
}

async fn get_plan(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let plan_path = plans_dir(&state.paths).join(&id).join("plan.md");
    let plan = PlanFile::load(&plan_path)?;
    Ok(Json(json!({"plan": plan})))
}

async fn delete_plan(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let dir = plans_dir(&state.paths).join(&id);
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    Ok(Json(json!({"status": "ok"})))
}

#[derive(Debug, Deserialize)]
struct MarkStepPayload {
    status: String,
    note: Option<String>,
}

// File-protocol shim: edits `~/.gars/plans/<id>/plan.md` in place. The
// canonical interface is the markdown file itself — agents and humans can
// rewrite it directly. This endpoint exists purely as a UI convenience.
async fn mark_plan_step(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath((id, idx)): AxumPath<(String, usize)>,
    Json(payload): Json<MarkStepPayload>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let plan_path = plans_dir(&state.paths).join(&id).join("plan.md");
    let mut plan = PlanFile::load(&plan_path)?;
    plan.mark(idx, &payload.status, payload.note)?;
    Ok(Json(json!({"status": "ok", "plan": plan})))
}

#[derive(Debug, Deserialize)]
struct SubagentSpawnPayload {
    agent: String,
    input: String,
    #[serde(default)]
    key_info: Option<String>,
    #[serde(default)]
    run: bool,
}

async fn list_subagents(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let runs = scan_runs(&state.paths.tasks);
    Ok(Json(json!({"runs": runs})))
}

async fn spawn_subagent(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<SubagentSpawnPayload>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let agents = AgentRegistry::load(&state.paths)?;
    let def = agents
        .get(&payload.agent)
        .ok_or_else(|| ApiError::bad_request(format!("unknown agent {}", payload.agent)))?
        .clone();
    let handle = allocate_workdir(&state.paths.tasks, &payload.agent)?;
    write_input(&handle, &payload.input, payload.key_info.as_deref())?;
    if payload.run {
        let state2 = state.clone();
        let handle2 = handle.clone();
        let def2 = def.clone();
        tokio::spawn(async move {
            let cfg = state2.config.read().await.clone();
            let reg = registry(&cfg);
            let _ =
                subagent_runner::run_subagent(handle2, def2, cfg, state2.paths.clone(), reg).await;
        });
    }
    Ok(Json(
        json!({"run_id": handle.run_id, "agent": handle.agent, "workdir": handle.workdir}),
    ))
}

async fn get_subagent(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(run_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let run = load_run(&state.paths.tasks, &run_id)
        .ok_or_else(|| ApiError::not_found("subagent not found"))?;
    Ok(Json(json!({"run": run})))
}

async fn run_subagent_route(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(run_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let run = load_run(&state.paths.tasks, &run_id)
        .ok_or_else(|| ApiError::not_found("subagent not found"))?;
    let agents = AgentRegistry::load(&state.paths)?;
    let def = agents
        .get(&run.agent)
        .ok_or_else(|| ApiError::bad_request("agent definition missing"))?
        .clone();
    let handle = SubagentHandle {
        run_id: run.run_id.clone(),
        agent: run.agent.clone(),
        workdir: run.workdir.clone(),
    };
    let cfg = state.config.read().await.clone();
    let reg = registry(&cfg);
    let reply = subagent_runner::run_subagent(handle, def, cfg, state.paths.clone(), reg)
        .await
        .map_err(|err| ApiError::internal(err.to_string()))?;
    Ok(Json(json!({"status": "ok", "reply": reply})))
}

#[derive(Debug, Deserialize)]
struct InterveneBody {
    message: String,
}

// File-protocol shim: writes `_intervene` in the subagent's workdir. The
// canonical interface IS that file; this endpoint exists purely so the
// Web UI doesn't have to do filesystem writes from the browser.
async fn intervene_subagent(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(run_id): AxumPath<String>,
    Json(body): Json<InterveneBody>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let run = load_run(&state.paths.tasks, &run_id)
        .ok_or_else(|| ApiError::not_found("subagent not found"))?;
    let handle = SubagentHandle {
        run_id: run.run_id,
        agent: run.agent,
        workdir: run.workdir,
    };
    gars_core::intervene(&handle, &body.message)?;
    Ok(Json(json!({"status": "ok"})))
}

// File-protocol shim: writes `_stop` in the subagent's workdir. Same
// "canonical interface IS the file" caveat as `intervene_subagent`.
async fn stop_subagent(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(run_id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let run = load_run(&state.paths.tasks, &run_id)
        .ok_or_else(|| ApiError::not_found("subagent not found"))?;
    let handle = SubagentHandle {
        run_id: run.run_id,
        agent: run.agent,
        workdir: run.workdir,
    };
    gars_core::stop(&handle, "stop requested")?;
    Ok(Json(json!({"status": "ok"})))
}

// ===== Schedules =====

async fn list_schedules(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let tasks = scheduler::list_tasks(&state.paths);
    let with_health: Vec<Value> = tasks
        .into_iter()
        .map(|t| {
            let st = scheduler::load_state(&state.paths, &t.id);
            let h = scheduler::health(&t, &st);
            json!({"task": t, "state": st, "health": h})
        })
        .collect();
    Ok(Json(json!({"schedules": with_health})))
}

async fn upsert_schedule(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(task): Json<scheduler::ScheduledTask>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    // Validate cron expression early.
    if let Err(err) = <cron::Schedule as std::str::FromStr>::from_str(&task.cron) {
        return Err(ApiError::bad_request(format!("invalid cron: {err}")));
    }
    let path = scheduler::save_task(&state.paths, &task)?;
    Ok(Json(json!({"status": "ok", "path": path})))
}

async fn get_schedule(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let path = scheduler::schedules_dir(&state.paths).join(format!("{id}.toml"));
    if !path.exists() {
        return Err(ApiError::not_found("schedule not found"));
    }
    let task = scheduler::load_task(&path)?;
    let st = scheduler::load_state(&state.paths, &task.id);
    let h = scheduler::health(&task, &st);
    Ok(Json(json!({"task": task, "state": st, "health": h})))
}

async fn delete_schedule(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    scheduler::delete_task(&state.paths, &id)?;
    Ok(Json(json!({"status": "ok"})))
}

async fn trigger_schedule(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let path = scheduler::schedules_dir(&state.paths).join(format!("{id}.toml"));
    if !path.exists() {
        return Err(ApiError::not_found("schedule not found"));
    }
    let task = scheduler::load_task(&path)?;
    let state2 = state.clone();
    tokio::spawn(async move {
        let mut st = scheduler::load_state(&state2.paths, &task.id);
        let _ = scheduler::run_task_now(&state2, &task, &mut st).await;
    });
    Ok(Json(json!({"status": "triggered", "id": id})))
}

async fn schedule_health(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let path = scheduler::schedules_dir(&state.paths).join(format!("{id}.toml"));
    if !path.exists() {
        return Err(ApiError::not_found("schedule not found"));
    }
    let task = scheduler::load_task(&path)?;
    let st = scheduler::load_state(&state.paths, &task.id);
    let h = scheduler::health(&task, &st);
    Ok(Json(json!({"health": h})))
}

// ===== Extension =====

async fn extension_state(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    authorize(&state, &headers)?;
    let list = state.extensions.list().await;
    Ok(Json(json!({"extensions": list})))
}

async fn extension_ws(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<axum::response::Response> {
    authorize(&state, &headers)?;
    Ok(ws.on_upgrade(move |socket| handle_extension_socket(state, socket)))
}

async fn handle_extension_socket(state: Arc<AppState>, mut socket: WebSocket) {
    // Expect a hello frame as the first message.
    let hello: ExtensionHello = match socket.recv().await {
        Some(Ok(WsMessage::Text(text))) => serde_json::from_str(&text).unwrap_or(ExtensionHello {
            browser: None,
            version: None,
        }),
        _ => ExtensionHello {
            browser: None,
            version: None,
        },
    };
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<String>(64);
    let handle = state.extensions.attach(hello, out_tx).await;
    let _ = state.event_bus.send(BusEvent {
        topic: "extension".to_string(),
        payload: json!({"event": "connected", "id": handle.id, "browser": handle.browser}),
    });
    let handle_for_send = handle.clone();
    loop {
        tokio::select! {
            outgoing = out_rx.recv() => {
                let Some(message) = outgoing else { break; };
                if socket.send(WsMessage::Text(message.into())).await.is_err() {
                    break;
                }
            }
            inbound = socket.recv() => {
                match inbound {
                    Some(Ok(WsMessage::Text(text))) => {
                        let text_string = text.to_string();
                        if !handle_for_send.handle_inbound(&text_string).await {
                            tracing::debug!("ext message did not match pending: {}", text_string);
                        }
                    }
                    Some(Ok(WsMessage::Binary(_))) => {}
                    Some(Ok(WsMessage::Ping(p))) => {
                        let _ = socket.send(WsMessage::Pong(p)).await;
                    }
                    _ => break,
                }
            }
        }
    }
    let id = handle.id.clone();
    state.extensions.detach(&id).await;
    let _ = state.event_bus.send(BusEvent {
        topic: "extension".to_string(),
        payload: json!({"event": "disconnected", "id": id}),
    });
}

async fn events_ws(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<axum::response::Response> {
    authorize(&state, &headers)?;
    let rx = state.event_bus.subscribe();
    Ok(ws.on_upgrade(move |socket| handle_events_socket(socket, rx)))
}

async fn handle_events_socket(mut socket: WebSocket, mut rx: broadcast::Receiver<BusEvent>) {
    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(event) => {
                        let line = serde_json::to_string(&event).unwrap_or_default();
                        if socket.send(WsMessage::Text(line.into())).await.is_err() {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => return,
                }
            }
            client_msg = socket.recv() => {
                if client_msg.is_none() {
                    return;
                }
            }
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "unauthorized".to_string(),
        }
    }
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }
    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (self.status, Json(json!({"error": self.message}))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(value: anyhow::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: value.to_string(),
        }
    }
}

impl From<std::io::Error> for ApiError {
    fn from(value: std::io::Error) -> Self {
        ApiError::internal(value.to_string())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(value: serde_json::Error) -> Self {
        ApiError::internal(value.to_string())
    }
}

impl From<toml::de::Error> for ApiError {
    fn from(value: toml::de::Error) -> Self {
        ApiError::bad_request(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_defaults_are_local() {
        let cfg = RootConfig::default();
        assert_eq!(server_bind(&cfg), "127.0.0.1");
        assert_eq!(server_port(&cfg), 9221);
    }
}
