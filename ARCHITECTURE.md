# DRY OS & DRY Kernel — Sistem Mimarisi

> **Durum Tarihi:** 10 Kasım 2026  
> **Aşama:** Geliştirme Öncesi Mimari Dondurma (Pre-Implementation Freeze)  
> **Epistemik Etiketler:** Bu belgedeki her kritik iddia aşağıdakilerden biriyle işaretlenmiştir:  
> - `[DOĞRULANDI]` — Birincil kaynak, ikili (binary) sembol analizi veya yerel sistem denetimiyle kanıtlandı.  
> - `[HİPOTEZ]` — Teknik olarak tutarlı ancak Faz 0 deneyleriyle yanlışlanmaya açık tasarım iddiası.  
> - `[TAHMİN]` — Mühendislik ölçek/satır sayısı öngörüsü.  
> - `[TEST EDİLMEDİ]` — Henüz yürütülebilir model, Loom veya donanım üzerinde sınanmadı.

---

## 1. Tasarım Felsefesi ve Ayrıcalık Sınırları

**DRY (Minimal Mechanisms, Maximum Useful Parallelism)**; çekirdek içinde sürücü, dosya sistemi, ağ protokolü, POSIX veya Win32 kavramı barındırmayan, yetenek (*capability*) tabanlı `no_std` Rust mikroçekirdeğidir.

- **Sıfır Yönetim Çekirdeği:** Tüm mantıksal CPU'lar simetrik işçi (*peer worker*) olarak çalışır; işletim sistemi koordinasyonu için CPU ayrılmaz.
- **Global Kilit Yasağı:** Sıcak sistem çağrısı, IPC, bildirim (*notification*) ve zamanlayıcı uyandırma yollarında *Big Kernel Lock (BKL)* bulunmaz `[HİPOTEZ — Faz 0/1'de ölçülecek]`.
- **Çift Düzlemli İletişim:**
  - **Kontrol Düzlemi:** Yetki devri, hata (*fault*) bildirimi ve yaşam döngüsü için senkron `Endpoint` IPC + Öncelik Bağışı (*Priority Donation*).
  - **Veri Düzlemi:** `sDDF` / `io_uring` tarzı paylaşımlı bellek kilitsiz halkalar (`IoRing`) + yalnızca karşı taraf uyuyorsa çekirdeğe giren `Notification` kapı zili (*doorbell*) `[DOĞRULANDI: LionsOS/sDDF literatür kanıtı; DRY uyarlaması TEST EDİLMEDİ]`.
- **Hibrit Sürücü Stratejisi:** Önyükleme ve çekirdek depolama/sanallaştırma yolları (`VirtIO`, `NVMe`, `GOP`) native Rust ile yazılır; geniş PCIe çevre birimi ekosistemi (Ethernet, USB xHCI, seçilmiş Wi-Fi) Linux kaynak kodundan izole `GPL-2.0-only` userspace *Driver Island* süreçlerine uyarlanır (`ADR-005`, `ADR-006`).

```
+-----------------------------------------------------------------------------------+
|                               USERSPACE (Ring 3)                                  |
|                                                                                   |
|  [Native DRY Apps]      [Linux ELF Personality]       [Future Win32/NT Subsys]    |
|         |                         |                              |                |
|         +------------+------------+------------------------------+                |
|                      | Kontrol: Endpoint IPC | Veri: IoRing + Notification        |
|  +-------------------v---------------------------------------------------------+  |
|  | Native Sistem Servisleri (Rust, MIT OR GPL-2.0-only, Ayrı VSpace)           |  |
|  | [Root/Cap Mgr] [Pager/VSpace Mgr] [PCI/IOMMU Bus Mgr] [VFS] [Net Mux/TCP]   |  |
|  +---------------+-------------------------------------+-----------------------+  |
|                  |                                     | CPU-Only IoRing          |
|  +---------------v---------------+     +---------------v-----------------------+  |
|  | Native Rust Sürücüler         |     | Linux Driver Islands (GPL-2.0-only)   |  |
|  | VirtIO-Blk/Net, NVMe, GOP     |     | Sürücü C Kodu + libdry_lx_<sınıf>     |  |
|  +---------------+---------------+     | Kalıcı IOVA DmaArena + Gölge vmemmap  |  |
+------------------|---------------------+-------------------+-------------------+--+
                   | Syscall ABI (ADR-002)                   | Strict IOMMU (VT-d)
+------------------v-----------------------------------------v----------------------+
|                         DRY KERNEL (Ring 0, Rust no_std)                          |
|  Untyped Retype | CSpace | VSpace (MMU) | IOMMU Bind | Endpoint/Notif | Scheduler |
+-----------------------------------------------------------------------------------+
```

### Ring 0 vs. Ring 3 Sorumluluk Çizgisi

| Mekanizma | Konum | Gerekçe |
| :--- | :--- | :--- |
| `CR3` / MMU sayfa tablosu yazımı ve hedefli TLB shootdown | **Ring 0** | Donanım bellek izolasyonu kökü. |
| IOMMU kök/domain tabloları ve IOTLB invalidation kuyruğu | **Ring 0** | Userspace DMA sürücülerinin çekirdeği ezmesini önleyen tek donanım engeli `[DOĞRULANDI]`. |
| CPU bağlam değişimi (`RSP`, FPU/SIMD), APIC/MSI-X yönlendirme | **Ring 0** | Ayrıcalıklı yazmaçlar (`MSR`, `XCR0`, `LAPIC`). |
| `CSpace` yetki doğrulaması ve epoch-tabanlı `Revoke` | **Ring 0** | Sahtelenemez erişim kontrolü. |
| Bellek politikası, sayfalama (pager), PCI tarama, sürücüler, VFS, TCP/IP | **Ring 3** | Hata izolasyonu ve TCB küçültme. |

---

## 2. Çekirdek Nesne Modeli ve Denetlenmiş Native Syscall Yüzeyi

> **Not:** Önceki 11 çağrılık taslak değişmez kabul edilmemiştir (`ADR-002`). Yapılan güvenlik ve yaşam döngüsü denetimi sonucunda 3 kritik eksiklik tespit edilerek yüzey **12 temel mekanizmaya** revize edilmiştir (Faz 1 uygulama bulgularına göre evrilebilir).

### 2.1. Tespit Edilen Taslak Açıkları ve Düzeltmeler
1. **`Map` ve `Unmap` Ayrımı + IOMMU Pin Sayacı:** Bir fiziksel çerçeve veya `DmaArena` hem `VSpace`'te hem `IommuDomain`'de haritalı olabilir. `dry_vspace_unmap` ayrı bir işlem olmalı ve nesne üzerindeki atomik `iommu_pin_count > 0` iken fiziksel belleğin `Revoke`/geri dönüşümü reddedilmelidir.
2. **`TCB`-Bağlı `Notification` ve `FaultEndpoint`:** Tek thread'li olay-güdümlü bir sunucunun hem senkron `Endpoint` mesajlarını hem de asenkron `IoRing`/IRQ zillerini tek noktadan bekleyebilmesi için `TCB`'ye bir `Notification` bağlanabilmelidir (seL4 MCS modeli). Ayrıca sayfa hatası (`#PF`), istisna ve yabancı `SYSCALL` tuzaklarının userspace pager/personality sunucusuna iletilmesi için her `TCB`'nin bir `fault_ep` yetkisi bulunmalıdır.
3. **Çekirdek İçi Kullanıcı Pointer'ı Yasağı:** Çekirdek sıcak yolunda kullanıcı belleğinden pointer okunmaz (`copy_from_user` sayfa hatası riski taşımaz). Tüm sıcak argümanlar donanım yazmaçlarında, soğuk yapılandırma verileri ise göreve ait önceden sabitlenmiş (*pinned*) tek sayfalık **IPC Buffer** çerçevesi üzerinden aktarılır.

### 2.2. Başlangıç Çekirdek Çağrıları (`v0.1` Aday Arayüz)

| # | Çağrı | Amaç | Yaşam Döngüsü & Güvenlik Kuralı |
| :- | :--- | :--- | :--- |
| 1 | `dry_obj_retype` | `Untyped` bellekten çekirdek nesnesi (`TCB`, `VSpace`, `CSpace`, `Endpoint`, `Notification`, `PageTable`, `DmaArena`) türetir. | Çekirdek içi `kmalloc` yoktur; bellek hizalaması ve kalan kapasite atomik doğrulanır. |
| 2 | `dry_cap_mutate` | `Copy`, `Move`, `Mint` (hak daraltma/badge), `Delete`, `Revoke`. | `Revoke`, aktif CPU epoch bariyeri ve `iommu_pin_count == 0` koşuluna tabidir. |
| 3 | `dry_vspace_map` | `Frame` veya `DmaArena` yetkisini hedef `VSpace` sanal adresine eşler. | Yalnızca capability haklarının (`R/W/X`) alt-kümesi haritalanabilir (`W^X` varsayılan). |
| 4 | `dry_vspace_unmap` | Sanal adres aralığını çözer ve yalnızca `vspace.active_cpus` maskesine TLB shootdown gönderir. | Eşleme kalkmadan fiziksel çerçeve yeniden tiplenemez. |
| 5 | `dry_iommu_bind` | Soğuk yolda `DmaArena` yetkisini belirli PCI BDF `IommuDomain` IOVA tabanına bağlar / çözer. | `IOMMU_READ` (TX) ve `IOMMU_WRITE` (RX) ayrımını ve VT-d Snoop Control bitini zorlar. |
| 6 | `dry_ipc_call` | Kontrol düzlemi senkron çağrı (4 mesaj yazmacı + 1 yetki aktarımı + Öncelik Bağışı). | Hedef sunucuya tek kullanımlık (*single-shot*) `Reply` yetkisi üretir. |
| 7 | `dry_ipc_reply_recv` | Tek çekirdek girişinde önceki istemciye yanıt verir (`reply_cap` varsa) ve yeni mesaj/bağlı `Notification` bekler. | `reply_cap` boş geçilirse saf `Recv`, `wait_ep` boş geçilirse bloklanmayan saf `Reply` olarak çalışır. |
| 8 | `dry_notify_signal` | `Notification` kelimesine atomik `fetch_or(badge)` uygular ve bağlı görevi uyandırır. | Asla bloklanmaz; kesme bağlamından ve sürücü halkalarından güvenle çağrılır. |
| 9 | `dry_notify_wait` | `H1 v2` protokolüyle bildirim bitlerini atomik tüketir veya `deadline_tsc` anına dek uyur. | Erken uyanma veya görev silinmesinde `TCB.timer_node` yerel zamanlayıcı yığınından sökülür. |
| 10 | `dry_thread_ctrl` | `TCB` başlatma/durdurma, `FaultEndpoint` ve `Notification` bağlama, öncelik/CPU yakınlığı ve yazmaç okuma/yazma. | Yalnızca `EXEC_BLOCKED` / askıdaki veya hata durumundaki görevin yazmaçları dışarıdan değiştirilebilir. |
| 11 | `dry_irq_ctrl` | Donanım MSI-X/IOAPIC vektörünü `Notification` nesnesine bağlar; `IRQF_ONESHOT` maskesini kaldırır (`Ack`). | Vektör başına tek aktif sahip; sürücü çökerse vektör otomatik maskelenir. |
| 12 | `dry_sys_ctl` | Yalnızca kök platform yetkisine açık CPU hotplug, güç geçişi ve donanım topoloji sorgusu. | Kullanıcı süreçlerine asla devredilemez. |

---

## 3. Zamanlayıcı Mimarisi (`H1` Özeti)

- **Durum Temsili:** Her `TCB` içinde tek bir `sched: AtomicU32` kelimesi iki ortogonal alanı paketler: `exec_state` (`READY`, `RUNNING`, `SW_PREEMPT`, `SW_PARK`, `BLOCKED`, `WAKING`, `EXITING`, `DEAD`) ve `park_token` (`NONE`, `ARMED`, `NOTIFIED`) `[HİPOTEZ — Faz 0 TLC + Loom ile doğrulanacak]`.
- **Temel Güvenlik Kuralı:** Hiçbir görev `switch_to()` assembly geçişi bitip yeni görevin stack'i üzerinde `post_switch(prev)` çalışmadan `EXEC_READY` olamaz ve runqueue'ya yayınlanamaz (`ADR-004`).
- **Aşamalı Karmaşıklık:** Faz 1'de CPU başına intrüzif öncelik kuyruğu + Priority Inheritance kullanılır; topoloji-farkındalıklı iş çalma (*work stealing*) ve `NO_HZ_FULL` Faz 3/4'te ölçümle eklenir.

---

## 4. Linux Sürücü Adası ve Uyumluluk Mimarisi (`H2` Özeti)

- **İzolasyon Birimi (*Driver Island*):** Upstream C sürücüsü (`GPL-2.0-only`) + sürücü sınıfına özel daraltılmış `libdry_lx_<sınıf>` çalışma zamanı aynı userspace sürecinde derlenir; dış dünyayla yalnızca DRY `IoRing` (CPU paylaşımlı bellek) üzerinden konuşur `[HİPOTEZ]`.
- **Gölge `vmemmap` ve Kalıcı IOVA Arena:** `igc.ko` ikili analizinde `[DOĞRULANDI]` görülen `vmemmap_base` ve `page_offset_base` satır-içi pointer aritmetiğini karşılamak ve paket başına IOTLB shootdown maliyetinden kaçınmak için 64-bit userspace adres alanında sabit bir `DmaArena` ve ona doğrusal indekslenen gölge `struct page[]` dizisi ayrılır `[TEST EDİLMEDİ]`.
- **Uygulama Uyumluluğu Ayrımı:**
  - *Linux ELF Uygulamaları:* Userspace personality sunucusu + `FaultEndpoint`/`SYSCALL` yönlendirme trampolini (çekirdek ABI'sine POSIX eklenmez).
  - *Windows PE/NT (Faz 5):* Userspace `ntdll` -> `dry-ntos` sunucusu + Wine DLL + `DXVK` (çekirdek modu anti-cheat kapsam dışıdır).
  - *GPU:* Faz 2–3'te yalnızca UEFI `GOP` ve `VirtIO-GPU`; `amdgpu` (~1.107 dış sembol, ~30.5 MiB ELF `[DOĞRULANDI]`) ve `nvidia-open` (`nvidia-uvm` `HMM`/`mmu_notifier` bağımlılığı `[DOĞRULANDI]`) çeviri kapsamı dışındadır.

