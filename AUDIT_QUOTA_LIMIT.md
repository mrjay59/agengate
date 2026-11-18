# AgentGate — Audit Quota Limit (Free-Model) & Rekomendasi

**Project**: `C:\Users\EDN\Documents\AION\project\agentgate`
**Date**: 2026-11-14
**Fokus**: quota limit (provider.free-limit) pada free model (`beam-free`, `iceman-free`, dll.) dan cara mencegah limit cepat habis.

---

## 1. Ringkasan Masalah

- Free model (suffix `-free` / nama mengandung `free`) di OpenCode provider **memicu `provider.free-limit` (HTTP 429)** ketika config `opencode.json` di-inject (tools/permission/isolation blocks).
- Provider OpenCode mendeteksi jumlah tool besar → memicu free-tier limit lebih cepat.
- Quota error bisa **hang ±2 menit** (sebelum fix terminal break di api.rs).
- Saat ini mitigasi sudah ada: `minimal_free_model = true`, `is_free_model` detection, `build_minimal_mcp_config`.

---

## 2. Mekanisme Mitigasi Saat Ini

| Komponen | File | Fungsi |
|----------|------|--------|
| Deteksi free model | `provider/bridge/opencode.rs:801` | `is_free_model(model)` → true jika `-free` atau `free` |
| Minimal config | `provider/bridge/opencode.rs:732` | `build_minimal_mcp_config` — hanya MCP, tanpa tools/permission/isolation |
| Skip permission block | `provider/bridge/opencode.rs:714-727` | `if !isolation_config.minimal_free_model` skip permission |
| Flag config | `agentgate.toml:67` | `minimal_free_model = true` |
| Terminal break | `api.rs:403-420` | Error event → langsung kirim [DONE] (tidak hang 2 menit) |
| Parser error mapping | `reasoning/parser.rs:340` | `provider.free-limit` → RunEvent::Error |
| Tool injection limit | service.rs | Free model: 10 tools, paid: 128 tools (INVEST-TOOL-FILTER) |

---

## 3. Temuan / Gap

### A. `is_free_model` terlalu longgar (false positive)
```rust
fn is_free_model(model: &str) -> bool {
    let lower = model.to_lowercase();
    lower.contains("-free") || lower.contains("free") && !lower.contains("non-free")
}
```
- **Bug**: precedence operator → `contains("-free") OR (contains("free") AND !contains("non-free"))`. Model seperti `freebase` / `free-tier` non-OpenCode ikut terdeteksi.
- **Risk**: model berbayar dengan kata "free" dalam nama (mis. `gpt-4-free-trial`) → dipaksa minimal config → kehilangan isolation.
- **Fix**: match suffix/exact: `model.ends_with("-free")` atau daftar prefix yang valid (`opencode/...-free`).

### B. `minimal_free_model` config tidak di-wire ke `IsolationConfig` default
- Field `minimal_free_model` di `IsolationConfig` default? Perlu verifikasi default = true. Jika default false → permission block tetap di-inject → free-limit.
- **Audit**: cek `config.rs` IsolationConfig default.

### C. Tool limit hard-coded (10/128)
- `INVEST-TOOL-FILTER` comment: free 10, paid 128. Seharusnya konfigurabel per provider / model.
- Free model dengan 10 tools masih bisa trigger free-limit jika tool schema besar.
- **Rekomendasi**: dynamic limit berdasarkan ukuran schema (bytes) bukan count.

### D. Tidak ada retry/backoff untuk 429
- Quota error → langsung error ke client. Tidak ada retry dengan backoff.
- **Rekomendasi**: implement exponential backoff (1s, 2s, 4s) max 3 retry untuk free-limit.

### E. Cache model scan tanpa quota awareness
- `list_models` cache 300s. Jika scan men-trigger quota (Ollama native `/api/tags`), tidak ada fallback.
- **Rekomendasi**: skip quota-heavy scan untuk free model.

### F. Logging quota error kurang terstruktur
- `provider.free-limit` tercatat via tracing tapi tidak ada metric khusus.
- **Rekomendasi**: tambah counter `agentgate_quota_errors_total{provider,model}`.

---

## 4. Rekomendasi Prioritas (Anti-Quota-Habis)

| # | Aksi | Effort | Impact |
|---|------|--------|--------|
| 1 | Fix `is_free_model` → suffix/exact match | Low | High (hindari false positive) |
| 2 | Pastikan `minimal_free_model` default true di `IsolationConfig` | Low | High |
| 3 | Dynamic tool limit berbasis bytes, bukan count | Medium | Medium |
| 4 | Retry/backoff untuk 429 provider.free-limit | Medium | High |
| 5 | Metric counter quota errors | Low | Medium |
| 6 | Skip config injection sepenuhnya untuk free model (sudah minimal, tapi pastikan tidak ada tools block) | Low | High |
| 7 | Cache aggressive untuk free model scan (TTL lebih panjang) | Low | Low |

---

## 5. File yang Perlu Diubah

- `src/provider/bridge/opencode.rs` — `is_free_model`, `build_minimal_mcp_config`
- `src/config.rs` — `IsolationConfig` default `minimal_free_model`
- `src/service.rs` — tool limit logic, retry policy
- `src/api.rs` — error handling (sudah terminal break)
- `src/reasoning/parser.rs` — error code mapping (sudah ada)

---

## 6. Quick Wins (Langsung Bisa Diterapkan)

1. Ubah `is_free_model`:
```rust
fn is_free_model(model: &str) -> bool {
    let lower = model.to_lowercase();
    // exact suffix match untuk OpenCode free models
    lower.ends_with("-free") || lower.contains("/beam-free") || lower.contains("/iceman-free")
}
```

2. Pastikan `minimal_free_model = true` di `agentgate.toml` (sudah).

3. Tambahkan retry di service layer untuk 429 (max 3, backoff 1s/2s/4s).

4. Tambahkan metric counter di `metrics.rs`.

---

## 7. Kesimpulan

Mitigasi dasar **sudah ada** (minimal config, terminal break, parser mapping). Gap utama: **deteksi free model terlalu longgar**, **tool limit hard-coded**, **tidak ada retry/backoff**, **tidak ada metric**. Prioritaskan fix #1 dan #2 (low effort, high impact) untuk mencegah quota cepat habis.