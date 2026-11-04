Hal yang Perlu Diperhatikan dalam Membangun Sistem SSE

Saya jelaskan dari dasar protokol sampai jebakan praktis di proyek agentgate, karena di situ SSE bertemu dengan subprocess CLI.

1. Dasar SSE

SSE (Server-Sent Events) adalah satu response HTTP yang tidak pernah “selesai” dulu. Server menulis potongan teks terus-menerus ke body, dan client membacanya secara bertahap. Arahnya satu: server ke client.

Header wajib:

HTTP/1.1 200 OK
Content-Type: text/event-stream
Cache-Control: no-cache
Connection: keep-alive          (HTTP/1.1)
X-Accel-Buffering: no           (agar nginx tidak mem-buffer)

Format body berupa teks UTF-8. Setiap event diakhiri satu baris kosong (\n\n):

data: {"hello":"world"}

data: {"hello":"lagi"}

Field yang dikenal spesifikasi:

Field	Fungsi
data:	Isi event. Beberapa baris data: berurutan digabung client dengan \n
event:	Nama tipe event (default message)
id:	ID event, dipakai untuk Last-Event-ID saat reconnect
retry:	Saran jeda reconnect (ms)
: (awal baris)	Komentar, diabaikan client. Cocok untuk heartbeat (: ping\n\n)

Satu hal yang sering terlewat: baris kosong itulah yang “mengirim” event. Tanpa \n\n, client menunggu terus.

2. Format Khusus OpenAI

Karena agentgate meniru OpenAI, SSE-nya harus mengikuti bentuk ini persis:

data: {"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1730000000,"model":"gemini/gemini-2.5-pro","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}

data: {"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1730000000,"model":"gemini/gemini-2.5-pro","choices":[{"index":0,"delta":{"content":"Halo"},"finish_reason":null}]}

data: {"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1730000000,"model":"gemini/gemini-2.5-pro","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: [DONE]

Poin penting:

Chunk pertama membawa delta.role = "assistant" tanpa konten.
Chunk berikutnya membawa delta.content berisi potongan teks.
Chunk terakhir punya delta: {} dan finish_reason terisi (stop, length, atau error).
Stream ditutup dengan literal data: [DONE]. Ini bukan JSON, jadi client tidak boleh mem-parse-nya sebagai JSON.
id dan created sama di semua chunk dalam satu respons.
Kalau client mengirim stream_options: {"include_usage": true}, kirim satu chunk tambahan sebelum [DONE] dengan choices: [] dan field usage.
Request OpenAI memakai POST, jadi EventSource bawaan browser (yang hanya GET) tidak bisa dipakai. Client memakai fetch + pembacaan stream. Ini wajar, tapi artinya Last-Event-ID dan auto-reconnect tidak berlaku.
3. Buffering: Musuh Utama Streaming

Streaming sering “terlihat jalan” di localhost tapi muncul sekaligus di produksi. Penyebabnya hampir selalu buffering, dan bisa terjadi di beberapa lapisan:

a. Reverse proxy (nginx, Caddy, Cloudflare). Nginx mem-buffer response secara default. Solusinya header X-Accel-Buffering: no atau proxy_buffering off;.

b. Middleware kompresi. gzip/brotli menahan data sampai blok cukup besar. Kalau memakai tower_http::CompressionLayer, kecualikan text/event-stream. Periksa juga versi crate yang dipakai, karena perilaku default predicate berbeda antar versi.

c. Buffering di sisi proses anak (spesifik proyek ini). Banyak program, saat stdout-nya berupa pipe dan bukan terminal, beralih ke block buffering (4–8 KB). Akibatnya teks dari CLI datang bergerombol, bukan token demi token. Cara menyiasatinya:

Utamakan mode --output-format stream-json atau sejenisnya, karena output per baris biasanya di-flush per baris.
Baca stdout per chunk yang tersedia, jangan menunggu EOF atau menunggu buffer penuh.
Jika CLI memang tidak mau flush, opsi terakhir adalah PTY (portable-pty), dengan konsekuensi output berisi ANSI yang harus dibersihkan.

d. Buffer di aplikasi sendiri. Jangan mengumpulkan delta ke String lalu mengirimnya sekaligus. Kirim setiap delta begitu tiba.

4. Mendeteksi Client Putus (Cancellation)

Ini bagian terpenting untuk gateway yang menjalankan subprocess. Kalau user menutup tab atau klien timeout, proses CLI harus dihentikan. Kalau tidak, proses yatim terus memakai CPU dan kuota/token akun.

Prinsipnya di axum/hyper: ketika client putus, body response di-drop, sehingga stream Anda di-drop. Manfaatkan itu dengan drop guard:

rust
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::{Stream, StreamExt};
use std::{convert::Infallible, time::Duration};
use tokio_util::sync::CancellationToken;

async fn chat_stream(
    state: AppState,
    req: ChatRequest,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AppError> {
    // 1) Validasi + start provider SEBELUM stream dibuat,
    //    supaya error awal bisa jadi HTTP 4xx/5xx yang benar.
    let cancel = CancellationToken::new();
    let guard = cancel.clone().drop_guard();   // cancel otomatis saat di-drop
    let mut events = state.registry.chat(&req, cancel.clone()).await?;

    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
    let created = unix_now();
    let model = req.model.clone();

    let body = async_stream::stream! {
        let _guard = guard;  // hidup selama stream hidup; drop => cancel => kill child

        yield Ok(Event::default().data(chunk_role(&id, created, &model)));

        while let Some(ev) = events.next().await {
            match ev {
                Ok(ChatEvent::Delta(text)) => {
                    yield Ok(Event::default().data(chunk_delta(&id, created, &model, &text)));
                }
                Ok(ChatEvent::Done { finish_reason }) => {
                    yield Ok(Event::default().data(chunk_finish(&id, created, &model, finish_reason)));
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    // Status sudah 200, jadi error dikirim sebagai event
                    yield Ok(Event::default().data(error_json(&e)));
                    break;
                }
            }
        }
        yield Ok(Event::default().data("[DONE]"));
    };

    Ok(Sse::new(body).keep_alive(
        KeepAlive::new().interval(Duration::from_secs(15)).text("ping"),
    ))
}

Di sisi provider, task yang memegang child process harus select! pada cancel.cancelled(). Begitu terpicu, bunuh seluruh process tree (process group di Unix, Job Object di Windows), lalu wait() agar tidak jadi zombie.

Jebakan: disconnect baru terdeteksi saat server mencoba menulis atau saat hyper menutup body. Karena itu heartbeat (poin 5) juga membantu mendeteksi koneksi yang sudah mati.

5. Heartbeat / Keep-Alive

CLI agent kadang “berpikir” 10–60 detik sebelum mengeluarkan token pertama. Selama itu koneksi tidak membawa data, dan banyak proxy/load balancer memutus koneksi idle (umumnya 30–120 detik).

Solusinya kirim komentar SSE secara berkala (: ping\n\n). Client mengabaikannya, tetapi proxy melihat ada trafik. KeepAlive bawaan axum sudah melakukan ini. Interval 10–20 detik biasanya aman.

6. Penanganan Error: Sebelum vs Sesudah Stream Dimulai

Begitu header 200 OK terkirim, status code tidak bisa diubah lagi. Maka ada dua fase:

Fase	Contoh error	Cara menangani
Sebelum stream	model tidak ada, JSON salah, API key salah, provider belum login, binary tidak ditemukan, antrean penuh	Balas HTTP 4xx/5xx biasa dengan body error JSON ala OpenAI
Sesudah stream	CLI crash di tengah jalan, timeout, exit code ≠ 0	Kirim satu event error (data: {"error":{...}}), lalu [DONE] dan tutup

Konsekuensinya untuk desain: lakukan sebanyak mungkin pekerjaan yang bisa gagal sebelum mengembalikan Sse::new(...). Misalnya spawn proses dan pastikan ia hidup, atau tunggu event pertama kalau perlu.

Pastikan juga stream selalu berakhir dengan rapi (finish chunk lalu [DONE]) di semua jalur, termasuk saat error. Banyak SDK menggantung kalau [DONE] tidak pernah datang.

7. Batas Chunk dan Encoding

Output CLI datang sebagai byte, bukan string rapi.

Karakter multibyte terpotong. Satu read() bisa berhenti di tengah karakter UTF-8 (emoji, aksara non-Latin). Jangan langsung from_utf8_lossy per chunk. Gunakan decoder inkremental, yaitu simpan byte sisa di buffer, atau baca per baris.
Baris JSONL terpotong. Mode JSONL harus di-buffer sampai ketemu \n, baru di-parse. Beri batas panjang baris maksimum agar satu baris raksasa tidak menghabiskan memori.
Newline di dalam data:. JSON dari serde_json tidak memuat newline mentah (newline di-escape jadi \n), jadi aman. Jangan menulis teks mentah multi-baris ke data: tanpa disadari.
CRLF di Windows. Output CLI di Windows sering \r\n. Normalisasi sebelum parsing.
ANSI escape. Bersihkan kode warna dan cursor dari output teks sebelum dijadikan delta.
8. Backpressure dan Client Lambat

Kalau CLI menghasilkan teks lebih cepat daripada client membaca, data menumpuk di memori.

Pakai channel terbatas (tokio::sync::mpsc::channel(N) dengan N kecil, misalnya 32–128), bukan unbounded.
Saat channel penuh, pembaca stdout otomatis melambat. Pipe OS pun akhirnya penuh dan proses anak ikut tertahan. Ini perilaku yang benar.
Jangan membuat buffer tak terbatas “demi kecepatan”.
Pertimbangkan idle write timeout: bila client tidak menerima data selama X detik, putuskan koneksi dan bunuh prosesnya.
9. Timeout: Jangan Salah Pasang
Timeout total request yang memotong body akan membunuh stream sah yang panjang. Hati-hati dengan middleware timeout global. Perilakunya berbeda antar versi dan layer, jadi uji dengan stream yang sengaja lama.
Yang lebih tepat ada tiga:
Timeout token pertama (misalnya 120 detik): CLI tidak mengeluarkan apa pun.
Idle timeout (misalnya 60–120 detik): tidak ada output baru di tengah jalan.
Batas durasi total yang longgar (misalnya 10–30 menit) sebagai pengaman terakhir.
Heartbeat SSE tidak boleh me-reset idle timeout CLI. Yang dihitung adalah output nyata dari proses anak.
10. Konkurensi dan Sumber Daya

Setiap stream memegang satu subprocess selama durasinya, jadi sumber dayanya mahal.

Pakai Semaphore per provider. Permit dipegang selama stream hidup dan dilepas saat stream selesai atau di-drop. Gunakan OwnedSemaphorePermit yang dipindahkan ke dalam stream.
Antrean penuh: balas 429 Too Many Requests (atau 503) dengan Retry-After. Jangan membiarkan request menggantung tanpa batas.
Batasi jumlah total koneksi dan ukuran body request.
Pastikan tidak ada proses zombie: setiap jalur (selesai, error, cancel, timeout) harus berakhir dengan kill + wait.
11. Logging dan Observabilitas
Jangan mencatat log per chunk. Itu membanjiri log dan memperlambat. Cukup catat awal, akhir, jumlah byte atau token, durasi, serta alasan penutupan (completed, client_disconnect, timeout, cli_error).
Beri request ID di setiap log supaya satu stream bisa ditelusuri.
Metrik berguna: jumlah stream aktif, waktu hingga token pertama (TTFT), durasi total, jumlah cancel.
Isi prompt dan respons adalah data sensitif, jadi log hanya jika di-opt-in.
12. Keamanan
Autentikasi dan validasi dilakukan sebelum stream dibuka, bukan sesudahnya.
CORS hanya diaktifkan bila benar-benar ada client browser. Jika aktif, pastikan header SSE ikut diizinkan.
Batasi jumlah stream per API key untuk mencegah penyalahgunaan.
Jangan membocorkan stderr mentah ke client. Itu bisa memuat path, token, atau detail internal. Kirim pesan error yang sudah disanitasi.
13. HTTP/1.1 vs HTTP/2
Di HTTP/1.1, SSE memakai chunked transfer encoding dan satu koneksi TCP per stream. Browser membatasi sekitar 6 koneksi per domain. Untuk client API (SDK, curl) ini jarang jadi masalah.
HTTP/2 mem-multiplex banyak stream dalam satu koneksi, sehingga batas itu hilang. Tapi untuk server lokal, HTTP/1.1 sudah cukup.
Kalau ada TLS-terminating proxy di depan, pastikan proxy tersebut tidak mem-buffer dan tidak memotong koneksi idle.
14. Strategi Pengujian
curl -N (flag -N mematikan buffering di curl) untuk melihat chunk muncul bertahap:
bash
   curl -N http://127.0.0.1:8787/v1/chat/completions \
     -H "Content-Type: application/json" \
     -d '{"model":"fake/slow","stream":true,"messages":[{"role":"user","content":"tes"}]}'
Fake CLI (skrip .sh dan .cmd) yang bisa diatur untuk:
mengeluarkan teks pelan-pelan,
diam lama sebelum token pertama,
crash di tengah,
mengeluarkan UTF-8 multibyte dan newline Windows,
menghasilkan output sangat besar.
Tes disconnect: buka stream, putuskan di tengah, lalu pastikan proses anak benar-benar mati (cek PID).
Tes client lambat: baca stream dengan sengaja pelan, lalu pastikan memori tidak membengkak.
Tes dengan SDK resmi (openai Python/JS) untuk memastikan format chunk dan [DONE] diterima dengan benar.
Jalankan di Linux dan Windows di CI, karena perilaku pipe dan proses berbeda.
15. Checklist Ringkas
 Header: text/event-stream, no-cache, X-Accel-Buffering: no
 Format chunk OpenAI benar: role → delta → finish → [DONE]
 Error awal jadi HTTP status; error tengah jadi event error + [DONE]
 Kompresi dikecualikan untuk SSE
 Delta dikirim begitu tiba, tanpa buffering
 Heartbeat 10–20 detik
 Drop guard → cancel → kill process tree → wait()
 Channel terbatas (backpressure)
 Timeout bertingkat: token pertama, idle, total
 Decoder UTF-8 inkremental dan buffer JSONL per baris
 Semaphore per provider, permit dipegang selama stream
 Log ringkas per stream, bukan per chunk
 Diuji dengan curl -N, fake CLI, disconnect, dan SDK resmi di Linux + Windows