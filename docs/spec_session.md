# SPEC_SESSION.md — Spesifikasi Manajemen Sesi `agentgate`

| | |
|---|---|
| **Status** | Draft v0.2 (menyertakan keputusan dari rantai AionUi → 9router → agentgate) |
| **Tanggal** | 2026-11-03 |
| **Cakupan provider** | `opencode`, `gemini`, `claude` (Claude Code), `ollama` |
| **Dokumen terkait** | `plan.md`, `tool-handling.md` (v2), `spec_tooling.md` (MCP Bridge) |
| **Perubahan v0.2** | (1) identitas sesi **header-first** + anchor `tool_call_id`; (2) model **multi-user** satu API key (subject); (3) pencocokan toleran terhadap kompresi di hulu (RTK, Headroom, kompres output) — lihat §5, §6.6, §15 |
| **Konvensi** | **MUST / SHOULD / MAY** mengikuti RFC 2119. ⚠️ = *harus diverifikasi pada versi CLI terpasang*. |

---

## 0. Ringkasan Eksekutif

API OpenAI bersifat **stateless**: client mengirim seluruh `messages[]` di setiap request. Agent CLI bersifat **stateful**: punya sesi, riwayat, dan konteks sendiri di disk. `agentgate` berada di antara keduanya dan harus memutuskan, **untuk setiap request**, apakah:

1. **Sticky** — melanjutkan sesi CLI yang sudah ada dan hanya mengirim pesan baru (cepat, hemat token), atau
2. **Replay** — memulai sesi CLI baru dan meratakan seluruh riwayat dari client ke prompt (selalu benar, lebih mahal).

**Prinsip utama: client adalah sumber kebenaran.** Sesi di gateway hanyalah *cache* untuk menghindari replay. Bila ada keraguan sekecil apa pun bahwa state CLI cocok dengan riwayat client, gateway **replay**. Karena itu menghapus, kedaluwarsakan, atau menolak sesi selalu aman.

Mekanisme inti:
- **Ledger** berbasis *hash chain* per sesi → deteksi apakah request merupakan kelanjutan (append) atau percabangan (edit/regenerate).
- **Identitas sesi berlapis (header-first)** → ID eksplisit bila client/9router meneruskannya; *anchor* `tool_call_id`; hash chain content-addressed hanya sebagai **fallback** (tidak andal bila ada kompresi konteks di hulu).
- **Pemisahan *gateway session* dan *CLI session*** → sesi gateway bisa merotasi sesi CLI ketika tercemar (*tainted*), tanpa terlihat oleh client.
- **Isolasi tenant dan subject** (API key + pengguna di baliknya) pada semua pencarian, memakai HMAC berkunci server. Tanpa subject pada mode multi-user, gateway **replay-only** (aman).

---

## 1. Latar Belakang & Masalah

### 1.1 Mengapa sesi perlu diatur

| Masalah | Dampak tanpa manajemen sesi |
|---|---|
| Tiap request memulai sesi CLI baru | Riwayat dikirim ulang terus: lambat, boros token, cold start berulang |
| Riwayat client diratakan jadi satu prompt | Agent mengira riwayat sebagai instruksi baru; struktur role hilang |
| Sesi CLI menumpuk di disk | Disk membesar; **data prompt pengguna tertinggal** di mesin B |
| Dua request paralel memakai sesi yang sama | Lock file, data korup, jawaban tercampur |
| Percakapan berbeda dengan awal identik | Sesi tertukar → kebocoran konteks antar-percakapan |
| Run bridge dibunuh di tengah tool call | Sesi CLI berisi `tool_use` tanpa hasil → gagal di-*resume* |
| Beberapa pengguna/API key | Kebocoran lintas-tenant |

### 1.2 Hubungan dengan bridge
`spec_tooling.md` mendefinisikan **Run** (proses CLI + panggilan MCP tertahan). Dokumen ini mendefinisikan **Session** yang membungkus banyak Run.

---

## 2. Tujuan, Non-Tujuan, Prinsip

### 2.1 Tujuan
- **G1.** Percakapan multi-giliran berjalan tanpa mengirim ulang riwayat ke CLI bila aman (sticky).
- **G2.** Tanpa kerja sama client: header sesi bersifat *opsional*.
- **G3.** Tidak pernah mencampur konteks antar-percakapan atau antar-tenant.
- **G4.** Tahan terhadap edit pesan, regenerate, retry, restart gateway, dan klien yang mengubah format riwayat.
- **G5.** Sumber daya terbatas: TTL, kuota, eviction, pembersihan artefak CLI.
- **G6.** Privasi: gateway tidak menyimpan isi percakapan secara default.

### 2.2 Non-Tujuan (v1)
- Menyimpan/menampilkan riwayat percakapan (itu tugas client).
- Sinkronisasi sesi antar-instance gateway (single-node).
- Ringkasan otomatis (summarization) riwayat; hanya pemotongan.
- Fork dari titik tengah riwayat (CLI umumnya tak mendukung); percabangan = replay.
- Sesi untuk Ollama (stateless; lihat §11).

### 2.3 Prinsip Desain
1. **Client = sumber kebenaran; sesi = cache.**
2. **Fail to replay**, bukan fail to error.
3. **Satu sesi, satu pemakai aktif** (serialisasi per sesi).
4. **Hash, bukan isi**: ledger menyimpan hash; isi percakapan tidak disimpan gateway.
5. **Tenant selalu bagian dari kunci** pencarian.
6. **Taint → rotasi**: sesi CLI yang tak konsisten dibuang, sesi gateway berlanjut.

---

## 3. Terminologi

| Istilah | Arti |
|---|---|
| **Tenant** | Identitas pemanggil di gateway = API key (atau `anon` bila auth mati). Di rantai AionUi→9router, tenant = key yang dipakai 9router |
| **Subject** | Pengguna di balik satu tenant (mis. orang-orang yang berbagi satu API key di 9router). Sumber: API key per-user, header `X-Agentgate-User`, atau field `user` |
| **Anchor** | Penanda sesi yang tahan perubahan isi pesan, mis. `tool_call_id` buatan gateway |
| **Conversation** | Percakapan logis di sisi client |
| **Session (gateway)** | Record gateway yang memetakan conversation → state CLI, berisi ledger |
| **CLI session** | Sesi internal CLI (id milik CLI) di disk mesin B |
| **cli_gen** | Generasi CLI session dalam satu Session (naik saat rotasi) |
| **Run** | Satu eksekusi proses CLI (lihat `spec_tooling.md`) |
| **Ledger** | Daftar hash pesan (hash chain) yang sudah "diketahui" oleh CLI session |
| **Head** | Hash terakhir ledger (+ panjangnya) |
| **Sticky / Replay / Fork / New** | Strategi pelayanan request (§7) |
| **Delta** | Pesan baru di request yang belum ada di ledger |
| **Taint** | Penanda bahwa CLI session tak lagi dapat dipercaya |
| **Rotate** | Mengganti CLI session baru (cli_gen+1) via replay |

---

## 4. Model Data & Hubungan

```
Tenant (API key)
  └── Session (gateway)  ──┬── Ledger (hash chain, head, checkpoints)
        │                  ├── CLI sessions: gen0, gen1, ... (aktif = gen terakhir)
        │                  └── Workdir tetap: {data}/sessions/{sid}/work/
        └── Run (0..1 aktif; Parked = menunggu hasil tool client)
```

- Satu Session ⇄ satu provider dan satu *mode class* (`bridge` | `chat_only` | `passthrough`).
- Perubahan provider atau mode class = Session berbeda.
- Perubahan model dalam provider yang sama **MAY** tetap sticky (CLI umumnya mengizinkan ganti model saat resume ⚠️).

---

## 5. Identifikasi Sesi (Header-First)

Rantai nyata **AionUi → 9router → agentgate** membawa tiga fakta yang membentuk desain ini:

1. Header kustom *kemungkinan* diteruskan oleh AionUi/9router (⚠️ harus dibuktikan, §5.5).
2. 9router dapat mengubah isi pesan sebelum sampai ke gateway: **kompres output tool (RTK)**, **kompres konteks (Headroom)**, dan **kompres output LLM** (§6.6). Akibatnya hash isi pesan tidak selalu stabil.
3. **Banyak orang berbagi satu API key** di sisi gateway (§5.4), sehingga identitas pengguna harus diturunkan dari sinyal lain.

Karena itu identitas sesi bersifat **berlapis**. Urutan resolusi (berhenti pada yang pertama cocok):

| # | Sinyal | Keandalan | Catatan |
|---|---|---|---|
| 1 | `tool_call_id` pada pesan `role:"tool"` di ujung `messages` | Sangat tinggi | Lewat `RunRegistry` → Run → Session (`spec_tooling.md` §10.4). **Wajib cek tenant dan subject sama** |
| 2 | **ID sesi eksplisit** (header/body, §5.1) | Tinggi (bila diteruskan) | Jalur utama; di-*namespace* per tenant+subject |
| 3 | **Anchor `tool_call_id` historis** (§5.3) | Tinggi (untuk percakapan yang pernah memakai tool) | Tahan kompresi isi pesan karena ID tidak diubah ⚠️ |
| 4 | **Hash chain content-addressed** (§6) | Sedang → rendah bila ada kompresi konteks | Fallback; **dimatikan** pada mode multi-user tanpa subject |
| 5 | **Fuzzy / tolerant** (§6.2, opt-in) | Rendah | Default mati |
| 6 | Tidak ada yang cocok | — | Buat Session baru |

### 5.1 ID eksplisit

Sumber dikonfigurasi lewat `session.id_sources` (urutan prioritas). Default:

| Sumber | Contoh |
|---|---|
| `header:X-Agentgate-Session` | `X-Agentgate-Session: conv-123` |
| `header:X-Session-Id` | idem |
| `header:X-Conversation-Id` | idem |
| `body:/metadata/agentgate_session` | `"metadata": {"agentgate_session": "conv-123"}` |
| `body:/metadata/conversation_id` | idem |
| `body:/conversation_id`, `body:/session_id` | field non-standar (sering di-*strip* perantara) |

Aturan:
- Opaque, ≤128 karakter, regex `^[A-Za-z0-9._:-]+$`.
- Disimpan sebagai `external_id` dengan `UNIQUE(tenant_id, subject_id, external_id)`; ID yang sama milik tenant/subject lain = sesi berbeda (tidak pernah bocor).
- Header `X-Agentgate-Session-Reset: true` → buang CLI session lama, mulai baru dengan ID yang sama.
- Respons **MUST** selalu memuat `X-Agentgate-Session: <id gateway>` dan `X-Agentgate-Session-Mode: new|sticky|replay|resume|branch`.
- Gateway **MUST NOT** bergantung pada header respons untuk kebenaran (perantara dapat menghapusnya).

### 5.2 Subject (siapa pengguna)

Sumber dikonfigurasi lewat `session.subject_sources` (urutan prioritas). Default:

| Sumber | Catatan |
|---|---|
| `key` | ID turunan API key (HMAC, bukan key mentah) — **paling kuat bila tiap pengguna punya key sendiri** |
| `header:X-Agentgate-User` | Diteruskan 9router/AionUi bila dikonfigurasi |
| `body:/user` | Field `user` OpenAI |

`subject_id = HMAC(K, tenant ‖ nilai_sumber)`. Bila tidak ada sumber selain `key` dan `key` dipakai banyak orang → lihat §5.4.

### 5.3 Anchor `tool_call_id`

- Saat gateway menerbitkan batch tool (`spec_tooling.md` §10.3), setiap `tool_call_id` dicatat di tabel `anchors(call_id → session_id)`.
- Pada request baru, gateway memindai `messages[]` (pesan assistant `tool_calls[].id` dan pesan `tool.tool_call_id`) dan mencari anchor.
- Jika ditemukan **satu** sesi (dan tenant+subject cocok) → kandidat kuat; verifikasi lanjutan sesuai §6 (panjang riwayat tidak boleh lebih pendek dari ledger yang relevan, kecuali mode `tolerant`).
- Anchor menunjuk **beberapa sesi berbeda** → ambigu → tidak sticky.
- Anchor kedaluwarsa bersama sesinya.
- Kekurangan: percakapan tanpa tool tidak punya anchor. Untuk itu andalkan ID eksplisit.

### 5.4 Model multi-user (banyak orang, satu API key)

| Kondisi | Perilaku |
|---|---|
| Subject tersedia (key per-user / header / `user`) | Isolasi penuh; semua mekanisme (§5 #2–#5) aktif |
| `multi_user = true` dan subject **tidak** tersedia | **Replay-only**: ID eksplisit, anchor, dan content-addressed **dinonaktifkan**; log `warn session.no_subject` |
| `multi_user = false` | Mode single-user; subject = key |

Alasan fail-closed: tanpa subject, dua pengguna yang mengirim riwayat identik (atau sengaja meniru) dapat melanjutkan **sesi CLI milik pengguna lain**, yang konteksnya memuat teks assistant dan hasil tool yang tidak pernah mereka lihat.

Opsi penerapan (urut dari paling kokoh):
1. **Satu API key agentgate per pengguna** dan satu koneksi provider per pengguna di 9router → `key` menjadi subject.
2. 9router/AionUi meneruskan `user` atau `X-Agentgate-User` → subject dari sumber tersebut.
3. Satu instance agentgate per pengguna (isolasi proses dan data).
4. Terima **replay-only** (benar tapi lebih mahal).

### 5.5 Memverifikasi sinyal sampai di gateway

Karena belum dipastikan apa yang sampai lewat rantai:
- `GET /admin/debug/signals` menampilkan, untuk N request terakhir, **nama** sinyal yang tiba (nama header, ada/tidak `user`, ada/tidak `metadata`, jumlah anchor ditemukan) — **tanpa nilai**.
- `agentgate doctor --probe-chain` memandu uji: kirim percakapan dari AionUi dengan penanda, lalu laporkan sinyal mana yang sampai dan apakah riwayat dimodifikasi (RTK/Headroom/output).
- Hasil menentukan jalur utama: bila ID eksplisit/subject sampai → header-first; bila tidak → andalkan anchor + replay, atau kunci per-user (§5.4).

---

## 6. Ledger & Pencarian Content-Addressed (Fallback)

### 6.1 Kanonikalisasi pesan (`canon`)

Tujuan: dua representasi pesan yang sama secara semantik menghasilkan byte identik, meski client/perantara (9router) mengubah format.

| Langkah | Aturan |
|---|---|
| Role | `developer` → `system`; role lain apa adanya |
| Teks | `content` string; atau gabungan bagian bertipe `text` dengan `\n`; bagian non-teks diganti penanda `[non-text]` |
| Normalisasi | Unicode NFC; `\r\n`→`\n`; hapus whitespace di awal/akhir keseluruhan teks |
| `tool_calls` (assistant) | Daftar `(name, args_canon)`; `args_canon` = JSON diparse, kunci diurutkan, ringkas; jika bukan JSON valid → teks ter-trim |
| `tool` | `tool_call_id` **tidak** ikut hash; isi hasil = teks ternormalisasi |
| Field lain | `name`, `refusal`, `audio`, dsb. diabaikan |

Bentuk akhir: `canon(msg) = CBOR/JSON{role, text, tool_calls[]}` (deterministik).

### 6.2 Dua hash chain

Seed (kunci server `K`, 32 byte acak, file `{data}/secret.key`, izin `0600`):

```
h0 = HMAC(K, "ag-session-v1" ‖ tenant_id ‖ subject_id ‖ provider ‖ mode_class)
```

Untuk tiap pesan ke-i:

```
strict_i = HMAC(K, strict_{i-1} ‖ canon_full(msg_i))
loose_i  = HMAC(K, loose_{i-1}  ‖ canon_loose(msg_i))
```

- `canon_full` memuat semua teks.
- `canon_loose` **membuang teks pesan assistant** (tetap memuat role dan `tool_calls` name+args). Alasan: teks assistant yang dikirim ulang client sering tidak identik dengan yang di-stream (trim, markdown, terjemahan 9router, bagian reasoning dibuang).

Mode pencocokan (`session.match`):

| Mode | Yang diabaikan | Yang dicocokkan | Kapan |
|---|---|---|---|
| `strict` | tidak ada | semua teks | Tanpa perantara yang mengubah teks |
| `loose` (**default**) | teks **assistant** | teks `user`, `system`, `tool` (ketat); `tool_calls` name+args | Ada "kompres output LLM" atau klien yang merapikan teks assistant |
| `tolerant` (opt-in) | teks assistant **dan** isi hasil tool; teks `user`/`system` hanya dibandingkan **256 karakter pertama** (ternormalisasi) | role, urutan, `tool_calls` name+args | Ada kompresi konteks di hulu. **Hanya untuk single-user** (`multi_user=false`) |

Catatan `loose`: kompresi hasil tool yang **deterministik** (input sama → output sama, mis. filter output ala RTK) tetap cocok karena gateway menyimpan hash teks *seperti yang diterima*. Kompresi yang bergantung pada posisi/ukuran konteks (Headroom) **tidak** deterministik antar giliran — lihat §6.6.

> Dengan HMAC berkunci server, hash tidak dapat diuji secara eksternal (tidak ada *oracle* untuk menebak isi atau keanggotaan lintas-tenant), dan isi percakapan tidak dapat direkonstruksi dari database.

### 6.3 Algoritma pencarian

Input: `messages[]` (n pesan), tenant, subject, provider, mode class. Langkah ini **dilewati** bila `multi_user=true` dan subject tidak tersedia (§5.4).

```
chain[0..n] = hitung strict & loose untuk setiap prefiks
untuk k = n-1 turun ke 1:                       // prefiks terpanjang yang berakhir sebelum pesan baru
    kandidat = sessions WHERE tenant=… AND provider=… AND mode=…
               AND head_len = k AND head_{match} = chain[k]
    jika kandidat tidak kosong → pilih satu (aturan §6.4) → STICKY dengan delta = messages[k..]
jika tidak ada → cek apakah ada checkpoint (bukan head) yang cocok → BRANCH (replay) ; selain itu NEW
```

Catatan:
- Mulai dari prefiks terpanjang (`k = n-1`, karena minimal 1 pesan baru harus ada).
- Biaya O(n) hash per request (murah; n ≤ ratusan). Indeks DB pada `(tenant_id, provider, head_len, head_strict)` dan `(…, head_loose)`.
- Pesan terakhir yang **harus** baru: bila prefiks penuh (k = n) cocok dengan head (client mengirim ulang request yang sama / *retry*) → perlakukan sebagai **duplikat** (lihat §10.4).

### 6.4 Beberapa kandidat (ambiguitas)
Dua sesi dengan prefiks identik (mis. dua chat baru dengan pesan pertama sama):
1. Buang yang sedang `busy`/`parked`.
2. Pilih `last_used_at` terbaru.
3. Jika tak ada kandidat idle → **tidak sticky** (BRANCH/NEW).

> Keterbatasan yang diketahui: dua percakapan dengan riwayat **identik** hingga titik tertentu bisa berbagi sesi. Secara semantik tidak berbahaya (konteksnya sama), namun client yang butuh jaminan ketat **SHOULD** mengirim `X-Agentgate-Session`.

### 6.5 Checkpoint
Setiap akhir giliran, simpan `(session_id, cli_gen, msg_count, strict, loose)` (maks `session.max_checkpoints`, default 64, yang tertua dibuang). Kegunaan:
- membedakan **BRANCH** (prefiks request cocok dengan checkpoint lama) dari **NEW** — untuk metrik dan diagnosis;
- bukan untuk rewind (CLI umumnya tak mendukung).

---

### 6.6 Kompatibilitas dengan kompresi di hulu (RTK, Headroom, kompres output)

Jawaban dari operator: 9router memiliki tiga fitur yang mengubah isi pesan sebelum sampai ke agentgate.

| Fitur di 9router | Yang dilakukan (ringkas) | Dampak pada request di agentgate | Dampak pada sesi | Penanganan |
|---|---|---|---|---|
| **Compress tool output (RTK)** | Menyaring/meringkas output tool (log, diff, listing) sebelum masuk konteks | Isi `role:"tool"` lebih pendek | Aman bila deterministik; hash teks diterima konsisten antar giliran | `loose`/`strict`; bila tidak deterministik → `tolerant` atau replay |
| **Compress context (Headroom)** | Memadatkan tool output, log, file, **dan riwayat percakapan**; kompresi reversibel (CCR) menyimpan asli dan menambahkan tool pengambil (mis. `headroom_retrieve`) | Pesan lama diringkas/dibuang; **jumlah dan isi pesan berubah antar giliran**; `tools[]` bisa memuat tool tambahan | Hash chain dan `head_len` **tidak stabil** | Header-first + anchor (§5); `ignore_tools_in_hash`; atau matikan fitur ini untuk rute agentgate |
| **Compress LLM output** | Membuat teks assistant lebih ringkas (instruksi dan/atau pasca-proses) | Teks assistant yang dikirim ulang client ≠ teks yang di-stream gateway | `strict` gagal | `loose` (default) |
| (opsional Headroom) catatan "ringkas" di akhir system prompt | Mengubah system prompt | Prefiks posisi 0 berubah saat fitur di-toggle | Replay satu kali setelah toggle | Terima; atau tetapkan `session.system_match = "prefix:512"` |

Aturan implementasi:
1. **Jangan mengandalkan hash isi** pada rute yang melewati kompresi konteks. Jalur utama: ID eksplisit (§5.1) dan anchor (§5.3).
2. **Tool tambahan dari perantara** (mis. `headroom_retrieve`) diperlakukan sebagai tool biasa oleh bridge (`spec_tooling.md` §8.7). `tools_hash` dihitung **tanpa** nama pada `session.ignore_tools_in_hash` (default `["headroom_retrieve"]`), dan hilangnya tool tersebut **tidak** memicu `ToolsIncompatible`.
3. **ID tool call harus dipertahankan perantara.** Seluruh jalur resume bergantung padanya; uji pada rantai nyata (`spec_tooling.md` §16.4).
4. **Dalam mode sticky, kompresi konteks di hulu hampir tidak berguna**: CLI hanya menerima *delta*, bukan seluruh riwayat. Pada replay, kompresi membantu mengecilkan transkrip tetapi mengorbankan stabilitas pencocokan.
5. **Rekomendasi operasional:** untuk rute ke agentgate, matikan *Compress context*; biarkan *Compress tool output* dan *Compress LLM output* tetap aktif (aman dengan `loose`). Ukur `sticky_ratio` dengan dan tanpa kompresi untuk memutuskan.
6. **RTK sebagai hook agent di mesin B** (bukan fitur 9router) hanya relevan pada mode PASSTHROUGH: hook menulis ulang perintah Bash yang dijalankan CLI di B. Di mode BRIDGE/CHAT_ONLY tidak ada Bash di B, sehingga tidak berlaku.

---

## 7. Strategi Pelayanan Request

### 7.1 Tabel strategi

| Strategi | Kondisi | CLI session | Prompt ke CLI |
|---|---|---|---|
| **resume** | `tool_call_id` cocok Parked run | sesi & Run yang sama | hasil tool (lihat `spec_tooling.md`) |
| **sticky** | Prefiks = head sesi idle, CLI mendukung resume, tidak tainted | lanjutkan `cli_session_id` | **delta saja** |
| **replay** | Tidak ada kecocokan / branch / tainted / limit / CLI tak dapat resume | **sesi CLI baru** (`cli_gen+1` atau Session baru) | transkrip seluruh riwayat + delta |
| **new** | Percakapan baru (1 pesan user, atau tanpa kecocokan & ledger kosong) | sesi CLI baru | pesan user (+system) |
| **branch** | Prefiks cocok *checkpoint* lama (client mengedit/regenerate) | sesi CLI baru (Session baru, `parent_id` = lama) | transkrip |

### 7.2 Alasan replay (enum, dicatat di log & header)

```rust
enum ReplayReason {
    NoMatch,            // tidak ada sesi cocok
    Branch,             // edit/regenerate terdeteksi
    Tainted,            // CLI session tercemar
    LimitRotate,        // melewati max_turns / ukuran / token
    CliNoResume,        // provider tidak mendukung sticky
    ToolsIncompatible,  // tool yang pernah dipakai tak ada lagi
    ExplicitMismatch,   // ID eksplisit tapi ledger tidak cocok (client = kebenaran)
    SessionExpired,     // TTL/eviction
    ModeChanged,        // mode class berbeda
    OffByConfig,        // session.mode = "off"
}
```

### 7.3 Delta
`delta = messages[head_len ..]`.
- Umumnya satu pesan `user` → dikirim apa adanya (tanpa pembungkus transkrip) agar CLI melihat giliran natural.
- Bila delta berisi >1 pesan (client menambah beberapa pesan sekaligus) → render sebagai transkrip singkat (format `spec_tooling.md` §11.2).
- Pesan `system` baru di delta → perlakukan sebagai perubahan system prompt → **replay** (`NoMatch`), sebab prefiks (posisi 0) berubah.

### 7.4 Perubahan `tools[]` antar request (mode bridge)
Simpan `tools_hash` dan daftar nama tool yang **pernah dipakai** di sesi.
- Himpunan tool baru ⊇ yang pernah dipakai → sticky tetap (MCP dikonfigurasi ulang per Run karena config disuntik per-run).
- Ada tool yang pernah dipakai tetapi hilang → **replay** (`ToolsIncompatible`).

---

## 8. Siklus Hidup Sesi

### 8.1 State machine Session

```
        buat                  Run dimulai                Run selesai OK
 (none) ────► Idle ───────────────────► Busy ─────────────────────────► Idle
                ▲                        │  │
                │ resume selesai         │  └─ batch tool terbit ─► Parked ──┐
                │                        │                                    │ hasil tiba
                │                        ▼                                    ▼
                │             crash / cancel / error                    Busy (resume)
                │                        │                                    
                │                        ▼         TTL parked habis / dibunuh
                │                    Tainted ◄────────────────────────────────┘
                │                        │ request berikut → ROTATE (replay)
                └────────────────────────┘
        Idle/Tainted ── TTL / evict / DELETE ──► Closed (artefak CLI dihapus)
```

| State | Arti | Boleh menerima |
|---|---|---|
| `idle` | Siap sticky | Request baru (kunci sesi diambil) |
| `busy` | Run sedang berjalan/streaming | Tidak (tunggu/branch, §9) |
| `parked` | Run menunggu hasil tool | Hanya *resume* untuk batch yang menunggu |
| `tainted` | CLI session tak dipercaya | Request berikut → **rotate** |
| `closed` | Selesai/dihapus | — |

### 8.2 Kondisi taint (MUST menandai `tainted`)
- Proses CLI dibunuh saat `busy` (client putus, timeout).
- Parked run kedaluwarsa atau dibatalkan (sisa `tool_use` tanpa hasil di sesi CLI).
- CLI crash / exit kode ≠ 0 / output tak terparse.
- Guard bridge melanggar (tool non-bridge terdeteksi).
- Gateway restart saat sesi `busy`/`parked`.
- Replay-resume sebagian (hasil hilang diselesaikan error-result **tidak** taint; konsisten secara struktur).

### 8.3 Rotate
`rotate(session)`: tutup CLI session lama (hapus artefak best-effort), `cli_gen += 1`, bangun CLI session baru via **replay** dari `messages[]` client, **reset ledger** ke riwayat request (client = kebenaran). Sesi gateway dan `external_id` tetap sama.

### 8.4 Retensi & eviction

| Parameter | Default | Keterangan |
|---|---|---|
| `session.idle_ttl_secs` | 3600 | Idle → `closed` |
| `session.max_age_secs` | 86400 | Umur mutlak |
| `session.max_per_tenant` | 64 | Lebihi → evict LRU idle milik tenant |
| `session.max_total` | 512 | Lebihi → evict LRU global |
| `session.max_turns` | 200 | Lebihi → rotate dengan pemotongan |
| `session.max_est_tokens` | 150000 | Estimasi (`chars/4`); lebihi → rotate dengan pemotongan |
| `session.max_cli_bytes` | 20 MB | Ukuran artefak CLI; lebihi → rotate |
| `session.gc_interval_secs` | 60 | Penyapu latar belakang |

- Sesi `busy`/`parked` **tidak pernah** di-evict oleh LRU (hanya oleh TTL parked / pembatalan).
- Eviction aman: client akan menyebabkan replay pada giliran berikutnya.

### 8.5 Penutupan & pembersihan artefak (privasi)
Saat `closed`:
1. Hapus artefak CLI session (metode per provider, §12) — best-effort; kegagalan dicatat dan dilaporkan di `doctor` (`session_cleanup: degraded`).
2. Hapus direktori `{data}/sessions/{sid}/` (retry di Windows bila file terkunci).
3. Hapus baris DB (atau tandai `closed` dan hapus setelah `audit_retention`).

---

## 9. Konkurensi

### 9.1 Aturan
- Satu Session = **satu Run aktif** (`busy` atau `parked`).
- Operasi mutasi Session dilakukan oleh satu *owner task* (hindari lock lintas `.await`); handler HTTP berkomunikasi lewat channel.

### 9.2 Request tiba saat Session tidak idle

| Kondisi | Kebijakan (`session.on_busy`) | Hasil |
|---|---|---|
| Sesi implisit (content-addressed), `busy` | `branch` *(default)* | Tunggu hingga `lock_wait_ms` (default 2000); bila masih busy → layani sebagai **branch** (Session baru, replay) |
| Sesi eksplisit, `busy` | `wait` *(default)* | Tunggu `lock_wait_ms`, lalu `409 session_busy` |
| Sesi `parked`, request **resume** yang cocok | — | Dilayani (§5 #1) |
| Sesi `parked`, request **non-resume** (user mengetik lagi) | `cancel_parked` *(default)* \| `reject` | `cancel_parked`: bunuh Run, tandai `tainted`, layani via rotate-replay. `reject`: `409 session_parked` |

### 9.3 Deduplikasi request in-flight
Retry otomatis (9router/client) bisa mengirim body identik saat request pertama masih berjalan.
- `session.dedupe_inflight = true` (default): fingerprint `HMAC(K, tenant ‖ body_bytes_kanon)`; duplikat saat request pertama masih in-flight → `409 duplicate_inflight` (tidak memulai Run kedua).
- Duplikat setelah selesai (request identik dikirim lagi, prefiks penuh = head) → diperlakukan sebagai **giliran baru dengan delta kosong**: tidak sticky; lakukan **replay** (jangan menimpa ledger).

### 9.4 Kunci global CLI
Beberapa CLI memakai SQLite/file lock di direktori global mereka (⚠️ per-CLI). `session.max_concurrent_runs_per_provider` (default 2) membatasi Run paralel; permit semaphore dipegang selama Run hidup (termasuk parked) seperti di `spec_tooling.md` §10.7.

---

## 10. Algoritma Request End-to-End

### 10.1 Pseudocode

```text
handle_chat(req, tenant):
  provider, model = resolve(req.model)
  if provider.kind == model_api:               # ollama
      return proxy_native(req)                 # tanpa sesi (§11)

  mode_class = classify_mode(req)              # bridge | chat_only | passthrough

  # 1) resume via tool_call_id
  if trailing_tool_msgs(req):
      run = registry.lookup(ids)
      if run && run.session.tenant == tenant:
          return resume(run, req)
      else → lanjut sebagai replay (ReplayReason::SessionExpired)

  # 2) dedupe in-flight
  if cfg.dedupe && inflight.contains(fingerprint(req)): return 409 duplicate_inflight

  # 3) resolusi sesi
  if explicit_id(req):
      s = store.get_or_create(tenant, explicit_id, provider, mode_class)
      plan = plan_for_explicit(s, req)         # sticky bila prefiks = head, selain itu replay(ExplicitMismatch)
  else:
      plan = lookup_content_addressed(req, tenant, provider, mode_class)   # §6.3

  # 4) kunci sesi sesuai on_busy (§9.2)
  s = acquire(plan.session, policy)            # bisa → branch / 409

  # 5) validasi sticky
  if plan == Sticky:
      if s.tainted           → plan = Replay(Tainted)
      if limits_exceeded(s)  → plan = Replay(LimitRotate) with truncation
      if !tools_compatible   → plan = Replay(ToolsIncompatible)
      if !provider.caps.resume → plan = Replay(CliNoResume)

  # 6) bangun prompt
  prompt = match plan:
     Sticky → render_delta(plan.delta)
     Replay/New/Branch → render_transcript(req.messages, truncate_to(cfg.max_chars))

  # 7) jalankan
  run = spawn(provider, s, plan, prompt)       # --resume <cli_session_id> bila sticky
  stream/collect hasil; catat ledger_pending
  on Done: commit_ledger(s, req.messages + assistant_msgs_emitted); s.state = idle
  on Park: s.state = parked
  on Cancel/Crash/Expire: s.state = tainted
  release(s)
```

### 10.2 Commit ledger
Pada `Done` (bukan sebelum):
```
ledger := canon(req.messages) + canon(assistant messages yang benar-benar dikirim gateway)
head   := (len, strict_chain, loose_chain)
checkpoint += head; turns += 1; est_tokens += estimasi
```
Assistant messages yang dihitung = yang akan dikirim ulang client di giliran berikutnya:
- teks akhir (satu pesan `assistant`);
- pada bridge: tiap batch `assistant(tool_calls)` + pesan `tool` hasilnya, berurutan, sebelum teks akhir.

Jika client **tidak** mengirim balik pesan assistant (membuangnya), request berikutnya tidak cocok → replay. Aman.

### 10.3 Contoh urutan multi-giliran (sticky)

```
Giliran 1  messages=[sys, user:"halo"]            → NEW      cli_sess=C1   head=len3(sys,user,asst)
Giliran 2  messages=[sys,user,asst,user:"lanjut"] → STICKY   delta=[user:"lanjut"]  --resume C1
Giliran 3  (user mengedit pesan ke-2)             → BRANCH   Session baru S2 (parent=S1), replay
Giliran 4  (S1 muncul lagi dari tab lain)         → STICKY pada S1 jika prefiks = head S1
```

### 10.4 Kasus khusus
| Kasus | Perilaku |
|---|---|
| Regenerate (prefiks sama, pesan assistant terakhir dibuang, user terakhir sama) | Prefiks `[..user]` = checkpoint lama → **BRANCH** (replay). Sesi lama tetap utuh |
| Client menambah pesan `system` di tengah | Replay (`NoMatch`) |
| Riwayat dipotong client (sliding window) | Prefiks tak cocok → replay; sesi baru terbentuk |
| Retry persis (prefiks penuh = head) | Replay giliran baru; ledger lama tidak ditimpa (§9.3) |
| Jarak waktu lama (sesi `closed`) | NEW + replay |

---

## 11. Perilaku per Provider

### 11.1 Kapabilitas sesi (`SessionCaps`)

```rust
pub struct SessionCaps {
    pub resume: bool,                 // dapat melanjutkan sesi CLI
    pub explicit_id: bool,            // kita yang menentukan id sesi CLI
    pub capture: CaptureMethod,       // ExplicitId | ParseEvent | ListDiff | None
    pub fork: bool,                   // dapat fork dari akhir sesi
    pub delete: DeleteMethod,         // Command | Api | FileRemoval | None
    pub cwd_scoped: bool,             // lokasi sesi bergantung cwd
    pub disable_persistence: bool,    // ada flag sesi sekali pakai
}
```
Hasil dicache per versi di `compat/{cli}-{version}.json` (diisi lewat spike, lihat `spec_tooling.md` §17) dan dipakai `session_preflight`.

### 11.2 Ringkasan

| Aspek | opencode | gemini | claude | ollama |
|---|---|---|---|---|
| Sticky didukung | ✅ ⚠️ | ✅ ⚠️ | ✅ | ❌ (stateless) |
| ID sesi CLI | dihasilkan CLI → **ditangkap** | dihasilkan CLI → **ditangkap** | **kita tentukan** (UUID) | n/a |
| Perintah lanjut | `--session <id>` / `--continue` ⚠️ | `--resume <id\|latest>` ⚠️ | `--resume <uuid>` | n/a |
| Sesi sekali pakai | hapus setelah selesai ⚠️ | hapus setelah selesai ⚠️ | `--no-session-persistence` ⚠️ | n/a |
| Lokasi sesi | data dir opencode (per proyek/cwd) ⚠️ | `~/.gemini/tmp/<hash-proyek>/chats` ⚠️ | `~/.claude/projects/<cwd-ter-encode>/<id>.jsonl` ⚠️ | n/a |
| Bergantung cwd | ya ⚠️ | ya ⚠️ | **ya** | n/a |
| Hapus sesi | `session delete` / API ⚠️ | hapus berkas ⚠️ | hapus `.jsonl` | n/a |
| Fork | ⚠️ | ⚠️ | `--fork-session` ⚠️ | n/a |

### 11.3 **Perubahan penting: cwd per-sesi**
Karena lokasi sesi CLI umumnya diturunkan dari **cwd**, sticky mensyaratkan **cwd stabil per Session**:

```
{data_dir}/sessions/{session_id}/work/       ← cwd untuk SEMUA Run dalam Session ini
```
Ini **menggantikan** "cwd kosong per Run" di `spec_tooling.md` §11.3 (tetap kosong dan terisolasi, tetapi dibagi antar-Run satu Session). Saat rotate, direktori boleh dipakai ulang; saat `closed`, dihapus.

### 11.4 claude
- Run pertama: `--session-id <uuid-baru>` (kita tentukan → `capture = ExplicitId`).
- Run lanjutan (sticky): `--resume <uuid>` + flag bridge seperti biasa.
- **Konflik:** `--no-session-persistence` **tidak boleh** dipakai pada mode sticky (kalau tidak, tidak ada yang dilanjutkan). Pakai hanya saat `session.mode = off` atau Session `ephemeral`.
- Penutupan: hapus `.jsonl` terkait (path dihitung dari cwd per-sesi ter-encode) dan folder proyek bila kosong ⚠️.
- Auto-compaction internal Claude Code memengaruhi konteks CLI tetapi tidak ledger (tidak masalah; ledger tidak bergantung pada isi CLI).
- `--system-prompt` pada resume: kirim ulang bila CLI mensyaratkannya, tetapi **jangan** mengubah ledger ⚠️.

### 11.5 opencode
- Run pertama: tanpa `--session`; **tangkap** id dari event JSON (`--format json`) atau selisih `opencode session list` sebelum/sesudah ⚠️ (`capture = ParseEvent | ListDiff`).
- Sticky: `opencode run --session <id> …` ⚠️. Sesi tersimpan di data dir opencode; pastikan cwd stabil (§11.3).
- **Jangan** override `XDG_DATA_HOME` (login/auth ada di sana).
- Penutupan: `opencode session delete <id>` ⚠️ atau API server bila memakai `opencode serve`.
- Fase 2 (`opencode serve`): satu proses hidup, `POST /session` membuat sesi, pesan via API sesi — menghilangkan cold start dan memberi id sesi pasti; Session gateway memetakan ke id sesi server.

### 11.6 gemini
- Run pertama: tangkap `session_id` dari event `init` pada `-o stream-json` ⚠️.
- Sticky: `gemini --resume <id>` ⚠️ (beberapa versi hanya mendukung `latest`/indeks → **berbahaya** jika ada Session lain di cwd yang sama; karena cwd per-sesi unik, `latest` aman di cwd itu ⚠️).
- Penutupan: hapus berkas chat sesi di direktori sementara proyek ⚠️.
- Bila versi terpasang tidak punya resume yang andal → `caps.resume = false` → selalu replay.

### 11.7 ollama (tanpa sesi)
- Stateless: gateway **tidak** membuat Session. Tiap request diteruskan penuh.
- Optimasi implisit: Ollama men-cache prefiks prompt (KV cache) selama model tetap dimuat → pertahankan urutan dan isi pesan apa adanya dan atur `keep_alive` agar giliran berikutnya cepat.
- Peringatan: `num_ctx` kecil memotong riwayat panjang tanpa pemberitahuan (lihat `spec_tooling.md` §12.5).

---

## 12. Penyimpanan & Pemulihan

### 12.1 Backend
`session.store = "sqlite" | "memory"`.
- `sqlite` (default): file `{data_dir}/agentgate.db`, mode WAL, `PRAGMA user_version` untuk migrasi, izin berkas `0600` (Linux/macOS), folder data di `%LOCALAPPDATA%\agentgate` (Windows).
- `memory`: tanpa persistensi (sesi hilang saat restart; CLI session yatim dibersihkan saat start).

Crate yang disarankan: `rusqlite` (bundled) atau `sqlx` (sqlite); `hmac` + `sha2`; `unicode-normalization`; `dashmap`.

### 12.2 Skema

```sql
CREATE TABLE sessions (
  id              TEXT PRIMARY KEY,           -- ag_ses_<128-bit hex>
  tenant_id       TEXT NOT NULL,              -- id turunan API key (bukan key mentah)
  subject_id      TEXT NOT NULL,              -- pengguna di balik tenant (HMAC)
  external_id     TEXT,                       -- X-Agentgate-Session dari client
  provider        TEXT NOT NULL,
  mode_class      TEXT NOT NULL,              -- bridge | chat_only | passthrough
  model           TEXT NOT NULL,
  workspace       TEXT,
  state           TEXT NOT NULL,              -- idle | busy | parked | tainted | closed
  cli_gen         INTEGER NOT NULL DEFAULT 0,
  cli_session_id  TEXT,                       -- id sesi milik CLI (aktif)
  head_len        INTEGER NOT NULL DEFAULT 0,
  head_strict     BLOB,
  head_loose      BLOB,
  tools_hash      BLOB,
  used_tools      TEXT,                       -- JSON array nama tool yang pernah dipakai
  parent_id       TEXT,
  turns           INTEGER NOT NULL DEFAULT 0,
  est_tokens      INTEGER NOT NULL DEFAULT 0,
  cli_bytes       INTEGER NOT NULL DEFAULT 0,
  created_at      INTEGER NOT NULL,
  last_used_at    INTEGER NOT NULL,
  expires_at      INTEGER NOT NULL,
  UNIQUE (tenant_id, subject_id, external_id)
);
CREATE INDEX idx_sess_lookup_strict ON sessions (tenant_id, subject_id, provider, mode_class, head_len, head_strict);
CREATE INDEX idx_sess_lookup_loose  ON sessions (tenant_id, subject_id, provider, mode_class, head_len, head_loose);
CREATE INDEX idx_sess_expiry        ON sessions (state, expires_at);

CREATE TABLE checkpoints (
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  cli_gen    INTEGER NOT NULL,
  msg_count  INTEGER NOT NULL,
  strict     BLOB NOT NULL,
  loose      BLOB NOT NULL,
  PRIMARY KEY (session_id, msg_count)
);
CREATE INDEX idx_cp_strict ON checkpoints (strict);
CREATE INDEX idx_cp_loose  ON checkpoints (loose);

CREATE TABLE anchors (                     -- tool_call_id buatan gateway → sesi
  call_id     TEXT PRIMARY KEY,
  session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  run_id      TEXT NOT NULL,
  created_at  INTEGER NOT NULL
);

CREATE TABLE cli_artifacts (               -- untuk pembersihan privasi
  session_id     TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  cli_gen        INTEGER NOT NULL,
  cli_session_id TEXT NOT NULL,
  path_hint      TEXT,
  deleted_at     INTEGER,
  PRIMARY KEY (session_id, cli_gen)
);
```

Tidak ada kolom yang menyimpan isi pesan, prompt, argumen tool, atau hasil tool.

### 12.3 Pemulihan saat start

1. Muat konfigurasi; buka DB; jalankan migrasi.
2. Semua Session berstatus `busy`/`parked` → set `tainted` (proses CLI sudah mati/yatim).
3. Baca PID file Run lama → bunuh proses yatim (process tree).
4. Sapu `cli_artifacts` yang `deleted_at IS NULL` untuk sesi `closed`/expired → coba hapus ulang.
5. Sapu direktori `{data}/sessions/*` tanpa baris DB.
6. Mulai penyapu GC.

Sesi `idle` tetap sticky-able selama artefak CLI-nya masih ada; bila `--resume` gagal (sesi hilang), tangkap kegagalan → **replay otomatis** pada request yang sama (`CliNoResume` → tandai `tainted`).

### 12.4 Kegagalan resume CLI
Jika `--resume` mengembalikan error "session not found"/exit ≠ 0 sebelum output apa pun:
1. Tandai `tainted`, rotate.
2. Ulang request sebagai replay (transparan ke client, maksimal 1 kali).
3. Log `session.resume_failed{provider, reason}`; metrik naik.

---

## 13. API & Header

### 13.1 Header

| Header | Arah | Fungsi |
|---|---|---|
| `X-Agentgate-Session` | req | ID sesi eksplisit (opsional) |
| `X-Agentgate-Session` | resp | ID sesi gateway yang melayani (selalu) |
| `X-Agentgate-Session-Mode` | resp | `new` \| `sticky` \| `replay` \| `resume` \| `branch` |
| `X-Agentgate-Replay-Reason` | resp | Nilai `ReplayReason` bila replay |
| `X-Agentgate-Session-Reset` | req | `true` = buang CLI session lama |
| `X-Agentgate-Session-Ephemeral` | req | `true` = jangan simpan (sekali pakai, tanpa sticky) |

Catatan: header respons ditambahkan sebelum body; pada SSE ikut di header HTTP awal. Perantara (9router) mungkin menghapusnya — gateway **tidak bergantung** pada header respons untuk kebenaran.

### 13.2 Endpoint admin (butuh API key admin / loopback)

| Method | Path | Fungsi |
|---|---|---|
| GET | `/admin/sessions` | Daftar (`?tenant=&state=&provider=`), tanpa isi percakapan |
| GET | `/admin/sessions/{id}` | Detail: state, cli_gen, turns, umur, head_len (bukan hash penuh) |
| POST | `/admin/sessions/{id}/rotate` | Paksa rotate |
| POST | `/admin/sessions/{id}/taint` | Tandai tainted (diagnostik) |
| DELETE | `/admin/sessions/{id}` | Tutup + bersihkan artefak |
| POST | `/admin/sessions/gc` | Jalankan GC sekarang |
| GET | `/admin/sessions/stats` | Statistik (sticky ratio, replay per alasan) |
| GET | `/admin/debug/signals` | Nama sinyal identitas yang tiba pada N request terakhir (header/`user`/`metadata`/anchor), **tanpa nilai** |

### 13.3 CLI

```
agentgate sessions list [--tenant T] [--state S]
agentgate sessions show <id>
agentgate sessions rm <id>        # tutup + bersihkan
agentgate sessions gc
agentgate doctor --sessions       # cek kapabilitas resume/cleanup per CLI
agentgate doctor --probe-chain    # uji sinyal identitas & modifikasi pesan di rantai 9router
```

---

## 14. Konstruksi Prompt untuk Sesi

| Strategi | Isi prompt | System prompt |
|---|---|---|
| **new** | pesan user (+ riwayat bila ada) | dikirim lewat mekanisme CLI (`spec_tooling.md` §11.1) |
| **sticky** | **delta saja**, tanpa pembungkus bila 1 pesan | tidak dikirim ulang (sudah dalam sesi CLI) ⚠️ |
| **replay/branch** | `<riwayat>…</riwayat><permintaan_saat_ini>…</permintaan_saat_ini>` | dikirim seperti `new` |

Pemotongan replay (`prompt.max_chars`, default 400.000): pertahankan system + N giliran terakhir; sisipkan `[riwayat lama dipotong]`; jangan memotong di tengah pasangan `tool_calls` ↔ hasil (buang keduanya bersama).

Estimasi token: `ceil(chars / 4)` (tanpa tokenizer; hanya untuk batas rotate dan `usage` perkiraan).

---

## 15. Keamanan & Privasi

| ID | Ancaman | Mitigasi |
|---|---|---|
| SE1 | Kebocoran lintas-tenant | `tenant_id` di seed hash **dan** di semua query; ID eksplisit di-*namespace* per tenant; cek tenant pada `tool_call_id` resume |
| SE2 | Menebak keanggotaan sesi lewat hash | HMAC berkunci server; tidak ada endpoint yang mengembalikan hash |
| SE3 | Pencurian sesi (session fixation/hijack) | ID gateway acak 128-bit; ID eksplisit hanya valid untuk tenant pembuat |
| SE4 | Data percakapan tertinggal di disk B | Hapus artefak CLI saat `closed`; TTL pendek; `doctor` melaporkan cleanup gagal; opsi `session.mode = off` |
| SE5 | Isi percakapan di log/DB | Tidak dicatat; hanya hash, ukuran, hitung; `session.debug_store_content=false` |
| SE6 | Sesi tercampur karena hash collision | HMAC-SHA256 (collision praktis mustahil); pencocokan panjang (`head_len`) ikut |
| SE7 | Exhaustion (banyak sesi/disk) | Kuota per tenant/global, `max_cli_bytes`, LRU, GC |
| SE8 | Dedupe disalahgunakan | Fingerprint ber-HMAC dan tenant-scoped |
| SE9 | Berkas kunci/DB dibaca user lain | Izin `0600`; Windows: ACL pengguna saja |
| SE10 | Sesi lama mengandung konteks sensitif yang tak ingin dibawa | `X-Agentgate-Session-Reset`/`Ephemeral`; `DELETE` admin |
| SE11 | Pengguna A melanjutkan sesi CLI pengguna B (satu API key) | `subject_id` di seed hash dan semua query; tanpa subject pada `multi_user` → **replay-only** (§5.4) |
| SE12 | Membajak sesi dengan menebak ID eksplisit | ID hanya valid dalam namespace tenant+subject; ID gateway acak 128-bit; ID eksplisit tak pernah memberi akses lintas subject |
| SE13 | Mode `tolerant` menyatukan percakapan berbeda | Hanya single-user; wajib opt-in; metrik `tolerant_match_total`; peringatan di `doctor` |
| SE14 | Tool tambahan dari perantara (mis. `headroom_retrieve`) disalahgunakan model | Diperlakukan sebagai tool biasa; eksekusi tetap di pihak yang menerima `tool_calls`; batas `max_tools` dan skema |

Catatan privasi: CLI sendiri menyimpan isi prompt di mesin B (di luar kendali gateway). Dokumentasikan ke pengguna dan sediakan mode `off`/`ephemeral` untuk percakapan sensitif.

---

## 16. Konfigurasi

```toml
[session]
mode               = "auto"        # off | auto
store              = "sqlite"      # sqlite | memory
match              = "loose"       # strict | loose | tolerant (tolerant hanya single-user)
multi_user         = true          # banyak orang di balik satu API key (jawaban operator: ya)
id_sources         = ["header:X-Agentgate-Session", "header:X-Session-Id", "header:X-Conversation-Id",
                      "body:/metadata/agentgate_session", "body:/metadata/conversation_id"]
subject_sources    = ["key", "header:X-Agentgate-User", "body:/user"]
anchors            = true          # lacak tool_call_id sebagai penanda sesi
content_lookup     = "auto"        # auto | off ; auto = aktif kecuali multi_user tanpa subject
fuzzy              = false         # opt-in; lihat match = "tolerant"
system_match       = "full"        # full | prefix:N
ignore_tools_in_hash = ["headroom_retrieve"]
on_busy            = "branch"      # branch | wait   (implisit); eksplisit selalu wait→409
lock_wait_ms       = 2000
on_parked_conflict = "cancel_parked"   # cancel_parked | reject
dedupe_inflight    = true
resume_retry_replay= true          # replay otomatis bila --resume gagal

idle_ttl_secs      = 3600
max_age_secs       = 86400
max_per_tenant     = 64
max_total          = 512
max_turns          = 200
max_est_tokens     = 150000
max_cli_bytes      = 20971520
max_checkpoints    = 64
gc_interval_secs   = 60
max_concurrent_runs_per_provider = 2

debug_store_content = false

[prompt]
max_chars = 400000

[provider.claude.session]
enabled = true
[provider.opencode.session]
enabled = true
[provider.gemini.session]
enabled = false                    # aktifkan setelah spike memverifikasi resume
[provider.ollama.session]
enabled = false                    # stateless
```

Prioritas: flag CLI > env (`AGENTGATE_SESSION_*`) > file > default.

---

## 17. Observabilitas

**Log terstruktur** (tanpa isi pesan):

| Event | Field |
|---|---|
| `session.resolve` | `tenant, provider, strategy, reason, candidates, match=strict|loose, n_msgs, head_len` |
| `session.create` | `session_id, provider, mode_class, parent_id` |
| `session.sticky` | `session_id, cli_gen, delta_msgs` |
| `session.replay` | `session_id, reason, replay_msgs, truncated` |
| `session.taint` | `session_id, cause` |
| `session.rotate` | `session_id, cli_gen_old, cli_gen_new, cause` |
| `session.close` | `session_id, cause, artifacts_deleted` |
| `session.resume_failed` | `provider, reason` |
| `session.evict` | `session_id, policy` |

**Metrik:**
- `agentgate_sessions{state}` (gauge)
- `agentgate_session_resolve_total{strategy}` dan `{reason}`
- `agentgate_sticky_ratio` (turunan)
- `agentgate_session_taint_total{cause}`
- `agentgate_session_rotate_total{cause}`
- `agentgate_session_cleanup_failures_total{provider}`
- `agentgate_session_lock_wait_seconds` (histogram)
- `agentgate_replay_prompt_chars` (histogram)

**Alarm yang disarankan:** `sticky_ratio` turun drastis (indikasi klien/9router mengubah format riwayat), `cleanup_failures_total` naik (artefak privasi menumpuk), `resume_failed` tinggi (versi CLI berubah).

---

## 18. Pengujian

### 18.1 Komponen tiruan
- **`fake-cli-session`**: meniru CLI dengan sesi (id, `--resume`, penyimpanan berkas), bisa gagal-resume dan hang.
- **`fake-client`**: mengirim urutan `messages[]` (append, edit, regenerate, retry, potong).
- **Klien perubah format**: menormalkan whitespace dan menghapus bagian reasoning untuk menguji mode `loose`.

### 18.2 Skenario wajib

| # | Skenario | Hasil |
|---|---|---|
| SS1 | Percakapan 5 giliran tanpa header | Giliran 2–5 **sticky**, delta = 1 pesan |
| SS2 | Edit pesan user ke-2 | **branch**; sesi lama utuh |
| SS3 | Regenerate jawaban terakhir | **branch** (prefiks = checkpoint) |
| SS4 | Dua percakapan awal identik, lalu berbeda | Tidak tertukar setelah divergen |
| SS5 | Dua tenant, percakapan identik | Tidak pernah berbagi sesi |
| SS6 | Teks assistant diubah client (trim/markdown) | Mode `loose` tetap sticky; `strict` → replay |
| SS7 | Dua request paralel ke sesi sama | Implisit → satu sticky, satu branch; eksplisit → 409 |
| SS8 | Request non-resume saat sesi `parked` | `cancel_parked`: Run dibunuh, rotate-replay, jawaban benar |
| SS9 | Parked kedaluwarsa lalu client lanjut | Taint → rotate-replay, percakapan berlanjut |
| SS10 | Restart gateway saat `idle` | Sesi tetap sticky (resume CLI) |
| SS11 | Restart gateway saat `parked` | `tainted`; replay; tak ada proses yatim |
| SS12 | `--resume` gagal (sesi CLI hilang) | Replay otomatis ≤1 kali, transparan |
| SS13 | TTL idle lewat | `closed`; artefak CLI terhapus; request berikut = new/replay |
| SS14 | Melewati `max_turns`/token | Rotate dengan pemotongan, tidak memotong pasangan tool |
| SS15 | Ganti system prompt | Replay (`NoMatch`) |
| SS16 | `tools[]` berubah (tool yang pernah dipakai hilang) | Replay (`ToolsIncompatible`) |
| SS17 | Retry persis saat in-flight | `409 duplicate_inflight` |
| SS18 | Retry persis setelah selesai | Replay giliran baru; ledger lama tak tertimpa |
| SS19 | ID eksplisit + ledger tak cocok | Replay (`ExplicitMismatch`), client = kebenaran |
| SS20 | `Reset` / `Ephemeral` | Sesi baru / tanpa sticky |
| SS21 | Privasi: periksa DB & log | Tidak ada isi pesan/argumen/hasil tool |
| SS22 | Kuota per tenant terlampaui | Evict LRU idle; busy/parked aman |
| SS23 | Windows: cwd per-sesi, file terkunci saat hapus | Retry hapus berhasil; tidak ada sisa |
| SS24 | Soak 500 sesi bergantian | Tanpa kebocoran proses/FD/disk |
| SS25 | Rantai AionUi→9router→agentgate | Sticky ratio ≥ 90% pada percakapan normal |
| SS26 | Dua subject, satu API key, riwayat identik | Tidak pernah berbagi sesi (SE11) |
| SS27 | `multi_user=true`, tanpa subject | Replay-only; tidak ada sticky; log `no_subject` |
| SS28 | ID eksplisit sampai lewat 9router | Sticky walau isi pesan dikompresi (Headroom aktif) |
| SS29 | Tanpa ID eksplisit, percakapan memakai tool, riwayat dikompresi | Sticky via anchor `tool_call_id` |
| SS30 | Compress LLM output aktif (teks assistant berubah) | `loose` tetap sticky; `strict` → replay |
| SS31 | RTK mengompres hasil tool secara deterministik | Sticky tetap (hash teks diterima konsisten) |
| SS32 | Headroom menambah tool `headroom_retrieve` ke `tools[]` | Tidak error; `tools_hash` stabil; bridge meneruskan call-nya |
| SS33 | `tolerant` pada single-user dengan kompresi konteks | Sticky; metrik `tolerant_match_total` naik |
| SS34 | `doctor --probe-chain` | Melaporkan sinyal mana yang sampai dan apakah pesan dimodifikasi |

### 18.3 Uji manual rantai nyata
1. Percakapan 6 giliran di AionUi; periksa `X-Agentgate-Session-Mode` (jika header sampai) atau `/admin/sessions/stats`.
2. Edit pesan lama di UI → harus muncul sebagai `branch`.
3. Matikan gateway di tengah tool call (bridge) lalu lanjutkan → harus pulih via replay.
4. Periksa direktori data CLI di B setelah `idle_ttl`: tidak ada artefak sesi tersisa.

---

## 19. Perubahan yang Diperlukan pada Dokumen Lain

> **Status:** perubahan pada `spec_tooling.md` dan `tool-handling.md` sudah diterapkan bersamaan dengan v0.2 dokumen ini.

| Dokumen | Perubahan |
|---|---|
| `spec_tooling.md` §11.3 | cwd **per-Session** (bukan per-Run); tetap kosong dan terisolasi |
| `spec_tooling.md` §12.4 | `--no-session-persistence` hanya untuk Session `ephemeral`; sticky memakai `--session-id`/`--resume` |
| `spec_tooling.md` §10.5 | Replay = rotate Session (`cli_gen+1`) bukan sekadar Run baru |
| `spec_tooling.md` §10.1 / §10.8 | Run Cancel/Expire/Crash ⇒ Session `tainted` |
| `spec_tooling.md` §16 | Tambah skenario SS8, SS9, SS11 ke matriks tes bridge |
| `tool-handling.md` §10 | Tambah modul `session/` dan alur resolusi sesi sebelum `classify` bridge |
| `plan.md` | Perbarui bagian "Manajemen sesi" mengacu ke dokumen ini |

---

## 20. Struktur Modul

```
src/
├── session/
│   ├── mod.rs           # SessionManager, SessionSlot, state machine
│   ├── canon.rs         # kanonikalisasi pesan
│   ├── ledger.rs        # hash chain strict/loose, checkpoint
│   ├── resolve.rs       # pencarian content-addressed + eksplisit
│   ├── plan.rs          # Resolution, ReplayReason, validasi sticky
│   ├── store/
│   │   ├── mod.rs       # trait SessionStore
│   │   ├── sqlite.rs
│   │   └── memory.rs
│   ├── gc.rs            # TTL, LRU, pembersihan artefak
│   ├── recovery.rs      # pemulihan saat start
│   └── prompt.rs        # delta/transkrip, pemotongan
├── provider/
│   └── session_caps.rs  # SessionCaps, capture/delete per CLI
└── server/
    └── routes_admin_sessions.rs
```

Kerangka tipe:

```rust
pub enum Resolution {
    Resume { run: Arc<Run> },
    Sticky { session: Arc<Session>, delta: Vec<Message> },
    Replay { session: Arc<Session>, reason: ReplayReason, history: Vec<Message> },
    New    { session: Arc<Session> },
    Branch { parent: SessionId, session: Arc<Session>, history: Vec<Message> },
}

#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn get(&self, id: &SessionId) -> Result<Option<SessionRow>>;
    async fn find_by_head(&self, q: HeadQuery) -> Result<Vec<SessionRow>>; // tenant, provider, mode, len, hash, kind
    async fn upsert(&self, row: &SessionRow) -> Result<()>;
    async fn add_checkpoint(&self, cp: Checkpoint) -> Result<()>;
    async fn expire_before(&self, ts: i64) -> Result<Vec<SessionRow>>;
    async fn delete(&self, id: &SessionId) -> Result<()>;
}

pub trait CliSessionOps: Send + Sync {
    fn caps(&self) -> SessionCaps;
    fn args_new(&self, ctx: &SpawnCtx) -> Vec<String>;                    // mis. claude --session-id <uuid>
    fn args_resume(&self, ctx: &SpawnCtx, cli_id: &str) -> Vec<String>;   // mis. claude --resume <uuid>
    fn capture_id(&self, ev: &CliEvent) -> Option<String>;                // opencode/gemini
    async fn delete(&self, cli_id: &str, cwd: &Path) -> Result<()>;
}
```

---

## 21. Rencana Implementasi

| Milestone | Isi | Kriteria selesai |
|---|---|---|
| **SS0** | `session.mode = off`: selalu replay (baseline); header respons `Mode: replay` | Percakapan multi-giliran benar tanpa sticky |
| **SS1** | `canon` + ledger (strict/loose) + store `memory` + resolusi content-addressed | SS1–SS6, SS15, SS18 lulus dengan fake CLI |
| **SS1b** | Identitas berlapis: `id_sources`, `subject_sources`, anchor, `multi_user` fail-closed, `/admin/debug/signals`, `doctor --probe-chain` | SS26–SS29, SS34; keputusan jalur utama (header vs anchor) setelah uji rantai nyata |
| **SS2** | State machine, konkurensi, `on_busy`, dedupe, taint/rotate | SS7–SS9, SS17, SS19–SS20 |
| **SS3** | Store SQLite + pemulihan start + GC/TTL/kuota | SS10–SS13, SS21–SS22 |
| **SS4** | Sticky **claude** (`--session-id`/`--resume`) + cleanup artefak | Sticky nyata; SS25 manual |
| **SS5** | Sticky **opencode** (capture id, `--session`) | idem |
| **SS6** | Sticky **gemini** (bila spike memverifikasi resume) | idem |
| **SS7** | Admin API/CLI/metrik + `doctor --sessions` | Dasbor stats berfungsi |
| **SS8** | Hardening: Windows, soak SS24, privasi SE1–SE10 | Semua tes hijau Linux + Windows |
| **SS9** | Opsional: `opencode serve` session API, fork, summarization | — |

Ketergantungan: SS2 butuh Run dari BR3/BR4 (`spec_tooling.md`) untuk skenario parked; SS0–SS1 dapat dikerjakan sebelum bridge.

---

## 22. Risiko & Pertanyaan Terbuka

| # | Risiko / pertanyaan | Dampak | Mitigasi / tindak lanjut |
|---|---|---|---|
| R1 | Client/9router mengubah riwayat (format, pemotongan, reasoning dibuang) | Sticky ratio rendah | Mode `loose`; metrik `sticky_ratio`; saran kirim `X-Agentgate-Session` |
| R2 | Flag sesi CLI berbeda/berubah antar versi | Sticky rusak | `SessionCaps` per versi di `compat/*.json`; `resume_retry_replay`; spike |
| R3 | Lokasi/format penyimpanan sesi CLI berubah | Cleanup privasi gagal | `doctor` memantau; mode `ephemeral`/`off`; hapus via perintah resmi bila ada |
| R4 | Resume CLI melanjutkan state yang tak cocok dengan ledger (mis. auto-compaction) | Jawaban menyimpang tipis | Ledger tak bergantung isi CLI; rotate berkala (`max_turns`) |
| R5 | Parked run + sesi CLI berisi `tool_use` tanpa hasil | Resume rusak | Taint → rotate otomatis |
| R6 | Hash ambigu (dua percakapan identik) | Berbagi sesi sementara | Pilih LRU idle; header eksplisit untuk jaminan |
| R7 | Replay mahal pada riwayat panjang | Lambat/boros | Pemotongan, rotate cerdas, sticky |
| R8 | Data sensitif tertinggal di CLI | Privasi | TTL, hapus artefak, `off`/`ephemeral` |
| R9 | Satu API key dipakai banyak orang (9router) — **dikonfirmasi** | Tenant = 1 untuk semua; risiko lintas-pengguna | Subject wajib (§5.4): key per-user / header / `user`; tanpa subject → replay-only |
| R10 | Kunci HMAC hilang/diganti | Semua sesi tak cocok | Replay otomatis (aman); jaga `secret.key` dalam backup |
| R11 | Lock global CLI (SQLite) | Error saat paralel | `max_concurrent_runs_per_provider`; uji di spike |
| R12 | Windows: berkas terkunci saat hapus | Artefak tertinggal | Retry dengan backoff; sapu saat start |
| R13 | Kompresi konteks (Headroom) mengubah/menghapus pesan lama | Hash chain tak stabil; sticky ratio turun | Header-first + anchor; matikan fitur untuk rute ini; `tolerant` (single-user) |
| R14 | Perantara tidak meneruskan header/`user`/`metadata` | Tidak ada ID eksplisit/subject | `doctor --probe-chain`; key per-user; replay-only sebagai fallback aman |
| R15 | Perantara menjalankan sendiri tool tambahan (mis. `headroom_retrieve`) dan mengirim request resume tambahan | Pola request tak terduga | Jalur resume normal berbasis `tool_call_id`; uji E2E; `max_steps` |
| R16 | AionUi tidak mengirim ID percakapan | Header-first tak berguna | Anchor (percakapan ber-tool) + replay; mintalah dukungan header kustom/`user` di konfigurasi provider AionUi atau 9router |

**Keputusan dari jawaban operator (2026-11-03):**

| Pertanyaan | Jawaban | Keputusan desain |
|---|---|---|
| Apakah AionUi/9router meneruskan header kustom? | "Sepertinya iya" (belum terbukti) | **Header-first** (§5.1), diverifikasi lewat `doctor --probe-chain` (§5.5) |
| Apakah 9router mengubah isi pesan? | **Ya**: Compress tool output (RTK), Compress context (Headroom), Compress LLM output | `loose` default, `tolerant` opt-in, anchor `tool_call_id`, rekomendasi matikan Compress context pada rute ini (§6.6) |
| Apakah banyak orang di balik satu API key? | **Ya** | `multi_user=true`, **subject wajib**, tanpa subject → replay-only (§5.4) |

**Pertanyaan terbuka (baru):**
1. Header/field apa yang benar-benar bisa dikirim AionUi per percakapan (ID percakapan, `user`)? Bisakah 9router menambah `X-Agentgate-User` per pengguna?
2. Apakah 9router punya API key per pengguna yang dapat dipetakan ke koneksi provider terpisah ke agentgate?
3. Apakah kompresi konteks 9router (Headroom) dapat dimatikan per rute/provider?
4. Apakah 9router mempertahankan `tool_call_id` dan menjalankan sendiri panggilan `headroom_retrieve` (bukan meneruskannya ke client)?
5. Versi CLI target dan hasil spike `resume`/`delete` per CLI?
6. Perlukah *summarization* pada rotate (bukan hanya pemotongan)?

---

## Lampiran A — Contoh Alur Header

```
Request 1 : POST /v1/chat/completions  (tanpa header sesi)
Response 1: 200  X-Agentgate-Session: ag_ses_9c1f…  X-Agentgate-Session-Mode: new

Request 2 : messages = [...giliran 1..., user baru]   (tanpa header)
Response 2: 200  X-Agentgate-Session: ag_ses_9c1f…  X-Agentgate-Session-Mode: sticky

Request 3 : user mengedit pesan lama
Response 3: 200  X-Agentgate-Session: ag_ses_77aa…  X-Agentgate-Session-Mode: branch
            X-Agentgate-Replay-Reason: Branch
```

## Lampiran B — Contoh Kanonikalisasi

Input (dua representasi yang harus identik):

```json
{"role":"user","content":"  Buat file  hello.txt\r\n"}
{"role":"user","content":[{"type":"text","text":"Buat file  hello.txt"}]}
```
Canon: `{"role":"user","text":"Buat file  hello.txt","tool_calls":[]}`
(spasi ganda internal dipertahankan; hanya trim tepi dan normalisasi newline.)

Assistant dengan tool call (id tidak ikut hash):
```json
{"role":"assistant","content":null,"tool_calls":[{"id":"call_x","type":"function","function":{"name":"write_file","arguments":"{\"content\":\"halo\",\"path\":\"hello.txt\"}"}}]}
```
Canon: `{"role":"assistant","text":"","tool_calls":[{"name":"write_file","args":{"content":"halo","path":"hello.txt"}}]}` (kunci argumen diurutkan).

## Lampiran C — Checklist Verifikasi Versi CLI (isi saat spike)

| Item | opencode | gemini | claude |
|---|---|---|---|
| Versi diuji | | | |
| Cara membuat sesi dengan ID tertentu | | | |
| Cara menangkap ID sesi (event/daftar) | | | |
| Perintah melanjutkan sesi (`--resume`/`--session`) | | | |
| Lokasi penyimpanan sesi & ketergantungan pada cwd | | | |
| Cara menghapus sesi (perintah/berkas) | | | |
| Sesi sekali pakai tersedia | | | |
| Fork tersedia | | | |
| Perilaku resume dengan model berbeda | | | |
| Perilaku resume bila sesi hilang (kode keluar/pesan) | | | |
| Perilaku sesi dengan `tool_use` tanpa hasil | | | |
| Lock global saat Run paralel | | | |
| Perilaku di Windows (path, file terkunci) | | | |
