# VNC Viewer

Rust + [egui](https://github.com/emilk/egui) ile yazılmış, TightVNC sunucularıyla uyumlu hafif bir VNC istemcisi.

## Çalıştırma

```sh
cargo run --release                     # bağlantı ekranı açılır
cargo run --release -- 192.168.1.10     # şifresiz sunucuya doğrudan bağlanır (port varsayılan 5900)
```

Windows için tek bir `.exe` üretir: `cargo build --release` → `target/release/vnc-viewer.exe`.

## Şu an desteklenenler

- RFB 3.3 / 3.7 / 3.8 el sıkışması
- Güvenlik: None ve VNC Authentication (TightVNC'nin varsayılan şifresi)
- Kodlamalar: Raw, CopyRect, DesktopSize (çözünürlük değişimi)
- Fare (sol/orta/sağ tık, tekerlek), klavye (harfler, Ctrl/Alt/Shift, ok tuşları, F1–F12)
- Pencereye sığacak şekilde ölçekleme

## Sıradakiler

- Tight ve ZRLE kodlamaları (yavaş ağlarda büyük hız farkı)
- Pano (clipboard) paylaşımı
- Kayıtlı bağlantılar, tam ekran, Ctrl+Alt+Del gönderme

## Hata ayıklama

`cargo run --example probe -- host:port [şifre] [cikti.ppm]` arayüz açmadan bağlanır ve ilk kareyi dosyaya kaydeder.
