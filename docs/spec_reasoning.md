# SPEC_REASONING.md — Spesifikasi Reasoning / Thinking Streaming `agentgate`

| | |
|---|---|
| **Status** | In Progress |
| **Tanggal** | 2026-11-03 |
| **Cakupan provider** | `opencode`, `gemini`, `claude` (Claude Code), `ollama` |
| **Dokumen terkait** | `plan.md`, `tool-handling.md`, `spec_tooling.md` (bridge), `spec_session.md` (sesi) |
| **Konvensi** | **MUST / SHOULD / MAY** mengikuti RFC 2119. ⚠️ = *harus diverifikasi pada versi CLI/model terpasang*. |

---

## 0. Ringkasan Eksekutif

**Pertanyaan:** bagaimana agar reasoning/thinking ikut tampil saat prompting, padahal `agentgate` mungkin menunggu hasil prompt CLI sampai selesai sehingga "thought" baru muncul bersamaan dengan jawaban?

**Jawaban singkat:** reasoning **tidak perlu menunggu jawaban selesai**. Model menghasilkan reasoning *sebelum* jawaban; yang membuatnya "bebarengan" adalah salah satu dari tiga lapis berikut:

| Lapis | Penyebab | Perbaikan |
|---|---|---|
| **L1 — Gateway** | `agentgate` membaca output CLI sampai proses selesai (mis. `wait_with_output`) atau mengumpulkan semua delta lalu mengirim sekaligus | Pipeline **streaming**: baca stdout per baris, teruskan event segera (§6) |
| **L2 — CLI** | CLI memberi output hanya di akhir, atau per *part* (bukan per token), atau tidak mengalirkan thinking | Pilih mode output terstruktur & granular yang tersedia (§7); fallback aktivitas (§8) |
| **L3 — Model/CLI menyembunyikan thinking** | Thinking *omitted*/diringkas/tidak diekspos pada mode non-interaktif | Tidak bisa dipaksa; gunakan fallback jujur (§8), jangan memalsukan reasoning |

Inti desain:
1. **Model event internal** yang memisahkan `Reasoning` dari `Text` dan `Activity` (§4).
2. **Pemetaan ke format OpenAI-compatible** yang dikenal klien: `delta.reasoning_content` (default), `delta.reasoning`, atau tag `<think>` inline (§5).
3. **Pipeline tanpa buffering** dengan *coalescer* kecil (flush ≤ 40 ms) dan urutan terjamin: reasoning → teks/tool_calls (§6).
4. **Adapter per CLI** yang jujur soal apa yang benar-benar tersedia (§7).
5. **Fallback aktivitas** (bukan "reasoning palsu") bila CLI tidak mengekspos thinking (§8).
6. Reasoning **tidak pernah** masuk riwayat/ledger sesi dan **tidak** dikirim balik sebagai instruksi ke CLI (§10).

---

## 1. Latar Belakang & Diagnosis

### 1.1 Lintasan data

```
Model ──thinking──► CLI/harness ──stdout──► agentgate ──SSE──► 9router ──SSE──► Client (AionUi)
        (1)             (2)                    (3)               (4)                (5)
```
Reasoning bisa "tertahan" di setiap tanda panah. Perilaku "baru muncul bersamaan dengan jawaban" berarti salah satu titik membuffer atau tidak menerima data tersebut.

### 1.2 Diagnosis cepat (urut dari hulu)

| Langkah | Cara uji | Hasil → kesimpulan |
|---|---|---|
| D1 | Jalankan CLI **langsung** dengan mode streaming dan beri cap waktu: `claude -p … --output-format stream-json --verbose --include-partial-messages \| ts '%H:%M:%.S'` | Ada baris thinking **sebelum** teks → CLI mengalirkan (L2 aman). Tidak ada / kosong → L3 |
| D2 | Panggil gateway dengan `curl -N` **langsung** (tanpa 9router) | Reasoning muncul bertahap → L1 aman |
| D3 | Panggil lewat 9router dengan `curl -N` | Hilang/terlambat → 9router membuffer atau menjatuhkan field reasoning (§11) |
| D4 | Tampilkan di AionUi | Tidak tampil walau D3 baik → format field tidak dikenali klien; coba `think_tags` (§5.1) |

`agentgate spike-reasoning` (§16) mengotomatiskan D1 dan menghasilkan laporan granularitas.

---

## 2. Tujuan, Non-Tujuan, Prinsip

### 2.1 Tujuan
- **G1.** Reasoning dari CLI/model diteruskan **secara real-time** (streaming) ke client, mendahului jawaban.
- **G2.** Kompatibel dengan klien OpenAI-compatible populer tanpa modifikasi (format dapat dipilih).
- **G3.** Jujur: label jelas antara reasoning mentah, ringkasan, dan aktivitas gateway.
- **G4.** Tidak merusak alur tool-bridge, sesi, dan keamanan.
- **G5.** Bisa dimatikan; ada batas ukuran; tidak masuk log.

### 2.2 Non-Tujuan
- Memaksa model/CLI menampilkan reasoning yang disembunyikan (tidak ada upaya mengekstrak CoT tersembunyi).
- Mensintesis "chain-of-thought" buatan dan menyajikannya sebagai reasoning model.
- Menyimpan reasoning di gateway.
- Mendekripsi/meneruskan blok thinking terenkripsi atau bertanda tangan.

### 2.3 Prinsip
1. **Stream, jangan kumpulkan.**
2. **Urutan lebih penting daripada kecepatan**: reasoning tidak boleh melompati teks.
3. **Jujur soal sumber**: raw | summary | activity | none.
4. **Reasoning bukan konten**: tidak masuk riwayat, ledger, atau prompt replay.
5. **Degradasi anggun**: tidak ada reasoning → tidak ada error.

---

## 3. Terminologi

| Istilah | Arti |
|---|---|
| **Raw thinking** | Teks pemikiran model yang diekspos apa adanya oleh CLI |
| **Summarized thinking** | Ringkasan pemikiran yang dibuat model/vendor |
| **Omitted thinking** | Blok thinking ada tetapi isinya kosong/disembunyikan |
| **Activity** | Pesan status buatan gateway dari event CLI (mis. "memanggil tool X"). **Bukan** reasoning model |
| **Block** | Satu segmen reasoning kontigu (satu blok thinking) |
| **Granularitas** | `token` (delta kecil), `block` (satu blok utuh), `final` (hanya di akhir), `none` |
| **Coalescer** | Komponen yang menggabungkan delta kecil menjadi chunk SSE dengan jeda singkat |

---

## 4. Model Event

### 4.1 Event internal (memperluas `RunEvent`/`CliEvent` pada `spec_tooling.md`)

```rust
pub enum ReasoningKind { Raw, Summary, Omitted, Activity }

pub enum RunEvent {
    Text(String),

    // reasoning
    ReasoningStart { block: u32, kind: ReasoningKind },
    ReasoningDelta { block: u32, text: String },
    ReasoningEnd   { block: u32 },

    // aktivitas sintetis (fallback, §8)
    Activity { kind: ActivityKind, label: String },

    ToolCalls(Batch),
    Usage(Usage),            // termasuk reasoning_tokens bila ada
    Done(FinishReason),
    Error(RunError),
}

pub enum ActivityKind { Starting, Thinking, ToolRequested, WaitingClient, Writing }
```

`CliEvent` pada adapter menghasilkan padanan (`Reasoning*`, `Text`, `ToolObserved`, dst.).

### 4.2 Aturan urutan (MUST)
1. Untuk satu giliran asisten: semua `Reasoning*` sebuah blok **mendahului** `Text`/`ToolCalls` yang muncul sesudah blok itu.
2. `ReasoningEnd` **MUST** dikirim sebelum `Text` pertama setelah blok tersebut (atau saat `Done`/`ToolCalls`).
3. Reasoning boleh muncul di **beberapa blok** (interleaved thinking: reasoning → tool → reasoning → teks). Pada bridge, tiap bagian jatuh pada response HTTP-nya sendiri (§9).
4. Blok `Omitted` tidak menghasilkan `ReasoningDelta` (hanya `Start`/`End`); gateway memutuskan sendiri apakah ditampilkan sebagai penanda (§8.3).
5. Tidak ada `ReasoningDelta` setelah `Done`.

---

## 5. Kontrak API

### 5.1 Format keluaran reasoning (`reasoning.format`)

| Format | Bentuk di SSE | Kapan dipakai |
|---|---|---|
| **`reasoning_content`** *(default)* | `delta.reasoning_content` | Klien yang mengikuti konvensi DeepSeek (banyak UI) |
| `reasoning` | `delta.reasoning` | Klien yang mengikuti konvensi OpenRouter |
| `both` | `reasoning_content` **dan** `reasoning` | Tidak tahu klien mana yang dipakai |
| `think_tags` | Disisipkan ke `delta.content` sebagai `<think>…</think>` | Klien yang mengurai tag (mis. UI yang menyaring `<think>`); fallback bila field khusus tidak ditampilkan |
| `none` | Tidak dikirim | Klien programatik; hemat bandwidth |

Pemilihan (prioritas tertinggi → terendah):
1. Header request `X-Agentgate-Reasoning: reasoning_content|reasoning|both|think_tags|none`
2. Objek `reasoning` pada body (`{"exclude": true}` → `none`)
3. Konfigurasi provider/server (`reasoning.format`)

> Beberapa UI mengenali **baik field khusus maupun tag inline**: UI akan membaca delta `reasoning_content`/`reasoning`/`thinking` bila ada, atau mengurai `<think>` pada teks. Karena dukungan klien berbeda-beda, format harus **bisa diganti tanpa rebuild** dan diuji pada rantai nyata (§11).

### 5.2 Contoh SSE

**`reasoning_content`** (mengikuti pola DeepSeek, `content` bernilai `null` selama reasoning):

```
data: {"id":"chatcmpl-ag-9a1","object":"chat.completion.chunk","created":1762160000,"model":"claude/sonnet","choices":[{"index":0,"delta":{"role":"assistant","content":null,"reasoning_content":"Saya perlu menghitung 47 × 53. "},"finish_reason":null}]}

data: {"id":"chatcmpl-ag-9a1","object":"chat.completion.chunk","created":1762160000,"model":"claude/sonnet","choices":[{"index":0,"delta":{"content":null,"reasoning_content":"47 × 50 = 2350, 47 × 3 = 141."},"finish_reason":null}]}

data: {"id":"chatcmpl-ag-9a1","object":"chat.completion.chunk","created":1762160000,"model":"claude/sonnet","choices":[{"index":0,"delta":{"content":"47 × 53 = 2491."},"finish_reason":null}]}

data: {"id":"chatcmpl-ag-9a1","object":"chat.completion.chunk","created":1762160000,"model":"claude/sonnet","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: [DONE]
```

**`think_tags`** (tag dikirim sebagai chunk utuh agar tidak terpecah di tengah):

```
data: {"…":"…","choices":[{"index":0,"delta":{"role":"assistant","content":"<think>\n"},"finish_reason":null}]}
data: {"…":"…","choices":[{"index":0,"delta":{"content":"Saya perlu menghitung 47 × 53. "},"finish_reason":null}]}
data: {"…":"…","choices":[{"index":0,"delta":{"content":"\n</think>\n\n"},"finish_reason":null}]}
data: {"…":"…","choices":[{"index":0,"delta":{"content":"47 × 53 = 2491."},"finish_reason":null}]}
```

**Non-stream:**

```json
{
  "id": "chatcmpl-ag-9a1",
  "object": "chat.completion",
  "created": 1762160000,
  "model": "claude/sonnet",
  "choices": [{
    "index": 0,
    "message": {
      "role": "assistant",
      "reasoning_content": "Saya perlu menghitung 47 × 53. …",
      "content": "47 × 53 = 2491."
    },
    "finish_reason": "stop"
  }],
  "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0,
             "completion_tokens_details": { "reasoning_tokens": 0 } }
}
```
Pada `think_tags` non-stream: `content = "<think>…</think>\n\n…jawaban"`.

### 5.3 Aturan format (MUST)
- Chunk pertama stream memuat `role:"assistant"` (boleh bersama delta reasoning).
- Selama reasoning, `delta.content` bernilai `null` atau dihilangkan (ikuti `reasoning.null_content`, default `true` = kirim `null`).
- Mode `think_tags`: `<think>` dan `</think>` dikirim sebagai **satu chunk utuh** masing-masing; `</think>` **MUST** dikirim sebelum teks jawaban pertama, juga bila giliran berakhir dengan `tool_calls` tanpa teks.
- Tag di dalam teks reasoning sendiri (`</think>` literal) **MUST** di-escape (`</​think>` dengan zero-width joiner) agar tidak menutup blok dini.
- Reasoning **tidak** boleh muncul di `tool_calls[].function.arguments`.
- Usage: bila diketahui, isi `completion_tokens_details.reasoning_tokens`.

### 5.4 Kontrol dari request

| Input client | Dinormalisasi menjadi |
|---|---|
| `reasoning_effort: "low" \| "medium" \| "high"` (OpenAI) | `ReasoningControl{enabled:true, effort}` |
| `reasoning: {"effort":…, "max_tokens":…, "exclude":…}` (gaya OpenRouter) | idem + `budget_tokens`, `exclude` |
| `thinking: {"type":"enabled","budget_tokens":N}` (gaya Anthropic) | `enabled:true, budget_tokens:N` |
| `think: true \| false \| "low\|medium\|high"` (gaya Ollama) | idem |
| Tidak ada | `enabled = nilai default provider` |

```rust
pub struct ReasoningControl {
    pub enabled: Option<bool>,
    pub effort: Option<Effort>,        // low | medium | high | max
    pub budget_tokens: Option<u32>,
    pub exclude: bool,                 // tampilkan ke client? (bukan: apakah model berpikir)
}
```
- `exclude=true` hanya menyembunyikan dari client; model tetap berpikir bila CLI memutuskan begitu.
- Pemetaan ke CLI: lihat §7. Parameter yang tidak didukung CLI → diabaikan + header `X-Agentgate-Ignored: reasoning_effort`.

### 5.5 Header respons

| Header | Nilai |
|---|---|
| `X-Agentgate-Reasoning-Format` | format yang dipakai |
| `X-Agentgate-Reasoning-Kind` | `raw` \| `summary` \| `omitted` \| `activity` \| `none` (jenis yang **mungkin** dikirim pada respons ini) |
| `X-Agentgate-Reasoning-Granularity` | `token` \| `block` \| `final` \| `none` (hasil kapabilitas adapter) |

Header dikirim sebelum body, jadi nilainya berasal dari **kapabilitas terverifikasi**, bukan dari apa yang kebetulan terjadi.

### 5.6 Riwayat dari client (echo)
Klien dapat mengirim balik reasoning pada riwayat (`reasoning_content`/`reasoning` pada pesan assistant, atau `<think>…</think>` dalam `content`). Gateway:
- **MUST** mengabaikan field `reasoning_content`/`reasoning`/`thinking` pada pesan assistant masuk.
- **MUST** membuang blok `<think>…</think>` dari `content` saat **kanonikalisasi** ledger (`spec_session.md` §6.1) dan saat membangun transkrip **replay** (`spec_tooling.md` §11.2).
- **MUST NOT** meneruskan reasoning historis ke CLI sebagai instruksi.

---

## 6. Pipeline Streaming (Menjawab "gateway masih menunggu")

### 6.1 Anti-pola (dilarang)

| Anti-pola | Akibat |
|---|---|
| `child.wait_with_output()` / `read_to_string()` sebelum mengirim apa pun | Semua event keluar saat proses selesai |
| Mengumpulkan semua `Delta` ke `String` lalu mengirim di akhir | Reasoning dan jawaban tiba bersamaan |
| Parsing JSONL setelah proses exit | Sama seperti di atas |
| Handler HTTP menunggu `Done` sebelum membuka response | Tidak ada streaming |
| Kompresi/buffer middleware pada `text/event-stream` | Chunk tertahan |
| Channel tak terbatas / blocking `send` di dalam lock | Backpressure salah, deadlock |

### 6.2 Pola yang benar

```rust
// 1) baca stdout per baris, tanpa menunggu proses selesai
let mut rd = tokio::io::BufReader::with_capacity(64 * 1024, stdout);
let mut line = Vec::with_capacity(8 * 1024);
loop {
    line.clear();
    let n = rd.read_until(b'\n', &mut line).await?;
    if n == 0 { break; }                          // EOF
    if line.len() > MAX_LINE { continue; }        // batas baris (mis. 4 MiB)
    for ev in parser.parse_line(&line) {          // 0..n RunEvent per baris
        tx.send(ev).await?;                       // channel BERBATAS (mis. 128)
    }
}
```
```rust
// 2) handler HTTP: buka response SEKARANG, alirkan dari channel
let body = async_stream::stream! {
    yield role_chunk();                           // chunk role segera (§6.4)
    while let Some(batch) = coalesced.next().await {
        yield sse_event(batch);
    }
    yield done();
};
```

### 6.3 Lintasan latensi & titik buffering

```
CLI stdout ─► parser ─► channel(128) ─► coalescer(≤40ms) ─► SSE writer ─► [9router] ─► client
   (a)          (b)         (c)              (d)                (e)          (f)
```

| Titik | Risiko | Mitigasi |
|---|---|---|
| (a) | Buffering pipe oleh CLI (block-buffered saat stdout bukan TTY) | Gunakan mode `stream-json`/JSONL (flush per baris); jangan memakai teks mentah |
| (b) | Parser menunggu blok lengkap | Parser per baris, stateful per `content_block` |
| (c) | Channel penuh | Bounded + `await`; jangan buffer tak terbatas |
| (d) | Coalescer menahan terlalu lama | Interval ≤ 40 ms; flush paksa pada batas blok |
| (e) | Kompresi / proxy buffering | `Cache-Control: no-cache`, `X-Accel-Buffering: no`, tanpa gzip untuk SSE |
| (f) | 9router membuffer atau menjatuhkan field | Uji D3 (§1.2); sesuaikan `reasoning.format` |

### 6.4 Chunk awal & TTFT
- `reasoning.early_role_chunk = true` (default): kirim chunk `role:"assistant"` segera setelah request diterima (sebelum CLI siap) bersama komentar SSE `: ping`. Ini menurunkan *waktu hingga byte pertama* dan membuat klien menampilkan indikator "mengetik/berpikir".
- Kegagalan sebelum event pertama pada mode ini dilaporkan sebagai event error di stream (status 200 sudah terkirim). Bila pemilik lebih suka status HTTP akurat, set `false`.

### 6.5 Coalescer

Tujuan: menghindari ribuan chunk 1–3 karakter tetapi tetap terasa real-time.

```
buffer_r (reasoning), buffer_t (text), timer 40 ms
on ReasoningDelta(d): buffer_r += d; arm timer
on Text(t):           flush(buffer_r) dulu → lalu buffer_t += t; arm timer      // jaga urutan
on ReasoningEnd:      flush(buffer_r)                                           // batas blok
on ToolCalls/Done:    flush(buffer_r); flush(buffer_t)                          // batas giliran
on timer:             flush(buffer_r | buffer_t)
on size ≥ 256 B:      flush
```
- Parameter: `reasoning.flush_ms` (default 40), `reasoning.flush_bytes` (default 256).
- Flush **MUST** memotong pada batas karakter UTF-8 (jangan memecah multibyte).
- Teks dan reasoning **tidak pernah** digabung dalam satu chunk.

### 6.6 Batas & backpressure
- `reasoning.max_chars_per_turn` (default 200.000). Terlampaui → buang sisa delta, kirim sekali penanda `[reasoning dipotong]` (format mengikuti `reasoning.format`), lanjut ke teks.
- `reasoning.max_blocks_per_turn` (default 64).
- Client lambat: channel penuh → pembaca stdout melambat (backpressure), bukan memori membengkak.

### 6.7 Cancel
Client putus saat reasoning mengalir → perlakuan sama dengan putus saat teks: cancel Run, kill process tree, Session `tainted` (`spec_session.md` §8.2).

### 6.8 Non-stream
Kumpulkan `ReasoningDelta` ke `reasoning_content` dan `Text` ke `content`; `finish_reason`, `usage` seperti biasa. Format `think_tags` menyatukan keduanya di `content`.

---

## 7. Ketersediaan Reasoning per CLI

> Kapabilitas berbeda tajam antar CLI, model, dan versi. Hasil spike disimpan di `compat/{cli}-{version}.json` pada bagian `reasoning` (§16) dan menjadi sumber kebenaran untuk header §5.5.

### 7.1 Ringkasan

| Provider | Sumber reasoning | Granularitas | Jenis | Catatan utama |
|---|---|---|---|---|
| **claude** | `stream-json` + `--include-partial-messages`: blok `thinking` + `thinking_delta` | **token** (jika tidak *omitted*) | summary/raw (tergantung model) | Pada mode `--print` Claude Code dapat memaksa tampilan thinking menjadi *omitted* untuk sebagian model ⚠️ |
| **gemini** | `stream-json`: event terdokumentasi hanya `init`, `message`, `tool_use`, `tool_result`, `error`, `result` | **none** (belum terdokumentasi) | — | Event thought tidak ada di skema headless resmi; parser harus toleran ⚠️ |
| **opencode** | `run --format json` (per *part*) atau `opencode serve` + SSE | **block** (run) / **token** (serve) ⚠️ | raw/summary | Event `reasoning` dilaporkan oleh sebagian parser, tetapi dokumentasi lain menyebut hanya 5 tipe event yang diteruskan ⚠️ |
| **ollama** | API native: field `message.thinking` | **token** | raw | Parameter `think`; thinking mendahului konten |

### 7.2 claude

**Perintah:** `claude -p --output-format stream-json --verbose --include-partial-messages …` (sudah ada di `spec_tooling.md` §12.4).

**Event relevan** (dibungkus `stream_event` bila `--include-partial-messages` aktif):

```
content_block_start {index:0, content_block:{type:"thinking"}}
content_block_delta {index:0, delta:{type:"thinking_delta", thinking:"…"}}   × banyak
content_block_delta {index:0, delta:{type:"signature_delta", signature:"…"}}
content_block_stop  {index:0}
content_block_start {index:1, content_block:{type:"text"}}
content_block_delta {index:1, delta:{type:"text_delta", text:"…"}}           × banyak
content_block_stop  {index:1}
```
Pemetaan:
| Event CLI | `RunEvent` |
|---|---|
| `content_block_start` bertipe `thinking` | `ReasoningStart{block, kind}` |
| `thinking_delta` (teks non-kosong) | `ReasoningDelta` |
| `signature_delta` | **diabaikan** (jangan diteruskan) |
| `content_block_stop` (blok thinking) | `ReasoningEnd` |
| `redacted_thinking` | `ReasoningStart{Omitted}` + `ReasoningEnd` |
| `text_delta` | `Text` |
| `content_block_start` bertipe `tool_use` | `ToolObserved` (guard) |

**Tanpa `--include-partial-messages`:** thinking datang sebagai **blok utuh** dalam pesan `assistant` (granularitas `block`): masih mendahului teks, tetapi tidak per token.

**Perhatian (L3):**
- Pada mode `--print`, tampilan thinking dapat dipaksa *omitted*: blok thinking datang dengan field `thinking` kosong + `signature`, tanpa `thinking_delta` ⚠️ (dilaporkan pada versi tertentu; sebagian model seperti Sonnet masih mengirim delta, sebagian lain tidak).
- Mengaktifkan thinking pada Claude Code dikendalikan oleh model/pengaturan (mis. variabel lingkungan anggaran thinking atau pengaturan sesi) ⚠️; `reasoning_effort`/`budget_tokens` dipetakan **best-effort** ke pengaturan tersebut, lalu diverifikasi lewat spike.
- Bila blok terdeteksi *omitted*: kirim sesuai §8.3 (jangan mengarang isi).

### 7.3 gemini

- `-o stream-json` menghasilkan `init`, `message` (potongan teks asisten, dikirim langsung saat dihasilkan), `tool_use`, `tool_result`, `error`, `result`.
- **Thought tidak ada pada skema headless yang terdokumentasi.** Sebagian integrasi pihak ketiga memperlakukan event tambahan sebagai reasoning ⚠️ — sifatnya tidak terdokumentasi dan dapat berubah.
- Aturan adapter:
  1. Parser **toleran**: event bertipe `thought`/`thinking`/`reasoning`, atau `message` dengan penanda thought, **bila ada** → `ReasoningDelta` (kind `Summary`).
  2. Event tak dikenal **tidak** error (disimpan sebagai `Unknown`).
  3. Bila versi terpasang tidak mengeluarkannya → `granularity = none` → fallback aktivitas (§8).
- Kontrol thinking (anggaran) lewat konfigurasi model di settings CLI ⚠️ (diverifikasi saat spike).

### 7.4 opencode

- `opencode run --format json` mengeluarkan JSONL per **part**: `step_start`, `text`, `tool_use`, `step_finish` (+ `error`); `text` dikirim sebagai part utuh (bukan delta token), dan `step_finish` memuat hitungan `tokens.reasoning` ⚠️.
- Tipe `reasoning` (`part.text` berisi pemikiran) dilaporkan ada oleh sebagian parser "bila model mendukung", sementara referensi lain menyebut `run` hanya meneruskan lima tipe ⚠️ → **jangan berasumsi**; spike memutuskan.
- Implikasi granularitas pada `run`: **block** (reasoning/teks muncul per part, bukan per token).
- **Jalur token-level (fase 2):** `opencode serve` + langganan event SSE server; event pembaruan *part* untuk teks/reasoning membawa delta ⚠️. Adapter `opencode-serve` dijadwalkan sebagai RS4b (sekaligus mengurangi cold start, lihat `spec_tooling.md` M6+).
- `reasoning_effort` dipetakan ke opsi/varian model opencode ⚠️.
- Hitungan `reasoning_tokens` dari `step_finish` → `Usage`.

### 7.5 ollama

- Model berkemampuan thinking mengeluarkan field `thinking` yang memisahkan jejak penalaran dari jawaban; pada chat native berada di `message.thinking`, dan pada stream reasoning datang **sebelum** token jawaban.
- Parameter `think`: boolean untuk sebagian besar model; model seperti GPT-OSS memerlukan level `low|medium|high`. Thinking aktif secara default pada model yang mendukung.
- Kapabilitas model: `POST /api/show` → `capabilities` memuat `thinking` ⚠️.
- **Rekomendasi:** untuk request yang memakai reasoning, gunakan **API native** `/api/chat` dan **konversi** ke format §5 (NDJSON `message.thinking` → `ReasoningDelta`, `message.content` → `Text`). Bentuk reasoning pada delta `/v1/chat/completions` (nama field) perlu diverifikasi per versi Ollama ⚠️, jadi jalur native lebih dapat diprediksi.
- Pemetaan kontrol: `think` ← `ReasoningControl` (boolean, atau level untuk model yang meminta level). `reasoning_effort:"none"` → `think:false`.
- Pada giliran lanjutan dengan tool, thinking dapat dikirim kembali ke Ollama sebagai field `thinking` (opsional, `ollama.replay_thinking`, default `false`).
- Jalur `/v1` (passthrough murni, `spec_tooling.md` §12.5) tetap tersedia bila `reasoning.format = passthrough`; gateway meneruskan field apa adanya.

### 7.6 Skema kapabilitas (`compat/*.json`)

```json
{
  "cli": "claude", "version": "x.y.z",
  "reasoning": {
    "available": true,
    "granularity": "token",
    "kind": "summary",
    "partial_flag": "--include-partial-messages",
    "omitted_in_headless": false,
    "time_to_first_reasoning_ms": 1350,
    "notes": "diisi spike"
  }
}
```

---

## 8. Fallback bila Reasoning Tidak Tersedia

### 8.1 Prinsip
Bila CLI/model tidak mengekspos reasoning (L3) atau granularitasnya `final`, gateway **tidak mengarang pemikiran**. Opsi yang diizinkan:

| Opsi (`reasoning.fallback`) | Perilaku |
|---|---|
| `none` | Tidak mengirim apa pun |
| `activity` *(default)* | Kirim **aktivitas gateway** (§8.2), diberi label jelas |
| `induce` *(tidak disarankan)* | Menyuruh model "berpikir lalu jawab" lewat prompt; menambah token dan merusak format — hanya eksperimen |

### 8.2 Aktivitas (sintetis, jujur)
Dihasilkan dari event nyata, bukan dari isi pemikiran model:

| Pemicu | Contoh label (`id`) |
|---|---|
| Run mulai | `Menyiapkan agen…` |
| CLI belum mengeluarkan output > 2 detik | `Model sedang bekerja…` |
| `tool_use` terdeteksi / batch diterbitkan | `Meminta tool: write_file` |
| Parked menunggu client | `Menunggu hasil tool dari klien…` |
| Teks pertama mulai | (tidak ada; langsung jawaban) |

Aturan:
- **Tidak** menyertakan argumen tool secara default (`reasoning.activity_args = false`) — hanya nama.
- Frekuensi dibatasi (`reasoning.activity_min_interval_ms`, default 1500); tidak mengulang label yang sama.
- Label dapat dilokalkan (`reasoning.activity_lang = "id"`).
- Diberi awalan penanda agar tidak tampak seperti pemikiran model, mis. `⏳ ` (konfigurasi `reasoning.activity_prefix`).
- Disalurkan lewat jalur reasoning yang sama (`reasoning_content`/`think_tags`) **hanya bila** `reasoning.activity_via_reasoning = true` (default). Bila `false`, aktivitas diabaikan (tidak masuk `content`).
- Header `X-Agentgate-Reasoning-Kind: activity`.

### 8.3 Blok *omitted*
- `reasoning.omitted_marker = "activity"` *(default)*: tampilkan satu aktivitas "Model berpikir (isi disembunyikan oleh penyedia)".
- `"none"`: tidak menampilkan apa pun.
- Tanda tangan/ciphertext **tidak pernah** diteruskan.

---

## 9. Interaksi dengan Tool-Bridge

| Situasi | Perilaku |
|---|---|
| Reasoning sebelum tool call | Dialirkan pada response #n, lalu `ToolCalls`, lalu `finish_reason: "tool_calls"` |
| Setelah resume | Reasoning baru (blok berikutnya) dialirkan pada response #n+1 |
| Run `AwaitingTools` (parked) | Tidak ada reasoning (CLI menunggu); bila `activity`, satu label `WaitingClient` dikirim **sebelum** response #n ditutup |
| Banyak tool dalam satu batch | Satu blok reasoning (jika ada) mendahului seluruh batch |
| `parallel_tool_calls:false` | Reasoning hanya pada awal giliran pertama; panggilan berikutnya tanpa reasoning ulang kecuali CLI mengeluarkannya |
| Thinking bertanda tangan (Anthropic) di antara giliran tool | Dikelola **di dalam sesi CLI** selama Run/sesi sama; pada **replay**, thinking historis hilang (diterima) |
| `tool_calls` tanpa teks | Pada `think_tags`, `</think>` tetap ditutup sebelum chunk `tool_calls` |

Reasoning **tidak** memengaruhi `tool_call_id`, anchor, atau ledger.

---

## 10. Interaksi dengan Sesi (`spec_session.md`)

| Aspek | Aturan |
|---|---|
| Kanonikalisasi ledger | Buang `<think>…</think>` dari teks assistant; abaikan `reasoning_content`/`reasoning` (berlaku di `strict` **dan** `loose`) |
| Commit ledger | Hanya `content` + `tool_calls` yang dihitung; reasoning **tidak** |
| Transkrip replay | Tidak memuat reasoning |
| Estimasi token (`max_est_tokens`) | Reasoning dihitung terpisah (`est_reasoning_tokens`, informasi saja); tidak memicu rotate sendiri |
| Sticky | Sesi CLI menyimpan thinking-nya sendiri; gateway tidak ikut campur |
| Taint | Cancel saat reasoning mengalir → `tainted` seperti biasa |

---

## 11. Kompatibilitas Klien dan Perantara (9router)

### 11.1 Klien
| Klien | Perilaku yang diketahui | Tindakan |
|---|---|---|
| UI yang mengikuti konvensi DeepSeek/OpenRouter | Membaca delta `reasoning_content` / `reasoning` / `thinking`; sebagian juga mengurai tag `<think>` pada teks | Pakai `reasoning_content` (default) atau `both` |
| UI yang hanya mengurai tag | Mengabaikan field khusus | Pakai `think_tags` |
| Klien programatik / SDK | Mengabaikan field tak dikenal; tag di `content` mencemari jawaban | Pakai `none` atau `reasoning_content` |
| **AionUi** | Dukungan reasoning pada mode OpenAI-compatible **belum terverifikasi** | `doctor --probe-reasoning` (§11.3); mulai dari `reasoning_content`, lalu `think_tags` |

Catatan: UI tertentu, saat membangun giliran berikutnya, menyerialisasi reasoning kembali ke `content` sebagai tag atau membuangnya, tergantung jenis koneksi. Karena itu §5.6 (membuang reasoning dari riwayat) bersifat wajib.

### 11.2 Perantara (9router)
- **Passthrough field:** 9router menerjemahkan format antar penyedia; belum diketahui apakah field `reasoning_content`/`reasoning` dipertahankan atau dipetakan ke padanan lain ⚠️. Uji D3.
- **Compress LLM output:** dapat mengubah teks keluaran, mungkin termasuk blok `<think>` ⚠️; bila reasoning hilang hanya pada `think_tags`, gunakan `reasoning_content`.
- **Compress context (Headroom):** menyentuh riwayat masuk (bukan keluaran), tetapi reasoning historis yang di-echo klien juga dapat terkompres; aturan §5.6 menetralkannya.
- **Buffering SSE:** pastikan 9router tidak membuffer; uji dengan `curl -N` melalui 9router.
- Header diagnostik `X-Agentgate-Reasoning-*` bisa dihapus perantara; kebenaran tidak bergantung padanya.

### 11.3 `doctor --probe-reasoning`
Mengirim tiga stream sintetis (tanpa memanggil model): `reasoning_content`, `reasoning`, `think_tags`, masing-masing dengan teks bertanda waktu. Operator melihat mana yang tampil di AionUi dan apakah tiba bertahap melalui 9router. Hasil direkomendasikan sebagai nilai `reasoning.format`.

---

## 12. Konfigurasi

```toml
[reasoning]
enabled              = true
format               = "reasoning_content"   # reasoning_content | reasoning | both | think_tags | none | passthrough
null_content         = true                  # kirim "content": null selama reasoning
early_role_chunk     = true
flush_ms             = 40
flush_bytes          = 256
max_chars_per_turn   = 200000
max_blocks_per_turn  = 64

fallback             = "activity"            # activity | none | induce
activity_via_reasoning = true
activity_args        = false
activity_min_interval_ms = 1500
activity_lang        = "id"
activity_prefix      = "⏳ "
omitted_marker       = "activity"            # activity | none

redact_patterns      = ["(?i)api[_-]?key\\s*[:=]\\s*\\S+", "sk-[A-Za-z0-9_-]{16,}"]   # best-effort

[provider.claude.reasoning]
partial_messages     = true                  # --include-partial-messages
[provider.opencode.reasoning]
source               = "run"                 # run | serve
[provider.gemini.reasoning]
lenient_events       = true
[provider.ollama.reasoning]
use_native_api       = true
replay_thinking      = false
```

Prioritas: header request > body request > config provider > config global.

---

## 13. Keamanan & Privasi

| ID | Risiko | Mitigasi |
|---|---|---|
| RS-S1 | Reasoning memuat data sensitif (isi file, token, argumen tool) | Tidak dicatat di log; `exclude`/`format=none`; `redact_patterns` best-effort sebelum dikirim |
| RS-S2 | Reasoning mengandung markup yang memecah UI (tag `<think>`, HTML) | Escape `</think>`; klien yang merender HTML bertanggung jawab atas sanitasi |
| RS-S3 | Aktivitas membocorkan argumen tool | `activity_args=false` default; hanya nama tool |
| RS-S4 | Reasoning raksasa menghabiskan memori/bandwidth | `max_chars_per_turn`, `max_blocks_per_turn`, channel berbatas |
| RS-S5 | Meneruskan tanda tangan/ciphertext thinking | Selalu dibuang (§7.2) |
| RS-S6 | Upaya mengekstrak reasoning tersembunyi lewat prompt | Tidak dilakukan; `induce` dinonaktifkan secara default dan ditandai eksperimen |
| RS-S7 | Reasoning historis di-echo klien lalu terbawa ke CLI | §5.6: dibuang dari kanonikalisasi dan replay |
| RS-S8 | Kepatuhan ketentuan penyedia terkait penampilan reasoning | Hanya meneruskan apa yang CLI ekspos; tinjau ToS per vendor |

---

## 14. Observabilitas

**Log** (tanpa isi reasoning):

| Event | Field |
|---|---|
| `reasoning.start` | `run_id, block, kind, granularity` |
| `reasoning.end` | `run_id, block, chars, duration_ms` |
| `reasoning.truncated` | `run_id, chars_dropped` |
| `reasoning.fallback` | `run_id, mode(activity|none), cause(L2|L3|omitted)` |
| `reasoning.format` | `run_id, format, source(header|body|config)` |

**Metrik:**
- `agentgate_reasoning_first_chunk_seconds` (histogram): permintaan → chunk reasoning pertama
- `agentgate_reasoning_lead_seconds` (histogram): chunk reasoning pertama → teks pertama. **Nilai ≈ 0 pada CLI yang seharusnya streaming = indikasi buffering (L1/L2).**
- `agentgate_reasoning_chars_total`, `agentgate_reasoning_blocks_total{kind}`
- `agentgate_reasoning_fallback_total{mode,cause}`
- `agentgate_reasoning_flush_total{reason=timer|size|boundary}`
- `agentgate_sse_chunk_bytes` (histogram)

**Alarm:** `reasoning_lead_seconds` p50 mendekati 0 untuk provider berkapabilitas `token` → ada buffering di jalur.

---

## 15. Pengujian

### 15.1 Komponen tiruan
- **`fake-cli-thinking`**: memancarkan JSONL berjadwal: `thinking_delta` setiap 50 ms selama 2 detik, lalu `text_delta`; varian: blok *omitted*, hanya block-level, tanpa thinking, JSON rusak, multibyte terpotong, CRLF.
- **`fake-ollama-think`**: NDJSON `message.thinking` lalu `message.content`.
- **`fake-client`**: mengukur **cap waktu tiba** tiap chunk.

### 15.2 Skenario

| # | Skenario | Hasil |
|---|---|---|
| RS1 | **Inti keluhan:** CLI memancarkan reasoning 2 detik sebelum teks | Chunk reasoning pertama tiba ≤ 150 ms setelah dipancarkan; chunk teks pertama tiba ≥ 1,8 detik **setelah** chunk reasoning pertama (bukan bersamaan) |
| RS2 | Overhead gateway | Selisih waktu emit CLI → terima client ≤ 60 ms (p95) |
| RS3 | Setiap format (`reasoning_content`, `reasoning`, `both`, `think_tags`, `none`) | JSON valid; field sesuai; `[DONE]` ada |
| RS4 | Non-stream | `reasoning_content` + `content` benar; usage terisi bila ada |
| RS5 | Reasoning → tool_calls (bridge) | Urutan: reasoning, `tool_calls`, `finish_reason=tool_calls`; `</think>` tertutup pada `think_tags` |
| RS6 | Reasoning → resume → reasoning → teks | Dua response berbeda, masing-masing urut benar |
| RS7 | Blok *omitted* (kosong + signature) | Tidak ada delta; fallback sesuai `omitted_marker`; signature tidak bocor |
| RS8 | CLI tanpa thinking | `fallback=activity` → label aktivitas; `none` → tidak ada |
| RS9 | Cancel di tengah reasoning | Proses mati ≤ 2 detik; Session `tainted` |
| RS10 | Coalescer | Batas blok memaksa flush; teks tidak mendahului reasoning |
| RS11 | Batas `max_chars_per_turn` | Dipotong, penanda dikirim sekali, jawaban tetap utuh |
| RS12 | Klien meng-echo `<think>` / `reasoning_content` pada riwayat | Ledger identik dengan tanpa reasoning; transkrip replay bersih |
| RS13 | Tag `</think>` literal di dalam reasoning | Di-escape; blok tidak menutup dini |
| RS14 | Tag tidak terpecah antar-chunk | `<think>` dan `</think>` selalu satu chunk |
| RS15 | Multibyte terpotong di batas baca | Teks utuh tanpa karakter rusak |
| RS16 | Baris JSON rusak di tengah | Dilewati; stream lanjut |
| RS17 | `reasoning.exclude=true` / header `none` | Tidak ada reasoning ke client; model tetap berjalan |
| RS18 | Ollama native → konversi | `message.thinking` → `ReasoningDelta`; `think` dipetakan benar; level GPT-OSS |
| RS19 | 50 stream paralel dengan reasoning | Tanpa kebocoran; latensi stabil |
| RS20 | Lewat 9router (`curl -N`) | Reasoning tiba bertahap atau terdokumentasi sebagai dijatuhkan |
| RS21 | Header `X-Agentgate-Reasoning-*` | Nilai sesuai kapabilitas terverifikasi |
| RS22 | Windows: CRLF dan stdout biner | Teks bersih; tidak ada `\r` tersisa |

### 15.3 Uji manual rantai nyata
1. Jalankan D1–D4 (§1.2) untuk tiap CLI.
2. Di AionUi: kirim "Think step by step: berapa 47 × 53?" dengan `reasoning.format` = `reasoning_content`, lalu `think_tags`; catat mana yang tampil dan apakah muncul bertahap.
3. Ulangi dengan **Compress LLM output** ON/OFF di 9router.
4. Ulangi dengan bridge: prompt yang memicu tool; pastikan reasoning muncul sebelum tool call.

---

## 16. Spike

`agentgate spike-reasoning --cli <claude|opencode|gemini|ollama> [--model M] [--prompt P]`

Prompt standar: *"Think step by step, then answer: what is 47*53?"* (reasoning pendek) dan *"Rancang algoritma …"* (reasoning panjang).

Keluaran: **timeline** event bercap waktu + ringkasan:

```
t=0.00s  spawn
t=1.21s  reasoning.start   kind=summary
t=1.24s  reasoning.delta   (n=16, total 412 chars)         ← granularitas token
t=3.80s  reasoning.end
t=3.81s  text.start
t=4.95s  done
=> granularity=token, kind=summary, lead=2.6s, omitted=false
```

| ID | Pertanyaan | Lulus bila |
|---|---|---|
| Q1 | Apakah event reasoning muncul pada mode non-interaktif? | ≥ 1 event reasoning dengan isi |
| Q2 | Granularitas? | `token` / `block` / `final` tercatat |
| Q3 | Jenis? | `raw` / `summary` / `omitted` |
| Q4 | Selisih reasoning → teks pertama (*lead*) | > 0 dan sesuai ekspektasi |
| Q5 | Flag yang diperlukan (`--include-partial-messages`, `--verbose`, `think`) | Tercatat |
| Q6 | Apakah kontrol effort/anggaran berpengaruh? | Panjang reasoning berubah |
| Q7 | Perilaku bersama tool-bridge | Reasoning sebelum `tool_use` terbaca |
| Q8 | Perilaku di Windows (buffering stdout) | Granularitas sama dengan Linux |

Hasil ditulis ke `compat/{cli}-{version}.json` (§7.6) dan dipakai untuk header §5.5.

---

## 17. Rencana Implementasi

| Milestone | Isi | Kriteria selesai |
|---|---|---|
| **RS0** | Pipeline streaming tanpa buffering (reader per baris, channel berbatas, SSE segera) | RS1–RS2 lulus dengan `fake-cli-thinking` |
| **RS1** | Model event + format §5 + coalescer + header + non-stream | RS3–RS6, RS10–RS17 |
| **RS2** | Adapter **claude** (`thinking_delta`, omitted, signature dibuang) | Spike claude; RS7 |
| **RS3** | Adapter **ollama** (native `think`/`message.thinking` → format) | RS18 |
| **RS4** | Adapter **opencode** `run` (block) → fase **serve** (token) | Spike; RS1 pada opencode |
| **RS5** | Adapter **gemini** (parser toleran + fallback) | RS8 pada gemini |
| **RS6** | Fallback aktivitas + marker omitted + i18n | RS8 |
| **RS7** | Matriks klien/9router + `doctor --probe-reasoning` | RS20; keputusan `reasoning.format` |
| **RS8** | Hardening: batas, redaksi, Windows, soak | RS19, RS22 |

Urutan disarankan: **RS0 → RS1 → RS3 (ollama, cepat diuji) → RS2 → RS4 → RS5 → RS6 → RS7 → RS8**. RS0 adalah syarat untuk semua yang lain karena langsung menjawab masalah "menunggu hasil".

---

## 18. Risiko & Pertanyaan Terbuka

| # | Risiko / pertanyaan | Dampak | Mitigasi |
|---|---|---|---|
| R1 | Claude Code `--print` memaksa thinking *omitted* pada model tertentu | Tidak ada reasoning dari claude | Spike per model; fallback aktivitas; model lain; pantau flag/pengaturan baru |
| R2 | Gemini CLI tidak mengekspos thought pada headless | Tanpa reasoning | Parser toleran; fallback aktivitas; pantau skema resmi |
| R3 | opencode `run --format json` tidak per token dan mungkin tanpa event reasoning | Reasoning blok/hilang | `opencode serve` (fase 2) |
| R4 | Klien (AionUi) tidak menampilkan field reasoning | Reasoning tak terlihat | `think_tags`/`both`; probe; konfigurasi per klien |
| R5 | 9router menjatuhkan atau memetakan ulang field reasoning, atau mengubah blok `<think>` | Reasoning hilang/rusak | Uji D3; pilih format yang selamat; laporkan ke operator 9router |
| R6 | `think_tags` mencemari konten klien programatik | Jawaban kotor | Default `reasoning_content`; format per header |
| R7 | Reasoning sensitif tertampil ke banyak pengguna (multi-user) | Kebocoran | `exclude` per subject; redaksi; default dapat dimatikan per tenant |
| R8 | Buffering stdout CLI di pipe | Granularitas turun ke `final` | Mode JSONL; spike Q2; PTY sebagai opsi terakhir |
| R9 | Nama field/event reasoning berubah antar versi | Parser rusak | Kapabilitas per versi di `compat/*.json`; parser toleran; golden file |
| R10 | Reasoning panjang membebani bandwidth/UI | Lambat | Batas, `exclude`, coalescer |

**Pertanyaan terbuka:**
1. Format reasoning mana yang ditampilkan AionUi pada mode OpenAI-compatible, dan apakah 9router meneruskannya?
2. Apakah 9router menyentuh blok `<think>` atau field reasoning saat *Compress LLM output* aktif?
3. Model/versi mana yang dipakai pada claude/gemini/opencode (menentukan ketersediaan thinking)?
4. Apakah pengguna menginginkan reasoning **mentah** saja, atau ringkasan/aktivitas juga dapat diterima?
5. Bolehkah reasoning ditampilkan ke semua pengguna (multi-user) atau perlu kebijakan per subject?

---

## 19. Perubahan yang Diperlukan pada Dokumen Lain

| Dokumen | Perubahan |
|---|---|
| `spec_tooling.md` §10.2 / §12.1 | Ganti `RunEvent::Reasoning(String)`/`CliEvent::Reasoning` dengan `ReasoningStart/Delta/End` + `Activity` (§4.1) |
| `spec_tooling.md` §7.4 | Tambahkan aturan urutan reasoning sebelum `tool_calls` dan format `delta.reasoning_content` |
| `spec_tooling.md` §12.2–12.5 | Tambahkan bagian "Reasoning" per adapter (rujuk §7 dokumen ini) |
| `spec_session.md` §6.1 | Kanonikalisasi: buang `<think>…</think>` dan abaikan field reasoning |
| `tool-handling.md` §10 | Tambahkan modul `reasoning/` (parser, coalescer, format) |
| `plan.md` | Tambahkan referensi dan milestone RS0–RS8 |

---

## Lampiran A — Kerangka Kode

```rust
// reasoning/format.rs
pub enum ReasoningFormat { ReasoningContent, Reasoning, Both, ThinkTags, None, Passthrough }

pub struct SseWriter { fmt: ReasoningFormat, null_content: bool, in_think: bool, /* id, created, model */ }

impl SseWriter {
    pub fn on_reasoning(&mut self, text: &str) -> Vec<Chunk> {
        match self.fmt {
            ReasoningFormat::ReasoningContent => vec![self.delta_reasoning_content(text)],
            ReasoningFormat::Reasoning        => vec![self.delta_reasoning(text)],
            ReasoningFormat::Both             => vec![self.delta_both(text)],
            ReasoningFormat::ThinkTags => {
                let mut out = vec![];
                if !self.in_think { out.push(self.delta_content("<think>\n")); self.in_think = true; }
                out.push(self.delta_content(&escape_close_tag(text)));
                out
            }
            _ => vec![],
        }
    }
    pub fn on_reasoning_end(&mut self) -> Vec<Chunk> {
        if self.fmt == ReasoningFormat::ThinkTags && self.in_think {
            self.in_think = false;
            return vec![self.delta_content("\n</think>\n\n")];
        }
        vec![]
    }
}
```

```rust
// reasoning/coalesce.rs  (inti: jaga urutan, flush pada batas)
async fn run(mut rx: Receiver<RunEvent>, tx: Sender<Chunk>, cfg: Coalesce) {
    let mut r = String::new(); let mut t = String::new();
    let mut tick = tokio::time::interval(Duration::from_millis(cfg.flush_ms));
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Some(RunEvent::ReasoningDelta { text, .. }) => { r.push_str(&text); if r.len() >= cfg.flush_bytes { flush_r(&mut r, &tx).await } }
                Some(RunEvent::ReasoningEnd { .. })         => { flush_r(&mut r, &tx).await; end_reasoning(&tx).await }
                Some(RunEvent::Text(s))                     => { flush_r(&mut r, &tx).await; t.push_str(&s); if t.len() >= cfg.flush_bytes { flush_t(&mut t, &tx).await } }
                Some(RunEvent::ToolCalls(b))                => { flush_r(&mut r, &tx).await; end_reasoning(&tx).await; flush_t(&mut t, &tx).await; send_tool_calls(b, &tx).await }
                Some(RunEvent::Done(f))                     => { flush_r(&mut r, &tx).await; end_reasoning(&tx).await; flush_t(&mut t, &tx).await; send_done(f, &tx).await; break }
                Some(_) => {}
                None    => break,
            },
            _ = tick.tick() => { flush_r(&mut r, &tx).await; flush_t(&mut t, &tx).await }
        }
    }
}
```
(`flush_*` memotong pada batas UTF-8; `end_reasoning` memanggil `SseWriter::on_reasoning_end`.)

## Lampiran B — Pemetaan `stream-json` Claude → `RunEvent` (ringkas)

```rust
fn map_claude_line(v: &Value, st: &mut ClaudeState) -> Vec<RunEvent> {
    // v: {"type":"stream_event","event":{"type":"content_block_delta","index":N,"delta":{...}}}
    match (event_type, delta_type) {
        ("content_block_start", _) if block_type == "thinking"          => vec![ReasoningStart{block:N, kind:Summary}],
        ("content_block_start", _) if block_type == "redacted_thinking" => vec![ReasoningStart{block:N, kind:Omitted}, ReasoningEnd{block:N}],
        ("content_block_delta", "thinking_delta") if !thinking.is_empty() => vec![ReasoningDelta{block:N, text}],
        ("content_block_delta", "signature_delta")                      => vec![],   // jangan diteruskan
        ("content_block_stop",  _) if st.is_thinking(N)                  => vec![ReasoningEnd{block:N}],
        ("content_block_delta", "text_delta")                            => vec![Text(text)],
        _ => vec![],
    }
}
```

## Lampiran C — Checklist Verifikasi (isi saat spike)

| Item | claude | gemini | opencode | ollama |
|---|---|---|---|---|
| Versi & model diuji | | | | |
| Flag yang diperlukan agar reasoning muncul | | | | |
| Granularitas (token/block/final/none) | | | | |
| Jenis (raw/summary/omitted) | | | | |
| *Omitted* pada mode non-interaktif? | | | | |
| Nama event/field reasoning di versi ini | | | | |
| Cara mengontrol effort/anggaran | | | | |
| Reasoning bersama tool_use (urutan) | | | | |
| Hitungan `reasoning_tokens` tersedia | | | | |
| Buffering stdout di Linux / Windows | | | | |
| Lead reasoning → teks (detik) | | | | |
