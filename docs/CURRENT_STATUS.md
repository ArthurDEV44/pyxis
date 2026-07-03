# Current Status

This file is the short source of truth after ADR-11. When it conflicts with historical PRDs or Phase 0 spikes, the order of authority is: code, status JSON files, ADR-11, then this file. Historical docs remain useful for intent and rationale, not for shipped scope.

## Shipped

- Runtime: Rust workspace with a headless `agent-core`, Ratatui TUI, headless `-p` mode, JSONL sessions, resume, and `/goal`.
- Provider: one wired adapter, `OpenAiChatGpt`, using the ChatGPT subscription channel through the Codex backend.
- Auth: OAuth PKCE flow and refresh-token rotation for the ChatGPT subscription, stored in the OS keyring.
- Tools: `read`, `glob`, `grep`, `write`, `edit`, and `bash`, with fail-closed tool metadata, permissions, taint propagation, and concurrent read dispatch.
- Sandbox: Linux Landlock filesystem confinement for the process tree, plus a cooperative local HTTP(S) proxy for subprocesses that honor `HTTP(S)_PROXY`.
- MCP: config loading, lifecycle state, stdio client plumbing, and tool listing. MCP tools are not yet exposed as callable model tools.
- Docs rename: `pyxis` is the public command and repo name. Internal crates still use `agent-*`.

## Deferred

- Public provider adapters: OpenAI BYOK, Anthropic, Gemini, OpenRouter, Ollama, Bedrock, Vertex, and Azure are architectural backlog, not shipped adapters.
- Public OpenAI Responses BYOK mode and server-side `previous_response_id` mode.
- MCP tools in the agent loop, stable connect UX, and per-server OAuth.
- Paneflow in-process embedding, GPU diff rendering, plan trees, and hunk review.
- Vector memory, sub-agents, prompt-cache strategy, VCR provider tests, packaged releases, macOS Seatbelt, and cross-platform hardening.

## Live Risks

- ChatGPT subscription auth is unofficial and revocable. It is a convenience channel, not a contractual foundation.
- The `originator=pyxis` rename validation still needs a live post-rename check against the ChatGPT backend.
- Network control is proxy-based and cooperative. It helps for HTTP(S) subprocesses, but it is not a kernel-level network sandbox and does not block raw sockets by itself.
- Linux is the only supported sandbox target today. Off-Linux filesystem confinement degrades explicitly.
