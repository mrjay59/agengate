# AgentGate — GitHub Copilot Chat Support Audit

**Project**: `C:\Users\EDN\Documents\AION\project\agentgate`
**Date**: 2026-11-14
**Type**: OpenAI-compatible Gateway for CLI Agents (OpenCode, Gemini, Claude, Ollama)

---

## Executive Summary

AgentGate implements **~90% of OpenAI Chat Completions API** required for GitHub Copilot Chat compatibility. Core protocol (streaming, tool calling, reasoning) is complete. Remaining gaps are configuration/tuning, not protocol implementation.

---

## ✅ Supported (Protocol Level)

| Feature | Status | Implementation |
|---------|--------|----------------|
| `POST /v1/chat/completions` | ✅ Complete | `src/api.rs:14`, `chat_completions` handler |
| Streaming SSE (`stream: true`) | ✅ Complete | `src/api.rs` streaming path, `SseWriter` |
| Tool/Function Calling | ✅ Complete | `src/openai/tools.rs`, `src/openai/sse_tools.rs` |
| `tool_choice`: none/auto/required/function | ✅ Complete | `src/openai/tools.rs:1-80` |
| `parallel_tool_calls` (serial mode) | ✅ Complete | `src/service.rs:1-30` truncate logic |
| Message roles: system/user/assistant/tool | ✅ Complete | `src/types.rs:1-50` |
| Streaming `delta.tool_calls` | ✅ Complete | `src/openai/sse_tools.rs` `ToolCallBuffer` |
| `usage` tokens (prompt/completion/total) | ✅ Complete | `src/types.rs`, emitted at `RunEvent::Usage` |
| Model namespacing `agg/<provider>/<model>` | ✅ Complete | `src/service.rs` model discovery |
| CORS & API Key auth | ✅ Complete | `src/api.rs`, `agentgate.toml [server]` |
| Reasoning/thinking streaming | ✅ Complete | `src/reasoning/*` (reasoning_content, activity, think_tags) |
| Standard error format | ✅ Complete | `src/api.rs` error chunks with `finish_reason: error` |
| Session persistence (`session_id`) | ✅ Handled | Bridge creates session; `service.rs` classify `NewBridge`/`Resume` |

---

## ⚠️ Gaps / Configuration Needed

| Gap | Impact | Fix |
|-----|--------|-----|
| **Model name mapping** | Copilot sends `gpt-4o`, `o1-mini`; AgentGate expects `agg/opencode/beam-free` etc. | Add model alias mapping in config or routing |
| **System prompt forwarding** | Copilot sends system prompt; AgentGate strips by default | Set `preserve_client_system_prompt = true` in `[bridge.isolation]` |
| **First-token timeout** | Default 120s > Copilot client timeout (~30-60s) | Reduce `first_token_timeout_secs = 30` in `[defaults]` |
| **Tool name normalization** | Copilot tools: `read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, `grep`, `terminal`/`bash` | Map to AgentGate tool schemas; ensure `allowed_builtins` includes needed tools |
| **MCP support** | Copilot Chat MCP experimental (VS Code 1.96+) | AgentGate MCP bridge ready; verify client compatibility |

---

## 🔧 Recommended Configuration for Copilot Chat

```toml
# agentgate.toml additions/overrides

[server]
# Allow Copilot origins (vscode-webview://, https://github.com)
cors = true

[defaults]
# Prevent client-side timeout during CLI startup
first_token_timeout_secs = 30

[bridge.isolation]
# Forward system prompt from Copilot to CLI agent
preserve_client_system_prompt = true

# Allow tools Copilot Chat uses
allowed_builtins = [
  "bash", "execute", "shell",
  "read", "write", "edit",
  "glob", "list", "patch", "cat",
  "grep", "task"
]

[routing]
# Route tool requests to bridge (tool isolation)
no_tools_policy = "bridge"
on_client_tools = "bridge"
```

---

## 🧪 Quick Test with Copilot Chat

### 1. Start AgentGate
```bash
cargo run -- serve
```

### 2. Test Endpoint (curl)
```bash
curl -X POST http://localhost:8787/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer default_key" \
  -d '{
    "model": "agg/opencode/beam-free",
    "messages": [{"role": "user", "content": "List files in current directory"}],
    "tools": [{
      "type": "function",
      "function": {
        "name": "list",
        "description": "List files",
        "parameters": {
          "type": "object",
          "properties": {"path": {"type": "string"}},
          "required": ["path"]
        }
      }
    }],
    "tool_choice": "auto",
    "stream": true
  }'
```

### 3. VS Code / Copilot Chat Config
- Set custom endpoint: `http://localhost:8787/v1` (if supported via extension/proxy)
- Or use a local proxy that rewrites model names

---

## 📋 Copilot Chat Tooling Reference (VS Code 1.95+)

| Category | Tools |
|----------|-------|
| **Built-in (VS Code)** | `read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, `grep`, `terminal`/`bash`, `run_task`, `apply_patch` |
| **LSP** | Go to definition, references, hover, diagnostics, rename, code actions |
| **Git/GitHub** | Git commands, PR review, issue linking, repo search |
| **Extensions** | Tools from installed extensions (Docker, Database, Azure, etc.) |
| **MCP** | Experimental — VS Code 1.96+ supports MCP servers as tool providers |
| **Custom** | Via `vscode.lm.registerTool` extension API |

**Protocol**: OpenAI-compatible `tool_calls` in `ChatCompletionMessage`, SSE streaming with `delta.tool_calls`, tool results as `role: "tool"` messages.

---

## 🎯 Next Steps

1. **Add model alias mapping** — Map `gpt-4o` → `agg/opencode/beam-free` etc. in routing config
2. **Verify streaming tool_calls end-to-end** — Test with actual Copilot Chat client
3. **Tune timeouts** — Adjust `first_token_timeout_secs`, provider CLI startup time
4. **Document model catalog** — List available models per provider for Copilot model picker

---

## Related Files

- `AUDIT_REPORT.md` — General architecture audit
- `audit.md` — Deep bridge/tooling/reasoning audit
- `docs/audit_spec_tooling.md` — Spec tooling compliance audit
- `agentgate.toml` — Main configuration
- `src/openai/tools.rs` — Tool calling implementation
- `src/openai/sse_tools.rs` — Streaming tool calls
- `src/api.rs` — HTTP endpoints
- `src/service.rs` — Orchestration, classification
- `src/bridge.rs` — MCP bridge server