# Specification: Ollama Provider (HTTP Native + Optional CLI)

## Overview

Ollama can be used in two ways:

1. **HTTP native (implemented, default)** — `OllamaNativeProvider` in
   `src/provider/ollama_native.rs` forwards OpenAI-compatible requests to the
   Ollama HTTP API (`/api/chat`, `/api/tags`, `/api/show`). This is the
   recommended and fully implemented path (spec_tooling.md §12.5, RS3).
2. **CLI bridge (optional, not yet implemented)** — interact with local models
   via the `ollama` CLI binary. This remains an optional future provider.

The HTTP path does **not** require the `ollama` binary to be on `PATH`; it only
requires a reachable Ollama server (`base_url`, default
`http://localhost:11434`).

## Implementation Status (RS9)

- [x] Native HTTP proxy (`OllamaNativeProvider::forward_chat`,
      `forward_chat_stream`, `stream_events`).
- [x] Model listing via `/api/tags` (`get_models`).
- [x] Tool-calling support detection (`model_supports_tools`).
- [x] Thinking/reasoning streaming via `message.thinking` (RS3 / spec_reasoning §7.5).
- [x] `num_ctx`, `temperature`, `top_p`, `num_predict` forwarding.
- [x] CLI bridge (`OllamaCliBridgeProvider` implementing `BridgeProviderV2`)
      in `src/provider/bridge/ollama_cli.rs`.
- [x] `ollama_mode: "http" | "cli"` selector in `ProviderConfig`
      (`None`/`"http"` = native HTTP, `"cli"` = bridge adapter).
- [x] `OllamaCliSession` for stdin interactions (`ollama run`, `--keepalive`,
      stdout/stderr capture, ANSI stripping).
- [x] Prompt-based tool calling (`prompt_tools_fallback = true`): instructions
      are appended to the prompt and JSON `tool_calls` are parsed from the reply
      (OpenAI-style, compact `{name, arguments}`, bare object, or array).
- [x] Session id reuse: `BridgeConfig.session_id` is honoured so the adapter
      matches the MCP session registered in `BridgeServer`.

Known limitation: the CLI adapter is still one-shot per request (the process is
closed at the end of a turn). Cross-request persistent resume is tracked
separately as P1 (`docs/spec_tooling_progress.md`).

## Configuration

```toml
[provider.ollama]
enabled        = true
base_url       = "http://127.0.0.1:11434"
num_ctx        = 16384
keep_alive     = "10m"
prompt_tools_fallback = false
ollama_mode    = "http"   # "http" (default) | "cli"
```

`ollama_mode = "cli"` routes chat requests through the bridge adapter
(`ollama run`), and the native HTTP path is skipped in `ServiceRegistry`.
`bin` may be omitted; it defaults to `ollama`.

## Binary Availability (CLI mode only)

Availability is checked with the same mechanism as other CLI providers
(`CliProvider::check_available`), which probes `PATH` (and Windows extensions
`.cmd/.bat/.exe/.ps1`). HTTP mode does not depend on the binary.

Integration tests cover both cases:

- `check_available()` returns `true` when `ollama` is installed.
- `check_available()` returns `false` gracefully when it is not (no panic).

## Roadmap (RS9)

1. **Bridge Adaptation** — ✅ `src/provider/bridge/ollama_cli.rs` extends
   `BridgeProviderV2`.
2. **Provider Selector** — ✅ `create_bridge_factory_v2` maps `ollama` /
   `ollama-cli`; `build_cli_provider` selects the bridge only for
   `ollama_mode = "cli"`.
3. **Session Management** — ✅ `OllamaCliSession` handles stdin, `--keepalive`,
   stdout/stderr, and tool-call parsing.
4. **Validation** — ✅ Unit tests cover argv construction, ANSI stripping,
   tool-call parsing (OpenAI/compact/fenced/bare), session-id reuse, and
   provider selection. Real-binary end-to-end testing remains manual.
