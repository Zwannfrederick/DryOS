/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Minimal Freestanding Linux Kernel API Shim for `dry-lx-rng.elf` (`Milestone 2C-1`).
 * Executes unmodified Linux v6.18 `drivers/char/hw_random/virtio-rng.c` inside an isolated
 * Ring 3 Driver Island backed by capability-gated VirtIO 1.0 Modern MMIO and pinned `DEVICE_DMA`.
 */

#include <stdarg.h>
#include <linux/completion.h>
#include <linux/err.h>
#include <linux/hw_random.h>
#include <linux/kernel.h>
#include <linux/scatterlist.h>
#include <linux/slab.h>
#include <linux/virtio.h>

#define VIRTQ_MAX_SIZE      16U
#define VIRTQ_DESC_F_WRITE  2U

#define VQ_DESC_OFF         0x000U
#define VQ_AVAIL_OFF        0x400U
#define VQ_USED_OFF         0x800U

#define SLAB_SLOT_BYTES     512U
#define SLAB_SLOT_COUNT     8U

struct virtq_desc {
    u64 addr;
    u32 len;
    u16 flags;
    u16 next;
};

struct virtq_avail {
    u16 flags;
    u16 idx;
    u16 ring[VIRTQ_MAX_SIZE];
    u16 used_event;
};

struct virtq_used_elem {
    u32 id;
    u32 len;
};

struct virtq_used {
    u16 flags;
    u16 idx;
    struct virtq_used_elem ring[VIRTQ_MAX_SIZE];
    u16 avail_event;
};

_Static_assert(sizeof(struct virtq_desc) == 16, "virtq_desc size");
_Static_assert(sizeof(struct virtq_used_elem) == 8, "virtq_used_elem size");
_Static_assert(__builtin_offsetof(struct virtq_used, idx) == 2, "virtq_used.idx offset");

static u64 g_bar_va = 0;
static u32 g_common_off = 0;
static u32 g_notify_off = 0;
static u32 g_notify_mult = 0;
static u32 g_isr_off = 0;
static u64 g_vq_va = 0;
static u64 g_vq_pa = 0;
static u64 g_slab_va = 0;
static u64 g_slab_pa = 0;

static u8 g_slab_run[SLAB_SLOT_COUNT];
static struct virtqueue g_vq0;
static bool g_vq0_active = false;
static struct hwrng *g_registered_rng = NULL;
static u64 g_vq_completions = 0;
static u8 g_dev_status = 0;

static inline u8 mmio_r8(u64 addr) {
    return *(volatile u8 *)(uintptr_t)addr;
}

static inline void mmio_w8(u64 addr, u8 val) {
    *(volatile u8 *)(uintptr_t)addr = val;
}

static inline u16 mmio_r16(u64 addr) {
    return *(volatile u16 *)(uintptr_t)addr;
}

static inline void mmio_w16(u64 addr, u16 val) {
    *(volatile u16 *)(uintptr_t)addr = val;
}

static inline u32 mmio_r32(u64 addr) {
    return *(volatile u32 *)(uintptr_t)addr;
}

static inline void mmio_w32(u64 addr, u32 val) {
    *(volatile u32 *)(uintptr_t)addr = val;
}

static inline void mmio_w64_split(u64 addr, u64 val) {
    mmio_w32(addr, (u32)val);
    mmio_w32(addr + 4ULL, (u32)(val >> 32));
}

__attribute__((weak)) void *memcpy(void *dst, const void *src, size_t n) {
    u8 *d = (u8 *)dst;
    const u8 *s = (const u8 *)src;
    for (size_t i = 0; i < n; i++) {
        d[i] = s[i];
    }
    return dst;
}

__attribute__((weak)) void *memset(void *dst, int c, size_t n) {
    u8 *d = (u8 *)dst;
    for (size_t i = 0; i < n; i++) {
        d[i] = (u8)c;
    }
    return dst;
}

static int fmt_u64(char *out, u64 val, unsigned int base, bool upper) {
    char tmp[24];
    int n = 0;
    const char *digits = upper ? "0123456789ABCDEF" : "0123456789abcdef";
    if (val == 0ULL) {
        out[0] = '0';
        return 1;
    }
    while (val > 0ULL && n < 24) {
        tmp[n++] = digits[val % base];
        val /= base;
    }
    for (int i = 0; i < n; i++) {
        out[i] = tmp[n - 1 - i];
    }
    return n;
}

int sprintf(char *buf, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    char *p = buf;
    while (*fmt != '\0') {
        if (*fmt != '%') {
            *p++ = *fmt++;
            continue;
        }
        fmt++;
        if (*fmt == 'd' || *fmt == 'i') {
            int v = va_arg(ap, int);
            u64 uv = (v < 0) ? (u64)(-(long long)v) : (u64)v;
            if (v < 0) {
                *p++ = '-';
            }
            p += fmt_u64(p, uv, 10U, false);
        } else if (*fmt == 'u') {
            p += fmt_u64(p, (u64)va_arg(ap, unsigned int), 10U, false);
        } else if (*fmt == 'x' || *fmt == 'X') {
            p += fmt_u64(p, (u64)va_arg(ap, unsigned int), 16U, *fmt == 'X');
        } else if (*fmt == 's') {
            const char *s = va_arg(ap, const char *);
            if (s == NULL) {
                s = "(null)";
            }
            while (*s != '\0') {
                *p++ = *s++;
            }
        } else {
            *p++ = *fmt;
        }
        if (*fmt != '\0') {
            fmt++;
        }
    }
    *p = '\0';
    va_end(ap);
    return (int)(p - buf);
}

void *kzalloc(size_t size, gfp_t flags) {
    (void)flags;
    if (g_slab_va == 0ULL || size == 0 || size > SLAB_SLOT_COUNT * SLAB_SLOT_BYTES) {
        return NULL;
    }
    size_t need = (size + SLAB_SLOT_BYTES - 1U) / SLAB_SLOT_BYTES;
    for (size_t i = 0; i + need <= SLAB_SLOT_COUNT; i++) {
        bool ok = true;
        for (size_t j = 0; j < need; j++) {
            if (g_slab_run[i + j] != 0U) {
                ok = false;
                break;
            }
        }
        if (ok) {
            g_slab_run[i] = (u8)need;
            for (size_t j = 1; j < need; j++) {
                g_slab_run[i + j] = 0xffU;
            }
            void *ptr = (void *)(uintptr_t)(g_slab_va + i * SLAB_SLOT_BYTES);
            memset(ptr, 0, need * SLAB_SLOT_BYTES);
            return ptr;
        }
    }
    return NULL;
}

void kfree(const void *ptr) {
    if (ptr == NULL || g_slab_va == 0ULL) {
        return;
    }
    uintptr_t addr = (uintptr_t)ptr;
    if (addr < g_slab_va || addr >= g_slab_va + SLAB_SLOT_COUNT * SLAB_SLOT_BYTES) {
        return;
    }
    size_t off = (size_t)(addr - g_slab_va);
    if ((off % SLAB_SLOT_BYTES) != 0) {
        return;
    }
    size_t idx = off / SLAB_SLOT_BYTES;
    u8 run = g_slab_run[idx];
    if (run == 0U || run == 0xffU) {
        return;
    }
    for (size_t j = 0; j < run && idx + j < SLAB_SLOT_COUNT; j++) {
        g_slab_run[idx + j] = 0U;
    }
}

void sg_init_one(struct scatterlist *sg, const void *buf, unsigned int buflen) {
    uintptr_t va = (uintptr_t)buf;
    sg->addr = (void *)buf;
    if (g_slab_va != 0ULL && va >= g_slab_va && va + buflen <= g_slab_va + 4096ULL) {
        sg->length = buflen;
        sg->dma_addr = g_slab_pa + (u64)(va - g_slab_va);
    } else {
        sg->length = 0;
        sg->dma_addr = 0;
    }
}

int hwrng_register(struct hwrng *rng) {
    if (rng == NULL || rng->read == NULL) {
        return -EINVAL;
    }
    g_registered_rng = rng;
    return 0;
}

void hwrng_unregister(struct hwrng *rng) {
    if (g_registered_rng == rng) {
        g_registered_rng = NULL;
    }
}

static void lx_del_vqs(struct virtio_device *vdev) {
    (void)vdev;
    if (g_bar_va != 0ULL) {
        u64 common = g_bar_va + g_common_off;
        mmio_w16(common + 0x16ULL, 0);
        mmio_w16(common + 0x1cULL, 0);
    }
    g_vq0_active = false;
    memset(&g_vq0, 0, sizeof(g_vq0));
}

static const struct virtio_config_ops g_vdev_ops = {
    .del_vqs = lx_del_vqs,
};

static struct virtio_device g_vdev = {
    .priv = NULL,
    .config = &g_vdev_ops,
};

struct virtqueue *virtio_find_single_vq(
    struct virtio_device *vdev,
    void (*callback)(struct virtqueue *vq),
    const char *name
) {
    if (g_bar_va == 0ULL || g_vq_va == 0ULL || g_vq_pa == 0ULL) {
        return ERR_PTR(-ENODEV);
    }
    u64 common = g_bar_va + g_common_off;
    mmio_w16(common + 0x16ULL, 0);
    u16 max_qsz = mmio_r16(common + 0x18ULL);
    if (max_qsz < 2U) {
        return ERR_PTR(-ENODEV);
    }
    u16 qsz = (max_qsz > VIRTQ_MAX_SIZE) ? VIRTQ_MAX_SIZE : max_qsz;
    if ((qsz & (u16)(qsz - 1U)) != 0U) {
        qsz = 8U;
    }
    mmio_w16(common + 0x18ULL, qsz);
    qsz = mmio_r16(common + 0x18ULL);
    if (qsz < 2U || qsz > VIRTQ_MAX_SIZE) {
        return ERR_PTR(-ENODEV);
    }

    memset((void *)(uintptr_t)g_vq_va, 0, 4096U);
    mmio_w64_split(common + 0x20ULL, g_vq_pa + VQ_DESC_OFF);
    mmio_w64_split(common + 0x28ULL, g_vq_pa + VQ_AVAIL_OFF);
    mmio_w64_split(common + 0x30ULL, g_vq_pa + VQ_USED_OFF);
    u16 notify_qoff = mmio_r16(common + 0x1eULL);
    mmio_w16(common + 0x1cULL, 1U);

    memset(&g_vq0, 0, sizeof(g_vq0));
    g_vq0.vdev = vdev;
    g_vq0.callback = callback;
    g_vq0.name = name;
    g_vq0.num = qsz;
    g_vq0.notify_off = notify_qoff;
    g_vq0_active = true;
    return &g_vq0;
}

void virtio_device_ready(struct virtio_device *vdev) {
    (void)vdev;
    u64 common = g_bar_va + g_common_off;
    g_dev_status |= (u8)VIRTIO_CONFIG_S_DRIVER_OK;
    mmio_w8(common + 0x14ULL, g_dev_status);
}

void virtio_reset_device(struct virtio_device *vdev) {
    (void)vdev;
    if (g_bar_va == 0ULL) {
        return;
    }
    u64 common = g_bar_va + g_common_off;
    mmio_w8(common + 0x14ULL, 0);
    for (int i = 0; i < 10000; i++) {
        if (mmio_r8(common + 0x14ULL) == 0U) {
            break;
        }
        __builtin_ia32_pause();
    }
    g_dev_status = 0;
}

int virtqueue_add_inbuf(
    struct virtqueue *vq,
    struct scatterlist *sg,
    unsigned int num,
    void *data,
    gfp_t gfp
) {
    (void)gfp;
    if (!g_vq0_active || vq != &g_vq0 || num != 1U || sg->dma_addr == 0ULL || sg->length == 0U) {
        return -EINVAL;
    }
    u16 slot = (u16)(vq->avail_idx & (u16)(vq->num - 1U));
    volatile struct virtq_desc *desc =
        (volatile struct virtq_desc *)(uintptr_t)(g_vq_va + VQ_DESC_OFF);
    volatile struct virtq_avail *avail =
        (volatile struct virtq_avail *)(uintptr_t)(g_vq_va + VQ_AVAIL_OFF);

    desc[slot].addr = sg->dma_addr;
    desc[slot].len = sg->length;
    desc[slot].flags = VIRTQ_DESC_F_WRITE;
    desc[slot].next = 0;

    vq->tokens[slot] = data;
    avail->ring[slot] = slot;
    __atomic_thread_fence(__ATOMIC_RELEASE);
    vq->avail_idx = (u16)(vq->avail_idx + 1U);
    avail->idx = vq->avail_idx;
    __atomic_thread_fence(__ATOMIC_SEQ_CST);
    return 0;
}

bool virtqueue_kick(struct virtqueue *vq) {
    if (!g_vq0_active || vq != &g_vq0) {
        return false;
    }
    u64 notify_addr = g_bar_va + (u64)g_notify_off + (u64)vq->notify_off * (u64)g_notify_mult;
    mmio_w16(notify_addr, 0U);
    return true;
}

void *virtqueue_get_buf(struct virtqueue *vq, unsigned int *len) {
    if (!g_vq0_active || vq != &g_vq0) {
        return NULL;
    }
    volatile struct virtq_used *used =
        (volatile struct virtq_used *)(uintptr_t)(g_vq_va + VQ_USED_OFF);
    u16 hw_idx = __atomic_load_n(&used->idx, __ATOMIC_ACQUIRE);
    if (hw_idx == vq->last_used_idx) {
        return NULL;
    }
    u16 u_slot = (u16)(vq->last_used_idx & (u16)(vq->num - 1U));
    u32 id = used->ring[u_slot].id;
    u32 dlen = used->ring[u_slot].len;
    vq->last_used_idx = (u16)(vq->last_used_idx + 1U);
    if (len != NULL) {
        *len = dlen;
    }
    if (id >= vq->num) {
        return NULL;
    }
    void *tok = vq->tokens[id];
    vq->tokens[id] = NULL;
    g_vq_completions++;
    return tok;
}

static void lx_poll_vq_once(void) {
    if (!g_vq0_active) {
        return;
    }
    volatile struct virtq_used *used =
        (volatile struct virtq_used *)(uintptr_t)(g_vq_va + VQ_USED_OFF);
    u16 hw_idx = __atomic_load_n(&used->idx, __ATOMIC_ACQUIRE);
    if (hw_idx != g_vq0.last_used_idx) {
        (void)mmio_r8(g_bar_va + (u64)g_isr_off);
        if (g_vq0.callback != NULL) {
            g_vq0.callback(&g_vq0);
        }
    }
}

int wait_for_completion_killable(struct completion *x) {
    for (u32 iter = 0; iter < 2000000U; iter++) {
        if (__atomic_load_n(&x->done, __ATOMIC_ACQUIRE) != 0U) {
            return 0;
        }
        lx_poll_vq_once();
        if (__atomic_load_n(&x->done, __ATOMIC_ACQUIRE) != 0U) {
            return 0;
        }
        __builtin_ia32_pause();
    }
    return -EINTR;
}

int lx_shim_init_device(
    u64 bar_va,
    u32 common_off,
    u32 notify_off,
    u32 notify_mult,
    u32 isr_off,
    u64 vq_va,
    u64 vq_pa,
    u64 slab_va,
    u64 slab_pa
) {
    g_bar_va = bar_va;
    g_common_off = common_off;
    g_notify_off = notify_off;
    g_notify_mult = notify_mult;
    g_isr_off = isr_off;
    g_vq_va = vq_va;
    g_vq_pa = vq_pa;
    g_slab_va = slab_va;
    g_slab_pa = slab_pa;
    g_registered_rng = NULL;
    g_vq_completions = 0;
    g_vq0_active = false;
    memset(g_slab_run, 0, sizeof(g_slab_run));

    struct virtio_driver *drv = lx_shim_get_virtio_driver();
    if (drv == NULL || drv->probe == NULL) {
        return -ENODEV;
    }

    u64 common = g_bar_va + g_common_off;
    virtio_reset_device(&g_vdev);

    g_dev_status = (u8)VIRTIO_CONFIG_S_ACKNOWLEDGE;
    mmio_w8(common + 0x14ULL, g_dev_status);
    g_dev_status |= (u8)VIRTIO_CONFIG_S_DRIVER;
    mmio_w8(common + 0x14ULL, g_dev_status);

    mmio_w32(common + 0x00ULL, 1U);
    u32 dev_feat_hi = mmio_r32(common + 0x04ULL);
    if ((dev_feat_hi & 1U) == 0U) {
        mmio_w8(common + 0x14ULL, VIRTIO_CONFIG_S_FAILED);
        return -ENODEV;
    }
    mmio_w32(common + 0x08ULL, 0U);
    mmio_w32(common + 0x0cULL, 0U);
    mmio_w32(common + 0x08ULL, 1U);
    mmio_w32(common + 0x0cULL, 1U);

    g_dev_status |= (u8)VIRTIO_CONFIG_S_FEATURES_OK;
    mmio_w8(common + 0x14ULL, g_dev_status);
    if ((mmio_r8(common + 0x14ULL) & VIRTIO_CONFIG_S_FEATURES_OK) == 0U) {
        mmio_w8(common + 0x14ULL, VIRTIO_CONFIG_S_FAILED);
        return -ENODEV;
    }

    int err = drv->probe(&g_vdev);
    if (err != 0) {
        virtio_reset_device(&g_vdev);
        return err;
    }
    if (drv->scan != NULL) {
        drv->scan(&g_vdev);
    }
    if (g_registered_rng == NULL) {
        if (drv->remove != NULL) {
            drv->remove(&g_vdev);
        }
        return -ENODEV;
    }
    return 0;
}

int lx_shim_read_entropy(u8 *out_buf, size_t len) {
    if (g_registered_rng == NULL || g_registered_rng->read == NULL || out_buf == NULL || len == 0) {
        return -ENODEV;
    }
    return g_registered_rng->read(g_registered_rng, out_buf, len, true);
}

void lx_shim_shutdown_device(void) {
    struct virtio_driver *drv = lx_shim_get_virtio_driver();
    if (drv != NULL && drv->remove != NULL && g_vdev.priv != NULL) {
        drv->remove(&g_vdev);
        g_vdev.priv = NULL;
    }
}

u64 lx_shim_irq_completions(void) {
    return g_vq_completions;
}
