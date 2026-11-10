# Provider Compatibility Mapping — 2026-11-09

**Purpose:** Document capability + `reasoning_effort` mapping per CLI provider. Templates in `compat/*.json` are consumed by runtime loader (not yet implemented — see gaps below).

---

## 1. Capability Matrix (from `docs/tooling_conformity_matrix.md`)

| Capability | opencode | claude | gemini | ollama |
|---|---|---|---|---|
| `chat` | ✅ | ✅ | ✅ | ✅ |
| `tools` | ✅ | ✅ | ✅ | ✅ |
| `streaming` | ✅ | ✅ | ✅ | ✅ |
| `reasoning` | ✅ | ✅ | ❌ | ❌ |
| `reasoning_effort` | ✅ | ✅ | ❌ | ❌ |
| `mcp` | ✅ | ✅ | ✅ | ❌ |
| `strict_mcp_config` | ❌ | ✅ | ❌ | ❌ |

---

## 2. reasoning_effort Mapping

### opencode
```json
"reasoning_effort": {
  "supported": true,
  "values": ["low", "medium", "high"],
  "default": "medium",
  "cli_flag": "--reasoning",
  "spec_note": "opencode v1 reasoning; maps to OpenAI `reasoning.effort`"
}
```

### claude
```json
"reasoning_effort": {
  "supported": true,
  "values": ["low", "medium", "high"],
  "default": "medium",
  "cli_flag": "--reasoning-effort",
  "spec_note": "Claude 3.5+ thinking budget; maps to OpenAI `reasoning.effort`"
}
```

### gemini
```json
"reasoning_effort": {
  "supported": false,
  "values": [],
  "default": null,
  "spec_note": "Gemini CLI does not expose reasoning effort control"
}
```

### ollama
```json
"reasoning_effort": {
  "supported": false,
  "values": [],
  "default": null,
  "spec_note": "Ollama native does not expose reasoning effort; model-dependent"
}
```

---

## 3. Runtime Loader — GAPS (not implemented)

| Gap | Description | Impact |
|---|---|---|
| **No loader** | `compat/*.json` templates exist but no runtime code loads them | Capability/reasoning_effort mapping not enforced at startup |
| **No validation** | Provider startup does not verify declared capabilities match actual CLI | Mismatch may cause runtime errors |
| **No hot-reload** | Config changes require restart | Operational friction |
| **No schema enforcement** | JSON templates not validated against schema | Typos in templates silent |

**Spec ref:** Audit §12.2-12.4 (capability declaration), P1c compat maps.

**Tracking:** To be implemented when runtime capability negotiation is spec'd.

---

## 4. Adapter-Specific Notes

### opencode
- MCP isolation: `OPENCODE_CONFIG_CONTENT` env var (P1a done)
- Reasoning: `--reasoning v1` (not `--reasoning-effort`)
- Built-in tools: disabled via empty config

### claude
- MCP isolation: `--mcp-config <file> --strict-mcp-config` (P1a done)
- Reasoning: `--reasoning-effort` (Claude 3.5+)
- Built-in tools: disable via `--tools ""` (P1 seq3 pending)
- Windows: `.cmd` wrapper handling needed (P1-3)

### gemini
- MCP isolation: `GEMINI_CLI_SYSTEM_SETTINGS_PATH` file (P1a done)
- Reasoning: not supported
- Built-in tools: disable via `excludeTools` in settings (P1 seq3 pending)
- Windows: `.cmd` wrapper handling needed (P1-3)

### ollama
- MCP: not supported
- Reasoning: model-dependent (not CLI-controlled)
- Native HTTP bridge available (`src/provider/ollama_native.rs`)

---

## 5. Files Reference

| File | Purpose |
|---|---|
| `compat/claude-1.0.0.json` | Full capability + reasoning_effort + MCP config template |
| `compat/opencode-1.0.0.json` | Full capability + reasoning_effort + MCP config template |
| `compat/gemini-1.0.0.json` | Full capability + reasoning_effort + MCP config template |
| `compat/ollama-1.0.0.json` | Capability template (no MCP, no reasoning_effort) |
| `docs/tooling_conformity_matrix.md` | Full spec compliance matrix |
| `src/provider/bridge/*.rs` | Adapter implementations (source of truth for runtime behavior) |

---

## 6. Update Protocol

When modifying capability mappings:
1. Update `compat/<provider>-1.0.0.json`
2. Update this doc (§2, §4)
3. Update `docs/tooling_conformity_matrix.md`
4. Note in `docs/spec_tooling_progress.md` with date
5. If runtime loader exists: bump version in template