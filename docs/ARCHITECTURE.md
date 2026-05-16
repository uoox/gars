# Architecture

gars keeps the original GenericAgent philosophy while cutting the program/user-data coupling and exposing the agent as a local background service.

## Runtime And Service

`gars` starts a local REST API service. `gars-core` owns the agent loop:

1. Build a request from system prompt, conversation state, and mounted tool specs.
2. Ask the configured LLM.
3. Parse native or text-protocol tool calls.
4. Dispatch tools through `ToolRegistry`.
5. Append compact tool results plus the working-memory anchor.
6. Trim old history when the context budget is exceeded.

The runtime is intentionally generic over `LlmClient`, so providers and tests can be swapped without touching tools.

## Memory

All user state lives in `GARS_HOME`, defaulting to `~/.gars`.

- `L0`: memory management SOP
- `L1`: minimal global index
- `L2`: verified environment facts
- `L3`: task SOPs and reusable helpers
- `L4`: archived raw sessions
- `gars.db`: SQLite runtime data for tasks, events, schedules, service state, and indexes

The rule is: No Execution, No Memory. Long-term memory should only be updated from successful tool results.

## LLM Providers

`gars-llm` supports:

- Anthropic-style native messages/tools
- OpenAI Chat Completions-compatible native tools
- text-protocol fallback for relays that do not preserve native tool fields
- mixin failover across named sessions

Most non-OpenAI providers can be configured as OpenAI-compatible endpoints.

## Browser

`gars-cdp` talks to an existing Chrome/Chromium DevTools endpoint. It lists tabs, executes JavaScript, and produces simplified visible text/HTML without introducing Node or Playwright as runtime dependencies.
