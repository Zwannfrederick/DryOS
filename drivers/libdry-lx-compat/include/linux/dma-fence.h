/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_DMA_FENCE_H
#define _DRY_LINUX_DMA_FENCE_H

#include <linux/spinlock.h>

struct dma_fence;
struct dma_fence_cb;

typedef void (*dma_fence_func_t)(struct dma_fence *fence, struct dma_fence_cb *cb);

struct dma_fence_cb {
    struct dma_fence_cb *next;
    dma_fence_func_t func;
};

struct dma_fence_ops {
    const char *(*get_driver_name)(struct dma_fence *fence);
    const char *(*get_timeline_name)(struct dma_fence *fence);
    void (*release)(struct dma_fence *fence);
};

struct dma_fence {
    spinlock_t *lock;
    const struct dma_fence_ops *ops;
    u64 context;
    u64 seqno;
    volatile u32 flags;
    volatile u32 refcount;
    int error;
    struct dma_fence_cb *cb_head;
};

#define DMA_FENCE_FLAG_SIGNALED_BIT 0

void dma_fence_init(struct dma_fence *fence, const struct dma_fence_ops *ops,
                    spinlock_t *lock, u64 context, u64 seqno);
struct dma_fence *dma_fence_get(struct dma_fence *fence);
void dma_fence_put(struct dma_fence *fence);
int dma_fence_signal(struct dma_fence *fence);
bool dma_fence_is_signaled(struct dma_fence *fence);
void dma_fence_set_error(struct dma_fence *fence, int error);
int dma_fence_add_callback(struct dma_fence *fence, struct dma_fence_cb *cb, dma_fence_func_t func);
long dma_fence_wait_timeout(struct dma_fence *fence, bool intr, unsigned long timeout_jiffies);

#endif /* _DRY_LINUX_DMA_FENCE_H */
