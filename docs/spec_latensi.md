# Spec Latensi — AgentGate

## 1. Ringkasan (satu halaman)

Gateway OpenAI-compatible (`/v1/chat/completions`, `/v1/models`). Latensi total = **routing → spawn CLI → MCP bridge (bila tools) → parser → SSE/non-stream return**.

| Jalur | Klasifikasi | Target TTFT | Target tuntas |
|---|---|---|---|
| Chat-only native (`ollama`, `opencode` tanpa tools) | `NewChatOnly` → `native_providers` (tanpa bridge) | <500 ms | <10 s |
| Bridge non-stream (tools) | `NewBridge` → `opencode run --model` + MCP | <2 s | <30 s |
| Bridge stream (tools) | SSE, early role chunk | <1 s | <30 s |
| Chat-only stream | native, SSE early role | <500 ms | <10 s |

Semua path **bounded** (tidak menggantung): stdout EOF = turn selesai, tunggu proses ≤10 s lalu kill; permit ≤30 s; fail-closed.

## 2. Struktur routing & budget per hop

- **Klasifikasi** (`src/run.rs`): `NewChatOnly` bila tanpa tools + provider native → `chat_only_run`/`call_provider_native_with`; `NewBridge` bila ada tools/client → bridge; `Resume`/`Replay` bila ada tool results.
- **Budget**:
  - Spawn CLI: 1–3 s (bin lookup, `OPENCODE_CONFIG_CONTENT`, MCP init).
  - MCP bridge: `initialize` + `tools/list` <1 s, `tools/call` hold ≤ `call_ttl_secs`.
  - Parser (`ReasoningParser` + `tool_view`): <5 ms per line, line max 4 MiB, CRLF normalize.
  - SSE: early role chunk @ t≈0, flush `flush_ms`/`flush_bytes`, keep-alive ping.
  - Permit: semaphore `max(1)` (clamp) + timeout 30 s (fail-closed, bukan hang).
- **Park/hold**: `park_run` return `tool_calls + finish_reason:"tool_calls"` ke client; resume post-response, tidak blocking response.

## 3. Optimasi yang sudah ada

- Chat-only **tidak lewati bridge/MCP** (native dulu) → hemat spawn + MCP overhead.
- `--model` eksplisit per spawn; arg order `run --model --format json` (2.1.4).
- `event_tx` tidak di-retain di `opencode.rs` (channel bisa close) → `wait_for_completion` tidak hang.
- Bounded: stdout EOF → tunggu ≤10 s → kill; `acquire_provider_permit` clamp `max(1)` + 30 s.
- Log singkat: hanya `line_count` + `line_len` (ANSI-stripped), preview failure ≤120 char.
- Parser error mapping: `error.message`/`error.status` → `ProviderResponse::Error` → 502 jelas (bukan 200 kosong).
- View step tool (`tool_view`): `grep→search`, `glob→list`, `create/write→write`, `edit→edit`, `bash/run→run`, selainnya→`dll` (faithful CLI projection). Operasi edit (`create/write/edit/patch/multiedit/delete/mkdir`) diberi ringkasan perubahan `(+N lines)` / `(-N +M)` dan path camelCase OpenCode (`filePath`) dikenali.
- `/v1/models`: scan `models_cmd` per provider dijalankan **paralel** (batas 10 s/provider) + cache TTL 300 s; cache di-*warm* saat startup (`warm_model_cache`) sehingga request pertama ≈ 0 ms (bukan 2–4 s). Ollama `/api/tags` hanya dipakai bila `ollama_mode != "cli"` dan dibatasi 2 s.
- Namespace `agg/` untuk integrasi 9router: model `provider/model` → `agg/provider/model`; model tanpa namespace → `agg/<provider>/<model>`. Resolusi provider memakai cache model, jadi `agg/9router/...` tetap dirutekan ke provider penemunya (opencode).
- Chat-only Ollama mode CLI memakai `CliProviderImpl` langsung (`ollama run`), bukan bridge MCP; blok `Thinking... / ...done thinking.` dipisah menjadi `reasoning_content` (stream & non-stream).

## 4. Optimasi yang belum (rekomendasi)

1. **Prewarm pool** CLI per model (opencode/ollama) untuk TTFT <200 ms.
2. **MCP session reuse** (`keep_alive` + idle TTL) untuk bridge berurutan.
3. **Fast error path**: short-circuit `provider.free-limit`/`quota` sebelum spawn MCP.
4. **Backpressure tuning**: channel 128, coalescer batching, `flush_ms` adaptif.
5. **Metric histogram** latensi per stage (spawn/parse/bridge) + alert TTFT >2 s.

## 5. Observabilitas

- Log event: `run.start`/`run.end`, `bridge.tools_list`, `agentgate.bridge.opencode.stdout` (`line_count,line_len`), `ollama stream_events DONE`.
- Header `X-Agentgate-*` (run_id, provider, model, mode, tool_calls) + metrics (`cli_exit`, `run_parked`, dll).
- Setiap request `run_id` untuk korelasi.

## 6. Cara ukur

```bash
# TTFT + total (stream)
curl -sN -w "\nTTFB %{time_starttransfer}s TOTAL %{time_total}s\n" \
  http://100.118.190.97:8787/v1/chat/completions \
  -H "Authorization: Bearer default_key" -H "Content-Type: application/json" \
  --data-binary '{"model":"opencode/flint-40b-free","messages":[{"role":"user","content":"Say hello"}],"stream":true}'

# Non-stream
curl -s -w "\nTOTAL %{time_total}s\n" ... --data-binary '{"model":"opencode/beam-free",...,"stream":false}'
# Lihat stdout.txt: run.start → run.end delta = latensi tuntas; head chunk = TTFT.
```

## 7. Failure & timeout matrix

| Kondisi | Gejala | Aksi gateway |
|---|---|---|
| CLI exit + error event | `error{message,status}` | 502 `provider_error` (fail-closed) |
| stdout EOF, proses hidup | EOF saja | tunggu ≤10 s → kill → fine (tidak hang) |
| Permit kosong | starvation | timeout 30 s → 500 (bukan hang selamanya) |
| Tool call tanpa result | `tool_calls` | return `finish_reason:"tool_calls"` + park post-response |
| Ollama stream macet | tidak DONE | bounded timeout + kill + DONE/error ke client |
| Reasoning terenkripsi/kosong (crouton-alpha-free) | `reasoning` event dengan text kosong | skip (tidak memancarkan reasoning kosong), konten tetap jalan |

## 8. Catatan reasoning (crouton-alpha-free)

- `opencode run --model opencode/crouton-alpha-free --thinking --format json` mengeluarkan event `type:"reasoning"` dengan `part.text=""` + `metadata.reasoningEncryptedContent` (terenkripsi).
- Gateway parser (`src/reasoning/parser.rs`, arm `"reasoning"`) mengekstrak `part.text`; jika kosong di-skip. Ini **benar**: tidak ada plaintext reasoning untuk dimunculkan, dan tidak ada synthetic reasoning.
- Model yang menghasilkan reasoning plaintext (mis. `opencode/beam-free`) tetap memunculkan `reasoning_content`.
- Implikasi latensi: `--thinking` menambah overhead spawn/processing; untuk model dengan reasoning terenkripsi, overhead tidak menghasilkan output reasoning. Pertimbangkan flag `--thinking` per-model bila diperlukan.

---

*Author: Lead (OpenCode). Dokumentasi saja, tidak mengubah logika.*
