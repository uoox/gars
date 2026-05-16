# gars

`gars` — 受 [GenericAgent](https://github.com/lsdefine/GenericAgent) 启发的 Rust 本机常驻 Agent 服务。**小核心 + 大 SOP**：行为差异由 Markdown SOP 表达，扩展靠加 SOP / 改配置，不靠加代码。

---

## 快速开始

```bash
# 1. 安装（装到 /usr/local/bin/，需要 sudo；可用 GARS_PREFIX 改装到别处）
curl -fsSL https://raw.githubusercontent.com/uoox/gars/main/scripts/install.sh | sh

# 2. 配置（填 LLM API key、admin token，可选定时/连接器）
garstool

# 3. 启动服务（前台 / 或用 garstool service install + start 后台跑）
gars

# 4. 浏览器访问 Web UI
open http://127.0.0.1:9221/ui/
```

UI 顶部填入 admin token（在 `~/.gars/config.toml` 里），就可以在三栏界面里管任务、技能、模式、设置。

---

## 这是什么

- **常驻服务**：`gars` 启 axum 在 `127.0.0.1:9221`，对外暴露 REST + WebSocket + SSE + `/ui/`（内嵌 Web UI）+ `/ext/` （浏览器扩展下载）。`garstool` 是交互式管理器（安装、启停、配置、迁移、查日志）。
- **用户面向 + 向外调用**：gars 是给*你*用的，不是给别的 agent 调用的。它通过工具调外（OCR、浏览器、Shell、文件、ADB、聊天平台），但**不**对外暴露 MCP server / 工具协议。
- **小核心 + 大 SOP**：主程序只做四件事 —— LLM 调用循环、少量原子工具、HTTP/WS 服务壳、cron 触发。"任务模式" 不是一等公民；行为差异由 markdown SOP 表达，加新行为 = 在 `~/.gars/skills/local/` 写一个 markdown，不用动 Rust 代码。
- **Markdown 是 LLM 知识正本**：memory L0-L4、skills/、agents/、plans/ 全是 markdown。SQLite (`gars.db`) 只承担 `tasks` / `task_events` / `l4_index` 三张高频读写表。运行态零碎走 `state.json`。
- **聊天平台接入**：仅 Telegram 和 飞书/Lark。完整 inbound：long-poll / webhook 签名 / 白名单 / 出站消息。不计划支持其他聊天平台。
- **浏览器扩展**：MV3，连 `/v1/extension` WebSocket，让 agent 驱动你已经在用的浏览器 tab（无需 `--remote-debugging-port`）。`web_scan` / `web_execute_js` 自动优先扩展、回落 CDP。

---

## 数据目录 `~/.gars`

| 路径 | 内容 |
|---|---|
| `config.toml` | 配置 |
| `state.json` | 运行态（启动时间、token 轮换记录等） |
| `gars.db` | SQLite：`tasks`、`task_events`、`l4_index` |
| `memory/` | L0-L4 markdown |
| `skills/{builtin,local,imported}/` | SOP markdown（builtin 启动强刷，local 永不动） |
| `agents/{builtin,local}/` | 子代理定义 toml |
| `plans/<run_id>/plan.md` | Plan 文件（markdown 是唯一真相） |
| `tasks/<run_id>/<agent>/` | 子代理文件协议工作目录 |
| `schedules/*.toml` + `done/` | 定时任务定义和报告 |
| `keychain.enc` | XOR 加密的本地凭证 |

---

## 添加新行为（**无需重启**）

```bash
# 写一个新的 SOP（markdown，带 frontmatter）
vim ~/.gars/skills/local/my_workflow_sop.md
```

然后新建任务时直接在 prompt 里 `请按 my_workflow_sop.md 来做` —— SOP 现在是
唯一的行为表达方式，不再有"模式" toml。`~/.gars/skills/{local,imported}/`
下的改动**保存即生效**，每次任务运行都重新扫盘。

> **关于重启**：唯一需要重启的场景是升级 gars 二进制本身（启动时会从内嵌
> 资源刷新 `~/.gars/skills/builtin/` 和 `~/.gars/agents/builtin/`，对
> `local/` 和 `imported/` 永不动）。

---

## REST API（速查）

`/health` 公开；其它 `/v1/*` 在 `[server] admin_token` 非空时需要
`Authorization: Bearer <admin_token>`。**默认 admin_token 为空 = 不需要鉴权**
（仅靠 `bind = "127.0.0.1"` 做隔离）。

```
GET  /                                   → 301 /ui/
GET  /ui/                                嵌入式 Web UI
GET  /ext/download/gars-extension.zip    Chrome 扩展包

GET  /health, /v1/status, /v1/tools, /v1/agents
GET  /v1/config           PUT /v1/config
GET  /v1/memory/{layer}   PUT /v1/memory/{layer}

POST /v1/chat,            POST /v1/chat/stream     （SSE）
GET  /v1/tasks            POST /v1/tasks
GET  /v1/tasks/{id}       GET /v1/tasks/{id}/events

GET  /v1/skills?q=        POST /v1/skills/import
GET  /v1/skills/{key}
GET  /v1/skills/market    GET /v1/skills/market/{id}    POST /v1/skills/market/install

GET  /v1/plans            POST /v1/plans
GET  /v1/plans/{id}       DELETE /v1/plans/{id}
POST /v1/plans/{id}/steps/{idx}/mark            # 文件协议薄壳

GET  /v1/subagents        POST /v1/subagents
POST /v1/subagents/{run_id}/{run,intervene,stop}  # intervene/stop 是文件协议薄壳

GET  /v1/schedules        POST /v1/schedules
POST /v1/schedules/{id}/trigger   GET /v1/schedules/{id}/health


GET  /v1/connectors       POST /v1/connectors/{id}/{reload,send}
POST /v1/connectors/{telegram,feishu}/webhook

GET  /v1/extension        GET /v1/extension/state
GET  /v1/events           （WebSocket，全局事件总线）
```

---

## 设计原则

- **小核心**：主程序只做四件事——LLM 调用循环、工具实现、HTTP/WS 服务壳、cron 触发。
- **大 SOP**：行为差异、模式语义、提示工程全在 markdown 里。这条线是上游 GenericAgent 一直坚持的，gars v0.6 起拉回这条线。
- **用户面向，向外调用**：gars 是给*你*用的；它调外部工具，不被外部 agent 反向调用。所以不做 MCP server。
- **Markdown 是知识正本**：人和 LLM 都直接可读、可 git diff、可手改。SQLite 只用于真正高频的运行态读写。
- **文件协议是真接口**：subagent 用 `input.txt`/`output.txt`/`_stop`/`_intervene` 等文件交流；plan 用 `plan.md`。REST 中的 `/mark` / `/intervene` / `/stop` 只是给 UI 用的薄壳——文件协议本身随时可直接用。

---

## 从源码开发

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release --bin gars --bin garstool
```

发布矩阵：Linux x86_64 / aarch64 / armv7、macOS aarch64。Windows 不再支持（v0.8 起退役），macOS Intel 不再支持（v0.4 起退役）。

---

## English

`gars` is a Rust local-loopback Agent service inspired by
[GenericAgent](https://github.com/lsdefine/GenericAgent). Design principle:
**small core, big SOPs** — behavior differences live in Markdown SOPs that the
LLM reads, not in Rust code paths. Adding a new "mode" means writing a
markdown SOP plus a small TOML, not editing a runner.

`gars` is **user-facing and outward-calling**: it drives external tools (OCR,
browser, shell, ADB, chat platforms) on your behalf. It does **not** expose
itself as an MCP server or tool backend for other agents.

Quickstart, REST API, data layout, design rationale: see the 中文 sections
above. The codebase, CHANGELOG, and PR history are the source of truth.

[GitHub Issues](https://github.com/uoox/gars/issues) for bug reports.
