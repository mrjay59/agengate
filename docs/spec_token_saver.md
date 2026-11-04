# SPEC_TOKEN_SAVER.md — Spesifikasi Penghematan Token `agentgate`

| | |
|---|---|
| **Status** | Draft v0.1 |
| **Tanggal** | 2026-11-03 |
| **Cakupan provider** | `opencode`, `gemini`, `claude` (Claude Code), `ollama` |
| **Dokumen terkait** | `plan.md`, `tool-handling.md`, `spec_tooling.md` (bridge), `spec_session.md` (sesi), `spec_reasoning.md` (reasoning) |
| **Konvensi** | **MUST / SHOULD / MAY** mengikuti RFC 2119. ⚠️ = *harus diverifikasi/diukur pada versi CLI dan model terpasang*. Angka penghematan adalah **perkiraan** sampai diukur oleh harness `bench-tokens` (§13). |

---

## 0. Ringkasan Eksekutif

`agentgate` boros token di tempat yang bisa diprediksi: **prompt dasar CLI**, **definisi tool yang dipasang bridge**, **riwayat yang dikirim ulang (replay)**, **hasil tool yang besar**, **thinking**, dan **panggilan tambahan** yang dilakukan CLI sendiri (judul sesi, ringkasan, dll.).

Prinsip: **ukur dulu, hemat kemudian, jangan merusak kebenaran.**

Strategi, dari yang paling aman dan paling besar dampaknya:

| # | Teknik | Jenis | Risiko | Level default |
|---|---|---|---|---|
| T1 | **Sticky session** (kirim delta, bukan seluruh riwayat) | lossless | rendah | `safe` |
| T2 | **Prefix stabil untuk cache** (urutan tool deterministik, tanpa timestamp) | lossless | rendah | `safe` |
| T3 | **Memangkas prompt dasar CLI** (override system prompt, matikan konteks/overhead) | lossless* | sedang | `safe` |
| T4 | **Slimming skema tool** | lossless → lossy | rendah–sedang | `safe`/`balanced` |
| T5 | Subset/lazy tool (eksperimental) | lossy | tinggi | `aggressive` |
| T6 | **Transformasi aman hasil tool** (ANSI, whitespace, baris berulang) | lossless | rendah | `safe` |
| T7 | **Pemangkasan hasil tool + Result Vault + `ag_expand`** | reversibel | sedang | `balanced` |
| T8 | **Memo panggilan tool read-only** | reversibel | sedang | `balanced` (opt-in tool) |
| T9 | **Riwayat**: elisi hasil lama, dedupe, windowing (replay) | lossy-bertanda | sedang | `balanced` |
| T10 | Ringkasan riwayat (fase 2) | lossy | tinggi | `aggressive` |
| T11 | **Kebijakan anggaran reasoning** | lossy | sedang | `pass` |
| T12 | **Kontrol output** (`max_tokens`, gaya ringkas) | lossy | sedang | `aggressive` |
| T13 | **Routing model/effort** untuk request pembantu (judul, ringkasan) | lossy | sedang | `balanced` |
| T14 | **Matikan panggilan overhead CLI** | lossless | rendah | `safe` |
| T15 | Cache respons persis (opsional) | lossless | sedang | off |
| T16 | **Anggaran & kuota** + degradasi otomatis | kebijakan | rendah | off |

*lossless pada isi percakapan; mengubah instruksi dasar CLI.

Aturan emas:
1. **Transformasi dilakukan *setelah* resolusi sesi dan perhitungan ledger** — ledger selalu memakai pesan **asli seperti diterima** (§4.3).
2. **Lossy harus bertanda dan reversibel** bila mungkin (marker + Result Vault).
3. **Idempoten**: menerapkan transformasi dua kali = sekali (agar tidak bertabrakan dengan RTK/Headroom di 9router, §6).
4. **Tidak pernah mengubah teks pesan `user`** (kecuali pemotongan riwayat lama pada replay, itupun bertanda).
5. **Kualitas dijaga oleh harness**: perubahan level default hanya bila tingkat keberhasilan tugas tidak turun melebihi ambang (§13.4).

---

## 1. Anatomi Token

### 1.1 Ke mana token pergi (per giliran, mode bridge)

```
┌───────────────────────── input (prompt) ─────────────────────────┐ ┌─ output ─┐
│ [A] prompt dasar CLI   [B] definisi tool   [C] system client     │ │ [G] reasoning
│ [D] riwayat (replay)/delta   [E] hasil tool   [F] pesan user     │ │ [H] jawaban/tool_calls
└──────────────────────────────────────────────────────────────────┘ └──────────┘
                    + [I] panggilan tambahan CLI (judul, ringkasan, kompaksi, cek giliran)
```

| Bagian | Sumber | Ukuran khas ⚠️ (diukur di spike) | Teknik |
|---|---|---|---|
| **A** Prompt dasar CLI | Bawaan CLI + file konteks (CLAUDE.md/AGENTS.md/GEMINI.md) + memori | Ribuan hingga puluhan ribu token | T3, T14 |
| **B** Definisi tool | `tools[]` client → MCP, ditambah tool tambahan perantara | ~200–1.000+ token per tool; 20–60 tool = puluhan ribu | T4, T5 |
| **C** System prompt client | AionUi | Bervariasi | (dipertahankan; hanya normalisasi, §5.9) |
| **D** Riwayat | Replay mengirim ulang semua; sticky hanya delta | Tumbuh linier | T1, T9, T10 |
| **E** Hasil tool | Output baca file, grep, shell | Sering terbesar | T6, T7, T8 |
| **F** Pesan user | User | Kecil–besar | (tidak diubah) |
| **G** Reasoning | Model | Bisa > jawaban | T11 |
| **H** Output | Model | — | T12 |
| **I** Overhead CLI | Panggilan LLM latar CLI | Bisa menggandakan konsumsi | T14 |

### 1.2 Mengapa cache penting
Penyedia memproses prompt dari atas ke bawah dan meng-cache **prefiks**. Bagian stabil (prompt dasar + definisi tool + riwayat awal) murah bila prefiksnya **identik byte-per-byte** antar permintaan; pembacaan cache jauh lebih murah daripada input biasa, sedangkan penulisan cache sedikit lebih mahal, dan cache kedaluwarsa setelah jeda (default beberapa menit). Perubahan pada definisi tool atau urutannya membatalkan cache dari titik itu. ⚠️ Angka harga/TTL bergantung penyedia dan versi; gateway **tidak** mengatur `cache_control` untuk CLI (CLI yang mengaturnya) tetapi **dapat menjaga prefiks tetap stabil** (T2).

### 1.3 Konteks biaya
Pada CLI berbasis langganan, "token" menjadi **kuota/limit laju** dan **latensi**, bukan tagihan per token. Spesifikasi ini memakai istilah *token-unit*; kebijakan anggaran (T16) bekerja pada token-unit yang dapat dipetakan ke biaya bila diperlukan.

---

## 2. Tujuan, Non-Tujuan, Prinsip

### 2.1 Tujuan
- **G1.** Menurunkan token input per giliran pada percakapan multi-giliran bertool (target: ≥ 40% vs replay naif; diverifikasi §13).
- **G2.** Menjaga hit cache setinggi mungkin (prefiks stabil).
- **G3.** Mencegah pemborosan tak terlihat (overhead CLI, loop tool, hasil tool raksasa).
- **G4.** Mengukur penghematan per teknik secara transparan.
- **G5.** Memberi pengguna kendali (level, opt-out per request).

### 2.2 Non-Tujuan
- Mengubah niat atau isi pesan user.
- Kompresi lossy tersembunyi (selalu bertanda).
- Menggantikan kompresi 9router (RTK/Headroom); keduanya harus hidup berdampingan (§6).
- Memodifikasi `cache_control` atau perilaku internal CLI di luar flag/config resmi.
- Optimasi harga per-vendor (routing antar-vendor berbasis harga).

### 2.3 Prinsip
1. **Measure first.** Tidak ada teknik aktif sebelum ada baseline.
2. **Lossless dulu**, lalu reversibel, baru lossy-bertanda. **Lossy-tanpa-tanda dilarang.**
3. **Original selalu bisa dipulihkan** (Vault) selama sesi hidup.
4. **Idempoten & deterministik** (output sama untuk input sama → cache-friendly).
5. **Fail open ke kebenaran:** bila ragu (tidak bisa memastikan aman), jangan transformasi.
6. **Transparan:** header, metrik, dan log selalu memperlihatkan apa yang dihemat.

---

## 3. Terminologi & Level

| Istilah | Arti |
|---|---|
| **Transformasi** | Fungsi yang mengubah payload (tool schema, hasil tool, riwayat, dll.) sebelum ke CLI |
| **Lossless** | Makna dan informasi tidak berubah (mis. buang kode warna ANSI, minify JSON) |
| **Lossy-bertanda** | Informasi dibuang tetapi ada penanda dan (bila mungkin) cara memulihkannya |
| **Vault** | Penyimpanan per-sesi untuk teks asli yang dipangkas |
| **Marker** | Teks penanda yang menyatakan ada bagian dipangkas dan cara mengambilnya |
| **Tool internal gateway** | Tool MCP yang dilayani **gateway sendiri**, tidak diteruskan ke client (mis. `ag_expand`) |
| **Memo** | Penyimpan hasil panggilan tool read-only yang identik dalam satu Run/giliran |
| **Upstream compressor** | Kompresi di hulu (RTK/Headroom/kompres output di 9router) |

### Level (`saver.level`)

| Level | Isi | Cocok untuk |
|---|---|---|
| `off` | Tidak ada transformasi (hanya pengukuran) | Baseline/debug |
| `safe` | **Hanya lossless**: T1, T2, T3 (aman), T4 (lossless), T6, T14 | Default konservatif |
| `balanced` *(rekomendasi)* | `safe` + T4 ringan, T7, T8 (opt-in tool), T9, T13 | Pemakaian harian |
| `aggressive` | `balanced` + T5, T10, T11 (cap), T12 | Anggaran ketat; kuota hampir habis |

Level dapat diganti per request (`X-Agentgate-Token-Mode`), per provider, atau otomatis oleh T16.

---

## 4. Arsitektur Pipeline

### 4.1 Posisi dalam alur request

```
request ─► auth ─► resolusi sesi + LEDGER (pesan ASLI) ─► classify mode
   ─► [SAVER] routing (T13) ─► tool set: urut+slim (T4,T5) ─► prompt: system stabil (T2,T3)
   ─► [SAVER] isi: delta/transkrip + transformasi hasil tool/riwayat (T6–T10)
   ─► spawn CLI (flag hemat T3,T14; effort/budget T11) ─► stream
   ─► [SAVER] outbound: usage normalisasi, akun (T16), metrik
```

### 4.2 Antarmuka transformasi

```rust
pub enum Lossiness { Lossless, LossyMarked }

pub trait Transform: Send + Sync {
    fn name(&self) -> &'static str;
    fn lossiness(&self) -> Lossiness;
    fn min_level(&self) -> Level;
    /// Idempoten: apply(apply(x)) == apply(x)
    fn apply(&self, cx: &mut Ctx, p: &mut Payload) -> Saved;
}

pub struct Saved { pub est_tokens: i64, pub bytes_before: usize, pub bytes_after: usize }
pub struct Ctx<'a> { pub session: &'a Session, pub provider: ProviderId, pub level: Level, pub vault: &'a Vault, pub cfg: &'a SaverCfg }
```

Pipeline = daftar `Transform` terurut per jenis payload (`ToolDefs`, `ToolResult`, `History`, `Prompt`). Setiap transformasi:
- **MUST** idempoten dan deterministik.
- **MUST** mencatat `Saved` untuk metrik.
- **MUST** dapat dimatikan per request (`X-Agentgate-No-Compress`).
- **MUST NOT** menyentuh pesan `user` (kecuali T9 pada riwayat lama, bertanda).

### 4.3 Aturan terpenting: *ledger memakai pesan asli*
`spec_session.md` mencocokkan sesi dengan hash dari pesan **seperti diterima**. Maka:
1. Resolusi sesi dan perhitungan hash dilakukan **sebelum** transformasi apa pun.
2. Transformasi bekerja pada **salinan**; ledger tidak pernah berubah karena saver.
3. Vault menyimpan **teks asli** (bukan hasil transformasi).
4. Bila saver mengubah apa yang dikirim ke CLI, ledger tetap merepresentasikan apa yang **client** lihat, bukan apa yang CLI lihat. Konsekuensi diterima: bila level diubah di tengah percakapan, sesi tidak perlu dirotasi.

### 4.4 Idempotensi & marker
Setiap output lossy membawa marker berformat tetap (Lampiran B) sehingga:
- transformasi kedua mengenali bahwa teks sudah dipangkas dan **tidak memangkas lagi**;
- model memahami adanya bagian hilang;
- 9router/Headroom yang memangkas lebih dulu tidak "dipangkas dua kali" (§6).

---

## 5. Katalog Teknik

### 5.1 T1 — Sticky session
Rujukan: `spec_session.md`. Menghindari pengiriman ulang seluruh riwayat; **penghematan terbesar** untuk percakapan panjang.
- Metrik: `agentgate_sticky_ratio` ≈ proporsi giliran yang tidak replay.
- Tambahan saver: saat replay terpaksa, terapkan T9 pada transkrip.
- Hubungan cache: sesi sticky mempertahankan prefiks di sisi penyedia (selama TTL cache belum lewat).

### 5.2 T2 — Prefix stabil untuk cache
Aturan perakitan prompt (MUST):

| Aturan | Alasan |
|---|---|
| Urutkan tool secara **deterministik** (nama, ascending) sebelum dipasang ke bridge | Urutan berubah = cache batal |
| Serialisasi skema dengan **urutan kunci tetap** (kanonik) dan tanpa whitespace acak | Perbedaan byte = cache batal |
| Tidak ada **timestamp, request-id, nomor acak, nama run** dalam prompt dasar/system/tool description | Memecah prefiks |
| Catatan gateway pada system prompt (`spec_tooling.md` §11.1) bersifat **statis** | Prefiks sama antar-giliran |
| Konten volatil (pesan user baru, hasil tool) selalu **di akhir** | Prefiks panjang tetap sama |
| `tools_hash` dihitung dari bentuk kanonik; perubahan set tool dicatat (`prefix_break` metrik) | Diagnostik |
| Tool tambahan dari perantara (`headroom_retrieve`) diposisikan **konsisten** (ikut urutan alfabet) | Stabil |

Uji: dua giliran dengan `tools[]` yang urutannya berbeda **MUST** menghasilkan `tools/list` yang identik byte-per-byte (TS6).

Catatan: gateway **tidak** menahan sesi hanya untuk menjaga cache tetap hangat (tanpa "ping keep-warm" secara default); cache yang kedaluwarsa adalah biaya normal setelah jeda.

### 5.3 T3 — Memangkas prompt dasar CLI
Tujuan: menghilangkan token yang tidak berguna pada mode bridge/chat_only (prompt dasar CLI mengasumsikan tool bawaan yang sudah dimatikan).

| CLI | Sarana ⚠️ | Efek |
|---|---|---|
| claude | `--system-prompt` (mengganti prompt bawaan), `--tools ""`, `--setting-sources ""`, `--strict-mcp-config` | Hilang prompt agen kode, deskripsi tool bawaan, CLAUDE.md/memori/hook, skema MCP milik pengguna |
| opencode | Config inline: `instructions: []`, nonaktifkan agent/tool/MCP yang tidak dipakai, `tools{…:false}` | Hilang instruksi proyek dan deskripsi tool bawaan |
| gemini | Override system prompt (mis. variabel lingkungan system-md ⚠️), `excludeTools`, `contextFileName` ke nama yang tidak ada, nonaktifkan memori | Hilang prompt bawaan dan GEMINI.md |
| ollama | Tidak ada prompt dasar; kirim hanya system client + catatan statis | — |

Syarat keselamatan: mengganti prompt dasar dapat menurunkan kualitas perilaku agentik. Karena pada bridge tool-nya milik client, prompt pengganti memuat panduan singkat memakai tool (`spec_tooling.md` §11.1). Perubahan ini **wajib lolos harness kualitas** (§13.4) sebelum dijadikan default per CLI.

### 5.4 T4 — Slimming skema tool

Transformasi atas `inputSchema` (urutan dari aman → lossy):

| Langkah | Jenis | Aktif di |
|---|---|---|
| S1 Buang `$schema`, `title`, `examples`, `$comment`, `deprecated`, `readOnly`/`writeOnly` | Lossless (tidak memengaruhi validasi/penggunaan) | `safe` |
| S2 Buang `additionalProperties:false` redundan pada objek tanpa `patternProperties` | Lossless fungsional (validasi dilakukan gateway; lihat catatan) | `safe` |
| S3 Minify (tanpa whitespace) + urutan kunci kanonik | Lossless | `safe` |
| S4 Inline `$ref` kecil; buang `$defs` tak terpakai | Lossless | `safe` |
| S5 Buang `default` yang sama dengan nilai implisit tipe | Lossless | `balanced` |
| S6 Pangkas `description` **properti** panjang (> N karakter, default 240) di kalimat terakhir utuh, bertanda `…` | Lossy-bertanda | `balanced` (ringan), `aggressive` (N=120) |
| S7 Pangkas `description` **tool** (> M karakter, default 600) | Lossy-bertanda | `aggressive` |
| S8 Buang enum description/anotasi non-esensial | Lossy | `aggressive` |

Catatan:
- Validasi argumen terhadap skema **asli** (bukan yang di-slim) tetap dilakukan gateway saat `tools/call` (`spec_tooling.md` §9.5) agar kontrak dengan client terjaga.
- Slimming **tidak pernah** menghapus `type`, `properties`, `required`, `enum`, `items`, `minimum/maximum` (batasan yang memengaruhi kebenaran argumen).
- Hasil slimming deterministik → bagian dari prefiks stabil (T2).
- Estimasi penghematan dicatat per tool (`tool_schema_bytes_before/after`).

### 5.5 T5 — Subset dan lazy tool *(eksperimental)*
Untuk client dengan puluhan tool:
- **Allowlist/denylist** per workspace/klien (`saver.tools.include/exclude`) — statis, aman.
- **Subset dinamis**: pilih K tool relevan berdasarkan pesan terakhir (kata kunci/embedding sederhana) + selalu sertakan tool yang pernah dipakai di sesi. Lossy: tool yang tidak diekspos tidak bisa dipanggil.
- **Lazy**: ekspos nama+deskripsi pendek dan tool meta `ag_tool_search(query)` yang memuat skema lengkap on-demand. Membutuhkan CLI yang me-*refresh* daftar tool setelah notifikasi `listChanged` ⚠️ — **belum dijamin** pada semua CLI dan merusak prefiks cache.
- Status: **off** kecuali `aggressive` dan hanya setelah spike membuktikan (RS: Q-ts5).

### 5.6 T6 — Transformasi aman hasil tool (lossless)

Dijalankan pada isi `role:"tool"` sebelum ke CLI. Semuanya idempoten dan tidak mengubah makna:

| Transformasi | Contoh |
|---|---|
| Buang kode ANSI/escape terminal | `\x1b[32mOK\x1b[0m` → `OK` |
| Normalisasi newline (`\r\n`→`\n`), buang `\r` progress | Bar progress satu baris |
| Buang whitespace akhir baris; kompres >2 baris kosong berurutan menjadi 1 | — |
| Ringkas **baris identik berurutan** menjadi `baris (×N)` | `warn: X` ×37 |
| Minify JSON yang valid (bila seluruh hasil adalah JSON) | Hemat indentasi |
| Ganti blob base64/data-URI panjang dengan placeholder bertanda + simpan di Vault | `data:image/png;base64,…(48 KB)` |
| Batas panjang baris ekstrem (mis. 20k karakter, file minified) dengan marker | Lossy-bertanda → masuk T7 |

Aturan:
- Hanya bila ukuran hasil > `saver.results.min_tokens_to_act` (default 200) — hasil kecil tidak disentuh.
- Bila teks sudah berisi marker agentgate/upstream (§6), lewati.
- Tidak mengubah isi di dalam blok kode yang diapit tanda kutip tiga **bila** hasil adalah dokumen Markdown utuh (heuristik konservatif; lewati bila ragu).

### 5.7 T7 — Pemangkasan hasil tool + Result Vault + `ag_expand`

**Masalah:** satu `grep`/`cat` bisa memasukkan puluhan ribu token yang sebagian besar tidak dibutuhkan.

**Aturan pemangkasan** (`balanced`):
- Ambang: hasil > `saver.results.max_tokens` (default 2.000 token estimasi).
- Pangkas ke **kepala N + ekor M** (default 60% / 25% dari batas), sisipkan marker di tengah (Lampiran B).
- Teks asli disimpan di **Vault** (`r_<8hex>`); marker menyertakan id dan cara mengambil bagian.
- Pangkas sadar-struktur bila mudah: untuk hasil bergaris, potong di batas baris; untuk JSON besar, potong pada elemen tingkat atas (sisipkan `…` dan jumlah elemen).
- **Pengecualian** (tidak dipangkas): hasil `isError`, hasil sudah di bawah ambang, tool pada `saver.results.never_truncate` (mis. `write_file`), dan permintaan eksplisit user untuk "isi lengkap" (heuristik opsional, off).

**Result Vault:**

| Properti | Nilai |
|---|---|
| Lingkup | Per Session (dan per Run untuk `ephemeral`) |
| Kunci | `r_` + 8 hex acak; tidak dapat ditebak |
| Isi | Teks asli hasil tool (+ metadata: tool, waktu, panjang) |
| Penyimpanan | Memori; opsional spill ke `{data}/sessions/{sid}/vault/` bila > `vault.mem_bytes` |
| Batas | `vault.max_bytes_per_session` (16 MB), `vault.max_entries` (256), LRU |
| TTL | Sama dengan sesi; dihapus saat `closed`/taint-rotate bila `vault.clear_on_rotate=true` |
| Privasi | Berisi data pengguna → ikut aturan retensi sesi; `saver.vault = off` mematikan T7 |

**Tool internal gateway `ag_expand`** — dilayani **gateway sendiri**, tidak diteruskan ke client:

```json
{
  "name": "ag_expand",
  "description": "Ambil bagian dari hasil tool yang dipangkas. Gunakan id dari penanda [agentgate:pangkas].",
  "inputSchema": {
    "type": "object",
    "properties": {
      "id":        { "type": "string" },
      "mode":      { "type": "string", "enum": ["head", "tail", "range", "grep"] },
      "start":     { "type": "integer", "minimum": 1 },
      "end":       { "type": "integer", "minimum": 1 },
      "pattern":   { "type": "string" },
      "max_lines": { "type": "integer", "minimum": 1, "maximum": 500 }
    },
    "required": ["id"]
  }
}
```
- Dipasang bersama tool client **hanya bila** Vault aktif dan sudah ada ≥ 1 entri (atau selalu pada `aggressive`, demi prefiks stabil — keputusan: **selalu dipasang bila `saver.vault=on`** agar daftar tool tidak berubah di tengah sesi dan prefiks cache tetap).
- `tools/call` untuk `ag_expand` diselesaikan **seketika** oleh Bridge (tanpa batch/park/`tool_calls` ke client) — lihat perubahan `spec_tooling.md` §19.
- Hasil `ag_expand` sendiri dibatasi (`max_lines`, `saver.results.max_tokens`) dan **tidak** dipangkas ulang.
- Id tak dikenal/kedaluwarsa → hasil `isError:true` dengan pesan jelas ("hasil asli sudah tidak tersedia; panggil ulang tool aslinya").
- Nama `ag_expand` dicadangkan; bila client punya tool bernama sama → tool client di-alias (`spec_tooling.md` §8.3).
- Guard (`spec_tooling.md` §13): `ag_expand` termasuk tool **bridge** yang diizinkan.

### 5.8 T8 — Memo panggilan tool read-only

Bila model memanggil tool read-only yang sama dengan argumen identik dalam satu Run/giliran (mis. membaca file yang sama dua kali):
- Gateway **tidak** mengirim `tool_calls` ke client lagi; menjawab langsung dengan penunjuk singkat: `[hasil identik dengan panggilan sebelumnya; id=r_…]` (isi asli tersedia via `ag_expand`).
- Hanya untuk tool pada `saver.memo.read_only_tools` (default **kosong**; opt-in eksplisit, mis. `read_file`, `list_dir`, `grep`).
- **Invalidasi** memo bila ada panggilan tool **non**-read-only di Run yang sama (tulis/eksekusi), atau lewat `saver.memo.ttl_secs` (default 120).
- Dikecualikan: tool yang menyentuh sumber berubah cepat (waktu, jaringan) — tanggung jawab daftar opt-in.
- Manfaat ganda: mengurangi token **dan** round-trip ke client.

### 5.9 T9 — Riwayat: elisi, dedupe, windowing (hanya replay)

Diterapkan saat membangun transkrip replay (sticky mengirim delta saja):

| Langkah | Aturan |
|---|---|
| **Elisi hasil tool lama** | Hasil tool yang lebih tua dari `saver.history.keep_last_results` (default 6 hasil terakhir) diganti marker + id Vault; pasangan `tool_call` ↔ hasil tetap ada |
| **Dedupe** | Hasil tool identik (hash) muncul sekali; berikutnya merujuk ke yang pertama |
| **Windowing** | Bila transkrip > `prompt.max_chars`: pertahankan system + N giliran terakhir; buang giliran tertua **berpasangan** (tool_call + hasil); sisipkan `[riwayat lama dipotong]` |
| **Buang reasoning** | Sudah wajib (`spec_reasoning.md` §5.6) |
| **Normalisasi system** | System prompt client dipertahankan; duplikat identik dihapus |
| **Tidak menyentuh** | Pesan `user` terakhir, system prompt, pesan assistant terakhir |

Semua elisi bertanda; model dapat meminta rincian lewat `ag_expand` (untuk hasil tool) bila Vault aktif.

### 5.10 T10 — Ringkasan riwayat *(fase 2, `aggressive`)*
Mengganti giliran lama dengan ringkasan yang dibuat model murah/lokal (mis. Ollama).
- Disimpan sebagai blok `[ringkasan]` bertanda; asli tetap bisa dipulihkan dari Vault.
- Biaya ringkasan harus < penghematan (cek `est`).
- Risiko tinggi (kehilangan detail) → default off; masuk harness kualitas.

### 5.11 T11 — Kebijakan anggaran reasoning

Reasoning adalah token **output** (mahal dan lambat). `spec_reasoning.md` menyerahkan kontrol ke client; saver menambahkan kebijakan:

| `saver.reasoning.policy` | Perilaku |
|---|---|
| `pass` *(default)* | Ikuti permintaan client |
| `cap` | Batasi effort ke `saver.reasoning.max_effort` (mis. `medium`) |
| `auto` | Effort rendah/off untuk request pembantu/sederhana (prompt pendek, tanpa tool, `max_tokens` kecil); tinggi untuk bertool/panjang |

- Heuristik `auto` konservatif dan dapat diaudit (`saver.reasoning.auto_rules`).
- Pemetaan ke CLI mengikuti `spec_reasoning.md` §7 (mis. matikan thinking pada claude lewat pengaturan anggaran thinking ⚠️; `think:false` pada ollama).
- `exclude` (sembunyikan dari client) **tidak** menghemat token; hanya `cap`/`auto`/`off` yang menghemat.

### 5.12 T12 — Kontrol output
- `max_tokens`/`max_completion_tokens` dari client **selalu dihormati** (dipotong di gateway untuk CLI tanpa dukungan: `finish_reason:"length"`).
- `saver.output.max_tokens` (default 0 = tidak ada) sebagai batas atas bila client tidak menetapkan.
- **Gaya ringkas** (opsional, `aggressive`): tambahkan satu kalimat statis ke system prompt ("Jawab ringkas; hindari pengulangan; jangan mengulang isi hasil tool."). Lossy pada gaya, bukan pada fakta; dapat dimatikan per request.
- Tidak berlaku bila 9router sudah menjalankan *Compress LLM output* (deteksi/konfigurasi `upstream.compress_output`, §6).

### 5.13 T13 — Routing model/effort untuk request pembantu

Banyak klien mengirim request pembantu (judul percakapan, ringkasan singkat, saran lanjutan) yang tidak butuh model besar atau agen bertool.

```toml
[[saver.route.rule]]
name  = "judul-percakapan"
match = { has_tools = false, max_tokens_lte = 64 }          # ciri bentuk request, bukan isi
route = { model = "ollama/qwen3:4b", mode = "chat_only", reasoning = "off" }

[[saver.route.rule]]
name  = "ringkasan-pendek"
match = { has_tools = false, est_prompt_tokens_lte = 800, system_contains = "ringkas" }
route = { provider_model = "claude/haiku" }
```

- Kriteria pencocokan: `has_tools`, `max_tokens_lte`, `est_prompt_tokens_lte/gte`, `system_contains` (substring pada system prompt), `stream`, `model` diminta, `subject`.
- **Opt-in dan eksplisit**: tidak ada aturan bawaan yang menebak isi.
- `agentgate analyze` (§13.5) membantu menemukan bentuk request yang sering muncul (tanpa menyimpan isi).
- Aturan yang dipakai dicatat di header respons `X-Agentgate-Route-Rule` dan log. Model akhir dilaporkan jujur pada field `model` respons (nama sebenarnya), bukan disamarkan.
- Bila rute menggeser ke provider lain, sesi (`spec_session.md`) mengikuti provider baru (Session berbeda).

### 5.14 T14 — Matikan panggilan overhead CLI

CLI agentik sering memanggil LLM **di latar** untuk hal yang tidak dibutuhkan gateway. Tiap panggilan = token tambahan:

| Jenis overhead ⚠️ | Contoh | Penanganan |
|---|---|---|
| Pembuatan judul sesi | opencode/lainnya membuat judul dengan model kecil | Nonaktifkan agent/judul lewat config inline atau arahkan ke model kecil/lokal |
| Ringkasan/kompaksi otomatis | Kompaksi konteks saat mendekati batas | Biarkan (berguna) tetapi **hitung** di usage; atur ambang bila bisa |
| Peringkasan output tool oleh CLI | Opsi ringkasan keluaran tool | Nonaktifkan (gateway sudah mengelola T6–T7) |
| Cek "siapa bicara berikutnya"/deteksi loop | Panggilan model tambahan | Nonaktifkan bila ada pengaturan |
| Telemetri/pembaruan/trafik non-esensial | Tidak memakai token model tetapi memakai jaringan | Nonaktifkan lewat env/config |

Aturan:
- Daftar overhead per CLI diisi **dari spike** (§13.3: bandingkan jumlah panggilan LLM/token yang dilaporkan CLI vs yang "berguna").
- Bila tidak bisa dimatikan, **ukur dan laporkan** pada usage dan metrik `agentgate_overhead_calls_total`.
- Jangan menonaktifkan prompt caching CLI (itu merugikan).

### 5.15 T15 — Cache respons persis *(opsional, off)*
- Untuk request **tanpa tools**, deterministik (`temperature ≤ 0` atau rute pembantu), dan bukan percakapan sesi.
- Kunci = HMAC(`tenant ‖ subject ‖ kanon(request)`); TTL pendek (default 10 menit); isi disimpan terenkripsi di memori.
- Diisolasi per subject; tidak pernah dipakai untuk request bertool atau sesi sticky.
- Penghematan: seluruh panggilan. Risiko: jawaban basi/privasi → default off.

### 5.16 T16 — Anggaran, kuota, dan degradasi

Per **subject** (`spec_session.md` §5.2), jendela harian:

| Parameter | Arti |
|---|---|
| `budget.tokens_per_day` | Batas total token-unit (prompt + completion; reasoning dihitung) |
| `budget.tokens_per_request` | Batas estimasi prompt per request |
| `budget.soft_pct` (80) | Di atas ini: **degradasi** otomatis |
| `budget.hard_pct` (100) | Di atas ini: tolak `429 token_budget_exceeded` |

Degradasi otomatis (urut): naikkan level (`safe→balanced→aggressive`) → T11 `cap` → T13 rute ke model murah → T12 gaya ringkas. Pemulihan otomatis saat jendela baru.
- Pra-cek memakai **estimasi**; pembukuan memakai **usage nyata** dari CLI (§8).
- Header `X-Agentgate-Budget-Remaining` (persen) dikirim bila anggaran aktif.
- Pembukuan disimpan di tabel `usage` (§8.4); tidak menyimpan isi.

---

## 6. Interaksi dengan Kompresi di Hulu (9router)

9router memiliki **Compress tool output (RTK)**, **Compress context (Headroom)**, dan **Compress LLM output** (`spec_session.md` §6.6). Dua lapis yang sama-sama memangkas dapat saling merusak atau tidak menambah apa pun.

### 6.1 Siapa memegang apa (matriks kepemilikan)

| Objek | Pemilik default | Alasan |
|---|---|---|
| Hasil tool | **9router (RTK)** bila aktif; saver hanya T6 lossless + T7 untuk hasil yang **masih** besar | Hindari pangkas ganda |
| Riwayat/konteks | **Saver** (sticky/T9); Headroom **disarankan dimatikan** pada rute ini | Delta sudah kecil; kompresi riwayat merusak pencocokan sesi |
| Output LLM | **9router** bila *Compress LLM output* aktif; saver T12 dilewati | Hindari instruksi ganda |
| Skema tool | **Saver** (T4) | Hulu tidak melihat skema yang dipasang ke CLI |
| Prompt dasar CLI | **Saver** (T3) | Hanya gateway yang mengendalikan CLI |

### 6.2 Deteksi dan konfigurasi
- Tidak ada cara pasti mendeteksi kompresor hulu; gunakan **konfigurasi eksplisit**: `upstream.tool_output = "rtk"|"none"`, `upstream.context = "headroom"|"none"`, `upstream.compress_output = true|false`.
- Petunjuk otomatis (best-effort): kehadiran tool `headroom_retrieve` di `tools[]` ⇒ `upstream.context = "headroom"`; hasil tool yang diawali pola marker kompresor yang dikenal ⇒ lewati T7.
- `agentgate doctor --probe-chain` (`spec_session.md` §5.5) dilengkapi laporan "kompresi terdeteksi" berdasarkan perbandingan ukuran hasil tool terkirim vs diterima (uji sintetis).

### 6.3 Aturan idempotensi
- Jika hasil tool **sudah pendek** (< `min_tokens_to_act`) atau sudah bermarker → tidak disentuh.
- Marker agentgate (Lampiran B) berbeda dan dapat dikenali kompresor lain; sebaliknya marker hulu dikenali lewat daftar pola (`upstream.markers`).
- Bila hulu mengompres secara **tidak deterministik** (bergantung posisi), jangan mengandalkan hash hasil tool untuk dedupe (T9); gunakan id panggilan.

### 6.4 `headroom_retrieve` dan Vault
- Bila Headroom memasang `headroom_retrieve`, itu urusan hulu. Saver **tidak** menyentuh tool tersebut selain T4 ringan.
- `ag_expand` dan `headroom_retrieve` dapat hidup berdampingan; keduanya adalah tool biasa bagi model. Beri deskripsi yang jelas agar model tidak tertukar.

---

## 7. Interaksi dengan Sesi, Bridge, dan Reasoning

| Dokumen | Aturan interaksi |
|---|---|
| **spec_session** | (1) Ledger = pesan asli (§4.3). (2) Vault mengikuti siklus hidup sesi (hapus saat `closed`, bersihkan saat rotate bila diatur). (3) Perubahan level tidak memicu rotate. (4) Saver tidak memakai konten Vault untuk pencocokan sesi |
| **spec_tooling** | (1) Tool internal (`ag_expand`) diselesaikan lokal oleh Bridge. (2) Urutan dan skema tool deterministik (T2/T4). (3) Memo (T8) menjawab `tools/call` tanpa batch ke client. (4) Hasil tool dari client ditransformasi **sebelum** diteruskan sebagai hasil MCP |
| **spec_reasoning** | (1) T11 mengendalikan effort; format output tidak berubah. (2) Reasoning tidak masuk transkrip (sudah) dan tidak dihitung ke ledger. (3) Aktivitas fallback tidak memakai token model |
| **Replay vs sticky** | T9 hanya pada replay/branch/new; sticky mengirim delta (transformasi T6–T7 tetap berlaku pada hasil tool di delta) |

Urutan saat resume bridge: hasil tool dari client → **T6 → T7 (Vault)** → hasil MCP ke CLI. `ledger` mencatat versi asli.

---

## 8. Pengukuran & Akuntansi Token

### 8.1 Estimator
- `est_tokens(text)` default: `ceil(chars / 4)`; kode/JSON `ceil(chars / 3)`; teks CJK `ceil(chars / 1.5)`.
- **Kalibrasi**: EMA rasio `usage_nyata / estimasi` per provider/model (jendela 200 request); estimator memakai rasio terkalibrasi.
- Estimasi hanya untuk keputusan (ambang, pra-cek anggaran, metrik penghematan); pembukuan memakai **usage nyata**.

### 8.2 Normalisasi usage

| Sumber | Field asli ⚠️ | → OpenAI/`usage` |
|---|---|---|
| claude (`result`) | input, output, cache creation, cache read | `prompt_tokens` = input + cache_read + cache_creation; `completion_tokens` = output; `prompt_tokens_details.cached_tokens` = cache_read |
| opencode (`step_finish`) | `tokens.input/output/reasoning/cache.read/cache.write` (per step) | Jumlahkan seluruh step; `reasoning_tokens` dari `reasoning` |
| gemini (`result`) | statistik token per model | Jumlahkan per model; `cached` bila ada |
| ollama | `prompt_eval_count`, `eval_count` | `prompt_tokens`, `completion_tokens` |

Run agentik multi-langkah: **jumlahkan semua langkah** (termasuk overhead CLI) agar angka mencerminkan konsumsi sebenarnya. Rincian per kategori dikirim di log dan `/admin/usage`.

### 8.3 Metrik

| Metrik | Keterangan |
|---|---|
| `agentgate_tokens_total{kind=prompt\|completion\|cached\|reasoning, provider}` | Konsumsi nyata |
| `agentgate_tokens_saved_est_total{technique}` | Estimasi hemat per teknik (T1…T16) |
| `agentgate_cache_hit_ratio{provider}` | `cached / prompt` bila dilaporkan |
| `agentgate_prefix_break_total{cause}` | Perubahan tools/system yang memecah prefiks |
| `agentgate_vault_entries`, `agentgate_vault_bytes`, `agentgate_vault_expand_total` | Vault |
| `agentgate_memo_hits_total` | Panggilan tool yang dijawab memo |
| `agentgate_overhead_calls_total{provider,kind}` | Panggilan latar CLI |
| `agentgate_budget_used_ratio{subject_hash}` | Pemakaian anggaran (subject di-hash) |
| `agentgate_route_rule_hits_total{rule}` | Aturan routing |
| `agentgate_tool_schema_tokens` (histogram) | Ukuran skema terpasang per Run |

Alarm: `cache_hit_ratio` turun mendadak (prefiks pecah), `overhead_calls_total` naik (CLI baru menambah panggilan), `vault_expand_total` tinggi pada satu tool (ambang pangkas terlalu agresif).

### 8.4 Tabel pembukuan

```sql
CREATE TABLE usage (
  tenant_id     TEXT NOT NULL,
  subject_id    TEXT NOT NULL,
  day           INTEGER NOT NULL,           -- yyyymmdd
  requests      INTEGER NOT NULL DEFAULT 0,
  prompt_tokens INTEGER NOT NULL DEFAULT 0,
  completion_tokens INTEGER NOT NULL DEFAULT 0,
  cached_tokens INTEGER NOT NULL DEFAULT 0,
  reasoning_tokens  INTEGER NOT NULL DEFAULT 0,
  saved_est     INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (tenant_id, subject_id, day)
);
```
Tidak ada kolom isi percakapan.

---

## 9. API & Header

### 9.1 Kontrol dari request

| Header | Fungsi |
|---|---|
| `X-Agentgate-Token-Mode: off\|safe\|balanced\|aggressive` | Level untuk request ini |
| `X-Agentgate-No-Compress: tools,results,history,output,route` | Opt-out per kategori |
| `X-Agentgate-Vault: off` | Matikan T7/T8 untuk request ini |

### 9.2 Header respons

| Header | Nilai |
|---|---|
| `X-Agentgate-Token-Mode` | Level efektif |
| `X-Agentgate-Tokens-Saved-Est` | Estimasi hemat **pra-eksekusi** (hasil T2–T9) |
| `X-Agentgate-Route-Rule` | Nama aturan routing yang dipakai (bila ada) |
| `X-Agentgate-Budget-Remaining` | Persen sisa anggaran (bila aktif) |

Usage nyata dikirim pada field `usage` (termasuk `prompt_tokens_details.cached_tokens` dan `completion_tokens_details.reasoning_tokens`); pada stream melalui chunk usage bila `stream_options.include_usage`.

### 9.3 Endpoint admin

| Method | Path | Fungsi |
|---|---|---|
| GET | `/admin/usage` | Ringkasan usage per hari/subject (tanpa isi) |
| GET | `/admin/saver/stats` | Penghematan per teknik, cache hit, overhead |
| GET | `/admin/saver/shapes` | Bentuk request yang sering muncul (ukuran, has_tools, max_tokens, hash system) — dasar aturan T13 |
| GET | `/admin/vault/{session}` | Daftar entri Vault (id, tool, ukuran; **tanpa isi**) |
| DELETE | `/admin/vault/{session}` | Hapus Vault sesi |

### 9.4 CLI

```
agentgate bench-tokens [--cli X] [--level L] [--scenario S]   # harness §13
agentgate analyze --last N                                      # bentuk request (tanpa isi)
agentgate doctor --tokens                                       # overhead dasar, flag hemat, usage tersedia
agentgate saver explain <request-id>                            # transformasi yang diterapkan + hemat
```

---

## 10. Konfigurasi

```toml
[saver]
level         = "balanced"       # off | safe | balanced | aggressive
vault         = "on"             # on | off
measure_only  = false            # true = hitung penghematan tanpa menerapkan

[saver.tools]
sort          = true             # T2: urut deterministik
slim          = "balanced"       # off | safe | balanced | aggressive (T4)
desc_prop_max = 240
desc_tool_max = 600
include       = []               # allowlist (kosong = semua)
exclude       = []
lazy          = false            # T5 eksperimental

[saver.results]
min_tokens_to_act = 200
max_tokens        = 2000         # T7
head_pct          = 60
tail_pct          = 25
never_truncate    = ["write_file"]
strip_ansi        = true
collapse_repeats  = true
minify_json       = true
blob_placeholder  = true

[saver.memo]
read_only_tools = []             # contoh: ["read_file","list_dir","grep"] (opt-in)
ttl_secs        = 120

[saver.history]
keep_last_results = 6
dedupe            = true
summarize         = false        # T10

[saver.reasoning]
policy     = "pass"              # pass | cap | auto
max_effort = "medium"

[saver.output]
max_tokens = 0
terse_style = false

[saver.cli]                      # T3/T14 (diverifikasi spike)
claude_override_system   = true
opencode_disable_title   = true
gemini_override_system   = true
disable_overhead_calls   = true

[saver.cache]
exact = false                    # T15

[vault]
mem_bytes            = 8388608
max_bytes_per_session= 16777216
max_entries          = 256
clear_on_rotate      = true

[upstream]                       # kompresor di hulu (9router)
tool_output     = "rtk"          # rtk | none
context         = "none"         # headroom | none  (disarankan none)
compress_output = false
markers         = []

[budget]
enabled            = false
tokens_per_day     = 0
tokens_per_request = 0
soft_pct           = 80
hard_pct           = 100

[[saver.route.rule]]             # contoh T13 (opt-in)
name  = "judul"
match = { has_tools = false, max_tokens_lte = 64 }
route = { model = "ollama/qwen3:4b", mode = "chat_only", reasoning = "off" }
```

Prioritas: header request > config provider > config global > default.

---

## 11. Keamanan & Privasi

| ID | Risiko | Mitigasi |
|---|---|---|
| TS-S1 | Vault menyimpan data sensitif (isi file/hasil perintah) | Per-sesi, id tak tertebak, TTL sesi, hapus saat `closed`/rotate, `vault=off`; tidak ada di log/DB (hanya metadata) |
| TS-S2 | `ag_expand` dipakai lintas sesi/subject | Id hanya valid dalam Session pemilik; cek tenant+subject; tidak ada endpoint publik berisi |
| TS-S3 | Pemangkasan membuang bagian penting sehingga model bertindak salah | Marker jelas + `ag_expand`; level `balanced` lolos harness kualitas; `never_truncate`; hasil `isError` tidak dipangkas |
| TS-S4 | Memo mengembalikan hasil basi setelah file berubah | Opt-in tool; invalidasi pada panggilan non-read-only; TTL pendek |
| TS-S5 | Cache respons persis membocorkan antar-pengguna | Kunci menyertakan tenant+subject; off secara default; tidak untuk request bertool/sesi |
| TS-S6 | Routing mengirim data ke model/provider berbeda tanpa disadari | Opt-in eksplisit; header `X-Agentgate-Route-Rule`; `model` respons jujur; kebijakan data per provider |
| TS-S7 | Ringkasan (T10) membocorkan atau mengubah fakta | Fase 2, default off; asli tetap di Vault; uji kualitas |
| TS-S8 | Skema di-slim mengubah kontrak validasi | Validasi pada skema **asli** di gateway |
| TS-S9 | Anggaran dipakai untuk menolak layanan pengguna lain | Anggaran per subject; subject wajib pada multi-user |
| TS-S10 | Marker disalahgunakan untuk prompt injection (hasil tool berisi marker palsu) | Marker disertai id Vault yang divalidasi; `ag_expand` hanya menerima id yang ada |
| TS-S11 | Log memuat isi hasil tool | Hanya ukuran/hash; isi tidak dicatat |

---

## 12. Pengujian

### 12.1 Komponen tiruan
- **`fake-cli-usage`**: menghasilkan event dan usage terjadwal (termasuk cache read/write, step ganda, overhead calls).
- **`fake-client-tools`**: set 40 tool dengan skema besar, hasil tool besar/berulang.
- **`fake-upstream-compressor`**: memodifikasi hasil tool dan menambah tool `headroom_retrieve`.
- Korpus hasil tool: log panjang, JSON besar, file minified, output berwarna ANSI, base64.

### 12.2 Skenario

| # | Skenario | Hasil |
|---|---|---|
| TS1 | Idempotensi tiap transformasi | `apply(apply(x)) == apply(x)` |
| TS2 | Lossless benar-benar lossless | Hasil T6 setara semantik (JSON ter-parse sama; teks setelah normalisasi sama) |
| TS3 | Pangkas + `ag_expand` | Marker memuat id; `ag_expand(range)` mengembalikan bagian asli persis |
| TS4 | Isolasi Vault | Id sesi/subject lain ditolak |
| TS5 | **Ledger tak terpengaruh** | Sticky tetap cocok setelah transformasi aktif/nonaktif/berubah level |
| TS6 | Urutan tool berbeda | `tools/list` identik byte-per-byte (hash sama) |
| TS7 | Slimming lossless | Validasi argumen identik (`required/type/enum/items` tak berubah) |
| TS8 | Pemangkasan deskripsi | Hanya di level yang ditentukan; bertanda; tidak memotong di tengah kalimat |
| TS9 | Memo | Panggilan kedua identik dijawab penunjuk; tidak ada `tool_calls` ke client; invalid setelah tulis |
| TS10 | Elisi riwayat | Pasangan call↔hasil utuh; hasil lama diganti marker |
| TS11 | Dedupe hasil | Hasil identik muncul sekali + rujukan |
| TS12 | Upstream compressor aktif | Tidak ada pangkas ganda; marker hulu dikenali; T7 dilewati |
| TS13 | `max_tokens` client | Dihormati; `finish_reason:"length"` |
| TS14 | Kebijakan reasoning `cap`/`auto` | Effort diturunkan sesuai aturan; request bertool tidak diturunkan oleh `auto` |
| TS15 | Routing request judul | Dialihkan ke model murah; header rute; `model` jujur |
| TS16 | Normalisasi usage per CLI | Jumlah multi-step benar; `cached_tokens`/`reasoning_tokens` terisi |
| TS17 | Anggaran soft | Level naik otomatis; header sisa anggaran |
| TS18 | Anggaran hard | `429 token_budget_exceeded` |
| TS19 | Cache respons persis | Hanya request tanpa tools; terisolasi per subject |
| TS20 | Overhead CLI dimatikan | Jumlah panggilan LLM pada `fake-cli-usage` turun sesuai flag |
| TS21 | Kalibrasi estimator | Galat relatif median ≤ 15% setelah 200 sampel |
| TS22 | `bench-tokens` reproduktif | Hasil sama (±2%) pada dua kali jalan |
| TS23 | Log/DB bersih | Tidak ada isi hasil tool/skema/argumen |
| TS24 | Opt-out header | `No-Compress: results` menonaktifkan T6–T7 untuk request itu |
| TS25 | Windows | Vault spill & path, CRLF di hasil tool |
| TS26 | TTL/cleanup Vault | Terhapus saat `closed`; rotate sesuai `clear_on_rotate` |
| TS27 | `ag_expand` internal | Tidak pernah muncul sebagai `tool_calls` ke client; tidak membuat batch/park |
| TS28 | Bentrok nama `ag_expand` | Tool client di-alias; keduanya berfungsi |
| TS29 | Stabilitas prefiks antar-giliran | Hash prefiks (system+tools) identik pada giliran berurutan |
| TS30 | Soak 500 giliran bertool | Tanpa kebocoran memori/Vault; penghematan stabil |

### 12.3 Uji manual rantai nyata
1. Jalankan skenario tetap (10 tugas) dengan `saver.level=off`, lalu `safe`, `balanced`, `aggressive`; catat token, latensi, keberhasilan.
2. Ulangi dengan **RTK ON/OFF** dan **Headroom ON/OFF** di 9router untuk memastikan tidak ada pangkas ganda dan melihat dampak pada sticky.
3. Periksa `cache_hit_ratio` pada sesi sticky vs replay.

---

## 13. Pengukuran, Spike, dan Penjaga Kualitas

### 13.1 Prinsip
Tidak ada teknik yang dijadikan default sebelum (a) baseline terukur, (b) penghematan terukur, (c) kualitas terjaga.

### 13.2 Skenario baku `bench-tokens`

| ID | Skenario | Fokus |
|---|---|---|
| B1 | Tanya-jawab 10 giliran tanpa tool | Sticky vs replay (T1) |
| B2 | Tugas bertool 30 langkah (baca banyak file, edit) | T4, T6, T7, T8 |
| B3 | Hasil tool raksasa (log 1 MB, grep luas) | T6, T7 |
| B4 | Banyak tool (40–60 skema) | T4, T5 |
| B5 | Request pembantu (judul/ringkasan) | T13 |
| B6 | Percakapan panjang (replay terpaksa) | T9, T10 |
| B7 | Tugas reasoning berat | T11 |
| B8 | Idle lalu lanjut | Efek TTL cache |

Tiap skenario dijalankan pada level `off/safe/balanced/aggressive` dan tiap CLI; keluaran: tabel token (prompt, cached, completion, reasoning), latensi, jumlah panggilan LLM (termasuk overhead), dan **skor keberhasilan** tugas.

### 13.3 Spike pengukuran per CLI

| ID | Pertanyaan | Hasil |
|---|---|---|
| Q1 | Token dasar saat prompt kosong (prompt dasar + tool terpasang) | Angka per CLI/model |
| Q2 | Efek flag T3 (override system, matikan konteks) | Selisih token |
| Q3 | Panggilan overhead apa saja, dan bisa dimatikan? | Daftar + cara |
| Q4 | Rasio cache read pada sticky vs replay | Persentase |
| Q5 | Token per skema tool (ukur 1, 10, 40 tool) | Kurva |
| Q6 | Field usage apa yang tersedia (cache, reasoning, per-step) | Peta field |
| Q7 | Efek urutan tool pada cache | Hit/miss |
| Q8 | Efek `tool_result` dipangkas pada keberhasilan tugas | Skor |
| Q9 | Perilaku bila `tools/list` berubah di tengah sesi (T5) | Didukung/tidak |
| Q10 | Perilaku auto-kompaksi CLI | Ambang, biaya |

Hasil → `compat/{cli}-{version}.json` bagian `tokens`.

### 13.4 Penjaga kualitas
- Setiap level punya **batas penurunan keberhasilan** terhadap `off` (default: ≤ 2 poin persentase pada B2/B3).
- Bila terlampaui, teknik penyebab dinonaktifkan dari level tersebut (daftar `saver.guard.disabled`).
- Rubrik keberhasilan berbasis pemeriksa otomatis (file dihasilkan benar, jawaban cocok pola), bukan penilaian model.

### 13.5 `analyze`
Menghitung distribusi **bentuk** request (tanpa isi): ukuran prompt, jumlah/ukuran tool, `has_tools`, `max_tokens`, hash system prompt. Menjadi dasar menulis aturan T13 dan memutuskan target T4/T5.

---

## 14. Rencana Implementasi

| Milestone | Isi | Kriteria selesai |
|---|---|---|
| **TS0** | **Akuntansi dan metrik dulu**: normalisasi usage, estimator, metrik, `bench-tokens` baseline, `measure_only` | Baseline B1–B8 per CLI tercatat |
| **TS1** | T6 transformasi aman + T14 flag overhead (hasil spike) | TS1, TS2, TS20 |
| **TS2** | T2 urutan deterministik + T4 slimming (safe/balanced) | TS6–TS8, TS29 |
| **TS3** | T7 Vault + `ag_expand` (tool internal di Bridge) + T8 memo | TS3–TS5, TS9, TS26–TS28 |
| **TS4** | T3 flag hemat per CLI (setelah spike Q1–Q3) | Penghematan terukur; kualitas lolos |
| **TS5** | T9 elisi/dedupe/windowing untuk replay | TS10, TS11 |
| **TS6** | T11/T12/T13: reasoning policy, output, routing + `analyze` | TS13–TS15 |
| **TS7** | T16 anggaran & kuota + degradasi | TS17, TS18 |
| **TS8** | Eksperimental: T5 lazy tool, T10 ringkasan, T15 cache persis | Opt-in; lolos kualitas |

Urutan disarankan: **TS0 → TS1 → TS2 → TS3 → TS4 → TS5 → TS6 → TS7**. TS0 adalah syarat: tanpa baseline, penghematan tidak dapat diklaim.

---

## 15. Risiko & Pertanyaan Terbuka

| # | Risiko / pertanyaan | Dampak | Mitigasi |
|---|---|---|---|
| R1 | Pemangkasan hasil tool menurunkan kualitas | Jawaban salah | Marker + `ag_expand`; harness kualitas; ambang konservatif |
| R2 | Pangkas ganda dengan RTK/Headroom | Informasi hilang | Idempotensi, matriks kepemilikan (§6), `upstream.*` |
| R3 | Override system prompt CLI melemahkan perilaku | Kualitas turun | Uji kualitas; level per CLI; dapat dimatikan |
| R4 | CLI tidak melaporkan usage lengkap | Pembukuan tak akurat | Estimator terkalibrasi; tandai `estimated=true` |
| R5 | Panggilan overhead CLI tidak bisa dimatikan | Konsumsi tersembunyi | Ukur dan laporkan; pilih model kecil untuk overhead bila bisa |
| R6 | Tool internal (`ag_expand`) membingungkan model / bentrok nama | Panggilan salah | Deskripsi jelas; alias; ukur frekuensi |
| R7 | Memo memberi hasil basi | Tindakan salah | Opt-in, invalidasi, TTL |
| R8 | Routing salah mengalihkan tugas penting ke model lemah | Kualitas turun | Aturan eksplisit berbasis bentuk; header rute; `analyze` |
| R9 | Daftar tool berubah memecah cache | Biaya naik | T2; metrik `prefix_break`; saran ke client |
| R10 | Cache penyedia berbeda perilaku/TTL antar-vendor/versi | Perkiraan meleset | Ukur (Q4/Q7); jangan mengandalkan angka tetap |
| R11 | Kuota anggaran memicu degradasi yang mengejutkan pengguna | UX | Header jelas; log; ambang dapat diatur |
| R12 | Vault memperbesar jejak data sensitif di mesin B | Privasi | TTL sesi, hapus saat tutup, `vault=off` |
| R13 | Estimasi hemat dianggap klaim pasti | Salah harap | Label "est"; bandingkan dengan usage nyata di `/admin/saver/stats` |

**Pertanyaan terbuka:**
1. Apa yang dominan biayanya: **kuota langganan** CLI, tagihan per token, atau latensi? (menentukan prioritas T1–T16)
2. Berapa jumlah dan ukuran tool yang biasa dikirim AionUi (menentukan nilai T4/T5)?
3. Apakah RTK dan/atau Headroom di 9router aktif pada rute ini, dan bisa dimatikan per rute?
4. Request pembantu apa yang dikirim AionUi (judul, ringkasan), dan model kecil/lokal apa yang tersedia untuk T13?
5. Versi CLI dan model target (menentukan flag T3/T14 dan perilaku cache)?
6. Seberapa besar toleransi terhadap penurunan kualitas (ambang §13.4)?

---

## 16. Perubahan yang Diperlukan pada Dokumen Lain

| Dokumen | Perubahan |
|---|---|
| `spec_tooling.md` §9.3 / §9.5 | Tambahkan **tool internal gateway** (`ag_expand`): muncul di `tools/list`, `tools/call` diselesaikan seketika (tanpa batch/park/`tool_calls`) |
| `spec_tooling.md` §8.3 / §13 | Cadangkan nama `ag_expand`; guard mengizinkannya; sebut urutan tool deterministik (T2) |
| `spec_tooling.md` §11.1 | Catatan gateway pada system prompt bersifat statis (tanpa timestamp/ID) |
| `spec_session.md` §8.5 | Pembersihan menyertakan Vault; ledger memakai pesan asli (rujuk §4.3 dokumen ini) |
| `spec_reasoning.md` §5.4 / §7 | Rujuk T11 (kebijakan anggaran) untuk pemetaan effort/anggaran |
| `tool-handling.md` §10 | Tambah modul `saver/` pada alur request (setelah resolusi sesi, sebelum spawn) |
| `plan.md` | Tambah referensi dan milestone TS0–TS8 |

---

## Lampiran A — Contoh Penghematan (ilustrasi, bukan klaim)

| Komponen | Sebelum | Sesudah | Teknik |
|---|---|---|---|
| Skema 40 tool | ~24.000 token | ~15.000 token | T4 (S1–S5) |
| `grep` 1.200 baris | ~14.000 token | ~1.900 token + marker | T7 |
| Log berwarna ANSI 300 baris | ~6.500 token | ~4.800 token | T6 |
| Riwayat 40 giliran (replay) | ~90.000 token | ~35.000 token | T9 |
| Riwayat 40 giliran (sticky) | ~90.000 token | ~1.500 token (delta) | T1 |
| Panggilan judul sesi | ~2.000 token model besar | ~300 token model kecil | T13/T14 |

*Angka hanya ilustrasi untuk menjelaskan urutan besaran; nilai nyata diukur lewat `bench-tokens`.*

## Lampiran B — Marker & Contoh Transformasi

Marker pemangkasan (format tetap agar idempoten dan dapat dikenali):

```
[agentgate:pangkas id=r_7f3a1c2e baris=1240 tampil=1-60,1180-1240 ambil=ag_expand(id, mode=range|grep|head|tail)]
```

Contoh hasil tool setelah T6+T7:

```
src/a.rs:12: fn main() {
src/a.rs:40: let x = 1;
… (60 baris pertama) …
[agentgate:pangkas id=r_7f3a1c2e baris=1240 tampil=1-60,1180-1240 ambil=ag_expand(id, mode=range|grep|head|tail)]
… (61 baris terakhir) …
```

Ringkas baris berulang:

```
warn: koneksi lambat  (×37)
```

Penunjuk memo (T8):

```
[agentgate:memo id=r_91bc0d44 sama-dengan-panggilan=#3 tool=read_file]
```

Marker riwayat (T9):

```
[agentgate:elisi id=r_55aa10ee hasil-tool-lama tool=grep ukuran=11820B]
[agentgate:riwayat-dipotong giliran=18]
```

## Lampiran C — Kerangka Kode

```rust
// saver/pipeline.rs
pub struct Pipeline { tools: Vec<Box<dyn Transform>>, results: Vec<Box<dyn Transform>>, history: Vec<Box<dyn Transform>> }

impl Pipeline {
    pub fn run_tool_result(&self, cx: &mut Ctx, text: String) -> (String, Saved) {
        if est_tokens(&text) < cx.cfg.results.min_tokens_to_act || has_marker(&text) { return (text, Saved::zero()); }
        let mut p = Payload::ToolResult(text);
        let mut saved = Saved::zero();
        for t in self.results.iter().filter(|t| t.min_level() <= cx.level) {
            saved += t.apply(cx, &mut p);       // idempoten; T6 lalu T7
        }
        (p.into_text(), saved)
    }
}

// vault.rs
pub struct Vault { mem: DashMap<VaultId, Entry>, bytes: AtomicUsize, cfg: VaultCfg }
impl Vault {
    pub fn put(&self, sess: &SessionId, tool: &str, original: String) -> VaultId { /* LRU + batas */ }
    pub fn expand(&self, sess: &SessionId, id: &VaultId, q: ExpandQuery) -> Result<String, VaultErr> {
        // cek kepemilikan sesi; head | tail | range | grep; batasi max_lines & token
    }
}
```

```rust
// bridge: tool internal diselesaikan lokal
async fn handle_tools_call(&self, run: &Run, name: &str, args: Value) -> ToolOutcome {
    if name == "ag_expand" {
        return match self.vault.expand(&run.session_id, &parse_id(&args)?, parse_query(&args)?) {
            Ok(t)  => ToolOutcome::Ok(t),
            Err(e) => ToolOutcome::Err(e.to_string()),
        };                                    // TIDAK masuk batch / park / tool_calls ke client
    }
    if let Some(prev) = self.memo.lookup(run, name, &args) { return ToolOutcome::Ok(prev.pointer()); }
    self.hold_and_forward_to_client(run, name, args).await   // jalur normal
}
```

## Lampiran D — Checklist Verifikasi per CLI (isi saat spike)

| Item | claude | gemini | opencode | ollama |
|---|---|---|---|---|
| Versi & model diuji | | | | |
| Token dasar (prompt kosong) | | | | |
| Efek override system prompt | | | | |
| Efek mematikan konteks (CLAUDE/AGENTS/GEMINI.md, memori) | | | | |
| Panggilan overhead terdeteksi (jenis, jumlah) | | | | |
| Cara mematikan overhead | | | | |
| Usage tersedia (cache read/write, reasoning, per-step) | | | | |
| Rasio cache sticky vs replay | | | | |
| Efek urutan tool pada cache | | | | |
| Token per skema tool | | | | |
| Efek hasil tool dipangkas pada keberhasilan | | | | |
| Perilaku daftar tool dinamis (`listChanged`) | | | | |
| Perilaku auto-kompaksi (ambang, biaya) | | | | |
