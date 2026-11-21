# AgentGate — Quota & Latency Audit Report

**Project**: `C:\Users\EDN\Documents\AION\project\agentgate`
**Focus**: Free-tier quota exhaustion (`provider.free-limit`) and request latency
**Date**: 2026-11-14

---

## 1. Executive Summary

- **Quota exhaustion** is driven by 3 compounding factors:
  1. Free models are hard-limited to **10 injected tools** (vs 128 for paid), causing *more CLI spawns* and *faster quota burn*.
  2. `minimal_free_model` defaults to `true`, which skips the `permission` block entirely — but `is_free_model()` detection is **broad** (any model containing "free"), so non-free models can be misclassified.
  3. `max_tools` check is **skipped** for free models (`service.rs:576-585`), allowing oversized tool sets to reach the provider unfiltered.

- **Latency** is driven by:
  1. `first_token_timeout` default = **30s** (config.rs default function) vs **120s** in generated config — inconsistent, causing premature timeout on cold starts.
  2. `provider_call_timeout` default = **30s** — too tight for free models that are slow to respond.
  3. `parked_holds_permit = true` + semaphore held for entire turn (including tool-wait) → **permit starvation** under concurrency.
  4. `scan_timeout = 5s` and `ollama probe = 2s` — tight, can cause stale fallback churn.

---

## 2. Quota / Free-Limit Findings

### F1 — Hardcoded 10-tool injection for free models (P0)
**File**: `src/provider.rs`
**Lines**: 1479-1486, 1710-1717

```rust
let is_free_model = model.to_lowercase().contains("-free") || model.to_lowercase() == "free";
let max_inject = if is_free_model { 10 } else { 128 };
```

**Impact**: Free models get only 10 tools → client with >10 tools triggers *truncation* → model re-requests missing tools → more CLI spawns → faster quota burn.

**Fix**: Remove `is_free_model` gate or make `max_inject` configurable via `[bridge.limits].max_tools` (default 128 for all).

---

### F2 — `minimal_free_model` defaults to `true` (P0)
**File**: `src/config.rs`
**Lines**: 244-247, 250-259

```rust
#[serde(default = "default_true")]
pub minimal_free_model: bool,
```

**Impact**: When `true`, skips `permission` block in `opencode.json` to avoid `provider.free-limit`. But this also disables isolation enforcement for free models.

**Fix**: Default to `false` and rely on `is_free_model()` detection + minimal config only when actually needed.

---

### F3 — Broad free-model detection (P1)
**File**: `src/provider/bridge/opencode.rs`
**Lines**: 801-807

```rust
fn is_free_model(model: &str) -> bool {
    let lower = model.to_lowercase();
    lower.contains("-free") || lower.contains("free") && !lower.contains("non-free")
}
```

**Impact**: Any model name containing "free" is treated as free — including paid tiers that happen to have "free" in the name (e.g. `free-tier-pro`).

**Fix**: Use explicit allowlist or config-driven `free_models` list instead of substring matching.

---

### F4 — `max_tools` check skipped for free models (P1)
**File**: `src/service.rs`
**Lines**: 576-585

```rust
let is_free_model = request.model.to_lowercase().contains("-free") || request.model.to_lowercase() == "free";
if !is_free_model && tools.len() > limits.max_tools {
    return Err(AppError::ToolError(...));
}
```

**Impact**: Free models bypass the 128-tool safety check, allowing oversized tool sets to reach the provider.

**Fix**: Apply `max_tools` uniformly; rely on truncation (F1) only as a last resort.

---

### F5 — `max_runs_per_key` default = 4 (P2)
**File**: `src/bridge.rs`
**Lines**: 775-792

```rust
if current_runs >= self.config.max_runs_per_key { ... }
```

**Impact**: Free-tier users can spawn 4 concurrent runs per `workspace:model` key, accelerating quota burn.

**Fix**: Default `max_runs_per_key = 1` for free models, or make it configurable per provider.

---

## 3. Latency Findings

### L1 — `first_token_timeout` default = 30s vs 120s in config (P0)
**File**: `src/config.rs`
**Lines**: 324-329

```rust
fn default_first_token_timeout() -> u64 { 30 }
```

**Impact**: When no config file is present, default is 30s — too tight for cold model loads. Premature timeout → client retries → more load.

**Fix**: Align default to 120s (match generated config).

---

### L2 — `provider_call_timeout` default = 30s (P1)
**File**: `src/service.rs`
**Lines**: 1232-1240

```rust
pub fn provider_call_timeout_secs() -> u64 {
    ...unwrap_or(30)
}
```

**Impact**: Free models are slow; 30s timeout causes premature failure → retry → more quota burn.

**Fix**: Default to 60s (or make configurable per model tier).

---

### L3 — Semaphore permit held for entire turn (P1)
**File**: `src/service.rs`
**Lines**: 930-931, 1055-1081

```rust
let _permit = self.acquire_provider_permit(provider_id).await?;
// ... entire new_bridge_run including tool-wait
```

**Impact**: Permit held during `bridge_turn_timeout()` (600s) while waiting for client tool results. With `max_concurrency_per_provider = 2`, two concurrent tool turns block all permits → starvation.

**Fix**: Release permit before tool-wait; re-acquire on resume. Or set `parked_holds_permit = false` by default.

---

### L4 — `scan_timeout = 5s` and `ollama probe = 2s` (P2)
**File**: `src/service.rs`
**Lines**: 344, 464

```rust
let scan_timeout = Duration::from_secs(5);
match tokio::time::timeout(Duration::from_secs(2), ollama.get_models()).await { ... }
```

**Impact**: Tight timeouts cause scan failures → stale fallback churn → repeated CLI spawns.

**Fix**: Increase to 10s / 5s, or make configurable.

---

### L5 — `stream_idle_timeout` default = 30s (P2)
**File**: `src/api.rs`
**Lines**: 1096-1103

```rust
.unwrap_or_else(|| Duration::from_secs(30))
```

**Impact**: Free-tier quota errors that never terminate the process used to hang for 2 minutes; now 30s. Good, but 30s may still be too long for fast-fail.

**Fix**: Keep 30s; ensure error events are emitted promptly (already done).

---

## 4. Recommendations (Priority Order)

| Priority | Action | Files |
|----------|--------|-------|
| P0 | Remove hardcoded 10-tool limit for free models; use 128 uniformly | `src/provider.rs` |
| P0 | Align `first_token_timeout` default to 120s | `src/config.rs` |
| P0 | Set `minimal_free_model = false` by default | `src/config.rs` |
| P1 | Apply `max_tools` check uniformly (don't skip for free) | `src/service.rs` |
| P1 | Increase `provider_call_timeout` default to 60s | `src/service.rs` |
| P1 | Release semaphore permit before tool-wait | `src/service.rs` |
| P2 | Replace broad `is_free_model()` with allowlist | `src/provider/bridge/opencode.rs` |
| P2 | Increase `scan_timeout` to 10s, `ollama probe` to 5s | `src/service.rs` |
| P2 | Default `max_runs_per_key = 1` for free models | `src/bridge.rs` |

---

## 5. Verification Checklist

- [ ] Build succeeds: `cargo build --release`
- [ ] `cargo test` passes (especially `align_args_tests`, `unalias_tests`)
- [ ] Run `agentgate serve` with `minimal_free_model = false` and verify no `provider.free-limit` errors
- [ ] Monitor `/_metrics` for `agentgate_cli_exit_total{code="1"}` reduction
- [ ] Monitor TTFT (`agentgate_ttft_seconds_*`) for improvement
- [ ] Test with `agg/opencode/beam-free` model end-to-end

---

## 6. Files Audited

- `src/provider.rs` — tool injection limits, timeouts, binary resolution
- `src/config.rs` — defaults, env overrides, isolation config
- `src/service.rs` — model scanning, concurrency, routing, latency guards
- `src/api.rs` — SSE streaming, idle timeout, first-token timeout
- `src/bridge.rs` — run lifecycle, parked runs, preflight isolation
- `src/provider/bridge/opencode.rs` — free-model detection, config injection
- `src/openai/tools.rs` — tool name aliasing, argument alignment
- `src/reasoning/limit.rs` — reasoning truncation (not quota-related)
- `src/metrics.rs` — observability (no changes needed)
