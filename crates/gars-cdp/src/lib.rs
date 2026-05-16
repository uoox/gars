use anyhow::{Context, Result, anyhow};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserConfig {
    pub host: String,
    pub port: u16,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 9222,
        }
    }
}

impl BrowserConfig {
    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabInfo {
    pub id: String,
    pub title: Option<String>,
    pub url: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub web_socket_debugger_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsResult {
    pub status: String,
    pub js_return: Value,
    pub raw: Value,
}

pub async fn list_tabs(config: &BrowserConfig) -> Result<Vec<TabInfo>> {
    let url = format!("{}/json", config.base_url());
    let tabs = reqwest::get(&url)
        .await
        .with_context(|| format!("connect to Chrome CDP endpoint {url}"))?
        .error_for_status()?
        .json::<Vec<TabInfo>>()
        .await?;
    Ok(tabs
        .into_iter()
        .filter(|tab| tab.kind.as_deref().unwrap_or("page") == "page")
        .collect())
}

pub async fn execute_js(
    config: &BrowserConfig,
    tab_id: Option<&str>,
    script: &str,
) -> Result<JsResult> {
    let tabs = list_tabs(config).await?;
    let tab = if let Some(tab_id) = tab_id {
        tabs.iter().find(|tab| tab.id == tab_id)
    } else {
        tabs.first()
    }
    .ok_or_else(|| {
        anyhow!(
            "No Chrome tab found. Start Chrome with --remote-debugging-port={}",
            config.port
        )
    })?;
    let ws_url = tab
        .web_socket_debugger_url
        .as_deref()
        .ok_or_else(|| anyhow!("Tab has no webSocketDebuggerUrl"))?;
    let (mut ws, _) = connect_async(ws_url).await?;
    let request = json!({
        "id": 1,
        "method": "Runtime.evaluate",
        "params": {
            "expression": script,
            "awaitPromise": true,
            "returnByValue": true,
            "timeout": 30_000
        }
    });
    ws.send(Message::Text(request.to_string().into())).await?;
    while let Some(msg) = ws.next().await {
        let msg = msg?;
        let text = match msg {
            Message::Text(text) => text.to_string(),
            Message::Binary(bytes) => String::from_utf8_lossy(&bytes).to_string(),
            _ => continue,
        };
        let value: Value = serde_json::from_str(&text)?;
        if value.get("id").and_then(Value::as_u64) != Some(1) {
            continue;
        }
        if let Some(err) = value.get("error") {
            return Ok(JsResult {
                status: "error".to_string(),
                js_return: err.clone(),
                raw: value,
            });
        }
        let result = value
            .pointer("/result/result/value")
            .cloned()
            .or_else(|| value.pointer("/result/result/description").cloned())
            .unwrap_or(Value::Null);
        return Ok(JsResult {
            status: "success".to_string(),
            js_return: result,
            raw: value,
        });
    }
    Err(anyhow!(
        "CDP websocket closed before Runtime.evaluate returned"
    ))
}

pub async fn scan_page(
    config: &BrowserConfig,
    tab_id: Option<&str>,
    text_only: bool,
    max_len: usize,
) -> Result<Value> {
    let tabs = list_tabs(config).await?;
    let tab_list: Vec<Value> = tabs
        .iter()
        .map(|tab| {
            json!({
                "id": tab.id,
                "title": tab.title,
                "url": tab.url.as_deref().map(|url| trim_url(url, 90)),
            })
        })
        .collect();
    let script = if text_only {
        visible_text_script(max_len)
    } else {
        simplified_html_script(max_len)
    };
    let result = execute_js(config, tab_id, &script).await?;
    Ok(json!({
        "status": result.status,
        "metadata": {
            "tabs_count": tab_list.len(),
            "tabs": tab_list,
            "active_tab": tab_id,
        },
        "content": result.js_return,
    }))
}

fn trim_url(url: &str, max: usize) -> String {
    if url.chars().count() <= max {
        url.to_string()
    } else {
        url.chars().take(max).collect::<String>() + "..."
    }
}

fn visible_text_script(max_len: usize) -> String {
    format!(
        r#"(function() {{
  const walker = document.createTreeWalker(document.body || document.documentElement, NodeFilter.SHOW_TEXT);
  const chunks = [];
  let node;
  while ((node = walker.nextNode())) {{
    const text = (node.nodeValue || '').replace(/\s+/g, ' ').trim();
    if (!text) continue;
    const parent = node.parentElement;
    if (!parent) continue;
    const style = getComputedStyle(parent);
    if (style.display === 'none' || style.visibility === 'hidden' || Number(style.opacity) === 0) continue;
    chunks.push(text);
    if (chunks.join('\n').length > {max_len}) break;
  }}
  return chunks.join('\n').slice(0, {max_len});
}})()"#
    )
}

fn simplified_html_script(max_len: usize) -> String {
    format!(
        r#"(function() {{
  function visible(el) {{
    const style = getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden' || Number(style.opacity) === 0) return false;
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
  }}
  const keep = ['A','BUTTON','INPUT','TEXTAREA','SELECT','OPTION','LABEL','H1','H2','H3','P','LI','TD','TH','ARTICLE','MAIN','SECTION'];
  const out = [];
  document.querySelectorAll(keep.join(',')).forEach((el, idx) => {{
    if (!visible(el)) return;
    const tag = el.tagName.toLowerCase();
    const text = (el.innerText || el.value || el.getAttribute('aria-label') || '').replace(/\s+/g, ' ').trim();
    if (!text && !['input','textarea','select'].includes(tag)) return;
    const id = el.id ? ` id="${{el.id}}"` : '';
    const href = tag === 'a' && el.href ? ` href="${{el.href}}"` : '';
    out.push(`<${{tag}} data-gars-idx="${{idx}}"${{id}}${{href}}>${{text.slice(0, 500)}}</${{tag}}>`);
  }});
  return out.join('\n').slice(0, {max_len});
}})()"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_base_url() {
        assert_eq!(BrowserConfig::default().base_url(), "http://127.0.0.1:9222");
    }
}
