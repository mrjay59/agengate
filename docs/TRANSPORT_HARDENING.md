# Transport incident analysis & hardening (Phase 5)

**Owner:** QA & Hardening
**Date:** 2026-11-27
**Status:** implementation complete; `cargo check --lib` clean, `cargo test` fully green (131 lib tests + all integration suites, including 8 new `transport_hardening_test.rs` tests). No deploy.
**Deployment:** NOT deployed (per lead/user instruction).

---

## 1. Reported incident

User-reported error strings:

- `provider_error: Transport: ECONNREFUSED: Unable to connect. Is the computer able to access the url?`
- `provider_error: Transport: ECONNRESET: The socket connection was closed unexpectedly …`
- `Error: Broken pipe (os error 32)`
- `provider_error: Standalone server exited before reporting readiness`
- Hypothesis from user: "too many requests at once".

These strings are **upstream/CLI transport failures surfaced as `AppError::ProviderError`**, not AgentGate-internal panics. They originate from:

| Path | Where | Mechanism |
|---|---|---|
| CLI bridge spawn | `src/provider/bridge/opencode.rs:399-439` | `Command::spawn()` fails with EAGAIN/EPERM under process pressure; bounded retry 4 attempts, backoff 250/500/750 ms. |
| opencode `serve` readiness | `src/provider/bridge/opencode.rs` (child lifecycle) | CLI exits before its MCP/serve endpoint is ready; error text flows back through `RunEvent::Error`. |
| Broken pipe / reset | `src/service.rs` stream forwarder (`RunEvent::Error`) | Client or CLI child closes the pipe mid-write; `os error 32` / `os error 104`. |
| HTTP upstream (Ollama native / new HTTP provider) | `src/provider/ollama_native.rs`, `src/provider/http.rs` | `reqwest` connect/reset/timeout; 429/5xx status responses. |
| Client disconnect | `src/api.rs` stream task (`tx.closed()`) | Previously could orphan CLI children; now drops receiver → provider kills child. |

Conclusion: "many requests" is a plausible *trigger* (process/socket pressure → EAGAIN/ECONNREFUSED), but it is not the only cause; the same errors occur with a single request when the upstream/CLI is down or not ready. Hardening therefore covers classification, bounded retry, backpressure, and cleanup together.

---

## 2. What was added

### 2.1 New module `src/transport.rs`

- `TransportErrorClass`: `ConnectionRefused`, `ConnectionReset`, `BrokenPipe`, `Dns`, `Tls`, `Timeout`, `Quota429`, `Server5xx`, `Readiness`, `Io`, `Aborted`, `Other`.
- `classify_message(&str)`, `classify_status(u16)`, `classify_reqwest(&reqwest::Error)`, `is_readiness_error(&str)`.
  - All five reported incident strings classify as transient (regression tests included).
  - Quota (429 / free-limit) is deliberately **not** transport-retryable — it goes through the quota-aware failover layer.
- `RetryPolicy` (default: 2 retries, 250 ms base, 4 s cap, ±25% jitter) with deterministic `delay_for_attempt()` for tests and `jittered_delay_for_attempt()` for production.
- `retry_with_backoff()` — bounded attempts, skips non-retryable classes, honours `Retry-After` (capped at 8× max delay), logs structured `target: "agentgate.transport"` events with `error_class`, `provider`, `model`, `attempt`, `delay_ms`, redacted error.
- `failover()` — ordered endpoint fallback (proxy pool / LB) with per-endpoint retry and structured decision logs.
- `redact_url()` / `redact_diagnostics()` — strips proxy userinfo, `Bearer` tokens, `agk_*`/`nvapi-*` keys, secret query params and key/value pairs.
- `SseParser` / `is_sse_done()` — incremental SSE decoder handling chunk splits, CRLF, comments, multi-line `data:` and `[DONE]`.
- `TransportFailure` with `status()`, `with_status()`, `with_retry_after()`; `from_reqwest` accepts owned **or** borrowed errors (used by both HTTP providers).

### 2.2 Wiring into existing code

- `src/service.rs` `is_transient_bridge_error()` now delegates to the shared classifier; all previously matched strings remain retryable, plus DNS/timeouts/5xx variants (FIX-TRANSPORT-CLASSIFY).
- `src/service.rs` stream retry and terminal error logs now carry `error_class`, `provider`, `model`, `attempt`, and redacted `error`, and increment `agentgate_errors_total{component="bridge_transport_*",error_type=<class>}`.
- `src/provider/ollama_native.rs` `get_models`/`forward_chat`/both stream paths now use `send_with_retry()`: bounded retry, status classification, `Retry-After`, HTTP-upstream metrics, redacted diagnostics (FIX-TRANSPORT-HARDENING).
- `src/metrics.rs` new spec §14 series (all additive): `agentgate_http_upstream_requests_total`, `..._errors_total`, `..._latency_ms{provider,model}` plus sum/count, `agentgate_quota_tokens_total`, `..._bytes_total`, `..._denied_total`, `agentgate_key_usage_total`, `agentgate_proxy_selected_total`, `agentgate_proxy_errors_total` (alias `record_proxy_errors`), `agentgate_lb_selection_total`, `agentgate_lb_fallback_total`. Quota/keystore teammates call these via `record_*` methods with the agreed signatures.
- `src/api.rs` fixed E0506 in stream usage accounting by replacing the capturing closure with a local macro (FIX-TRANSPORT-BUILD). Semantics unchanged.

### 2.3 Existing guards verified (no duplicate work needed)

- **Backpressure/concurrency:** `src/service.rs:1799-1851` per-provider `Semaphore` with 30 s overall deadline, 500 ms per-try, 200 ms→2 s backoff; `ProviderPermit` RAII releases the gauge. Stream channel is bounded at 128 (`src/service.rs:1875-1881`). `max_runs_per_key` / parked-run accounting prevents run leaks.
- **Circuit breaker equivalent:** quota-disabled model state + watcher (`src/service.rs` `QuotaState`) disables a model on first free-limit notification and reroutes/falls back to `opencode/9router/free-forever`.
- **Cleanup:** `src/provider/bridge/opencode.rs:407` `kill_on_drop(true)`; process-tree kill on Linux/Windows (`:1290-1320`, `:1426-1453`, `:1565-1592`); MCP shim idle timeout 300 s (`src/provider/bridge/shim.rs:113-121,191`). Client disconnect closes the event channel and triggers child kill (`src/api.rs:480-490`).
- **Spawn retry:** `src/provider/bridge/opencode.rs:399-439` already retries EAGAIN/EPERM with bounded backoff.

No new global circuit breaker was introduced: the existing per-model quota circuit breaker + per-provider semaphore already cover the burst scenario without adding a second concurrency authority.

---

## 3. Tests added

- Unit tests in `src/transport.rs`: classification of all incident strings, status mapping, retryable set, readiness helper, exponential/capped backoff, redaction of proxy credentials/keys, `Retry-After` parsing (seconds + HTTP-date), SSE split-frame parsing, retry success/non-retryable/`Retry-After`, endpoint failover.
- Regression test in `src/service.rs::transport_regression_tests`: the exact five incident messages are transient; quota/tool errors are fatal.
- Integration tests `tests/transport_hardening_test.rs` (wiremock):
  - mock upstream 429 + `Retry-After` → retry succeeds;
  - quota 429 fails fast (no transport retry);
  - mock SSE stream → events + `[DONE]`;
  - ECONNREFUSED (closed port) → failover to healthy mock upstream;
  - proxy pool 503 → second proxy succeeds, proxy metrics recorded;
  - `/_metrics` exposes all new series.

---

## 4. Build triage (parallel-edit conflicts)

Observed while teammates were editing the same working tree; all outside my scope:

| Area | Symptom | Owner | Status |
|---|---|---|---|
| `src/metrics.rs` | duplicate quota_* fields from concurrent edits | me + quota teammate | resolved; quota teammate removed duplicates, metrics.rs owned by me |
| `src/provider/ollama_native.rs` / `tests/transport_hardening_test.rs` | closure `?` + `map_err(fn)` arity in my first pass | me | fixed |
| `src/api.rs:415` | E0506 capturing closure in stream usage accounting | quota teammate area | fixed by me (macro), semantics unchanged |
| `src/gateway/store.rs` | `IpAddr::is_private`, `&self` mutation, missing field | Proxy/LB teammate | reported, being fixed |
| `src/keystore.rs` | `rng.fill`, rusqlite `Value` args | Quota/Key Store teammate | reported, fixed by them |
| `src/service.rs` | `LbCandidate` imports during gateway wiring | Proxy/LB teammate | reported, fixed by them |
| `src/provider/http.rs` | closure `?`, `status()` API, `FnMut` capture, reqwest proxy feature | Gateway Runtime teammate | API ergonomics added on my side (`from_reqwest` owned+borrowed, `status()`); teammate fixed remaining errors |
| `src/provider/combo.rs` | `cycle_prevention` test failed transiently while combo module landed | Gateway Runtime teammate | resolved; full suite green |
| `tests/transport_hardening_test.rs` | `attempts` move-out of `FnMut` + borrowed `&str` escaping async future | me | fixed (Arc counter, owned endpoint strings) |
| `src/transport.rs` unit tests | EOF event fields lost between `push()` calls; quota class used in `Retry-After` test; `Retry-After` cap too low | me | fixed (persistent pending event, retryable class, larger cap) |

Final verification: `cargo test` → `test result: ok. 131 passed; 0 failed` (lib) and every integration binary `ok`, including `transport_hardening_test` 8/8.

No deploy performed.
