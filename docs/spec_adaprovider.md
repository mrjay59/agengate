# SPEC_ADAPROVIDER.md — Rincian Implementasi Adapter Provider (opencode + ollama)

Dokumen ini ditujukan untuk agent/developer lain yang akan mengimplementasikan
adapter provider **opencode** (bridge) dan **ollama** (native passthrough) sesuai
`docs/spec_tooling.md`.

## 1. Tujuan
Implementasikan dua jalur provider yang belum lengkap:
- **opencode**: bridge mode (CLI agent dengan MCP, disable builtin tools).
- **ollama**: native passthrough (HTTP OpenAI-compatible, tanpa bridge).

## 2. Scope
- `src/provider/opencode.rs` (bridge adapter)
- `src/provider/ollama_native.rs` (native adapter)
- `src/service.rs` (routing berdasarkan provider)
- `src/config.rs` (base_url, bridge flag)
- `src/api.rs` (error handling)

## 3. Task opencode (Bridge)
1. Spawn CLI dengan config injection:
   - Env `OPENCODE_CONFIG_CONTENT` berisi JSON MCP config + disable builtin tools.
   - `cwd` = direktori kosong per-run.
   - Argumen: `opencode run --model <model> --format json` (prompt via stdin).
2. Parse output JSONL → emit `Text`, `Reasoning`, `ToolObserved` untuk guard.
3. Implement guard: jika event tool non-bridge → kill run, return `tool_isolation_violation`.
4. Bridge MCP hold/resolve sudah ada di `api.rs`; pastikan session token disuntik.
5. Preflight version check + uji isolasi `probe_ok`.

## 4. Task ollama (Native)
1. Tambah provider `ollama` kelas `model-api`.
2. Endpoint upstream default `http://127.0.0.1:11434`.
3. `GET /v1/models` → gabung dari `/api/tags`, tandai kapabilitas tools.
4. `POST /v1/chat/completions` → proxy body hampir apa adanya (mutasi minimal: model, normalisasi id/created/model berprefix).
5. Stream/non-stream diteruskan byte-per-byte + `data: [DONE]`.
6. Jika model tidak support tools dan request membawa tools → HTTP 400 `model_no_tool_support`.

## 5. Integrasi service.rs
- Tambah deteksi provider: `ollama` → native passthrough; lainnya → bridge (jika tools ada).
- Gunakan `classify` dari `run.rs` untuk menentukan mode.
- Error mapping sesuai §7.6.

## 6. Konfigurasi
Tambahkan di `agentgate.toml`:
```
[[provider]]
id = "ollama"
base_url = "http://127.0.0.1:11434"
bridge = false
```

## 7. Adapter gemini (Bridge)
1. Spawn: `gemini -m <model> -o stream-json` (prompt via stdin + `-p`).
2. Inject settings via `GEMINI_CLI_SYSTEM_SETTINGS_PATH` (MCP httpUrl + excludeTools).
3. Profil skema `strict_gemini`.
4. Guard: event `tool_use` dengan nama bukan bridge → violation.
5. Parsing: `message`→Text, `tool_use`→guard, `result`→Usage/selesai.

## 8. Adapter claude (Bridge)
1. Spawn: `claude -p --output-format stream-json --verbose --include-partial-messages --strict-mcp-config --tools "" --setting-sources "" --max-turns 64`.
2. Inject MCP via `--mcp-config` (JSON) + `--allowedTools "mcp__agentgate"`.
3. System prompt via `--system-prompt`.
4. Guard: `tool_use` blok tanpa prefix `mcp__agentgate__` → violation.
5. Parsing: `stream-json` events (assistant/user/result).

## 9. Ollama (CLI/HTTP)
- Ollama bukan agent CLI; gunakan HTTP native (`/v1/chat/completions`).
- Jika ingin wrapper CLI: panggil `ollama run` hanya untuk teks (tanpa tools); untuk tools gunakan HTTP passthrough.
- Pastikan `num_ctx` cukup (≥8192) saat tools ada.

## 10. Acceptance Criteria
- Request `ollama/...` dengan tools mengembalikan tool_calls native.
- Request opencode/gemini/claude dengan tools menghasilkan tool_calls via bridge (hold tools/call).
- Guard menolak tool non-bridge dengan 502 `tool_isolation_violation`.
- Streaming SSE valid (chunk + [DONE]).
