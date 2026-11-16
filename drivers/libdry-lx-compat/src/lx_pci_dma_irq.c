/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#include <linux/dma-mapping.h>
#include <linux/interrupt.h>
#include <linux/pci.h>

#define DRY_PCI_BAR_FLAG_MMIO (1U << 0)
#define DRY_PCI_BAR_FLAG_PREFETCH (1U << 2)
#define DRY_PCI_BAR_FLAG_IO (1U << 4)
#define DRY_PCI_DEV_FLAG_MSI (1U << 1)
#define DRY_PCI_DEV_FLAG_MSIX (1U << 2)

#define DMA_COHERENT_BYTES (64U * 1024U)
#define DMA_SKB_SLOT_BYTES 2048U
#define DMA_SKB_SLOT_COUNT 128U
#define DMA_SKB_POOL_BYTES (DMA_SKB_SLOT_BYTES * DMA_SKB_SLOT_COUNT)
#define DMA_BOUNCE_OFFSET (DMA_COHERENT_BYTES + DMA_SKB_POOL_BYTES)
#define DMA_BOUNCE_SLOTS 32U
#define DMA_BOUNCE_BYTES (DMA_BOUNCE_SLOTS * DMA_SKB_SLOT_BYTES)
#define DMA_ARENA_MIN_BYTES (DMA_BOUNCE_OFFSET + DMA_BOUNCE_BYTES)

struct dry_irq_slot {
    bool active;
    bool masked;
    bool msi_bound;
    int irq;
    irq_handler_t handler;
    void *dev_id;
    u64 handled_count;
};

static struct pci_dev *g_active_pdev = NULL;
static void *g_mapped_bar_va[6];
static uint32_t g_mapped_bar_mask = 0U;

static uint64_t g_dma_va = 0ULL;
static uint64_t g_dma_pa = 0ULL;
static uint64_t g_dma_bytes = 0ULL;
static size_t g_coherent_used = 0U;
static uint8_t g_skb_dma_used[DMA_SKB_SLOT_COUNT];
static size_t g_bounce_cursor = 0U;

static struct dry_irq_slot g_irq_slot;

extern void *dry_lx_rust_ioremap_bar(uint32_t bar_idx, uint32_t cache_mode, uint64_t size_bytes);

__attribute__((weak)) int32_t dry_lx_rust_dma_arena_init(
    uint64_t *va_out, uint64_t *pa_out, uint64_t *bytes_out) {
    (void)va_out;
    (void)pa_out;
    (void)bytes_out;
    return -ENOSYS;
}

__attribute__((weak)) int32_t dry_lx_rust_irq_bind(uint32_t *vec_out) {
    (void)vec_out;
    return -ENOSYS;
}

__attribute__((weak)) int32_t dry_lx_rust_irq_ack(uint32_t action) {
    (void)action;
    return -ENOSYS;
}

static void format_bdf(char *dst, u16 bdf) {
    static const char hex[] = "0123456789abcdef";
    u8 bus = (u8)(bdf >> 8);
    u8 dev = (u8)((bdf >> 3) & 0x1fU);
    u8 fn = (u8)(bdf & 0x07U);
    dst[0] = '0'; dst[1] = '0'; dst[2] = '0'; dst[3] = '0'; dst[4] = ':';
    dst[5] = hex[(bus >> 4) & 0xfU]; dst[6] = hex[bus & 0xfU]; dst[7] = ':';
    dst[8] = hex[(dev >> 4) & 0xfU]; dst[9] = hex[dev & 0xfU]; dst[10] = '.';
    dst[11] = hex[fn & 0x7U]; dst[12] = '\0';
}

static int ensure_dma_arena(void) {
    if (g_dma_va != 0ULL && g_dma_bytes >= DMA_ARENA_MIN_BYTES) {
        return 0;
    }
    uint64_t va = 0ULL, pa = 0ULL, bytes = 0ULL;
    if (dry_lx_rust_dma_arena_init(&va, &pa, &bytes) != 0 || va == 0ULL || pa == 0ULL ||
        bytes < DMA_ARENA_MIN_BYTES) {
        return -ENOMEM;
    }
    g_dma_va = va;
    g_dma_pa = pa;
    g_dma_bytes = bytes;
    g_coherent_used = 0U;
    dry_lx_memset(g_skb_dma_used, 0, sizeof(g_skb_dma_used));
    g_bounce_cursor = 0U;
    return 0;
}

void dry_lx_pci_init_device(struct pci_dev *pdev, u16 vendor_id, u16 device_id,
                            u16 subvendor, u16 subdevice, u16 bdf, u8 revision,
                            u16 dev_flags, const struct dry_lx_pci_bar_init *bars) {
    dry_lx_memset(pdev, 0, sizeof(*pdev));
    dry_lx_memset(g_mapped_bar_va, 0, sizeof(g_mapped_bar_va));
    dry_lx_memset(&g_irq_slot, 0, sizeof(g_irq_slot));
    g_mapped_bar_mask = 0U;
    g_dma_va = 0ULL;
    g_dma_pa = 0ULL;
    g_dma_bytes = 0ULL;
    g_coherent_used = 0U;
    dry_lx_memset(g_skb_dma_used, 0, sizeof(g_skb_dma_used));
    g_bounce_cursor = 0U;

    pdev->vendor = vendor_id;
    pdev->device = device_id;
    pdev->subsystem_vendor = subvendor;
    pdev->subsystem_device = subdevice;
    pdev->revision = revision;
    pdev->dev_flags = dev_flags;
    pdev->dev.bdf = bdf;
    pdev->dev.dma_mask = DMA_BIT_MASK(64);
    pdev->dev.coherent_dma_mask = DMA_BIT_MASK(64);
    pdev->dev.rpm_state = RPM_ACTIVE;
    pdev->current_state = PCI_D0;
    pdev->irq = (dev_flags & (DRY_PCI_DEV_FLAG_MSI | DRY_PCI_DEV_FLAG_MSIX)) ? 0x40 : 11;
    format_bdf(pdev->slot_name, bdf);

    for (u64 b = 0; b < 6 && bars != NULL; b++) {
        u64 sz = bars[b].size_bytes;
        u32 fl = bars[b].flags;
        if (sz == 0ULL) {
            continue;
        }
        if ((fl & DRY_PCI_BAR_FLAG_MMIO) != 0U) {
            pdev->resource[b].flags = IORESOURCE_MEM |
                ((fl & DRY_PCI_BAR_FLAG_PREFETCH) ? IORESOURCE_PREFETCH : 0UL);
            pdev->resource[b].page_suboff = bars[b].page_suboff;
            pdev->resource[b].start = (b << 56) | 0x01000000ULL | (u64)bars[b].page_suboff;
            pdev->resource[b].end = pdev->resource[b].start + sz - 1ULL;
        } else if ((fl & DRY_PCI_BAR_FLAG_IO) != 0U) {
            pdev->resource[b].flags = IORESOURCE_IO;
            pdev->resource[b].start = 0x1000ULL + b * 0x100ULL;
            pdev->resource[b].end = pdev->resource[b].start + sz - 1ULL;
        }
    }
    g_active_pdev = pdev;
}

const struct pci_device_id *dry_lx_match_pci_id_table(
    const struct pci_device_id *tbl, const struct pci_dev *pdev) {
    for (; tbl && tbl->vendor != 0U; tbl++) {
        if (tbl->vendor != pdev->vendor && tbl->vendor != PCI_ANY_ID) continue;
        if (tbl->device != pdev->device && tbl->device != PCI_ANY_ID) continue;
        if (tbl->subvendor != pdev->subsystem_vendor && tbl->subvendor != PCI_ANY_ID) continue;
        if (tbl->subdevice != pdev->subsystem_device && tbl->subdevice != PCI_ANY_ID) continue;
        return tbl;
    }
    return NULL;
}

uint32_t dry_lx_pci_mapped_bar_mask(void) {
    return g_mapped_bar_mask;
}

int pci_enable_device(struct pci_dev *pdev) {
    if (!pdev) return -ENODEV;
    pdev->enabled = true;
    return 0;
}

int pcim_enable_device(struct pci_dev *pdev) {
    return pci_enable_device(pdev);
}

void pci_disable_device(struct pci_dev *pdev) {
    if (pdev) {
        pdev->enabled = false;
        pdev->bus_master = false;
    }
}

void pci_set_master(struct pci_dev *pdev) {
    if (pdev) {
        pdev->bus_master = true;
    }
}

int pci_set_mwi(struct pci_dev *pdev) {
    if (!pdev) return -ENODEV;
    pdev->mwi = true;
    return 0;
}

void pci_clear_mwi(struct pci_dev *pdev) {
    if (pdev) {
        pdev->mwi = false;
    }
}

int pci_request_regions(struct pci_dev *pdev, const char *res_name) {
    (void)res_name;
    return pdev ? 0 : -ENODEV;
}

void pci_release_regions(struct pci_dev *pdev) {
    (void)pdev;
}

int pci_read_config_word(const struct pci_dev *pdev, int where, u16 *val) {
    if (!pdev || !val) return -EINVAL;
    (void)where;
    *val = 0x0010U;
    return 0;
}

int pci_write_config_word(const struct pci_dev *pdev, int where, u16 val) {
    (void)pdev; (void)where; (void)val;
    return 0;
}

int pci_enable_wake(struct pci_dev *pdev, int state, bool enable) {
    (void)state;
    if (pdev) {
        pdev->wake_enabled = enable;
    }
    return 0;
}

int pci_set_power_state(struct pci_dev *pdev, int state) {
    if (!pdev) return -ENODEV;
    pdev->current_state = state;
    return 0;
}

void *devm_request_mem_region(struct device *dev, unsigned long start, unsigned long n, const char *name) {
    (void)dev; (void)n; (void)name;
    return start ? (void *)(uintptr_t)start : NULL;
}

void *devm_request_region(struct device *dev, unsigned long start, unsigned long n, const char *name) {
    (void)dev; (void)n; (void)name;
    return start ? (void *)(uintptr_t)start : NULL;
}

static void __iomem *ioremap_bar_token(unsigned long offset, unsigned long size, uint32_t cache_mode) {
    uint32_t bar_idx = (uint32_t)((uint64_t)offset >> 56);
    if (bar_idx >= 6U || size == 0UL || !g_active_pdev) {
        return NULL;
    }
    if ((g_active_pdev->resource[bar_idx].flags & IORESOURCE_MEM) == 0UL) {
        return NULL;
    }
    if (g_mapped_bar_va[bar_idx] != NULL) {
        return (void __iomem *)g_mapped_bar_va[bar_idx];
    }
    void *page_va = dry_lx_rust_ioremap_bar(bar_idx, cache_mode, (uint64_t)size);
    if (!page_va) {
        return NULL;
    }
    u16 suboff = g_active_pdev->resource[bar_idx].page_suboff;
    void *bar_va = (uint8_t *)page_va + suboff;
    g_mapped_bar_va[bar_idx] = bar_va;
    g_mapped_bar_mask |= (1U << bar_idx);
    return (void __iomem *)bar_va;
}

void __iomem *pci_iomap(struct pci_dev *pdev, int bar, unsigned long maxlen) {
    if (!pdev || bar < 0 || bar >= 6) return NULL;
    unsigned long len = pci_resource_len(pdev, bar);
    if (maxlen != 0UL && maxlen < len) {
        len = maxlen;
    }
    uint32_t wc = (pdev->resource[bar].flags & IORESOURCE_PREFETCH) ? 1U : 0U;
    return ioremap_bar_token(pci_resource_start(pdev, bar), len, wc);
}

void __iomem *ioremap(resource_size_t offset, unsigned long size) {
    return ioremap_bar_token((unsigned long)offset, size, 0U);
}

void iounmap(volatile void __iomem *addr) {
    (void)addr;
}

void __iomem *devm_ioremap(struct device *dev, unsigned long offset, unsigned long size) {
    (void)dev;
    return ioremap_bar_token(offset, size, 0U);
}

void __iomem *devm_ioremap_wc(struct device *dev, unsigned long offset, unsigned long size) {
    (void)dev;
    return ioremap_bar_token(offset, size, 1U);
}

void memcpy_fromio(void *to, const volatile void __iomem *from, size_t count) {
    uint8_t *d = (uint8_t *)to;
    const volatile uint8_t *s = (const volatile uint8_t *)from;
    for (size_t i = 0; i < count; i++) {
        d[i] = s[i];
    }
}

int dma_set_mask_and_coherent(struct device *dev, u64 mask) {
    if (!dev || mask < DMA_BIT_MASK(32)) {
        return -EIO;
    }
    dev->dma_mask = mask;
    dev->coherent_dma_mask = mask;
    return 0;
}

void *dma_alloc_coherent(struct device *dev, size_t size, dma_addr_t *dma_handle, gfp_t flag) {
    (void)dev; (void)flag;
    if (ensure_dma_arena() != 0 || size == 0U) {
        return NULL;
    }
    size_t aligned = ALIGN(size, 64U);
    if (g_coherent_used + aligned > DMA_COHERENT_BYTES) {
        return NULL;
    }
    size_t off = g_coherent_used;
    g_coherent_used += aligned;
    void *va = (void *)(uintptr_t)(g_dma_va + off);
    dry_lx_memset(va, 0, aligned);
    if (dma_handle) {
        *dma_handle = g_dma_pa + (uint64_t)off;
    }
    return va;
}

void dma_free_coherent(struct device *dev, size_t size, void *cpu_addr, dma_addr_t dma_handle) {
    (void)dev; (void)size; (void)cpu_addr; (void)dma_handle;
}

void *dry_lx_dma_alloc_skbuf(size_t size, dma_addr_t *dma_out) {
    if (ensure_dma_arena() != 0 || size > DMA_SKB_SLOT_BYTES) {
        return NULL;
    }
    for (size_t i = 0; i < DMA_SKB_SLOT_COUNT; i++) {
        if (g_skb_dma_used[i] == 0U) {
            g_skb_dma_used[i] = 1U;
            size_t off = DMA_COHERENT_BYTES + i * DMA_SKB_SLOT_BYTES;
            void *va = (void *)(uintptr_t)(g_dma_va + off);
            if (dma_out) {
                *dma_out = g_dma_pa + (uint64_t)off;
            }
            return va;
        }
    }
    return NULL;
}

void dry_lx_dma_free_skbuf(void *ptr) {
    if (!ptr || g_dma_va == 0ULL) return;
    uintptr_t p = (uintptr_t)ptr;
    uintptr_t pool_start = (uintptr_t)(g_dma_va + DMA_COHERENT_BYTES);
    uintptr_t pool_end = pool_start + DMA_SKB_POOL_BYTES;
    if (p < pool_start || p >= pool_end) return;
    size_t idx = (size_t)(p - pool_start) / DMA_SKB_SLOT_BYTES;
    if (idx < DMA_SKB_SLOT_COUNT) {
        g_skb_dma_used[idx] = 0U;
    }
}

dma_addr_t dma_map_single(struct device *dev, void *ptr, size_t size, enum dma_data_direction dir) {
    (void)dev;
    if (!ptr || size == 0U || ensure_dma_arena() != 0) {
        return DMA_MAPPING_ERROR;
    }
    uintptr_t p = (uintptr_t)ptr;
    if (p >= (uintptr_t)g_dma_va && p + size <= (uintptr_t)(g_dma_va + g_dma_bytes)) {
        return g_dma_pa + (uint64_t)(p - (uintptr_t)g_dma_va);
    }
    if (dir != DMA_TO_DEVICE || size > DMA_SKB_SLOT_BYTES) {
        return DMA_MAPPING_ERROR;
    }
    size_t slot = g_bounce_cursor % DMA_BOUNCE_SLOTS;
    g_bounce_cursor++;
    size_t off = DMA_BOUNCE_OFFSET + slot * DMA_SKB_SLOT_BYTES;
    dry_lx_memcpy((void *)(uintptr_t)(g_dma_va + off), ptr, size);
    return g_dma_pa + (uint64_t)off;
}

void dma_unmap_single(struct device *dev, dma_addr_t addr, size_t size, enum dma_data_direction dir) {
    (void)dev; (void)addr; (void)size; (void)dir;
}

int dma_mapping_error(struct device *dev, dma_addr_t dma_addr) {
    (void)dev;
    return dma_addr == DMA_MAPPING_ERROR;
}

int request_irq(unsigned int irq, irq_handler_t handler, unsigned long flags,
                const char *name, void *dev) {
    (void)flags; (void)name;
    if (!handler) {
        return -EINVAL;
    }
    bool msi_bound = false;
    if (g_active_pdev && (g_active_pdev->dev_flags & (DRY_PCI_DEV_FLAG_MSI | DRY_PCI_DEV_FLAG_MSIX))) {
        uint32_t vec = 0U;
        if (dry_lx_rust_irq_bind(&vec) == 0) {
            msi_bound = true;
            irq = vec;
            g_active_pdev->irq = (int)vec;
        }
    }
    g_irq_slot.active = true;
    g_irq_slot.masked = false;
    g_irq_slot.msi_bound = msi_bound;
    g_irq_slot.irq = (int)irq;
    g_irq_slot.handler = handler;
    g_irq_slot.dev_id = dev;
    return 0;
}

void free_irq(unsigned int irq, void *dev_id) {
    if (g_irq_slot.active && g_irq_slot.dev_id == dev_id && g_irq_slot.irq == (int)irq) {
        if (g_irq_slot.msi_bound) {
            (void)dry_lx_rust_irq_ack(2U);
        }
        g_irq_slot.active = false;
        g_irq_slot.handler = NULL;
        g_irq_slot.dev_id = NULL;
    }
}

void disable_irq(unsigned int irq) {
    if (g_irq_slot.active && g_irq_slot.irq == (int)irq) {
        g_irq_slot.masked = true;
    }
}

void enable_irq(unsigned int irq) {
    if (g_irq_slot.active && g_irq_slot.irq == (int)irq) {
        g_irq_slot.masked = false;
    }
}

int dry_lx_irq_poll(void) {
    if (!g_irq_slot.active || g_irq_slot.masked || !g_irq_slot.handler) {
        return IRQ_NONE;
    }
    irqreturn_t ret = g_irq_slot.handler(g_irq_slot.irq, g_irq_slot.dev_id);
    if (ret == IRQ_HANDLED) {
        g_irq_slot.handled_count++;
        if (g_irq_slot.msi_bound) {
            (void)dry_lx_rust_irq_ack(0U);
        }
    }
    return ret;
}

u64 dry_lx_irq_handled_count(void) {
    return g_irq_slot.handled_count;
}

bool dry_lx_irq_is_msi(void) {
    return g_irq_slot.msi_bound;
}
