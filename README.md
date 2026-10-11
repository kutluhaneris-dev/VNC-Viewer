# VNC Viewer

Rust + [egui](https://github.com/emilk/egui) ile yazılmış, TightVNC sunucularıyla uyumlu hafif bir VNC istemcisi.

## İndirme (Windows)

En son sürüm her zaman [Releases](https://github.com/kutluhaneris-dev/VNC-Viewer/releases/latest) sayfasındadır:
**Assets** altındaki `VNC-Viewer.exe` dosyasını indirip çift tıklayın, kurulum gerekmez.
`main`'e her birleştirmede yeni sürüm otomatik oluşturulur.

## Kaynaktan çalıştırma

```sh
cargo run --release                     # bağlantı ekranı açılır
cargo run --release -- 192.168.1.10     # şifresiz sunucuya doğrudan bağlanır (port varsayılan 5900)
```

Windows için tek bir `.exe` üretir: `cargo build --release` → `target/release/vnc-viewer.exe`.

Hesap, lisans, güncelleme bildirimi veya telemetri yoktur. Kayıtlı bağlantılar yalnızca bu bilgisayarda
(Windows: `%APPDATA%\vncviewer\data\app.ron`, Linux: `~/.local/share/vncviewer/app.ron`) saklanır.
"Şifreyi hatırla" seçilirse şifre bu dosyada düz metin olarak durur.

## Şu an desteklenenler

- Kayıtlı bağlantılar (kart görünümü, arama, düzenle/sil, son bağlantı zamanı)
- Adres çubuğu: `pc`, `pc:5901`, `pc::5901` (TightVNC biçimi) ve IPv6
- Sunucu şifre isterse soran pencere, isteğe bağlı "şifreyi hatırla"
- Oturum araç çubuğu: tam ekran (Ctrl+Alt+Enter), pencereye sığdır / gerçek boyut, Ctrl+Alt+Del, sadece izle
- RFB 3.3 / 3.7 / 3.8 el sıkışması
- Güvenlik: None ve VNC Authentication (TightVNC'nin varsayılan şifresi)
- Kodlamalar: Tight (zlib + JPEG, TightVNC'nin hızlı kodlaması), CopyRect, Raw, DesktopSize (çözünürlük değişimi)
- Fare (sol/orta/sağ tık, tekerlek), klavye (harfler, Ctrl/Alt/Shift, ok tuşları, F1–F12)

## Sıradakiler

- ZRLE kodlaması (RealVNC / TigerVNC sunucuları için)
- Görüntü kalitesi ayarı (JPEG kalitesi şu an 7/9 sabit)
- Pano (clipboard) paylaşımı
- Kayıtlı şifreleri işletim sisteminin anahtar deposunda saklamak

## Hata ayıklama

`cargo run --example probe -- host:port [şifre] [cikti.ppm]` arayüz açmadan bağlanır ve ilk kareyi dosyaya kaydeder.
