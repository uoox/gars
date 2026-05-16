# Changelog

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
