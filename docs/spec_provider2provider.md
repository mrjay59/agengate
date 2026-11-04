# Specification: Provider-to-Provider Bridge (Hybrid Mode)

## Overview
Standardizes support for dual-mode interaction (HTTP API + CLI Bridge) across all supported providers (OpenCode, Gemini, Claude, Ollama).

## Architecture
- **BridgeProviderV2**: Interface covering both HTTP and CLI invocation.
- **Mode Switching**: Configurable via `agentgate.toml` `mode` field ("http" | "cli").

## Provider Status
| Provider | HTTP | CLI Bridge |
| :--- | :--- | :--- |
| **Ollama** | Supported | Supported |
| **OpenCode** | To Do | Supported |
| **Gemini** | To Do | Supported |
| **Claude** | To Do | Supported |

## Implementation Task (Roadmap RS10)
1. **Bridge Refactor**: Extend `BridgeProviderFactory` to inject client mode.
2. **Standardization**: Ensure `HttpProvider` and `CliBridge` share the same `BridgeSession` trait.
3. **Docs**: Maintain consistency in `agentgate.toml`.
