# AgentGate Fix Taskboard

**Source**: AUDIT_REPORT.md
**Status**: ✅ **P0, P1 & P2 COMPLETE**

---

## P0 - Critical (Quota/Free Limit) ✅ ALL DONE

- [x] **T01**: Reduce `max_inject` from 128 → 10 for free models (`src/provider.rs`)
- [x] **T02**: Change `minimal_free_model` default from `true` → `false` (`src/config.rs`)
- [x] **T03**: `max_runs_per_key` is already 1 via `default_1()` (`src/bridge.rs`)
- [x] **T04**: Add `max_inject` config to `[bridge.limits]` (`src/config.rs` + `agentgate.toml`)

## P1 - High (Latency) ✅ ALL ALREADY IMPLEMENTED

- [x] **T05**: `bridge_turn_timeout` = 120s (was 300s) - **Already 120s**
- [x] **T06**: `first_token_timeout` = 30s (was 120s) - **Already 30s**
- [x] **T07**: `provider_call_timeout` = 15s (was 30s) - **Already 15s**
- [x] **T08**: Background cache warm at startup - **Already implemented**

## P2 - Medium ✅ ALL DONE

- [x] **T09**: Implement Ollama native tool calling (`src/provider/ollama_native.rs`) - **Already implemented** (lines 269-278, 288-297, 328-338)
- [x] **T10**: Add config validation on load (`src/config.rs`) - **Done** (validate() method)
- [x] **T11**: Add more granular metrics (`src/metrics.rs`) - **Done**

---

## Summary of Changes

| File | Change |
|------|--------|
| `src/config.rs` | Added `max_inject` field to `LimitsConfig` (default 10), added `default_10()` function, fixed `minimal_free_model` serde default, added `validate()` method |
| `src/provider.rs` | Free models (`*-free` suffix) now use `max_inject` (10) instead of `max_tools` (128) |
| `agentgate.toml` | Added `max_inject = 10` to `[bridge.limits]` example config |
| `src/metrics.rs` | Added granular metrics: HTTP requests, provider calls, bridge sessions, continuations, cache hits/misses, tool injections, errors + helper methods |

## Verification

```bash
cargo check           # ✅ Passes (warning only)
cargo run -- doctor   # ✅ Runs diagnostics with config validation
cargo run -- models   # ✅ Shows free models (iceman-free, longcat-2.5-free, etc.)
```

## Impact

**Free models** (beam-free, iceman-free, longcat-2.5-free, etc.) will now only receive **10 tools** instead of **128**, preventing `provider.free-limit` quota exhaustion.

**Paid models** continue to receive full **128 tools**.

**Latency** timeouts were already at optimal values (120s/30s/15s).

**Config validation** now catches invalid configs at startup.

**Metrics** now expose granular data for observability (Prometheus format via `render_prometheus_full()`).