# AgentGate — Free Model Combo / Smart Rotator Spec

**Status**: Draft  
**Author**: OpenCode (lead)  
**Date**: 2026-11-19  
**Target**: `agentgate` — model routing layer

---

## 1. Problem Statement

Free-tier models (9router `free-forever`, `beam-free`, `iceman-free`, etc.) have **daily rate limits** (requests/minute, requests/day, tokens/day). When one model exhausts its quota:

- Current behavior: returns `provider.free-limit` error → request fails
- Desired behavior: **transparent failover** to next available free model
- Auto-recovery: model re-enabled next day when quota resets

---

## 2. Design Goals

| Goal | Requirement |
|------|-------------|
| **Zero-config combo** | Declare `combo = ["free-forever", "beam-free", "iceman-free"]` once |
| **Automatic disable** | Detect quota exhaustion → mark model unavailable for rest of day |
| **Auto-recovery** | Next day 00:00 UTC (or provider-specific reset) → re-enable |
| **Observability** | Metrics: `combo_active_model`, `combo_failover_total`, `combo_quota_reset` |
| **No code changes per provider** | Config-driven, works with any OpenAI-compatible provider |
| **Fail-fast on total exhaustion** | All models in combo exhausted → return clear error to client |

---

## 3. Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    Combo Router (new)                       │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │ free-forever│  │  beam-free  │  │ iceman-free │  ...    │
│  │  (active)   │  │ (standby)   │  │ (disabled)  │         │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘         │
│         │                │                │                 │
│         ▼                ▼                ▼                 │
│  ┌─────────────────────────────────────────────────┐       │
│  │           Quota State Store (per model)         │       │
│  │  { model_id: { disabled_until, error_count,     │       │
│  │                last_error, reset_time_utc } }   │       │
│  └─────────────────────────────────────────────────┘       │
└─────────────────────────────────────────────────────────────┘
```

**Integration point**: Between `route_request` (service.rs) and provider call.

---

## 4. Configuration

### 4.1 New Config Section: `[combo]`

```toml
# agentgate.toml

[combo]
# Enable combo router
enabled = true

# Default combo name (referenced by provider or routing rule)
default_combo = "free-tier"

# Per-model quota reset assumption (used when provider doesn't return reset time)
default_reset_hour_utc = 0        # 00:00 UTC = next day
default_reset_timezone = "UTC"    # or "local"

[[combo.groups]]
name = "free-tier"
# Ordered priority: first = primary, rest = fallbacks
models = [
    "agg/9router/free-forever",
    "agg/9router/beam-free",
    "agg/9router/iceman-free",
    "agg/opencode/beam-free",
]
# Optional: per-combo override
reset_hour_utc = 0
# Optional: max consecutive errors before disable (default 3)
max_errors_before_disable = 3
# Optional: cooldown after disable (minutes), default 1440 (24h)
cooldown_minutes = 1440

[[combo.groups]]
name = "paid-fallback"
models = [
    "agg/9router/ag-poli",
    "openrouter/claude-3.5-sonnet",
]
# Paid models: no auto-disable, just priority order
```

### 4.2 Provider Reference to Combo

```toml
[[provider]]
id = "9router"
# ... existing fields ...
# New: reference combo group instead of static model list
combo_group = "free-tier"
# Or keep models list for backward compat; combo takes precedence if set
```

---

## 5. Core Logic

### 5.1 Model Selection Algorithm

```rust
fn select_model(combo: &ComboGroup, store: &QuotaStore) -> Result<ModelId> {
    for model in &combo.models {
        let state = store.get(model);
        
        // Skip if disabled and not yet reset
        if let Some(disabled_until) = state.disabled_until {
            if Utc::now() < disabled_until {
                continue; // try next model
            } else {
                // Reset window passed → re-enable
                store.reenable(model);
            }
        }
        
        // Model available
        return Ok(model.clone());
    }
    
    // All exhausted
    Err(ComboExhausted { 
        combo: combo.name, 
        models: combo.models.clone(),
        reset_times: store.all_reset_times(&combo.models)
    })
}
```

### 5.2 Quota Exhaustion Detection

Trigger on **any** of:
1. HTTP 429 with `provider.free-limit` / `rate_limit_exceeded` in body
2. Provider-specific error codes (configurable per provider)
3. Response headers: `x-ratelimit-remaining: 0`, `retry-after`

```rust
fn is_quota_exhausted(response: &ProviderResponse) -> bool {
    // 1. HTTP status
    if response.status == 429 { return true; }
    
    // 2. Error body patterns
    let body = response.body.to_lowercase();
    if body.contains("free-limit") 
        || body.contains("rate limit") 
        || body.contains("quota exceeded")
        || body.contains("daily limit") { return true; }
    
    // 3. Headers
    if response.headers.get("x-ratelimit-remaining") == Some("0") { return true; }
    if response.headers.get("retry-after").is_some() { return true; }
    
    false
}
```

### 5.3 Disable & Schedule Recovery

```rust
fn on_quota_exhausted(model: &str, response: &ProviderResponse, combo: &ComboGroup) {
    let mut state = store.get_mut(model);
    
    state.error_count += 1;
    state.last_error = Some(response.error_message());
    
    // Only disable after threshold (avoid flapping on transient 429)
    if state.error_count >= combo.max_errors_before_disable {
        // Parse retry-after or use default reset
        let disabled_until = parse_retry_after(response)
            .unwrap_or_else(|| next_reset_time(combo));
        
        state.disabled_until = Some(disabled_until);
        state.reset_time_utc = Some(disabled_until);
        
        // Emit metric/event
        metrics::increment("combo_failover_total", labels! {model, combo: combo.name});
        info!(event="combo.disable", model, disabled_until=?disabled_until, combo=%combo.name);
    }
}
```

### 5.4 Reset Time Calculation

Priority order:
1. `Retry-After` header (seconds or HTTP-date)
2. Provider-specific known reset (e.g., 9router = 00:00 UTC)
3. Combo config `reset_hour_utc` (default 0)
4. Fallback: 24h from now

```rust
fn next_reset_time(combo: &ComboGroup) -> DateTime<Utc> {
    let now = Utc::now();
    let reset_hour = combo.reset_hour_utc.unwrap_or(0);
    
    let mut next = now.date_naive().and_hms_opt(reset_hour, 0, 0).unwrap().and_utc();
    if next <= now { next += Duration::days(1); }
    next
}
```

---

## 6. State Persistence

### Option A: In-Memory (MVP)
- `HashMap<String, ModelQuotaState>` in `AppState`
- Lost on restart → models re-enabled (safe: false positive = brief retry)
- Simple, zero deps

### Option B: File-Backed (Production)
- `combo_state.json` in workdir
- Survives restart
- Atomic write via temp file + rename

```json
{
  "version": 1,
  "updated_at": "2026-11-19T10:30:00Z",
  "models": {
    "agg/9router/free-forever": {
      "disabled_until": "2026-11-20T00:00:00Z",
      "error_count": 5,
      "last_error": "provider.free-limit: daily quota exhausted",
      "reset_time_utc": "2026-11-20T00:00:00Z"
    },
    "agg/9router/beam-free": {
      "disabled_until": null,
      "error_count": 0,
      "last_error": null,
      "reset_time_utc": "2026-11-20T00:00:00Z"
    }
  }
}
```

**Recommendation**: Start with **Option A (in-memory)**, add file persistence as follow-up.

---

## 7. Metrics & Observability

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `combo_active_model` | Gauge | `combo`, `model` | 1 = currently selected |
| `combo_failover_total` | Counter | `combo`, `from_model`, `to_model` | Failover events |
| `combo_quota_exhausted_total` | Counter | `combo`, `model`, `error_type` | Quota hit detection |
| `combo_model_reenabled_total` | Counter | `combo`, `model` | Auto-recovery events |
| `combo_all_exhausted_total` | Counter | `combo` | All models in combo exhausted |

**Log events** (structured):
```json
{"event":"combo.select","combo":"free-tier","model":"agg/9router/beam-free","reason":"primary_exhausted"}
{"event":"combo.disable","combo":"free-tier","model":"agg/9router/free-forever","disabled_until":"2026-11-20T00:00:00Z"}
{"event":"combo.reenable","combo":"free-tier","model":"agg/9router/free-forever","reason":"daily_reset"}
```

---

## 8. Integration Points

### 8.1 Service Layer (`src/service.rs`)

Modify `route_request` / `call_provider_native_with`:

```rust
// Before calling provider
let model_id = if let Some(combo) = resolve_combo_for_provider(provider_id) {
    combo_router.select_model(combo, &quota_store)?
} else {
    request.model.clone()
};

// After provider response
if is_quota_exhausted(&response) {
    combo_router.on_quota_exhausted(&model_id, &response, combo);
    // Retry with next model (recursive or loop)
    return retry_with_next_model(combo, quota_store, request).await;
}
```

### 8.2 Config Loading (`src/config.rs`)

Add `ComboConfig`, `ComboGroup` structs with serde.

### 8.3 New Module: `src/combo/`

```
src/combo/
├── mod.rs           // Public API: ComboRouter, QuotaStore
├── config.rs        // ComboConfig, ComboGroup deserialization
├── state.rs         // ModelQuotaState, in-memory store
├── detector.rs      // is_quota_exhausted, parse_retry_after
├── scheduler.rs     // next_reset_time, timezone handling
└── metrics.rs       // Prometheus metrics registration
```

---

## 9. Edge Cases & Handling

| Scenario | Behavior |
|----------|----------|
| All models exhausted | Return `503 Service Unavailable` with `Retry-After` = earliest reset |
| Model recovers mid-day (manual) | Admin API: `POST /_admin/combo/reenable?model=...` |
| Provider returns `Retry-After: 3600` | Disable for 1h, not 24h (respect provider signal) |
| Clock skew / DST | Use UTC everywhere; `reset_hour_utc` is UTC |
| Combo with mixed free/paid | Paid models never auto-disabled (error_count ignored) |
| Concurrent requests hitting limit | Atomic state update via `Mutex`/`RwLock`; first exhausts, rest failover |
| Config reload | Rebuild combo groups; preserve quota state for unchanged models |

---

## 10. Implementation Plan (Phased)

### Phase 1: Core (Week 1)
- [ ] `src/combo/` module skeleton
- [ ] Config parsing (`ComboConfig`, `ComboGroup`)
- [ ] In-memory `QuotaStore` with `ModelQuotaState`
- [ ] `is_quota_exhausted()` detector
- [ ] `next_reset_time()` scheduler (UTC, configurable hour)
- [ ] Integration in `service.rs` model selection
- [ ] Basic metrics (counters + gauges)

### Phase 2: Resilience (Week 2)
- [ ] File-backed state persistence (`combo_state.json`)
- [ ] Admin API: `/_admin/combo/status`, `/_admin/combo/reenable`
- [ ] Per-provider error pattern config
- [ ] Unit tests: detector, scheduler, state machine

### Phase 3: Polish (Week 3)
- [ ] Structured logging integration
- [ ] Dashboard / Grafana dashboard JSON
- [ ] Documentation: `docs/spec_free_model_combo.md` (this file)
- [ ] Load test: simulate quota exhaustion burst

---

## 11. Config Example (Complete)

```toml
# agentgate.toml

[combo]
enabled = true
default_combo = "free-tier"
default_reset_hour_utc = 0
default_reset_timezone = "UTC"

[[combo.groups]]
name = "free-tier"
models = [
    "agg/9router/free-forever",
    "agg/9router/beam-free", 
    "agg/9router/iceman-free",
    "agg/opencode/beam-free",
]
reset_hour_utc = 0
max_errors_before_disable = 3
cooldown_minutes = 1440

[[combo.groups]]
name = "premium-fallback"
models = [
    "agg/9router/ag-poli",
    "openrouter/claude-3.5-sonnet",
    "openrouter/gpt-4o-mini",
]
# No auto-disable for paid

[combo.error_patterns]
# Custom patterns per provider (optional)
"9router" = ["free-limit", "daily quota", "rate limit"]
"openrouter" = ["rate limit", "quota exceeded", "429"]
"ollama" = []  # Local, no quota

[[provider]]
id = "9router"
combo_group = "free-tier"
# models = [...]  # ignored when combo_group set
```

---

## 12. Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| False positive disable (transient 429) | `max_errors_before_disable = 3` default; configurable |
| Provider changes error format | Configurable `error_patterns` per provider; fallback to 429 + headers |
| Clock drift causes early/late re-enable | Use UTC; log actual reset time; admin override |
| Combo exhaustion storms client | Return `Retry-After` header; client should backoff |
| State loss on restart (in-memory) | Phase 2: file persistence; in-memory = fail-open (safe) |

---

## 13. Future Extensions

- **Weighted routing**: Distribute load across available models (not just priority)
- **Token-aware**: Track token usage per model, not just request count
- **Multi-region**: Different reset times per region (e.g., US vs EU endpoints)
- **ML-based prediction**: Learn quota patterns, pre-emptively rotate
- **Cross-combo fallback**: free-tier exhausted → paid-fallback (with cost alert)

---

## 14. Acceptance Criteria

1. **Config-only setup**: Add `[combo]` section → works without code changes
2. **Transparent failover**: Client sees single model; combo handles rotation
3. **Auto-recovery**: Model disabled at 14:00 UTC → re-enabled 00:00 UTC next day (or per `Retry-After`)
4. **Metrics visible**: `combo_failover_total` increments on each rotation
5. **Total exhaustion**: All free models dead → 503 with `Retry-After` = earliest reset
6. **Zero regression**: Existing single-model providers work unchanged

---

## 15. References

- `QUOTA_LATENCY_AUDIT.md` — root cause analysis of free-limit issues
- `src/provider.rs:1479` — hardcoded 10-tool limit for free models
- `src/provider/bridge/opencode.rs:801` — `is_free_model()` detection
- `src/service.rs:1238` — `provider_call_timeout_secs()`
- OpenAI API: `Retry-After`, `X-RateLimit-*` headers
- 9router free tier: daily reset 00:00 UTC (observed)