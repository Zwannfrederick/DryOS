/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#include <linux/dma-buf.h>
#include <linux/jiffies.h>

#define MAX_DMABUF_SLOTS 16U

static struct dma_buf g_dmabuf_pool[MAX_DMABUF_SLOTS];
static uint8_t g_dmabuf_used[MAX_DMABUF_SLOTS];

void dma_fence_init(struct dma_fence *fence, const struct dma_fence_ops *ops,
                    spinlock_t *lock, u64 context, u64 seqno) {
    if (!fence) return;
    fence->ops = ops;
    fence->lock = lock;
    fence->context = context;
    fence->seqno = seqno;
    fence->flags = 0U;
    fence->refcount = 1U;
    fence->error = 0;
    fence->cb_head = NULL;
}

struct dma_fence *dma_fence_get(struct dma_fence *fence) {
    if (fence) {
        __atomic_fetch_add(&fence->refcount, 1U, __ATOMIC_RELAXED);
    }
    return fence;
}

void dma_fence_put(struct dma_fence *fence) {
    if (!fence) return;
    if (__atomic_sub_fetch(&fence->refcount, 1U, __ATOMIC_ACQ_REL) == 0U) {
        if (fence->ops && fence->ops->release) {
            fence->ops->release(fence);
        }
    }
}

bool dma_fence_is_signaled(struct dma_fence *fence) {
    if (!fence) return true;
    return (__atomic_load_n(&fence->flags, __ATOMIC_ACQUIRE) &
            (1U << DMA_FENCE_FLAG_SIGNALED_BIT)) != 0U;
}

void dma_fence_set_error(struct dma_fence *fence, int error) {
    if (fence) {
        fence->error = error;
    }
}

int dma_fence_signal(struct dma_fence *fence) {
    if (!fence) return -EINVAL;
    if (fence->lock) spin_lock(fence->lock);
    u32 prev = __atomic_fetch_or(&fence->flags, (1U << DMA_FENCE_FLAG_SIGNALED_BIT), __ATOMIC_RELEASE);
    struct dma_fence_cb *cb = fence->cb_head;
    fence->cb_head = NULL;
    if (fence->lock) spin_unlock(fence->lock);

    if ((prev & (1U << DMA_FENCE_FLAG_SIGNALED_BIT)) != 0U) {
        return -EINVAL;
    }
    while (cb) {
        struct dma_fence_cb *next = cb->next;
        cb->next = NULL;
        if (cb->func) {
            cb->func(fence, cb);
        }
        cb = next;
    }
    return 0;
}

int dma_fence_add_callback(struct dma_fence *fence, struct dma_fence_cb *cb, dma_fence_func_t func) {
    if (!fence || !cb || !func) return -EINVAL;
    if (dma_fence_is_signaled(fence)) {
        return -ENOENT;
    }
    if (fence->lock) spin_lock(fence->lock);
    if (dma_fence_is_signaled(fence)) {
        if (fence->lock) spin_unlock(fence->lock);
        return -ENOENT;
    }
    cb->func = func;
    cb->next = fence->cb_head;
    fence->cb_head = cb;
    if (fence->lock) spin_unlock(fence->lock);
    return 0;
}

long dma_fence_wait_timeout(struct dma_fence *fence, bool intr, unsigned long timeout_jiffies) {
    (void)intr;
    if (!fence) return 1L;
    unsigned long deadline = dry_lx_jiffies() + timeout_jiffies;
    for (;;) {
        if (dma_fence_is_signaled(fence)) {
            unsigned long now = dry_lx_jiffies();
            return (deadline > now) ? (long)(deadline - now) : 1L;
        }
        if (timeout_jiffies == 0UL || time_after(dry_lx_jiffies(), deadline)) {
            return 0L;
        }
        __builtin_ia32_pause();
    }
}

int dry_lx_dmabuf_wrap_framecap(u64 frame_cap, void *vaddr, size_t size,
                                u32 cap_flags, struct dma_buf **out) {
    if (!vaddr || size == 0U || !out) {
        return -EINVAL;
    }
    if ((cap_flags & (DRY_DMABUF_FLAG_RO_EXPORT | DRY_DMABUF_FLAG_DEVICE_DMA)) == 0U) {
        return -EINVAL;
    }
    for (size_t i = 0; i < MAX_DMABUF_SLOTS; i++) {
        if (g_dmabuf_used[i] == 0U) {
            g_dmabuf_used[i] = 1U;
            struct dma_buf *db = &g_dmabuf_pool[i];
            db->frame_cap = frame_cap;
            db->vaddr = vaddr;
            db->size = size;
            db->cap_flags = cap_flags;
            db->refcount = 1U;
            db->resv_fence = NULL;
            *out = db;
            return 0;
        }
    }
    return -ENOMEM;
}

struct dma_buf *dma_buf_get(struct dma_buf *dmabuf) {
    if (dmabuf) {
        __atomic_fetch_add(&dmabuf->refcount, 1U, __ATOMIC_RELAXED);
    }
    return dmabuf;
}

void dma_buf_put(struct dma_buf *dmabuf) {
    if (!dmabuf) return;
    if (__atomic_sub_fetch(&dmabuf->refcount, 1U, __ATOMIC_ACQ_REL) == 0U) {
        if (dmabuf->resv_fence) {
            dma_fence_put(dmabuf->resv_fence);
            dmabuf->resv_fence = NULL;
        }
        uintptr_t p = (uintptr_t)dmabuf;
        uintptr_t base = (uintptr_t)&g_dmabuf_pool[0];
        if (p >= base && p < base + sizeof(g_dmabuf_pool)) {
            size_t idx = (size_t)(dmabuf - &g_dmabuf_pool[0]);
            g_dmabuf_used[idx] = 0U;
        }
    }
}

int dry_lx_dmabuf_attach_fence(struct dma_buf *dmabuf, struct dma_fence *fence) {
    if (!dmabuf || !fence) return -EINVAL;
    if (dmabuf->resv_fence) {
        dma_fence_put(dmabuf->resv_fence);
    }
    dmabuf->resv_fence = dma_fence_get(fence);
    return 0;
}

bool dry_lx_dmabuf_is_ro(const struct dma_buf *dmabuf) {
    return dmabuf && ((dmabuf->cap_flags & DRY_DMABUF_FLAG_RO_EXPORT) != 0U);
}

void *dry_lx_dmabuf_vaddr(const struct dma_buf *dmabuf) {
    return dmabuf ? dmabuf->vaddr : NULL;
}

size_t dry_lx_dmabuf_size(const struct dma_buf *dmabuf) {
    return dmabuf ? dmabuf->size : 0U;
}
