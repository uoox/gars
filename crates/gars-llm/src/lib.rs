use std::{collections::HashMap, time::Duration};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use futures_util::StreamExt;
use gars_core::{
    ChatMessage, ChatRequest, ChatResponse, DeltaSink, LlmClient, Role, ToolCall, ToolSpec,
    parse_text_tool_calls,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::time::sleep;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct LlmSection {
    #[serde(default)]
    pub sessions: Vec<SessionConfig>,
    #[serde(default)]
    pub mixins: Vec<MixinConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionConfig {
    pub name: String,
    pub provider: String,
    pub api_base: String,
    pub model: String,
    pub api_key_env: Option<String>,
    pub api_key: Option<String>,
    #[serde(default = "default_true")]
    pub native_tools: bool,
    #[serde(default)]
    pub text_protocol: bool,
    #[serde(default)]
    pub stream: bool,
    pub max_tokens: Option<u64>,
    pub temperature: Option<f64>,
    pub reasoning_effort: Option<String>,
    pub thinking_type: Option<String>,
    pub thinking_budget_tokens: Option<u64>,
    pub user_agent: Option<String>,
    #[serde(default)]
    pub fake_cc_system_prompt: bool,
    pub connect_timeout_secs: Option<u64>,
    pub read_timeout_secs: Option<u64>,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MixinConfig {
    pub name: String,
    #[serde(default)]
    pub sessions: Vec<String>,
    #[serde(default = "default_retries")]
    pub max_retries: usize,
    #[serde(default = "default_base_delay")]
    pub base_delay_ms: u64,
    #[serde(default = "default_spring_back")]
    pub spring_back_secs: u64,
}

fn default_retries() -> usize {
    3
}

fn default_base_delay() -> u64 {
    800
}

fn default_spring_back() -> u64 {
    300
}

pub fn build_client(section: &LlmSection, requested: &str) -> Result<Box<dyn LlmClient>> {
    if let Some(mixin) = section.mixins.iter().find(|m| m.name == requested) {
        let mut clients = Vec::new();
        for name in &mixin.sessions {
            let session = section
                .sessions
                .iter()
                .find(|session| &session.name == name)
                .with_context(|| {
                    format!("mixin {} references missing session {name}", mixin.name)
                })?;
            clients.push(build_session_client(session)?);
        }
        return Ok(Box::new(MixinClient {
            name: mixin.name.clone(),
            clients,
            max_retries: mixin.max_retries,
            base_delay_ms: mixin.base_delay_ms,
        }));
    }
    let session = section
        .sessions
        .iter()
        .find(|session| session.name == requested)
        .with_context(|| format!("LLM session or mixin not found: {requested}"))?;
    build_session_client(session)
}

fn build_session_client(config: &SessionConfig) -> Result<Box<dyn LlmClient>> {
    let api_key = match (&config.api_key, &config.api_key_env) {
        (Some(key), _) => key.clone(),
        (None, Some(env_name)) => std::env::var(env_name)
            .with_context(|| format!("environment variable {env_name} is not set"))?,
        (None, None) => String::new(),
    };
    let default_ua = if config.fake_cc_system_prompt {
        "claude-cli/2.1.113 (external, cli)".to_string()
    } else {
        format!("gars/{}", env!("CARGO_PKG_VERSION"))
    };
    let http = Client::builder()
        .connect_timeout(Duration::from_secs(
            config.connect_timeout_secs.unwrap_or(10),
        ))
        .timeout(Duration::from_secs(config.read_timeout_secs.unwrap_or(240)))
        .user_agent(config.user_agent.clone().unwrap_or(default_ua))
        .build()?;
    let provider = config.provider.to_lowercase().replace('-', "_");
    if provider.contains("anthropic") || provider.contains("claude") {
        Ok(Box::new(AnthropicClient {
            config: config.clone(),
            api_key,
            http,
            session_id: Uuid::new_v4().to_string(),
        }))
    } else {
        Ok(Box::new(OpenAiCompatibleClient {
            config: config.clone(),
            api_key,
            http,
        }))
    }
}

struct MixinClient {
    name: String,
    clients: Vec<Box<dyn LlmClient>>,
    max_retries: usize,
    base_delay_ms: u64,
}

#[async_trait]
impl LlmClient for MixinClient {
    fn name(&self) -> &str {
        &self.name
    }

    async fn chat(&mut self, request: ChatRequest) -> Result<ChatResponse> {
        if self.clients.is_empty() {
            return Err(anyhow!("mixin {} has no sessions", self.name));
        }
        let attempts = self.max_retries.max(1);
        let mut last_error = None;
        for attempt in 0..attempts {
            let idx = attempt % self.clients.len();
            match self.clients[idx].chat(request.clone()).await {
                Ok(response) => return Ok(response),
                Err(err) => {
                    tracing::warn!("LLM session {} failed: {err:#}", self.clients[idx].name());
                    last_error = Some(err);
                    if attempt + 1 < attempts {
                        let round = (attempt / self.clients.len()) as u32;
                        let delay = self
                            .base_delay_ms
                            .saturating_mul(2u64.saturating_pow(round));
                        sleep(Duration::from_millis(delay.min(30_000))).await;
                    }
                }
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow!("all mixin sessions failed")))
    }

    async fn chat_stream(
        &mut self,
        request: ChatRequest,
        on_delta: DeltaSink<'_>,
    ) -> Result<ChatResponse> {
        // Streaming should only be attempted against the primary session
        // because retry-streams would replay deltas. On failure we fall back
        // to non-streaming retry semantics.
        if self.clients.is_empty() {
            return Err(anyhow!("mixin {} has no sessions", self.name));
        }
        if let Some(client) = self.clients.first_mut()
            && let Ok(response) = client.chat_stream(request.clone(), on_delta).await
        {
            return Ok(response);
        }
        self.chat(request).await
    }
}

struct OpenAiCompatibleClient {
    config: SessionConfig,
    api_key: String,
    http: Client,
}

#[derive(Default)]
struct OpenAiToolCallAcc {
    id: String,
    name: String,
    args: String,
}

#[async_trait]
impl LlmClient for OpenAiCompatibleClient {
    fn name(&self) -> &str {
        &self.config.name
    }

    async fn chat_stream(
        &mut self,
        request: ChatRequest,
        on_delta: DeltaSink<'_>,
    ) -> Result<ChatResponse> {
        if self.config.text_protocol || !self.config.native_tools {
            // Text protocol streaming is not worth the complexity since the
            // entire content is parsed for tool_use blocks afterwards.
            return self.chat(request).await;
        }
        self.openai_stream(request, on_delta).await
    }

    async fn chat(&mut self, request: ChatRequest) -> Result<ChatResponse> {
        if self.config.text_protocol || !self.config.native_tools {
            return self.chat_text_protocol(request).await;
        }

        let mut messages = vec![json!({"role": "system", "content": request.system})];
        messages.extend(request.messages.iter().map(openai_message));
        let mut payload = json!({
            "model": self.config.model,
            "messages": messages,
            "stream": false,
        });
        if !request.tools.is_empty() {
            payload["tools"] = serde_json::to_value(&request.tools)?;
        }
        if let Some(max_tokens) = self.config.max_tokens {
            payload["max_tokens"] = json!(max_tokens);
        }
        if let Some(temperature) = self.config.temperature {
            payload["temperature"] = json!(normalize_temperature(&self.config.model, temperature));
        }
        if let Some(reasoning_effort) = &self.config.reasoning_effort {
            payload["reasoning_effort"] = json!(reasoning_effort);
        }

        let raw = self
            .post_json(auto_openai_chat_url(&self.config.api_base), payload)
            .await?;
        parse_openai_response(raw)
    }
}

impl OpenAiCompatibleClient {
    async fn openai_stream(
        &self,
        request: ChatRequest,
        on_delta: DeltaSink<'_>,
    ) -> Result<ChatResponse> {
        let mut messages = vec![json!({"role": "system", "content": request.system})];
        messages.extend(request.messages.iter().map(openai_message));
        let mut payload = json!({
            "model": self.config.model,
            "messages": messages,
            "stream": true,
        });
        if !request.tools.is_empty() {
            payload["tools"] = serde_json::to_value(&request.tools)?;
        }
        if let Some(max_tokens) = self.config.max_tokens {
            payload["max_tokens"] = json!(max_tokens);
        }
        if let Some(temperature) = self.config.temperature {
            payload["temperature"] = json!(normalize_temperature(&self.config.model, temperature));
        }
        if let Some(reasoning_effort) = &self.config.reasoning_effort {
            payload["reasoning_effort"] = json!(reasoning_effort);
        }

        let url = auto_openai_chat_url(&self.config.api_base);
        let mut req = self.http.post(&url).json(&payload);
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("POST {url}"))?
            .error_for_status()?;

        let mut content = String::new();
        let mut tool_calls_acc: Vec<OpenAiToolCallAcc> = Vec::new();
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(idx) = buf.find('\n') {
                let line = buf[..idx].to_string();
                buf.drain(..=idx);
                let line = line.trim();
                let Some(payload) = line.strip_prefix("data:") else {
                    continue;
                };
                let payload = payload.trim();
                if payload == "[DONE]" {
                    break;
                }
                let Ok(value): std::result::Result<Value, _> = serde_json::from_str(payload) else {
                    continue;
                };
                if let Some(choice) = value.pointer("/choices/0") {
                    if let Some(text) = choice.pointer("/delta/content").and_then(Value::as_str)
                        && !text.is_empty()
                    {
                        content.push_str(text);
                        on_delta(text);
                    }
                    if let Some(arr) = choice
                        .pointer("/delta/tool_calls")
                        .and_then(Value::as_array)
                    {
                        for tc in arr {
                            let index =
                                tc.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                            while tool_calls_acc.len() <= index {
                                tool_calls_acc.push(OpenAiToolCallAcc::default());
                            }
                            let entry = &mut tool_calls_acc[index];
                            if let Some(id) = tc.get("id").and_then(Value::as_str) {
                                entry.id = id.to_string();
                            }
                            if let Some(name) = tc.pointer("/function/name").and_then(Value::as_str)
                            {
                                entry.name = name.to_string();
                            }
                            if let Some(args) =
                                tc.pointer("/function/arguments").and_then(Value::as_str)
                            {
                                entry.args.push_str(args);
                            }
                        }
                    }
                }
            }
        }
        let tool_calls: Vec<ToolCall> = tool_calls_acc
            .into_iter()
            .filter(|t| !t.name.is_empty())
            .map(|t| ToolCall {
                id: t.id,
                name: t.name,
                arguments: serde_json::from_str(&t.args).unwrap_or(Value::Null),
            })
            .collect();
        Ok(ChatResponse {
            content,
            tool_calls,
            raw: Value::Null,
            ..Default::default()
        })
    }

    async fn chat_text_protocol(&self, request: ChatRequest) -> Result<ChatResponse> {
        let prompt = build_text_protocol_prompt(&request);
        let messages = vec![json!({"role": "user", "content": prompt})];
        let mut payload = json!({
            "model": self.config.model,
            "messages": messages,
            "stream": false,
        });
        if let Some(max_tokens) = self.config.max_tokens {
            payload["max_tokens"] = json!(max_tokens);
        }
        if let Some(temperature) = self.config.temperature {
            payload["temperature"] = json!(normalize_temperature(&self.config.model, temperature));
        }
        let raw = self
            .post_json(auto_openai_chat_url(&self.config.api_base), payload)
            .await?;
        let mut response = parse_openai_response(raw)?;
        let (calls, cleaned) = parse_text_tool_calls(&response.content);
        if !calls.is_empty() {
            response.tool_calls = calls;
            response.content = cleaned;
        }
        Ok(response)
    }

    async fn post_json(&self, url: String, payload: Value) -> Result<Value> {
        let mut req = self.http.post(&url).json(&payload);
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        Ok(req
            .send()
            .await
            .with_context(|| format!("POST {url}"))?
            .error_for_status()?
            .json::<Value>()
            .await?)
    }
}

struct AnthropicClient {
    config: SessionConfig,
    api_key: String,
    http: Client,
    session_id: String,
}

#[derive(Default)]
struct AnthropicToolAcc {
    id: String,
    name: String,
    args: String,
}

#[async_trait]
impl LlmClient for AnthropicClient {
    fn name(&self) -> &str {
        &self.config.name
    }

    async fn chat_stream(
        &mut self,
        request: ChatRequest,
        on_delta: DeltaSink<'_>,
    ) -> Result<ChatResponse> {
        if self.config.text_protocol || !self.config.native_tools {
            return self.chat(request).await;
        }
        self.anthropic_stream(request, on_delta).await
    }

    async fn chat(&mut self, request: ChatRequest) -> Result<ChatResponse> {
        let mut messages: Vec<Value> = request.messages.iter().map(anthropic_message).collect();
        if self.config.text_protocol || !self.config.native_tools {
            messages = vec![
                json!({"role": "user", "content": [{"type": "text", "text": build_text_protocol_prompt(&request)}]}),
            ];
        }
        let mut payload = json!({
            "model": self.config.model.replace("[1m]", "").replace("[1M]", ""),
            "messages": messages,
            "max_tokens": self.config.max_tokens.unwrap_or(8192),
            "stream": false,
            "metadata": { "user_id": self.session_id },
        });
        if !request.tools.is_empty() && self.config.native_tools && !self.config.text_protocol {
            payload["tools"] = json!(
                request
                    .tools
                    .iter()
                    .map(openai_tool_to_anthropic)
                    .collect::<Vec<_>>()
            );
        }
        if let Some(temperature) = self.config.temperature {
            payload["temperature"] = json!(temperature);
        }
        if let Some(thinking_type) = &self.config.thinking_type {
            let mut thinking = json!({"type": thinking_type});
            if thinking_type == "enabled" {
                thinking["budget_tokens"] =
                    json!(self.config.thinking_budget_tokens.unwrap_or(4096));
            }
            payload["thinking"] = thinking;
        }
        if let Some(reasoning_effort) = &self.config.reasoning_effort
            && let Some(effort) = map_anthropic_effort(reasoning_effort)
        {
            payload["output_config"] = json!({"effort": effort});
        }
        if self.config.fake_cc_system_prompt {
            if let Some(first) = payload["messages"]
                .as_array_mut()
                .and_then(|m| m.first_mut())
                && let Some(content) = first.get_mut("content").and_then(Value::as_array_mut)
            {
                content.insert(0, json!({"type": "text", "text": request.system}));
            }
            payload["system"] = json!([{"type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude."}]);
        } else {
            payload["system"] = json!(request.system);
        }
        let raw = self
            .post_json(auto_anthropic_url(&self.config.api_base), payload)
            .await?;
        let mut response = parse_anthropic_response(raw)?;
        if (self.config.text_protocol || !self.config.native_tools)
            && response.tool_calls.is_empty()
        {
            let (calls, cleaned) = parse_text_tool_calls(&response.content);
            response.tool_calls = calls;
            response.content = cleaned;
        }
        Ok(response)
    }
}

impl AnthropicClient {
    async fn anthropic_stream(
        &self,
        request: ChatRequest,
        on_delta: DeltaSink<'_>,
    ) -> Result<ChatResponse> {
        let messages: Vec<Value> = request.messages.iter().map(anthropic_message).collect();
        let mut payload = json!({
            "model": self.config.model.replace("[1m]", "").replace("[1M]", ""),
            "messages": messages,
            "max_tokens": self.config.max_tokens.unwrap_or(8192),
            "stream": true,
            "system": request.system,
            "metadata": { "user_id": self.session_id },
        });
        if !request.tools.is_empty() {
            payload["tools"] = json!(
                request
                    .tools
                    .iter()
                    .map(openai_tool_to_anthropic)
                    .collect::<Vec<_>>()
            );
        }
        if let Some(temperature) = self.config.temperature {
            payload["temperature"] = json!(temperature);
        }

        let url = auto_anthropic_url(&self.config.api_base);
        let mut req = self
            .http
            .post(&url)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", anthropic_beta_header(&self.config.model))
            .json(&payload);
        if self.api_key.starts_with("sk-ant-") {
            req = req.header("x-api-key", &self.api_key);
        } else if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("POST {url}"))?
            .error_for_status()?;

        let mut content = String::new();
        let mut thinking = String::new();
        let mut tool_calls_acc: Vec<AnthropicToolAcc> = Vec::new();
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(idx) = buf.find('\n') {
                let line = buf[..idx].to_string();
                buf.drain(..=idx);
                let line = line.trim();
                let Some(payload) = line.strip_prefix("data:") else {
                    continue;
                };
                let payload = payload.trim();
                if payload.is_empty() {
                    continue;
                }
                let Ok(event): std::result::Result<Value, _> = serde_json::from_str(payload) else {
                    continue;
                };
                match event.get("type").and_then(Value::as_str) {
                    Some("content_block_start") => {
                        let block = event.get("content_block");
                        let idx_block =
                            event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                        if let Some(b) = block
                            && b.get("type").and_then(Value::as_str) == Some("tool_use")
                        {
                            while tool_calls_acc.len() <= idx_block {
                                tool_calls_acc.push(AnthropicToolAcc::default());
                            }
                            let entry = &mut tool_calls_acc[idx_block];
                            entry.id = b
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            entry.name = b
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                        }
                    }
                    Some("content_block_delta") => {
                        let delta = event.get("delta");
                        if let Some(d) = delta {
                            match d.get("type").and_then(Value::as_str) {
                                Some("text_delta") => {
                                    if let Some(t) = d.get("text").and_then(Value::as_str)
                                        && !t.is_empty()
                                    {
                                        content.push_str(t);
                                        on_delta(t);
                                    }
                                }
                                Some("thinking_delta") => {
                                    if let Some(t) = d.get("thinking").and_then(Value::as_str) {
                                        thinking.push_str(t);
                                    }
                                }
                                Some("input_json_delta") => {
                                    let idx_block =
                                        event.get("index").and_then(Value::as_u64).unwrap_or(0)
                                            as usize;
                                    while tool_calls_acc.len() <= idx_block {
                                        tool_calls_acc.push(AnthropicToolAcc::default());
                                    }
                                    if let Some(t) = d.get("partial_json").and_then(Value::as_str) {
                                        tool_calls_acc[idx_block].args.push_str(t);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Some("message_stop") => break,
                    _ => {}
                }
            }
        }
        let tool_calls: Vec<ToolCall> = tool_calls_acc
            .into_iter()
            .filter(|t| !t.name.is_empty())
            .map(|t| ToolCall {
                id: t.id,
                name: t.name,
                arguments: serde_json::from_str(&t.args).unwrap_or(Value::Null),
            })
            .collect();
        Ok(ChatResponse {
            content,
            thinking,
            tool_calls,
            raw: Value::Null,
        })
    }

    async fn post_json(&self, url: String, payload: Value) -> Result<Value> {
        let mut req = self
            .http
            .post(&url)
            .header("content-type", "application/json")
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", anthropic_beta_header(&self.config.model))
            .json(&payload);
        if self.api_key.starts_with("sk-ant-") {
            req = req.header("x-api-key", &self.api_key);
        } else if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        Ok(req
            .send()
            .await
            .with_context(|| format!("POST {url}"))?
            .error_for_status()?
            .json::<Value>()
            .await?)
    }
}

fn openai_message(message: &ChatMessage) -> Value {
    json!({
        "role": message.role.as_str(),
        "content": message.content,
    })
}

fn anthropic_message(message: &ChatMessage) -> Value {
    let role = match message.role {
        Role::Assistant => "assistant",
        _ => "user",
    };
    json!({
        "role": role,
        "content": [{"type": "text", "text": message.content}],
    })
}

fn auto_openai_chat_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/chat/completions")
    } else {
        format!("{base}/v1/chat/completions")
    }
}

/// Build the `anthropic-beta` request header. Adds the 1M-context beta when
/// the model name carries the `[1m]` / `[1M]` suffix used by mykey_template.
fn anthropic_beta_header(model: &str) -> String {
    let mut betas = vec![
        "claude-code-20250219",
        "interleaved-thinking-2025-05-14",
        "prompt-caching-2024-07-31",
    ];
    if model.contains("[1m]") || model.contains("[1M]") {
        betas.push("context-1m-2025-08-07");
    }
    betas.join(",")
}

fn auto_anthropic_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/messages") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/messages")
    } else {
        format!("{base}/v1/messages")
    }
}

fn normalize_temperature(model: &str, temperature: f64) -> f64 {
    let model = model.to_lowercase();
    if model.contains("kimi") || model.contains("moonshot") {
        1.0
    } else if model.contains("minimax") {
        temperature.clamp(0.01, 1.0)
    } else {
        temperature
    }
}

fn map_anthropic_effort(effort: &str) -> Option<&'static str> {
    match effort {
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" => Some("max"),
        _ => None,
    }
}

fn openai_tool_to_anthropic(tool: &ToolSpec) -> Value {
    json!({
        "name": tool.function.name,
        "description": tool.function.description,
        "input_schema": tool.function.parameters,
    })
}

fn parse_openai_response(raw: Value) -> Result<ChatResponse> {
    let message = raw
        .pointer("/choices/0/message")
        .ok_or_else(|| anyhow!("OpenAI-compatible response missing choices[0].message"))?;
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .filter_map(|call| {
                    let function = call.get("function")?;
                    let name = function.get("name")?.as_str()?.to_string();
                    let args = function
                        .get("arguments")
                        .and_then(Value::as_str)
                        .and_then(|s| serde_json::from_str::<Value>(s).ok())
                        .unwrap_or(Value::Null);
                    Some(ToolCall {
                        id: call
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        name,
                        arguments: args,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(ChatResponse {
        content,
        tool_calls,
        raw,
        ..Default::default()
    })
}

fn parse_anthropic_response(raw: Value) -> Result<ChatResponse> {
    let mut text = Vec::new();
    let mut thinking = Vec::new();
    let mut tool_calls = Vec::new();
    for block in raw
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Anthropic response missing content"))?
    {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => text.push(
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ),
            Some("thinking") => thinking.push(
                block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ),
            Some("tool_use") => {
                tool_calls.push(ToolCall {
                    id: block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    name: block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("bad_json")
                        .to_string(),
                    arguments: block.get("input").cloned().unwrap_or(Value::Null),
                });
            }
            _ => {}
        }
    }
    Ok(ChatResponse {
        content: text.join("\n").trim().to_string(),
        thinking: thinking.join("\n").trim().to_string(),
        tool_calls,
        raw,
    })
}

fn build_text_protocol_prompt(request: &ChatRequest) -> String {
    let tools_json = serde_json::to_string(&request.tools).unwrap_or_else(|_| "[]".to_string());
    let mut out = String::new();
    out.push_str(&request.system);
    out.push_str(
        r#"

### Interaction Protocol
1. Think inside <thinking> tags when useful.
2. Put a one-line physical snapshot inside <summary>.
3. To call tools, emit one or more blocks exactly as:
<tool_use>{"name":"tool_name","arguments":{...}}</tool_use>

### Tools (mounted, always in effect)
"#,
    );
    out.push_str(&tools_json);
    out.push_str("\n\n");
    for message in &request.messages {
        let role = match message.role {
            Role::Assistant => "ASSISTANT",
            _ => "USER",
        };
        out.push_str(&format!("=== {role} ===\n{}\n", message.content));
    }
    out.push_str("=== ASSISTANT ===\n");
    out
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct RootConfig {
    pub language: Option<String>,
    pub default_llm: Option<String>,
    pub context_char_budget: Option<usize>,
    #[serde(default)]
    pub llm: LlmSection,
    #[serde(default)]
    pub server: HashMap<String, Value>,
    #[serde(default)]
    pub browser: HashMap<String, Value>,
    #[serde(default)]
    pub vision: Option<Value>,
    #[serde(default)]
    pub skills: Option<Value>,
    #[serde(default)]
    pub archive: Option<Value>,
    #[serde(default)]
    pub connectors: Option<Value>,
}

pub fn parse_root_config(toml_str: &str) -> Result<RootConfig> {
    Ok(toml::from_str(toml_str)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_url_rules() {
        assert_eq!(
            auto_openai_chat_url("https://x/v1"),
            "https://x/v1/chat/completions"
        );
        assert_eq!(auto_anthropic_url("https://a"), "https://a/v1/messages");
    }

    #[test]
    fn parses_config() {
        let cfg = parse_root_config(
            r#"
default_llm = "primary"
[[llm.sessions]]
name = "primary"
provider = "openai_compatible"
api_base = "http://localhost:9999/v1"
model = "x"
"#,
        )
        .unwrap();
        assert_eq!(cfg.llm.sessions[0].name, "primary");
    }
}
