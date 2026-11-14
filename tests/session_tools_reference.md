# Referensi Tools Sesi (OpenCode) untuk Pengujian

Dokumen ini merangkum tool yang **tersedia di sesi OpenCode ini** untuk keperluan pengujian di `C:\Users\EDN\Documents\AION\project\agentgate\tests`.

## Tool yang tersedia

| Tool | Fungsi | Catatan penggunaan |
|---|---|---|
| `read` | Membaca isi file atau direktori (teks, gambar, PDF). | Baris teks diberi prefix nomor (`<line>: ...`) sebagai referensi; dukung `offset`/`limit` untuk file/direktori besar. |
| `shell` | Menjalankan perintah shell dan mengembalikan output. | Windows via PowerShell; kutip path dengan spasi/karakter khusus. Bisa set `workdir`, `timeout`, `background`. Output besar bisa terpotong (preview) dengan hasil lengkap tersimpan. |
| `glob` | Mencari path file berdasarkan pola glob. | Contoh: `**/*.ts`, `src/**/*.tsx`. Opsi: `path`, `hidden`, `limit`. |
| `execute` (`exec`) | Menjalankan JavaScript di runtime Code Mode untuk mengorkestrasi panggilan tool/HTTP dan mengkomposisi hasil. | `fetch` tersedia; impor, akses filesystem langsung, dan timer (`setTimeout`/sleep) tidak tersedia. Akses eksternal lain via `tools`. Hanya tool yang tercantum di katalog Code Mode/search yang dapat dipanggil di dalam `{ code }` (gunakan path/signature persis, mis. `await tools["aionui-team"].team_task_list({})`). `await` panggilan yang penting; panggilan independen bisa paralel (`Promise.allSettled`). |
| `write` (`create/write`) | Menulis/membuat file di filesystem lokal (overwrite jika ada). | Direktori induk dibuat otomatis. Untuk perubahan parsial pada file yang ada, gunakan `edit`. |
| `edit` | Mengganti teks persis (`oldString` → `newString`) pada file. | `oldString` harus unik (atau `replaceAll: true`); pertahankan indentasi persis. Cocok untuk perubahan tertarget. |

## Konteks untuk testing
- **Workspace:** `C:\Users\EDN\Documents\AION\project\agentgate` (repo git, Windows).
- **Path pengujian:** `C:\Users\EDN\Documents\AION\project\agentgate\tests` (dokumen ini berada di path tersebut).
- **Catatan:** `shell` berjalan di Windows (PowerShell). File/path dapat absolute atau relatif terhadap direktori kerja sesi. Gunakan `write` untuk membuat/overwrite file utuh; gunakan `edit` untuk patch parsial.

*Dibuat untuk mendukung tugas verifikasi/pengujian (mis. VERIFY-01) di sesi ini.*
