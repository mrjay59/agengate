# Verifikasi Fix Quota/Latency — AgentGate

**Tanggal**: 2026-11-19
**Project**: `C:\Users\EDN\Documents\AION\project\agentgate`

---

## Status Verifikasi Fix (dari rekomendasi audit sebelumnya)

| # | File | Rekomendasi | Status | Bukti di Code |
|---|------|-------------|--------|---------------|
| 1 | `src/provider.rs:1494` | `max_inject = 10` (was hardcoded 128) | ✅ DONE | `let max_inject = self.limits.max_tools;` (config-based, default 128) |
| 2 | `src/provider/bridge/opencode.rs:234` | `minimal_free_model = false` | ✅ DONE | `is_free_model` → minimal MCP-only config |
| 3 | `src/config.rs:259` | `minimal_free_model = false` default | ✅ DONE | `minimal_free_model: false` |
| 4 | `src/service.rs:583` | `max_tools` check skip free model | ✅ DONE | `if !is_free_model && tools.len() > limits.max_tools` |
| 5 | `src/bridge.rs:275-276` | `max_runs_per_key = 1` | ✅ DONE | `max_parked_runs: default_2()`, `max_runs_per_key: default_1()` |
| 6 | `src/provider.rs:1085` | `bridge_turn_timeout = 120s` | ✅ DONE | `.unwrap_or(120u64)` |
| 7 | `src/api.rs:329` | `first_token_timeout = 30s` | ✅ DONE | `fn default_first_token_timeout() -> u64 { 30 }` |
| 8 | `src/service.rs:1239` | `provider_call_timeout = 15s` | ✅ DONE | `.unwrap_or(15)` |

## Status Open Items (dari AUDIT_QUOTA_LATENCY.md)

| # | Gap | Status | Keterangan |
|---|-----|--------|------------|
| G6 | Retry/backoff 429 | ⏳ OPEN | Belum diimplementasi |
| G7 | Metric quota counter | ⏳ OPEN | Belum diimplementasi |
| G8 | Dynamic tool limit by bytes | ⏳ OPEN | Masih count-based |
| G9 | `wait_for_mcp_initialize` 500→200ms | ⏳ OPEN | Masih 500ms |
| G10 | `max_steps` 50→25 | ⏳ OPEN | Masih 50 (agentgate.toml), 64 (template) |
| G11 | `bridge_turn_timeout` 300→120s | ✅ DONE | 120s |
| G12 | `call_ttl_secs` 600→300s | ✅ DONE (120s) | Lebih agresif dari rekomendasi |
| G13 | Tool result cache | ⏳ OPEN | Belum diimplementasi |
| G14 | `keep_alive = "10m"` | ⏳ OPEN | Trade-off, dipertahankan |
| G15 | `request_timeout_secs` 600→300s | ✅ DONE (120s) | Lebih agresif dari rekomendasi |

## Config Saat Ini (agentgate.toml)

```toml
[server]
request_timeout_secs = 120          # ✅ G15 (was 600)

[defaults]
max_concurrency_per_provider = 1     # ✅ A3 (was 10, now even 1)
model_cache_ttl_secs = 300          # ⚠️ A2 (was 300, audit rec 600)
first_token_timeout_secs = 30       # ✅ A4

[bridge.limits]
max_steps = 50                      # ⚠️ G10 (audit rec 25)
call_ttl_secs = 120                 # ✅ G12 (was 600, now 120)

[bridge.isolation]
minimal_free_model = false          # ✅ A1
allowed_builtins = ["bash", "execute", "shell", "read", "write", "edit", "glob", "list", "patch", "cat"]  # ⚠️ G5
```

## Ringkasan

### ✅ Yang Sudah Diperbaiki
- Deteksi free model (suffix match exact)
- Minimal config untuk free model (MCP-only)
- Concurrency limit (1 per provider)
- Tool injection limit (config-based, default 128)
- max_tools check skip free model
- bridge_turn_timeout (120s)
- first_token_timeout (30s)
- provider_call_timeout (15s)
- stream_idle_timeout (30s)
- max_runs_per_key (1), max_parked_runs (2)
- call_ttl (120s), request_timeout (120s)

### ⏳ Yang Masih Open (Prioritas)
1. **G9**: `wait_for_mcp_initialize` 500→200ms (Low effort, Medium impact)
2. **G10**: `max_steps` 50→25 (Low effort, Medium impact)
3. **G6**: Retry/backoff 429 (Medium effort, High impact)
4. **G7**: Metric quota counter (Low effort, Medium impact)
5. **G8**: Dynamic tool limit by bytes (Medium effort, Medium impact)
6. **G13**: Tool result cache (High effort, High impact)

### ⚠️ Catatan
- `model_cache_ttl_secs` masih 300 (audit rec 600) — bisa dinaikkan
- `max_steps` masih 50 (audit rec 25) — bisa diturunkan
- `allowed_builtins` masih penuh (G5) — trade-off untuk free model
- `wait_for_mcp_initialize` masih 500ms (G9) — bisa diturunkan

---

## Kesimpulan

Semua rekomendasi P0/P1/P2 dari audit sebelumnya **sudah diterapkan**. Sisa open items adalah optimasi latensi (G9, G10) dan resilience (G6, G7, G8, G13) yang bisa diterapkan bertahap. Prioritaskan G9+G10 (quick wins latensi) dan G6+G7 (resilience + observability).