use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GarsPaths {
    pub home: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    pub memory: PathBuf,
    pub l4_raw_sessions: PathBuf,
    pub tasks: PathBuf,
    pub runs: PathBuf,
    pub tmp: PathBuf,
    pub logs: PathBuf,
    pub browser: PathBuf,
    pub schedules: PathBuf,
}

impl GarsPaths {
    pub fn resolve(home_override: Option<PathBuf>) -> Result<Self> {
        let home = match home_override {
            Some(path) => path,
            None => match env::var_os("GARS_HOME") {
                Some(path) => PathBuf::from(path),
                None => dirs::home_dir()
                    .context("Could not determine home directory")?
                    .join(".gars"),
            },
        };
        Ok(Self {
            config: home.join("config.toml"),
            state: home.join("state.json"),
            memory: home.join("memory"),
            l4_raw_sessions: home.join("memory").join("L4_raw_sessions"),
            tasks: home.join("tasks"),
            runs: home.join("runs"),
            tmp: home.join("tmp"),
            logs: home.join("logs"),
            browser: home.join("browser"),
            schedules: home.join("schedules"),
            home,
        })
    }

    pub fn ensure(&self) -> Result<()> {
        for dir in [
            &self.home,
            &self.memory,
            &self.l4_raw_sessions,
            &self.tasks,
            &self.runs,
            &self.tmp,
            &self.logs,
            &self.browser,
            &self.schedules,
            &self.schedules.join("done"),
        ] {
            fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        write_if_missing(&self.config, &default_config())?;
        write_if_missing(&self.memory.join("global_mem_insight.txt"), DEFAULT_L1)?;
        write_if_missing(&self.memory.join("global_mem.txt"), DEFAULT_L2)?;
        write_if_missing(&self.memory.join("memory_management_sop.md"), DEFAULT_L0)?;
        write_if_missing(&self.memory.join("tool_access_stats.json"), "{}\n")?;
        Ok(())
    }
}

/// On-disk service state (replaces the removed `service_state` SQLite table).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ServiceState {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub last_admin_token_rotation: Option<String>,
    #[serde(default, flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

impl ServiceState {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let s = serde_json::to_string_pretty(self)?;
        fs::write(path, s).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }
}

pub fn legacy_home() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".GA"))
}

pub fn migration_hint(paths: &GarsPaths) -> Option<String> {
    let legacy = legacy_home()?;
    if legacy.exists() && !paths.home.exists() {
        Some(format!(
            "Legacy GenericAgent data found at {}. Run garstool migration from the interactive menu before using {} if you want to preserve it.",
            legacy.display(),
            paths.home.display()
        ))
    } else {
        None
    }
}

pub fn default_config() -> String {
    // admin_token defaults to empty: REST is unauthenticated out of the box.
    // Users who expose 127.0.0.1 to other hosts (or run behind a tunnel)
    // should set a token via the Web UI → 设置 → 通用 (or by editing
    // config.toml directly).
    DEFAULT_CONFIG_TEMPLATE.replace("__ADMIN_TOKEN__", "")
}

fn write_if_missing(path: &Path, content: &str) -> Result<()> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, content).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

pub fn global_memory_prompt(paths: &GarsPaths) -> Result<String> {
    let l1 = fs::read_to_string(paths.memory.join("global_mem_insight.txt")).unwrap_or_default();
    let l2 = fs::read_to_string(paths.memory.join("global_mem.txt")).unwrap_or_default();
    Ok(format!(
        "cwd = {} (./)\n\n[Memory] ({})\n{}\n../memory/global_mem.txt:\n{}\n",
        paths.tmp.display(),
        paths.memory.display(),
        l1,
        l2
    ))
}

pub fn record_memory_access(paths: &GarsPaths, file: impl AsRef<Path>) -> Result<()> {
    let stats_path = paths.memory.join("tool_access_stats.json");
    let mut value: serde_json::Value = fs::read_to_string(&stats_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));
    let key = file
        .as_ref()
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let count = value
        .get(&key)
        .and_then(|v| v.get("count"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        + 1;
    value[&key] = json!({
        "count": count,
        "last": Local::now().format("%Y-%m-%d").to_string()
    });
    fs::write(stats_path, serde_json::to_string_pretty(&value)? + "\n")?;
    Ok(())
}

pub fn archive_session(paths: &GarsPaths, name: &str, content: &str) -> Result<PathBuf> {
    fs::create_dir_all(&paths.l4_raw_sessions)?;
    let safe_name = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let path = paths.l4_raw_sessions.join(format!(
        "{}_{}.md",
        Local::now().format("%Y%m%d_%H%M%S"),
        safe_name
    ));
    fs::write(&path, content)?;
    Ok(path)
}

pub const DEFAULT_CONFIG_TEMPLATE: &str = r#"# gars configuration.
# Secrets should normally live in environment variables. Example:
#   export OPENAI_API_KEY=...
#   export ANTHROPIC_API_KEY=...

language = "zh"
default_llm = "primary"
context_char_budget = 180000

[server]
bind = "127.0.0.1"
port = 9221
# Empty admin_token = REST is unauthenticated (default since v0.0.2).
# Listening on 127.0.0.1 is the safety net; set a real token here if
# you bind to 0.0.0.0 or tunnel/forward the port off-box.
admin_token = "__ADMIN_TOKEN__"

[browser]
host = "127.0.0.1"
port = 9222

# Skill Search 走 lsdefine/GenericAgent 的远程 105K 技能卡 API.
# 远程返回 SearchResult{ final_score, relevance, quality, match_reasons,
# warnings, skill }. remote 留空字符串 = 只用本地 BM25.
# 注意原项目说明: 中文 query 匹配差, 用英文.
[skills]
remote = "http://www.fudankw.cn:58787"
remote_key_env = "SKILL_SEARCH_KEY"
remote_timeout_secs = 8
# Sophub marketplace (https://fudankw.cn/sophub/) base URL.
market = "https://fudankw.cn"

[[llm.sessions]]
name = "primary"
provider = "openai_compatible"
api_base = "https://api.openai.com/v1/chat/completions"
api_key_env = "OPENAI_API_KEY"
model = "gpt-5.2"
native_tools = true
stream = false
max_tokens = 8192
temperature = 1.0

# ─────────────── Claude presets ─────────────────
# Inspired by lsdefine/GenericAgent mykey_template.py. Uncomment whichever
# block matches your provider, drop in the key (or export the env var), and
# set default_llm above to point at the session name.
#
# 1. Anthropic 官方直连 — sk-ant-* 用 x-api-key 鉴权，model 后缀 [1m] 触发
#    1M context beta. 真 Anthropic 端点不要开 fake_cc_system_prompt.
# [[llm.sessions]]
# name = "claude"
# provider = "anthropic"
# api_base = "https://api.anthropic.com"
# api_key_env = "ANTHROPIC_API_KEY"
# model = "claude-sonnet-4-6"
# native_tools = true
# thinking_type = "adaptive"
# max_tokens = 16384
# stream = true
#
# 2. CC switch / Claude Code 透传渠道 — sk-user-* / sk-* / cr_* 等用 Bearer，
#    必须 fake_cc_system_prompt = true. user_agent 可以 pin 旧版本绕过 UA 校验.
# [[llm.sessions]]
# name = "cc-relay"
# provider = "anthropic"
# api_base = "https://your-cc-switch-host/claude/office"
# api_key_env = "CC_RELAY_KEY"
# model = "claude-opus-4-7"
# native_tools = true
# fake_cc_system_prompt = true
# thinking_type = "adaptive"
# user_agent = "claude-cli/2.1.113 (external, cli)"
# max_tokens = 32768
# stream = true
#
# 3. CRS (claude-relay-service) — cr_* key + 同样要求 fake_cc_system_prompt.
# [[llm.sessions]]
# name = "crs-claude"
# provider = "anthropic"
# api_base = "https://your-crs-host/api"
# api_key_env = "CRS_CLAUDE_KEY"
# model = "claude-opus-4-7[1m]"
# native_tools = true
# fake_cc_system_prompt = true
# thinking_type = "adaptive"
# max_tokens = 32768
# read_timeout_secs = 180
#
# 4. 智谱 GLM-5.1 Anthropic 兼容路径.
# [[llm.sessions]]
# name = "glm-claude"
# provider = "anthropic"
# api_base = "https://open.bigmodel.cn/api/anthropic"
# api_key_env = "ZHIPU_API_KEY"
# model = "glm-5.1"
# native_tools = true
#
# 5. MiniMax Anthropic 兼容路径.
# [[llm.sessions]]
# name = "minimax-claude"
# provider = "anthropic"
# api_base = "https://api.minimaxi.com/anthropic"
# api_key_env = "MINIMAX_API_KEY"
# model = "MiniMax-M2.7"
# native_tools = true
# temperature = 1.0

# ─────────────── OpenAI-compatible presets ─────────────────
# 任何提供 OpenAI chat/completions 兼容接口的 provider 都用 provider =
# "openai_compatible"。下面是社区最常见的几家，按需取消注释。
#
# 6. Kimi / Moonshot (温度会被夹到 1.0).
# [[llm.sessions]]
# name = "kimi"
# provider = "openai_compatible"
# api_base = "https://api.moonshot.cn/v1/chat/completions"
# api_key_env = "MOONSHOT_API_KEY"
# model = "kimi-k2"
# native_tools = true
# stream = true
# max_tokens = 16384
#
# 7. DeepSeek (V4 系列原生 Claude 协议；这里走 OpenAI 兼容).
# [[llm.sessions]]
# name = "deepseek"
# provider = "openai_compatible"
# api_base = "https://api.deepseek.com/v1/chat/completions"
# api_key_env = "DEEPSEEK_API_KEY"
# model = "deepseek-chat"
# native_tools = true
# stream = true
# max_tokens = 16384
#
# 8. OpenRouter (聚合 300+ 模型).
# [[llm.sessions]]
# name = "openrouter"
# provider = "openai_compatible"
# api_base = "https://openrouter.ai/api/v1/chat/completions"
# api_key_env = "OPENROUTER_API_KEY"
# model = "anthropic/claude-sonnet-4-6"
# native_tools = true
# stream = true
#
# 9. 智谱 GLM OAI 兼容路径 (与 1d 的 Anthropic 路径同 key，不同端点).
# [[llm.sessions]]
# name = "glm-oai"
# provider = "openai_compatible"
# api_base = "https://open.bigmodel.cn/api/paas/v4/chat/completions"
# api_key_env = "ZHIPU_API_KEY"
# model = "glm-5.1"
# native_tools = true
#
# 10. MiniMax OAI 路径 (M2.7 会带 <think> 标签，建议优先 Anthropic 路径).
# [[llm.sessions]]
# name = "minimax-oai"
# provider = "openai_compatible"
# api_base = "https://api.minimaxi.com/v1/text/chatcompletion_v2"
# api_key_env = "MINIMAX_API_KEY"
# model = "MiniMax-M2.7"
# native_tools = true
# temperature = 0.7
#
# 11. 阿里通义千问 (DashScope OAI 兼容).
# [[llm.sessions]]
# name = "qwen"
# provider = "openai_compatible"
# api_base = "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions"
# api_key_env = "DASHSCOPE_API_KEY"
# model = "qwen3-max"
# native_tools = true
#
# 12. 字节豆包 (火山引擎 ARK).
# [[llm.sessions]]
# name = "doubao"
# provider = "openai_compatible"
# api_base = "https://ark.cn-beijing.volces.com/api/v3/chat/completions"
# api_key_env = "ARK_API_KEY"
# model = "doubao-1-6-pro"
# native_tools = true
#
# 13. SiliconFlow (硅基流动) 聚合.
# [[llm.sessions]]
# name = "siliconflow"
# provider = "openai_compatible"
# api_base = "https://api.siliconflow.cn/v1/chat/completions"
# api_key_env = "SILICONFLOW_API_KEY"
# model = "Pro/deepseek-ai/DeepSeek-V4"
# native_tools = true
#
# 14. Stepfun 阶跃星辰.
# [[llm.sessions]]
# name = "stepfun"
# provider = "openai_compatible"
# api_base = "https://api.stepfun.com/v1/chat/completions"
# api_key_env = "STEPFUN_API_KEY"
# model = "step-3"
# native_tools = true
#
# 15. Cerebras (超快 inference).
# [[llm.sessions]]
# name = "cerebras"
# provider = "openai_compatible"
# api_base = "https://api.cerebras.ai/v1/chat/completions"
# api_key_env = "CEREBRAS_API_KEY"
# model = "llama-4-maverick-17b-128e-instruct"
# native_tools = true
#
# 16. Groq (LPU 加速).
# [[llm.sessions]]
# name = "groq"
# provider = "openai_compatible"
# api_base = "https://api.groq.com/openai/v1/chat/completions"
# api_key_env = "GROQ_API_KEY"
# model = "llama-3.3-70b-versatile"
# native_tools = true
#
# 17. Together.ai 聚合.
# [[llm.sessions]]
# name = "together"
# provider = "openai_compatible"
# api_base = "https://api.together.xyz/v1/chat/completions"
# api_key_env = "TOGETHER_API_KEY"
# model = "deepseek-ai/DeepSeek-V4"
# native_tools = true
#
# 18. Mistral (官方 La Plateforme).
# [[llm.sessions]]
# name = "mistral"
# provider = "openai_compatible"
# api_base = "https://api.mistral.ai/v1/chat/completions"
# api_key_env = "MISTRAL_API_KEY"
# model = "mistral-large-latest"
# native_tools = true
#
# 19. Gemini OAI 兼容 (Google AI Studio 兼容端点).
# [[llm.sessions]]
# name = "gemini"
# provider = "openai_compatible"
# api_base = "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"
# api_key_env = "GEMINI_API_KEY"
# model = "gemini-3.0-pro"
# native_tools = true
#
# 20. xAI Grok.
# [[llm.sessions]]
# name = "grok"
# provider = "openai_compatible"
# api_base = "https://api.x.ai/v1/chat/completions"
# api_key_env = "XAI_API_KEY"
# model = "grok-5"
# native_tools = true
#
# ─────────────── 本地 / 自托管 ─────────────────
#
# 21. llama.cpp server (./server --port 8080).
# [[llm.sessions]]
# name = "llamacpp"
# provider = "openai_compatible"
# api_base = "http://127.0.0.1:8080/v1/chat/completions"
# model = "local"
# native_tools = false
# text_protocol = true
#
# 22. Ollama (默认 11434, 11434 上有 OAI 兼容路径).
# [[llm.sessions]]
# name = "ollama"
# provider = "openai_compatible"
# api_base = "http://127.0.0.1:11434/v1/chat/completions"
# model = "llama3.3:70b"
# native_tools = true
#
# 23. vLLM (./vllm serve …, 默认 8000).
# [[llm.sessions]]
# name = "vllm"
# provider = "openai_compatible"
# api_base = "http://127.0.0.1:8000/v1/chat/completions"
# model = "deepseek-ai/DeepSeek-V4"
# native_tools = true
#
# 24. LM Studio (默认 1234).
# [[llm.sessions]]
# name = "lmstudio"
# provider = "openai_compatible"
# api_base = "http://127.0.0.1:1234/v1/chat/completions"
# model = "local"
# native_tools = false
# text_protocol = true

[[llm.mixins]]
name = "primary"
sessions = ["primary"]
max_retries = 3
base_delay_ms = 800
spring_back_secs = 300

# Mixin failover example: try Claude first, fall back to OpenAI.
# [[llm.mixins]]
# name = "robust"
# sessions = ["claude", "primary"]
# max_retries = 6
# base_delay_ms = 800
# spring_back_secs = 300
"#;

pub const DEFAULT_L1: &str = r#"Facts(L2): global_mem.txt | CodeRoot: program dir | SOPs(L3): memory/*.md or *.py | META-SOP(L0): memory/memory_management_sop.md
L1 Insight is a minimal index; sync L1 when L2/L3 changes; keep index minimal. Read META-SOP(L0) before writing any memory.

[CONSTITUTION]
1. Probe before claiming; use tools for evidence.
2. Update working checkpoints during long tasks; avoid retry loops.
3. No Execution, No Memory: write only action-verified facts to L1/L2/L3.
4. User data lives under GARS_HOME; do not write into the installed program directory during normal use.
"#;

pub const DEFAULT_L2: &str = r#"# [Global Memory - L2]

Use this file for environment-specific facts verified by successful tool calls.
"#;

pub const DEFAULT_L0: &str = r#"## Memory Management SOP

Core axiom: No Execution, No Memory.

L1: global_mem_insight.txt, the minimal index. Keep it under 30 lines.
L2: global_mem.txt, verified environment facts.
L3: memory/*.md or scripts, task-specific SOPs and reusable helpers.
L4: L4_raw_sessions, archived raw sessions.

Only write information that was verified by successful actions. Prefer small
local patches. Do not store volatile state, generic common sense, secrets, or
unverified guesses.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_respects_home_override() {
        let dir = tempfile::tempdir().unwrap();
        let paths = GarsPaths::resolve(Some(dir.path().join("gars"))).unwrap();
        paths.ensure().unwrap();
        assert!(paths.config.exists());
        assert!(paths.memory.join("global_mem_insight.txt").exists());
        assert!(!PathBuf::from("config.toml").exists());
    }
}
