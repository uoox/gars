# Changelog

## v0.0.3 — 2026-05-17

### 还核：删除"任务模式"概念，回归小核心

gars 现在做的事情和上游 GenericAgent 严格对齐：聊天工具链接、Agent loop +
LLM 调用、少量原子工具、REST API + 解耦前端。"任务模式" 不再是一等公民。

**Removed**

- **`crates/gars-skills/src/modes.rs`** (整文件): `ModeDef`, `load_mode`,
  `load_all_modes`, `save_local_mode`, `delete_local_mode`, `mode_hint`,
  `load_sop_bodies`, `resolve_mode`, `modes_dir` — 全删。
- **`crates/gars-skills/assets/modes/builtin/*.toml`** (6 文件): chat,
  schedule, trigger, subagent, plan, goal — 不再随二进制 ship。SOP（即
  markdown）单独表达行为差异；约定靠用户在 `~/.gars/skills/local/` 里写。
- **`crates/gars-server/src/goal_mode.rs`** (298 行): `GoalState`,
  consecutive-done detection, wall-clock budget enforcement — 全删。
  Goal 不再是 mode；想要"带预算的长跑任务"就让 SOP 自己处理时间和早停。
- **REST 路由**: `GET/POST /v1/modes`, `GET/DELETE /v1/modes/{key}`,
  `GET/POST /v1/goals`, `GET /v1/goals/{id}`, `POST /v1/goals/{id}/stop`
  — 删除。`/v1/tasks`, `/v1/chat`, `/v1/schedules`, `/v1/subagents`,
  `/v1/plans` 保留作为原子接口。
- **`crates/gars-core/src/plan_mode.rs`** 里的 `StepStatus` 8-状态枚举，
  以及 `marker()` / `parse()` / `strip_marker()` — 删。Plan 现在把每步
  状态当作裸字符串 marker（`"[ ]"` / `"[D]"` / `"[FIX]"`）；markdown
  文件是唯一真相。
- 前端 `ui.html`：composer 模式下拉、新建任务里的模式选择、Settings 里
  的「模式管理」子页 — 全删。

**Changed**

- `crates/gars-server/src/scheduler.rs` (-47 行): 不再加载 `mode_hint`；
  去掉 `cooldown_secs` 和 `max_delay_hours` 这两个字段及其判断。cron + 60s
  tick + done report 三件事保留。
- `crates/gars-server/src/subagent_runner.rs` (-41 行): 不再加载
  `mode_hint`；去掉 `[tool:name]` 行日志和 `---` 轮次 marker（让 SOP 自己
  决定要不要在 output.txt 里写这些）。
- `crates/gars-skills/src/assets.rs`: `init_user_skills` 简化，不再刷
  `modes/builtin/`；`InitSummary.builtin_modes` 字段移除。
- **新建任务 UI** 简化为「标题（可选） + Prompt」两栏，不选模式不选预算。
  想要更精细控制的高级用法走 REST 或写 SOP。
- 设置面板从 8 个子页减到 7 个（删模式管理；通用/LLM/技能/连接器/扩展/
  归档/关于）。

**Net diff** — roughly:

```
gars-skills/src/modes.rs                257 → 0     (-257)
gars-server/src/goal_mode.rs            298 → 0     (-298)
gars-core/src/plan_mode.rs              316 → 251   (-65)
gars-server/src/scheduler.rs            335 → 288   (-47)
gars-server/src/subagent_runner.rs      109 → 68    (-41)
gars-server/assets/ui.html              1887 → 1606 (-281)
assets/modes/builtin/*.toml             6 files     deleted
+ small touches in lib.rs, plan_tools.rs, cli garstool.rs, assets.rs
```

约 −1000 行代码 + 6 个 TOML 文件，朝"小核心"再走一步。

## v0.0.2 — 2026-05-16

### Changed

- **REST URL 默认不需要密码**. `default_config()` no longer auto-generates a
  UUID `admin_token`; it ships the empty string instead, and `authorize()`
  was already coded to skip auth when the token is empty. Listening on
  `127.0.0.1` remains the safety net. Set a token from **设置 → 通用** (or
  edit `config.toml`) only when you bind to `0.0.0.0` or expose the port
  off-box. `garstool` configure wizard's `admin_token` prompt now defaults
  to empty with hint "留空 = 不需要鉴权". The `uuid` dependency is dropped
  from `gars-memory` and `gars-cli`.

- **Web UI 完全重设计** (`crates/gars-server/assets/ui.html`, 1101 → 1887
  lines). Layout switched from 3-column (sidebar / list / detail) to
  modern 2-column chat:
  - Top bar: gradient-dot logo + crumb + ⚙ 设置 button + connection pill
  - **Left**: tasks pane — `＋ 新建任务` button, search filter,
    `💬 新对话` pseudo-item to return to ad-hoc chat
  - **Right**: chat-style pane — header (task title / 新对话 sub-text) +
    message stream (user bubbles right-aligned blue, gars bubbles left
    panel-colored) + composer (textarea + mode select + send)
  - Enter to send, Shift+Enter newline. Composer mode = chat → direct
    SSE streaming; other modes → pre-fill the 新建任务 modal
  - **Settings overlay** (⚙): 8 sub-tabs replacing the old sidebar tabs
    - 通用: friendly form for `bind` / `port` / `admin_token` (with
      generate-random / clear / show-hide)
    - LLM: raw TOML editor (LLM config too complex for a form)
    - 模式管理: list + create + delete (moved from sidebar)
    - 技能管理: 本地 + sophub 市场 (moved from sidebar)
    - 连接器: full enable/configure form for Telegram + Feishu, with
      `重载` button calling `/v1/connectors/{id}/reload`
    - 浏览器扩展: status + install steps + wait-for-connect
    - 归档: `auto` toggle + `idle_secs` + 立即运行一次
    - 关于: status + links
  - **TOML surgical-edit** helpers `tomlGet` / `tomlSet` / `quoteToml`
    in the UI patch only the keys you change, preserving comments and
    unrelated sections in `config.toml`. Each Settings sub-page reads
    raw text, mutates in place, writes raw text back.
  - Styling: light theme default with `prefers-color-scheme: dark`
    auto-switch, soft shadows, 12px radii, gradient accents.

### Internal

- `assets/config.toml` resynced with `DEFAULT_CONFIG_TEMPLATE` (drift
  guard test `assets_config_matches_template` enforces this).

## v0.0.1 — 2026-05-16

Initial release.

`gars` — Rust local-loopback Agent service inspired by
[GenericAgent](https://github.com/lsdefine/GenericAgent). Design principle:
**small core, big SOPs** — behavior differences live in Markdown SOPs that
the LLM reads, not in Rust code paths.

Highlights:

- Unified `run_task` execution path; modes are TOML files at
  `~/.gars/modes/{builtin,local}/<key>.toml` that bundle SOPs + tool
  allow-lists + budget knobs. New behaviors = new SOP + new mode TOML,
  no code changes.
- Six built-in modes: chat, schedule, trigger, subagent, plan, goal.
- File-protocol subagent runner (`input.txt` / `output.txt` /
  `reply.txt` / `_stop` / `_keyinfo` / `_intervene` / `context.json` /
  `[ROUND END]`).
- Plan mode with 8 step markers (`[ ]` / `[✓]` / `[D]` / `[P]` / `[?]` /
  `[FIX]` / `[SKIP]` / `[✗]`).
- Goal mode with wall-clock budget enforcement + consecutive-done
  early stop.
- REST + WebSocket + SSE on `127.0.0.1:9221` with embedded Web UI at
  `/ui/`.
- Chat platform connectors: **Telegram** and **Feishu/Lark** only —
  inbound long-poll / webhook signature / chat-id allow-list, outbound
  via `/v1/connectors/{id}/send`.
- Chrome MV3 extension speaking `/v1/extension` WebSocket; `web_scan`
  and `web_execute_js` tools fall back from extension to CDP.
- macOS LaunchAgent (`com.github.uoox.gars`) + Linux systemd user unit
  (`gars.service`) lifecycle managed by `garstool`.
- One-line installer (`curl … | sh`) that verifies SHA256, installs
  to `/usr/local/bin/`, and hands off to `garstool` for the configure
  wizard.

Supported platforms: Linux x86_64 / aarch64 / armv7, macOS aarch64.
