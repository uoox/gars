use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

impl ToolCall {
    pub fn new(name: impl Into<String>, arguments: Value) -> Self {
        Self {
            id: format!("toolu_{}", Uuid::new_v4().simple()),
            name: name.into(),
            arguments,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolSpec {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolFunction,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl ToolSpec {
    pub fn function(name: &str, description: &str, parameters: Value) -> Self {
        Self {
            kind: "function".to_string(),
            function: ToolFunction {
                name: name.to_string(),
                description: description.to_string(),
                parameters,
            },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ChatResponse {
    pub content: String,
    pub thinking: String,
    pub tool_calls: Vec<ToolCall>,
    pub raw: Value,
}

/// Callback handed to `chat_stream` to consume per-delta text chunks.
pub type DeltaSink<'a> = &'a mut (dyn FnMut(&str) + Send);

#[async_trait]
pub trait LlmClient: Send + Sync {
    fn name(&self) -> &str;
    async fn chat(&mut self, request: ChatRequest) -> Result<ChatResponse>;

    /// Streaming variant: emits content deltas via `on_delta` as they arrive,
    /// returning the final assembled `ChatResponse`. Default implementation
    /// falls back to the non-streaming `chat` and emits the full content as
    /// a single delta, so callers can always rely on this method existing.
    async fn chat_stream(
        &mut self,
        request: ChatRequest,
        on_delta: DeltaSink<'_>,
    ) -> Result<ChatResponse> {
        let response = self.chat(request).await?;
        if !response.content.is_empty() {
            on_delta(&response.content);
        }
        Ok(response)
    }
}

#[async_trait]
impl<T: LlmClient + ?Sized> LlmClient for Box<T> {
    fn name(&self) -> &str {
        (**self).name()
    }

    async fn chat(&mut self, request: ChatRequest) -> Result<ChatResponse> {
        (**self).chat(request).await
    }

    async fn chat_stream(
        &mut self,
        request: ChatRequest,
        on_delta: DeltaSink<'_>,
    ) -> Result<ChatResponse> {
        (**self).chat_stream(request, on_delta).await
    }
}

/// Expand `{{file:path:start:end}}` and `{{file:path}}` references and
/// `{{glob:pattern}}` for use inside tool inputs. Relative paths resolve
/// against `base`.
pub fn expand_file_refs(content: &str, base: &std::path::Path) -> anyhow::Result<String> {
    let mut out = String::new();
    let mut rest = content;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str("{{");
            rest = after;
            continue;
        };
        let body = after[..end].trim();
        rest = &after[end + 2..];
        if let Some(spec) = body.strip_prefix("file:") {
            out.push_str(&expand_file_spec(spec, base)?);
        } else if let Some(pattern) = body.strip_prefix("glob:") {
            out.push_str(&expand_glob(pattern.trim(), base)?);
        } else {
            out.push_str("{{");
            out.push_str(body);
            out.push_str("}}");
        }
    }
    out.push_str(rest);
    Ok(out)
}

fn expand_file_spec(spec: &str, base: &std::path::Path) -> anyhow::Result<String> {
    use std::path::PathBuf;
    let parts: Vec<&str> = spec.rsplitn(3, ':').collect();
    let path_part;
    let mut start_line: Option<usize> = None;
    let mut end_line: Option<usize> = None;
    if parts.len() == 3 {
        end_line = Some(parts[0].parse()?);
        start_line = Some(parts[1].parse()?);
        path_part = parts[2];
    } else if parts.len() == 1 {
        path_part = parts[0];
    } else {
        anyhow::bail!("file ref must be {{file:path}} or {{file:path:start:end}}");
    }
    let path = PathBuf::from(path_part);
    let path = if path.is_absolute() {
        path
    } else {
        base.join(path)
    };
    let file = std::fs::read_to_string(&path)
        .map_err(|err| anyhow::anyhow!("read {}: {err}", path.display()))?;
    if let (Some(s), Some(e)) = (start_line, end_line) {
        let lines: Vec<&str> = file.lines().collect();
        if s == 0 || e < s || e > lines.len() {
            anyhow::bail!("file ref line range out of bounds: {}", path.display());
        }
        Ok(lines[s - 1..e].join("\n"))
    } else {
        Ok(file)
    }
}

fn expand_glob(pattern: &str, base: &std::path::Path) -> anyhow::Result<String> {
    let mut out = String::new();
    let abs_pattern = if std::path::PathBuf::from(pattern).is_absolute() {
        pattern.to_string()
    } else {
        base.join(pattern).to_string_lossy().into_owned()
    };
    let entries = simple_glob(&abs_pattern);
    for path in entries {
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        out.push_str("\n# ===== ");
        out.push_str(&path.display().to_string());
        out.push_str(" =====\n");
        out.push_str(&content);
    }
    Ok(out)
}

fn simple_glob(pattern: &str) -> Vec<std::path::PathBuf> {
    // Lightweight glob: matches `*` only in the last path component.
    let path = std::path::PathBuf::from(pattern);
    if !pattern.contains('*') {
        return if path.exists() { vec![path] } else { vec![] };
    }
    let parent = path.parent().unwrap_or(std::path::Path::new("."));
    let file_pattern = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let Ok(rd) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    rd.flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_name()?.to_str()?.to_string();
            if glob_matches(file_pattern, &name) {
                Some(p)
            } else {
                None
            }
        })
        .collect()
}

fn glob_matches(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.is_empty() {
        return pattern == text;
    }
    if !text.starts_with(parts[0]) {
        return false;
    }
    let mut cursor = parts[0].len();
    for part in parts.iter().skip(1).take(parts.len().saturating_sub(2)) {
        let Some(idx) = text[cursor..].find(part) else {
            return false;
        };
        cursor += idx + part.len();
    }
    let last = parts.last().copied().unwrap_or("");
    text[cursor..].ends_with(last)
}

pub fn smart_truncate(data: impl AsRef<str>, max_len: usize) -> String {
    let data = data.as_ref();
    if data.chars().count() <= max_len {
        return data.to_string();
    }
    if max_len <= 32 {
        return data.chars().take(max_len).collect();
    }
    let half = (max_len.saturating_sub(28)) / 2;
    let head: String = data.chars().take(half).collect();
    let tail_vec: Vec<char> = data.chars().rev().take(half).collect();
    let tail: String = tail_vec.into_iter().rev().collect();
    format!("{head}\n...[truncated]...\n{tail}")
}

pub fn trim_history_tags(text: &str, max_inner: usize) -> String {
    let mut out = text.to_string();
    for tag in [
        "thinking",
        "think",
        "tool_use",
        "tool_result",
        "history",
        "key_info",
    ] {
        out = trim_single_tag(&out, tag, max_inner);
    }
    out
}

fn trim_single_tag(text: &str, tag: &str, max_inner: usize) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut rest = text;
    let mut out = String::with_capacity(text.len());
    while let Some(start) = rest.find(&open) {
        let (prefix, after_prefix) = rest.split_at(start);
        out.push_str(prefix);
        if let Some(end) = after_prefix.find(&close) {
            let inner_start = open.len();
            let inner_end = end;
            out.push_str(&open);
            out.push_str(&smart_truncate(
                &after_prefix[inner_start..inner_end],
                max_inner,
            ));
            out.push_str(&close);
            rest = &after_prefix[end + close.len()..];
        } else {
            out.push_str(after_prefix);
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out
}

pub fn parse_text_tool_calls(content: &str) -> (Vec<ToolCall>, String) {
    let mut calls = Vec::new();
    let mut cleaned = String::new();
    let mut rest = content;

    while let Some(start) = find_tool_open(rest) {
        let (before, after_start) = rest.split_at(start);
        cleaned.push_str(before);
        let Some(gt) = after_start.find('>') else {
            cleaned.push_str(after_start);
            return (calls, cleaned.trim().to_string());
        };
        let open_tag = &after_start[..=gt];
        let close_tag = if open_tag.starts_with("<tool_call") {
            "</tool_call>"
        } else {
            "</tool_use>"
        };
        let body_start = gt + 1;
        let Some(end) = after_start[body_start..].find(close_tag) else {
            cleaned.push_str(after_start);
            return (calls, cleaned.trim().to_string());
        };
        let body = after_start[body_start..body_start + end].trim();
        if let Ok(call) = parse_tool_json(body) {
            calls.push(call);
        } else {
            calls.push(ToolCall::new(
                "bad_json",
                serde_json::json!({ "msg": format!("Failed to parse tool_use JSON: {}", smart_truncate(body, 220)) }),
            ));
        }
        rest = &after_start[body_start + end + close_tag.len()..];
    }
    cleaned.push_str(rest);

    if calls.is_empty()
        && let Some(idx) = content
            .find("[{\"type\":\"tool_use\"")
            .or_else(|| content.find("[{\"type\": \"tool_use\""))
        && let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&content[idx..])
    {
        let mut parsed = Vec::new();
        for item in items {
            if item.get("type").and_then(Value::as_str) == Some("tool_use") {
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("bad_json")
                    .to_string();
                let args = item.get("input").cloned().unwrap_or(Value::Null);
                parsed.push(ToolCall {
                    id: item
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    name,
                    arguments: args,
                });
            }
        }
        if !parsed.is_empty() {
            return (parsed, content[..idx].trim().to_string());
        }
    }

    (calls, cleaned.trim().to_string())
}

fn find_tool_open(text: &str) -> Option<usize> {
    let a = text.find("<tool_use");
    let b = text.find("<tool_call");
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn parse_tool_json(body: &str) -> Result<ToolCall> {
    let body = body
        .trim()
        .trim_matches('`')
        .strip_prefix("json\n")
        .unwrap_or(body.trim().trim_matches('`'))
        .trim();
    let value: Value = serde_json::from_str(body)?;
    let name = value
        .get("name")
        .or_else(|| value.get("function"))
        .or_else(|| value.get("tool"))
        .and_then(Value::as_str)
        .unwrap_or("bad_json")
        .to_string();
    let args = value
        .get("arguments")
        .or_else(|| value.get("args"))
        .or_else(|| value.get("params"))
        .or_else(|| value.get("parameters"))
        .cloned()
        .unwrap_or(Value::Null);
    Ok(ToolCall {
        id: value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        name,
        arguments: args,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_xml_tool_call() {
        let text = r#"hello <tool_use>{"name":"file_read","arguments":{"path":"a"}}</tool_use>"#;
        let (calls, cleaned) = parse_text_tool_calls(text);
        assert_eq!(cleaned, "hello");
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(calls[0].arguments["path"], "a");
    }

    #[test]
    fn trims_tags() {
        let text = "<history>abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz</history>";
        let trimmed = trim_history_tags(text, 40);
        assert!(trimmed.contains("truncated"));
    }
}
