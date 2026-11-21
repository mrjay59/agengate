# AgentGate — Audit Quota Limit (Free-Model) & Latensi

**Project**: `C:\Users\EDN\Documents\AION\project\agentgate`
**Date**: 2026-11-18
**Fokus**: `provider.free-limit` (HTTP 429) pada free model (`beam-free`, `iceman-free`, `flint-40b-free`, `deepseek-v4.1-flash-free`) dan latensi end-to-end.
**Build/Test**: `cargo build` OK, `cargo test` 29 suite OK (67+ unit tests).

---

## 1. Ringkasan Eksekutif

- **Mitigasi dasar sudah ada dan berfungsi**: deteksi free model, minimal config (MCP-only), terminal break di SSE, parser mapping `provider.free-limit` → `RunEvent::Error`, tool injection limit config-based, concurrency limit.
- **Quota cepat habis** disebabkan oleh: (a) deteksi free model terlalu longgar → false positive/negative, (b) config TTL tidak di-wire ke runtime (hardcoded), (c) concurrency default 10 untuk free tier, (d) tidak ada retry/backoff 429, (e) tidak ada metric quota.
- **Latensi** disebabkan oleh: (a) model scan timeout, (b) `wait_for_mcp_initialize` sleep 500ms per spawn, (c) `max_steps = 50` → banyak loop tool, (d) `bridge_turn_timeout 300s` terlalu longgar, (e) first_token_timeout default mismatch di template.

### Fix yang sudah diterapkan pada commit ini (audit lanjutan)

| # | File | Perubahan | Dampak |
|---|------|-----------|--------|
| A1 | `src/provider/bridge/opencode.rs` | `is_free_model` → `ends_with("-free")` (suffix exact) | Hindari false positive/negative |
| A2 | `src/service.rs` | `model_cache_ttl` pakai `defaults.model_cache_ttl_secs` (bukan hardcoded 300) | Cache TTL config benar-benar dipakai |
| A3 | `agentgate.toml` | `max_concurrency_per_provider = 2` (dari 10) | Kurangi beban free tier |
| A4 | `src/config.rs` (template) | `model_cache_ttl_secs = 600`, `first_token_timeout_secs = 30` | Konsisten dengan default fn |

---

## 2. Mekanisme Mitigasi Saat Ini (Sudah Ada)

| Komponen | File:Line | Fungsi |
|----------|-----------|--------|
| Deteksi free model | `provider/bridge/opencode.rs:801` | `is_free_model(model)` |
| Minimal config (MCP-only) | `provider/bridge/opencode.rs:732` | `build_minimal_mcp_config` |
| Skip permission block | `provider/bridge/opencode.rs:714-727` | `if !isolation_config.minimal_free_model` |
| Flag config | `agentgate.toml` | `minimal_free_model = false` |
| Terminal break SSE | `api.rs:403-420` | Error → emit [DONE] (tidak hang 2 menit) |
| Parser error mapping | `reasoning/parser.rs:340` | `provider.free-limit` → RunEvent::Error |
| Tool injection limit | `provider.rs:1492,1709` | `max_inject = limits.max_tools` (128) |
| max_tools check skip free | `service.rs:576-579` | Skip limit untuk free model |
| Concurrency semaphore | `service.rs:1059-1081` | Per-provider, default 2 |
| Model scan cache | `service.rs:315-340` | TTL 300s (→600s setelah A2) |
| Warm cache startup | `main.rs:204-207` | Background `warm_model_cache()` |
| scan_models timeout | `service.rs:344` | 5s per provider |
| first_token_timeout | `service.rs:112-116` | default 30s (setelah commit) |
| provider_call_timeout | `service.rs:1234-1240` | default 30s |
| stream_idle_timeout | `api.rs:1094-1101` | default 30s |
| bridge_turn_timeout | `provider.rs:1082-1089` | default 300s |
| max_runs_per_key | `bridge.rs:775-792` | default 1 (config) |
| max_parked_runs | `bridge.rs:821-833` | default 2 (config) |
| is_bridge_tool guard | `bridge.rs:965-984` | fail-closed isolation |

---

## 3. Temuan / Gap (Belum Diperbaiki)

### G1. `is_free_model` precedence bug (FIXED A1)
- Lokasi: `provider/bridge/opencode.rs:804-807` (sebelum fix).
- Bug: `lower.contains("-free") || lower.contains("free") && !lower.contains("non-free")` → precedence salah → model seperti `freebase` ikut terdeteksi.
- Status: **Fixed** → `lower.ends_with("-free")`.

### G2. `model_cache_ttl` hardcoded (FIXED A2)
- Lokasi: `service.rs:118` (sebelum fix) `Duration::from_secs(300)`.
- Bug: config `defaults.model_cache_ttl_secs` diabaikan → cache expire lebih cepat → scan lebih sering → quota.
- Status: **Fixed** → pakai `defaults.model_cache_ttl_secs`.

### G3. Concurrency default terlalu tinggi (FIXED A3)
- Lokasi: `agentgate.toml` `max_concurrency_per_provider = 10`.
- Bug: free tier tidak tahan 10 concurrent CLI spawn → quota cepat habis.
- Status: **Fixed** → 2.

### G4. Template config mismatch (FIXED A4)
- Lokasi: `config.rs` write_default_config template.
- Bug: template masih `model_cache_ttl_secs = 300`, `first_token_timeout_secs = 120` (default fn sudah 600/30).
- Status: **Fixed**.

### G5. `allowed_builtins` menonaktifkan isolation (OPEN)
- Lokasi: `agentgate.toml` `[bridge.isolation]` `allowed_builtins = ["bash", "execute", "shell", "read", "write", "edit", "glob", "list", "patch", "cat"]`.
- Dampak: di selective mode, conflicting tools yang di-allow tidak di-disable → CLI built-in tetap aktif → bypass bridge isolation + lebih banyak tool execution → quota.
- Trade-off: permission block (deny) memicu `provider.free-limit`. Jadi allowed_builtins penuh adalah kompromi untuk free model.
- Rekomendasi: biarkan untuk free model, tapi dokumentasikan. Untuk paid model, gunakan selective default (kosongkan allowed_builtins) agar isolation aktif.

### G6. Tidak ada retry/backoff untuk 429 (OPEN — Medium effort)
- Lokasi: `service.rs` `chat_completion`, `new_bridge_run`, `call_provider_native_with`.
- Dampak: quota error langsung error ke client, tidak ada retry.
- Rekomendasi: implement exponential backoff (1s, 2s, 4s) max 3 retry khusus untuk error yang mengandung `provider.free-limit` / 429.

### G7. Tidak ada metric quota (OPEN — Low effort)
- Lokasi: `metrics.rs` (tidak ada counter quota).
- Dampak: tidak bisa monitor quota error rate.
- Rekomendasi: tambah `quota_errors: AtomicU64` + `record_quota_error(provider, model)` + render di prometheus. Panggil di parser.rs saat `provider.free-limit`.

### G8. Tool limit berbasis count, bukan bytes (OPEN — Medium effort)
- Lokasi: `provider.rs:1492,1709` `max_inject = limits.max_tools`.
- Dampak: 128 tools besar → prompt besar → token besar → quota.
- Rekomendasi: dynamic limit berdasarkan cumulative schema bytes (mis. max 64KB total) bukan count.

### G9. `wait_for_mcp_initialize` sleep 500ms (OPEN — Low effort)
- Lokasi: `provider/bridge/opencode.rs:811-815`.
- Dampak: +500ms per spawn (setiap request yang spawn CLI).
- Rekomendasi: kurangi ke 200ms atau polling MCP ready.

### G10. `max_steps = 50` terlalu tinggi (OPEN — Low effort)
- Lokasi: `agentgate.toml` `[bridge.limits] max_steps = 50`.
- Dampak: model bisa melakukan 50 loop tool → quota habis.
- Rekomendasi: 20-25 untuk free tier.

### G11. `bridge_turn_timeout = 300s` (OPEN — Low effort)
- Lokasi: `provider.rs:1082-1089`.
- Dampak: hung process bisa hidup 5 menit.
- Rekomendasi: 120s default (env override tetap).

### G12. `call_ttl_secs = 600` di bridge.limits (OPEN — Low effort)
- Lokasi: `agentgate.toml`.
- Dampak: parked run bisa hidup 10 menit.
- Rekomendasi: 300s.

### G13. Tidak ada dedup/cache tool result (OPEN — High effort)
- Dampak: tool call identik dieksekusi ulang → quota.
- Rekomendasi: cache tool result by (name, args hash) dengan TTL pendek (30s).

### G14. `keep_alive = "10m"` untuk opencode (OPEN — Trade-off)
- Dampak: session persistent 10 menit → akumulasi resource, tapi kurangi spawn.
- Rekomendasi: pertahankan (mengurangi spawn = mengurangi quota), tapi monitor.

### G15. `request_timeout_secs = 600` di agentgate.toml (OPEN — Low)
- Dampak: request bisa hang 10 menit sebelum timeout.
- Rekomendasi: 120-300s.

---

## 4. Rekomendasi Prioritas

| # | Aksi | Effort | Impact | Status |
|---|------|--------|--------|--------|
| 1 | Fix `is_free_model` suffix match | Low | High | ✅ Done (A1) |
| 2 | Wire `model_cache_ttl` ke config | Low | Medium | ✅ Done (A2) |
| 3 | `max_concurrency_per_provider` 2 | Low | High | ✅ Done (A3) |
| 4 | Template config konsisten | Low | Low | ✅ Done (A4) |
| 5 | Retry/backoff 429 (G6) | Medium | High | ⏳ Open |
| 6 | Metric quota counter (G7) | Low | Medium | ⏳ Open |
| 7 | Dynamic tool limit by bytes (G8) | Medium | Medium | ⏳ Open |
| 8 | Kurangi `wait_for_mcp_initialize` 500→200ms (G9) | Low | Medium | ⏳ Open |
| 9 | `max_steps` 50→25 (G10) | Low | Medium | ⏳ Open |
| 10 | `bridge_turn_timeout` 300→120s (G11) | Low | Medium | ⏳ Open |
| 11 | `call_ttl_secs` 600→300s (G12) | Low | Low | ⏳ Open |
| 12 | Tool result cache (G13) | High | High | ⏳ Open |
| 13 | `request_timeout_secs` 600→300s (G15) | Low | Low | ⏳ Open |

---

## 5. Latensi Hot-Path (Prioritas)

| Path | Bottleneck | Mitigasi |
|------|------------|----------|
| `/v1/models` | CLI scan (opencode models) | Cache TTL 600s (A2), warm at startup |
| `/v1/chat/completions` (bridge) | Spawn CLI + MCP init + 500ms sleep | Kurangi sleep (G9), warm cache |
| SSE stream first token | first_token_timeout 30s | OK (default 30s) |
| Tool call round-trip | bridge hold + client execute | call_ttl 300s (G12), continuation park |
| Replay fallback | Re-spawn CLI + re-render conversation | max_steps 25 (G10) |

---

## 6. File yang Perlu Diubah (Open Items)

- `src/service.rs` — retry/backoff (G6), dynamic tool limit (G8)
- `src/metrics.rs` — quota counter (G7)
- `src/reasoning/parser.rs` — panggil metric quota (G7)
- `src/provider/bridge/opencode.rs` — sleep reduction (G9)
- `agentgate.toml` — max_steps, call_ttl, request_timeout, bridge_turn_timeout env (G10-G12, G15)
- `src/provider.rs` — bridge_turn_timeout default (G11)

---

## 7. Quick Wins (Bisa Langsung Diterapkan)

```toml
# agentgate.toml
[defaults]
max_concurrency_per_provider = 2
first_token_timeout_secs = 30
model_cache_ttl_secs = 600

[bridge.limits]
max_runs_per_key = 1
max_parked_runs = 2
max_tools = 128
max_steps = 25
call_ttl_secs = 300

[server]
request_timeout_secs = 300
```

```rust
// provider/bridge/opencode.rs
tokio::time::sleep(Duration::from_millis(200)).await; // dari 500
```

```rust
// provider.rs
.unwrap_or(120u64) // bridge_turn_timeout, dari 300
```

---

## 8. Kesimpulan

Mitigasi dasar **sudah ada dan berfungsi** (minimal config, terminal break, parser mapping, config-based tool limit, concurrency semaphore, cache). Gap utama yang tersisa: **retry/backoff 429**, **metric quota**, **dynamic tool limit by bytes**, **sleep 500ms**, **max_steps 50**, **bridge_turn_timeout 300s**. Prioritaskan G6 (retry/backoff) dan G7 (metric) untuk observability + resilience, lalu G9-G12 untuk latensi.

**Fix yang diterapkan pada audit ini (A1-A4)** sudah mengurangi false positive deteksi free model dan memastikan cache TTL config benar-benar dipakai, plus concurrency default lebih aman untuk free tier.