# Panduan pengguna Soli

Soli adalah engine spot memecoin Solana. Mode bawaan adalah **dry run**: saldo dan transaksi paper bersifat virtual. Preview tidak mengirim transaksi ke jaringan dan bukan bukti profit.

## Mulai dari status

Minta menu dan status untuk melihat command yang tersedia serta keadaan engine saat ini:

```text
/soli-menu
/soli status
/soli lifecycle
```

Di Hermes Slack, gunakan `!soli-menu` dan `!soli status` di DM. Di shared channel, awali dengan mention bot, misalnya `@Soli !soli status`. Adapter lain perlu terhubung dan dikonfigurasi pada host masing-masing; dukungan protokol tidak otomatis menghubungkan akun.

Status utama menunjukkan mode engine, saldo ledger paper, posisi, fill, batas risiko, dan lifecycle paper. `lifecycle` menunjukkan mode paper, jeda, waktu kedaluwarsa, serta batas entry. Saldo paper bukan saldo wallet sungguhan. Nilai awal virtual 1.000 USDC hanya dipakai saat database baru dibuat; baca status untuk saldo aktual. `/soli wallet` membaca wallet publik jika alamat wallet dan RPC tersedia; ledger paper ditampilkan terpisah.

## Command yang sering dipakai

```text
/soli status             # status engine dan ledger
/soli lifecycle          # kontrol siklus paper
/soli wallet             # informasi wallet/posisi sesuai mode
/soli positions
/soli pnl
/soli report             # laporan terbaru
/soli learning           # bukti pembelajaran dari ronde selesai
/soli candidates         # kandidat sinyal live; bukan order
/soli market MINT        # data pasar token
/soli risk MINT          # pemeriksaan risiko token
```

Ganti `MINT` dengan alamat mint token Solana. Data provider bisa tidak tersedia atau belum lengkap; minta Soli menyebutkan data yang hilang, jangan menebak harga atau hasil keamanan. Pemeriksaan risiko yang lolos bukan jaminan profit maupun jaminan bisa menjual.

MCP menyediakan tool `soli_menu`, `soli_status`, `soli_token`, `soli_preview`, dan `soli_paper_fill`. Tool status/read-only tidak mengubah portfolio. `soli_token` menerima view `risk`, `market`, `samples`, atau `replay`.

## Simulasi order paper

1. Periksa token dengan `risk` dan `market`.
2. Minta preview paper untuk mint dan jumlah yang diinginkan.
3. Tinjau mode, mint, jumlah, minimum output, biaya, dan masa berlaku dari preview.
4. Jika memang ingin mencatat simulasi, isi **preview paper yang masih berlaku** dengan quote ID-nya. Fill paper memakai saldo virtual saja.

Contoh di Hermes Slack DM:

```text
!soli preview buy MINT 10000000
```

Di shared channel, sertakan mention bot: `@Soli !soli preview buy MINT 10000000`.

Contoh CLI lokal:

```text
soli-agent preview buy MINT 10000000
soli-agent paper-fill QUOTE_ID
```

Contoh tool MCP: `soli_preview` dengan `side: "buy"`, mint, jumlah `"10000000"`, dan `mode: "paper"`; lalu `soli_paper_fill` dengan `quote_id` dari preview. Preview kedaluwarsa setelah sekitar 20 detik. Buat preview baru jika sudah kedaluwarsa.

Jumlah beli memakai unit atomik USDC: **1 USDC = 1.000.000 unit**, jadi `10000000` berarti 10 USDC. Jumlah jual memakai unit mentah token; exit yang didukung adalah seluruh posisi. Selalu gunakan jumlah dan minimum output yang dikembalikan preview, bukan perkiraan.

Preview tidak mengeksekusi order. Fill paper hanya mengubah ledger virtual; ia tidak menandatangani atau menyiarkan transaksi. Tool MCP menolak jalur paper fill saat engine live.

## Kontrol paper

Lifecycle paper mendukung `manual`, `semi`, dan `full`. `full` mengizinkan entry dan exit otomatis yang memenuhi strategi dan pemeriksaan engine; `semi` memerlukan persetujuan entry paper dan dapat menjalankan exit strategi; `manual` menghentikan entry otomatis, tetapi mandat exit yang sudah aktif tetap berjalan. Kontrol paper bertahan setelah restart.

Owner dapat mengatur siklus melalui command terautentikasi:

```text
/soli mode manual|semi|full JAM ENTRY_USDC
/soli pause entries|all
/soli resume-cycle entries|all
/soli pause all
/soli resume-cycle all
```

Durasi mode paper adalah 1–168 jam, dan ukuran entry 1–100 USDC. `pause entries` menghentikan entry baru, sedangkan exit dan rekonsiliasi tetap berjalan. `pause all` menghentikan entry/exit otomatis; rekonsiliasi tetap berjalan. Force-close dapat mengabaikan jeda strategi, tetapi tetap memerlukan quote dan pemeriksaan transaksi yang lolos. Lihat [daftar command lengkap](commands.md) sebelum memakai kontrol posisi.

## Live trading dan keamanan

Mode live **tidak aktif hanya karena Soli terpasang, status berhasil dibaca, atau preview dibuat**. Live memerlukan setup signer terisolasi dan otorisasi owner terpisah. MCP Soli tidak menyediakan tool eksekusi real-order atau pengelolaan key.

Jangan kirim private key, seed phrase, owner token, credential, atau secret lewat chat. Soli tidak menjanjikan profit; likuiditas, slippage, kegagalan provider, dan kondisi chain dapat membatasi quote atau exit. Baca [operasi live](operations.md) hanya jika owner memang mengelola setup live, serta [validasi dan batasan](validation.md) sebelum mempertimbangkan dana nyata.

## Referensi

- [Daftar command](commands.md)
- [Instalasi dan koneksi host](../INSTALL.md)
- [Integrasi Hermes](hermes.md)
- [Batasan validasi](validation.md)
