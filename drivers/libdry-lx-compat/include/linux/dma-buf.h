/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_DMA_BUF_H
#define _DRY_LINUX_DMA_BUF_H

#include <linux/dma-fence.h>

#define DRY_DMABUF_FLAG_RO_EXPORT (1U << 0)
#define DRY_DMABUF_FLAG_DEVICE_DMA (1U << 1)

struct dma_buf {
    u64 frame_cap;
    void *vaddr;
    size_t size;
    u32 cap_flags;
    u32 refcount;
    struct dma_fence *resv_fence;
};

int dry_lx_dmabuf_wrap_framecap(u64 frame_cap, void *vaddr, size_t size,
                                u32 cap_flags, struct dma_buf **out);
struct dma_buf *dma_buf_get(struct dma_buf *dmabuf);
void dma_buf_put(struct dma_buf *dmabuf);
int dry_lx_dmabuf_attach_fence(struct dma_buf *dmabuf, struct dma_fence *fence);
bool dry_lx_dmabuf_is_ro(const struct dma_buf *dmabuf);
void *dry_lx_dmabuf_vaddr(const struct dma_buf *dmabuf);
size_t dry_lx_dmabuf_size(const struct dma_buf *dmabuf);

#endif /* _DRY_LINUX_DMA_BUF_H */
