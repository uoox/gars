# Changelog

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
