/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_DMA_MAPPING_H
#define _DRY_LINUX_DMA_MAPPING_H

#include <linux/pci.h>

enum dma_data_direction {
    DMA_BIDIRECTIONAL = 0,
    DMA_TO_DEVICE = 1,
    DMA_FROM_DEVICE = 2,
    DMA_NONE = 3,
};

#define DMA_BIT_MASK(n) (((n) == 64) ? ~0ULL : ((1ULL << (n)) - 1ULL))
#define DMA_MAPPING_ERROR (~(dma_addr_t)0ULL)

int dma_set_mask_and_coherent(struct device *dev, u64 mask);
void *dma_alloc_coherent(struct device *dev, size_t size, dma_addr_t *dma_handle, gfp_t flag);
void dma_free_coherent(struct device *dev, size_t size, void *cpu_addr, dma_addr_t dma_handle);
dma_addr_t dma_map_single(struct device *dev, void *ptr, size_t size, enum dma_data_direction dir);
void dma_unmap_single(struct device *dev, dma_addr_t addr, size_t size, enum dma_data_direction dir);
int dma_mapping_error(struct device *dev, dma_addr_t dma_addr);

void *dry_lx_dma_alloc_skbuf(size_t size, dma_addr_t *dma_out);
void dry_lx_dma_free_skbuf(void *ptr);

#endif /* _DRY_LINUX_DMA_MAPPING_H */
