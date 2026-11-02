# Rencana Migrasi: Axum Streaming & Concurrency (AgentGate)

## Tujuan
Mengubah `agentgate` dari model **Request-Response Blocking** menjadi **Asynchronous Streaming** guna mendukung:
1. *Real-time Thinking/Reasoning* (UI Thinking).
2. *Low-latency* chat experience (seperti ChatGPT).
3. *High Concurrency* (mendukung banyak user bersamaan).

---

## 1. Plan.md (Langkah Kerja)

### Fase 1: Persiapan Asinkron
- [ ] Update `provider.rs` untuk mengganti `std::process::Command` (blocking) menjadi `tokio::process::Command` (async).
- [ ] Update trait `Provider` agar mendukung pengambilan output melalui stream (async generator atau channel).

### Fase 2: Implementasi Channel (MPSC)
- [ ] Implementasi channel (`tokio::sync::mpsc`) untuk mengalirkan data antara *background task* (CLI execution) dan *request thread* (Axum response).

### Fase 3: Integrasi API Streaming
- [ ] Refactor `api.rs` untuk menggunakan `axum::response::Sse` (Server-Sent Events) atau `Body::from_stream`.
- [ ] Implementasi *Heartbeat* (jika perlu) untuk menjaga koneksi tetap hidup selama proses berpikir panjang.

### Fase 4: Optimasi & Concurrency
- [ ] Implementasi *Graceful Shutdown* saat client memutus koneksi (menghentikan proses CLI/model secara paksa).
- [ ] Stress-test untuk memastikan tidak ada *memory leak* pada setiap *stream task*.

---

## 2. Spec.md (Spesifikasi Teknis)

### Arsitektur Data Flow
1. **Client** mengirim `POST /v1/chat/completions` (dengan `stream: true`).
2. **Axum Handler** men-spawn *background task* menggunakan `tokio::spawn`.
3. **Background Task**:
   - Membaca `stdout` CLI model.
   - Parsing output secara *real-time* (JSONL atau `<think>`).
   - Push chunk melalui `mpsc::Sender`.
4. **Axum Response**:
   - Mengambil data dari `mpsc::Receiver`.
   - Mengkonversi chunk menjadi *SSE Event* (`data: {...}\n\n`).

### Perubahan Kontrak Interface
- **Response Format**:
  - `data: {"delta": {"reasoning_content": "..."}}` (Event Thinking)
  - `data: {"delta": {"content": "..."}}` (Event Response)
  - `data: [DONE]` (Event Penutup)

### Kompatibilitas Concurrency
- Setiap request adalah satu *Tokio Task*.
- `ServiceRegistry` harus di-wrap dalam `Arc` dan hanya memegang *read-only* reference ke konfigurasi.
- *State* eksekusi (buffer stdout) tidak boleh berbagi memori antar request (isolation).
