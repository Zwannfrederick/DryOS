# DRY Desktop, DRY Shell, Açılış Deneyimi ve Paket Mağazası Spesifikasyonu

> **Belge Tarihi:** 11 Kasım 2026  
> **Durum:** **`PLANNED` (`P2` – `P4` Tasarım Spesifikasyonu)**  
> **Mevcut Uygulama Tabanı (`DONE`):** `Milestone 1C` (`1df3842`) itibarıyla yalnızca yetenek-kapılı Ring 3 UEFI GOP çerçeve tamponu eşlemesi (`SYS_FB_MAP`) ve statik doğrulama ekranı (`userspace/src/bin/dry_display.rs`, `1280x800x32`) tamamlanmıştır. Bu belgedeki pencere yöneticisi, compositor, `DRY Shell`, `Clean/Diagnostic Boot` ve `DRY Store` bileşenleri henüz kodlanmamıştır (`PLANNED`).  
> **İlgili Kararlar:** `ADR-007`, `ADR-008`, `ADR-009`, `ADR-010`, `ADR-011`.

---

## 1. Mimari Sınır ve Tasarım Felsefesi (`ADR-008`, `ADR-009`)

1. **Kesin Ring 3 Sorumluluk Sınırı:** Pencere yönetimi, yüzey birleştirme (*compositing*), yazıtipi işleme, animasyon, bulanıklık (*blur*), tema ve menü mantığının tamamı **Ring 3 kullanıcı alanı süreçlerinde** çalışır. `dry-kernel` içinde grafik politikası veya görsel efekt kodu bulunmaz; çekirdek yalnızca `Endpoint`/`Notification`/`IoRing` IPC, paylaşımlı bellek (`Frame`/`DmaArena`), IRQ bildirimi ve yetenek-kapılı ekran/GPU erişimi sağlar.
2. **Özgün Birleşik Tasarım Dili:** `DRY Desktop`, üç farklı geleneğin en güçlü yönlerini doğrudan kopyalamadan DRY OS'ye özgü tutarlı bir dilde birleştirir:
   - **Hyprland mirası:** Dinamik döşeme (*tiling*), akıcı çoklu çalışma alanı (*workspace*) geçişleri ve eksiksiz klavye kontrolü.
   - **KDE Plasma mirası:** İşlevsel derinlik, esnek özelleştirme, pencere kuralları ve rahat geleneksel pencere kullanımı.
   - **macOS / GNOME mirası:** Sade görsel hiyerarşi, tutarlı sistem/uygulama menüleri, global arama ve yüksek keşfedilebilirlik.
3. **İşlevsel Minimalizm:** Özellik kırpılmaz; aynı işlev daha düşük bellek ayak izi, sıfır gereksiz arka plan servisi ve donanım kapasitesine uyum sağlayan tek bir compositor ile sunulur.

---

## 2. Tek Compositor Üzerinde Hibrit Pencere Yöneticisi (`PLANNED — P2`)

Ayrı "floating oturumu" ve ayrı "tiling oturumu" kurulmaz; tek `dry-compositor` süreci çalışma alanı (*workspace*) bazında tüm yerleşim kiplerini yönetir:

| Özellik / Kural | Tasarım Sözleşmesi | Durum |
| :--- | :--- | :---: |
| **Varsayılan Kip (`Floating`)** | İlk kurulumda pencereler serbest taşınabilir, yeniden boyutlandırılabilir ve kenar/köşe tutturma (*snap*) desteklidir. | `PLANNED` |
| **Tek Tık / Kısayol `Tiling`** | Üst bardaki kip anahtarı veya klavye kısayoluyla aktif çalışma alanı anında dinamik döşeme ağacına geçer. | `PLANNED` |
| **`Stacking` ve `Tabbed` Gruplar** | Hem `Floating` hem `Tiling` düğümleri içinde pencereler sekme (*tabbed*) veya yığın (*stack*) grubu olarak birleştirilebilir. | `PLANNED` |
| **Transient / Diyalog Kuralı** | Dosya seçici, onay kutusu, *popup*, bildirim ve küçük yardımcı (*utility/transient*) pencereler her zaman `Floating` katmanında açılır; **alttaki `Tiling` ağacını yeniden hesaplatmaz ve sütun düzenini bozmaz.** | `PLANNED` |
| **Geometri Korunumu** | Her pencere son `last_floating_rect` ve son `tile_anchor` bilgisini taşır; `Floating <-> Tiling` geçişlerinde göreli sıra ve önceki serbest boyut/konum mümkün olduğunca korunur. | `PLANNED` |
| **Çalışma Alanları & Kurallar** | Monitör başına bağımsız çoklu çalışma alanı ve uygulama kimliği/`SecLabel` kompartmanına göre bildirimsel pencere kuralları (ör. belirli aracı her zaman `Workspace 3` veya `Floating` aç). | `PLANNED` |

---

## 3. Görsel Tasarım, Yoğunluk Kipleri ve Donanım Uyarlamalı *Fallback* (`PLANNED — P2`)

- **Üst Sistem ve Erişilebilirlik Barı (`DRY Top Bar`):**
  - Ekranın üst kenarında `liquid-glass` esinli, yüksek okunabilirlikli sistem çubuğu yer alır.
  - İçerik: Çalışma alanı göstergesi, **aktif uygulama menüleri** (*Global Menu*), merkezde veya kısayolla açılan **Global Arama / Komut Başlatıcı**, sağda ağ/ses/güç/güvenlik kiralaması (`DRY Vault` aktif `DEV_WORKSPACE` rozeti) ve erişilebilirlik kontrolleri.
- **Tema ve UI Yoğunluğu:**
  - **Koyu (`Dark`)** ve **Açık (`Light`)** renk temaları (tüm metin ve ikincil etiketlerde WCAG AA kontrast zorunlu).
  - **Normal UI** (rahat dokunma/fare hedefleri, standart boşluklar) ve **Compact UI** (dikey ekran alanını maksimize eden daraltılmış üst bar, sekme ve kenar boşlukları; dizüstü ve geliştirici iş akışları için tek tıkla seçilebilir).
- **Donanım-Uyarlamalı Efekt Katmanı (*Hardware Tiering*):**
  - *Tam Katman (Donanım GPU hızlandırmalı):* Ölçülü arka plan bulanıklığı (*dual-kawase / downsample blur*), yumuşak pencere gölgeleri ve `200–300 ms` GPU tabanlı (`transform`/`opacity`) akıcı geçişler.
  - *Düşük Donanım / GOP Fallback Katmanı:* Gerçek zamanlı bulanıklık ve çok geçişli gölgeler otomatik devre dışı bırakılır; opak/yüksek kontrastlı yüzeyler ve hasar-dikdörtgeni (*dirty-rect*) kopyasıyla CPU/bellek bant genişliği korunur.
  - *Erişilebilirlik:* Kullanıcı ayarıyla **Hareket Azaltma (*Reduced Motion*)** veya **Tüm Animasyonları Kapatma** (`0 ms` anlık geçiş) her donanım katmanında desteklenir.

---

## 4. Fare + Klavye Tamamlayıcılığı ve Ortak `Action API` (`PLANNED — P2`)

DRY Desktop'ta fare ve klavye birbirinin alternatifi değil, aynı komut modelinin iki yüzüdür:

```text
[Üst Uygulama Menüsü]   [Sağ Tık Bağlam Menüsü]   [Klavye Kısayolu]   [Command Palette / Arama]
          \                       |                       /                       /
           +----------------------+----------------------+-----------------------+
                                  v
              Ortak Tipli Action API (`ActionId` + Parametre + Yetki Kapsamı)
                                  v
                    [DRY Shell / Compositor / Uygulama Komut İşleyicisi]
```

- **Keşfedilebilirlik -> Hız Geçişi:** Yeni bir kullanıcı her eylemi üst menüde veya sağ tık bağlam menüsünde yanında atanmış kısayol etiketiyle görür; deneyimli kullanıcı aynı `ActionId` eylemini doğrudan kısayolla veya global komut paletinden çağırır. Menü ile kısayol arasında davranış farkı veya ayrı kod yolu oluşmaz.

---

## 5. İki Açılış Deneyimi ve Kimlik Doğrulama Güvenlik Ayrımı (`ADR-010`, `PLANNED — P2`)

### 5.1. Tek Olay Akışı, İki Görünüm (`Clean Boot` & `Diagnostic Boot`)
`dry-kernel` erken önyükleme halkası ve `dry-init` kök süpervizörü tek bir yapılandırılmış `BootEvent` akışı üretir; `dry-boot-splash` süreci bu akışı iki görünüm kipinden biriyle ekrana çizer:
1. **`Clean Boot` (Varsayılan):** Koyu, sade arka plan; merkezde DRY logosu ve ince etkinlik göstergesi; teknik metin kalabalığı olmadan oturum servisleri hazır olduğunda pürüzsüz biçimde `DRY Login` ekranına geçer.
2. **`Diagnostic Boot`:** Merkezde sade ve yüksek okunabilirlikli canlı günlük paneli; çekirdek, SMP, sürücüler, `ADR-007` güvenlik doğrulamaları ve servis durumları Arch Linux açıklığında akar; kritik bir hata oluştuğunda doğrudan kök neden ve kurtarma kabuğu seçeneklerini gösterir.
- **Canlı Geçiş ve Dürüstlük Kuralları:** Önyükleme sırasında `Esc` tuşuyla iki görünüm arasında anında geçiş yapılabilir (varsayılan kip Limine/sistem yapılandırmasından da seçilebilir). **Gerçek olmayan, zamanlayıcıyla uydurulmuş yüzde (`%47...`) çubukları kesinlikle gösterilmez;** yalnızca gerçek birim/bağımlılık tamamlanma adımları yansıtılır.

### 5.2. `Login / Authentication Manager` ile `Password Manager` Ayrımı
- **`dry-login` (Authentication Manager — `SecDomain::RootSystem`):** Yalnızca kullanıcı kimlik doğrulamasını yapar, `DRY Vault` üzerinden kullanıcı oturumu yetkilerini üretir ve kullanıcı çalışma alanını başlatır.
- **`dry-passvault` (User Password Manager — `SecDomain::Userspace`):** Kullanıcının web siteleri, SSH/API anahtarları ve uygulama parolalarını tutan ayrı, şifreli kullanıcı alanı uygulamasıdır. Oturum açma ekranının güvenilir bilgi işlem tabanı (*TCB*) kullanıcı parola kasası kodunu veya ağ ayrıştırıcılarını içermez.

---

## 6. `DRY Store` ve `drypkg` Kullanıcı Deneyimi İlkeleri (`ADR-011`, `PLANNED — P4`)

- **Temel Akış:** **Uygulamayı bul -> Kur -> Çalıştır.** `DRY Store` (GUI) ve `drypkg` (CLI) aynı paket çözümleme, imza doğrulama ve atomik nesil motorunu paylaşır.
- **Otomatik fakat Disiplinli Runtime Yönetimi:** Bir Linux ELF veya (ileride) seçilmiş Win32 uygulaması kurulurken gereken uyumluluk katmanı otomatik çözümlenir ve kullanıcıya açıkça gösterilir; kullanıcı bu türde bir paket kurmadığı sürece sistemde hiçbir yabancı runtime yüklü durmaz.
- **Capability ve Uyumluluk Şeffaflığı:** Kurulum kartında uygulamanın istediği `ADR-007` yetkileri (ağ, depolama dizini, mikrofon/kamera, GPU) ve uyumluluk rozeti (**`Native DRY`** / **`Linux ABI`** / **`Win32 Seçilmiş Uyumluluk`** + bilinen sınırlamalar) açıkça listelenir; tüm Linux/Windows yazılımlarının çalışacağı yönünde yanıltıcı ifade kullanılmaz.
