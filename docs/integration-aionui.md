# Integrasi AgentGate ↔ AionUi

## Ringkas
AionUi bisa memakai AgentGate sebagai **Custom API provider** (OpenAI-compatible). AgentGate membungkus CLI agent lokal (opencode, copilot, gemini, …) menjadi endpoint `/v1/*`. Tidak perlu API key vendor.

## Alur minimal (sudah bisa sekarang)
1. Jalankan AgentGate: `agentgate serve --host 127.0.0.1 --port 8787`
2. Di AionUi: Settings → Provider → **Custom API**
   - Base URL: `http://127.0.0.1:8787/v1`
   - API Key: `dummy` (AgentGate tidak validasi bila `api_keys` kosong)
   - Model: pilih dari `/v1/models`, mis. `opencode/beam-free`
3. Test chat non-stream dulu, lalu stream.

## Gap yang perlu ditutup
| Gap | Dampak | Prioritas |
|---|---|---|
| CORS tidak di-wire (config `cors` ada tapi tidak dipakai) | WebUI browser AionUi tidak bisa panggil langsung | Tinggi |
| Bearer auth tidak ditegakkan (meski `api_keys` terisi) | Bind non-loopback tidak aman; AionUi kirim key tapi tidak divalidasi | Tinggi |
| `GET /` (info service) belum ada, padahal plan menyebut | AionUi explorer/health probe kehilangan info provider | Sedang |
| Streaming sintetis (kumpul semua lalu kirim chunk) | UX streaming AionUi terasa lambat; cancel mid-stream tidak efektif | Sedang |
| Model id = `provider/model` (prefix ganda) | AionUi daftar model menampilkan prefix; perlu alias/plain | Rendah |
| Tidak ada lifecycle coupling | User harus start AgentGate manual | Rendah (opsional) |

## Rekomendasi implementasi (urutan)
1. **Wire CORS** (tower-http `CorsLayer`) berdasarkan `config.server.cors`.
2. **Auth middleware** Bearer: jika `api_keys` non-empty, wajibkan `Authorization: Bearer <key>`; jika bind non-loopback dan `api_keys` kosong → tolak start.
3. **Tambah `GET /`** info: nama, versi, provider terdeteksi, link endpoint.
4. **Real streaming**: spawn CLI, baca stdout incremental, kirim SSE chunk per delta, cancel via token saat client disconnect.
5. (Opsional) alias model tanpa prefix untuk AionUi.

## Kontrak yang harus dijaga untuk AionUi
- `/v1/models` → `{object:"list", data:[{id, provider, name}]}` (OpenAI).
- `/v1/chat/completions` → non-stream JSON & SSE `chat.completion.chunk` + `data: [DONE]`.
- Field `reasoning_content` (thinking) di message/delta.
- Role `tool` → `assistant` (sudah disanitasi).
- Error shape OpenAI: `{error:{message,type}}`.

## Checklist verifikasi
- [ ] `curl /health` → 200
- [ ] `curl /v1/models` → list non-empty
- [ ] Chat non-stream mengembalikan content non-empty
- [ ] Chat stream mengirim `[DONE]`
- [ ] AionUi Custom API test connection sukses
- [ ] (jika CORS) request dari origin AionUi WebUI lulus preflight
