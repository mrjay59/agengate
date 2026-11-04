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
- [ ] CLI bridge (`OllamaCliProvider` implementing `BridgeProviderV2`) — pending.
- [ ] `ollama_mode: "http" | "cli"` selector in `ProviderConfig` — pending.
- [ ] `OllamaCliSession` for stdin interactions — pending.

## Configuration

```toml
[provider.ollama]
enabled        = true
base_url       = "http://127.0.0.1:11434"
num_ctx        = 16384
keep_alive     = "10m"
prompt_tools_fallback = false
# mode = "http"   # future: "http" | "cli"
```

## Binary Availability (CLI mode only)

When CLI mode is eventually implemented, availability must be checked with the
same mechanism as other CLI providers (`CliProvider::check_available`), which
probes `PATH` (and Windows extensions `.cmd/.bat/.exe/.ps1`). HTTP mode does not
depend on the binary.

Integration tests should cover both cases:

- `check_available()` returns `true` when `ollama` is installed.
- `check_available()` returns `false` gracefully when it is not (no panic).

## Roadmap (RS9)

1. **Bridge Adaptation** (pending): Implement `src/provider/bridge/ollama_cli.rs`
   extending `BridgeProviderV2`.
2. **Provider Selector** (pending): Update `BridgeProviderFactory` to support
   CLI-based Ollama selection.
3. **Session Management** (pending): Implement `OllamaCliSession` for `stdin`
   interactions.
4. **Validation** (in progress): Unit/integration tests for binary check, stream
   parsing, and error handling. HTTP path already covered by `ollama_native`
   usage in integration tests; CLI binary check to be added to
   `tests/provider_detection.rs`.
