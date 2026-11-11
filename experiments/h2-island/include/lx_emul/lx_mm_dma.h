/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * DRY OS — H2 Shadow Memory, Linear Shadow `vmemmap`, Persistent `DmaArena`, and MMIO
 */
#ifndef LX_EMUL_MM_DMA_H
#define LX_EMUL_MM_DMA_H

#include "lx_types.h"

struct device;

#define GFP_KERNEL   0x01U
#define GFP_ATOMIC   0x02U
#define __GFP_ZERO   0x04U
#define __GFP_NOWARN 0x08U
#define __GFP_COMP   0x10U

static inline void *kmalloc(size_t size, gfp_t flags)
{
	if (size == 0)
		size = 8;
	if (flags & __GFP_ZERO)
		return calloc(1, size);
	return malloc(size);
}
static inline void *kzalloc(size_t size, gfp_t flags)
{
	(void)flags;
	return calloc(1, size ? size : 8);
}
static inline void *kcalloc(size_t n, size_t size, gfp_t flags)
{
	(void)flags;
	return calloc(n ? n : 1, size ? size : 1);
}
static inline void *kmalloc_array(size_t n, size_t size, gfp_t flags)
{
	if (flags & __GFP_ZERO)
		return calloc(n ? n : 1, size ? size : 1);
	return malloc((n * size) ? (n * size) : 8);
}
static inline void *vmalloc(unsigned long size) { return malloc(size ? size : 8); }
static inline void *vzalloc(unsigned long size) { return calloc(1, size ? size : 8); }
static inline void *vmalloc_array(size_t n, size_t size) { return malloc((n * size) ? (n * size) : 8); }
static inline void kfree(const void *ptr) { free((void *)ptr); }
static inline void vfree(const void *ptr) { free((void *)ptr); }
static inline void kvfree(const void *ptr) { free((void *)ptr); }

#define kfree_rcu(ptr, rcu_head_field) kfree(ptr)
#define kvfree_rcu(ptr, ...)           kvfree(ptr)
#define kvfree_rcu_mightsleep(ptr)     kvfree(ptr)

/*
 * Shadow `struct page` in linear shadow `vmemmap` backing `dry_dma_arena`:
 * O(1) pointer arithmetic `virt_to_page()` / `page_address()` inside the arena!
 */
struct page {
	_Atomic int refcount;
	uint32_t order;
	uint32_t arena_pfn;
	bool in_use;
	bool pfmemalloc;
};

struct page *lx_alloc_arena_pages(gfp_t gfp, unsigned int order);
void lx_free_arena_pages(struct page *page, unsigned int order);
void *page_address(const struct page *page);
struct page *virt_to_page(const void *addr);

#define alloc_pages(gfp, order)      lx_alloc_arena_pages((gfp), (order))
#define __dev_alloc_pages(gfp, ord)  lx_alloc_arena_pages((gfp), (ord))
#define dev_alloc_pages(ord)         lx_alloc_arena_pages(GFP_ATOMIC, (ord))
#define dev_alloc_page()             lx_alloc_arena_pages(GFP_ATOMIC, 0)
#define __free_pages(page, order)    lx_free_arena_pages((page), (order))
#define __free_page(page)            lx_free_arena_pages((page), 0)

static inline int page_count(const struct page *p)
{
	return atomic_load_explicit((_Atomic int *)&p->refcount, memory_order_relaxed);
}
static inline int page_ref_count(const struct page *p) { return page_count(p); }
static inline void page_ref_add(struct page *p, int nr)
{
	atomic_fetch_add_explicit(&p->refcount, nr, memory_order_relaxed);
}
static inline void page_ref_inc(struct page *p) { page_ref_add(p, 1); }
static inline void get_page(struct page *p) { page_ref_inc(p); }
static inline void put_page(struct page *p)
{
	if (atomic_fetch_sub_explicit(&p->refcount, 1, memory_order_acq_rel) == 1)
		lx_free_arena_pages(p, p->order);
}
static inline void __page_frag_cache_drain(struct page *page, unsigned int count)
{
	if (atomic_fetch_sub_explicit(&page->refcount, (int)count, memory_order_acq_rel) == (int)count)
		lx_free_arena_pages(page, page->order);
}
static inline bool page_is_pfmemalloc(const struct page *page) { return page->pfmemalloc; }
static inline bool dev_page_is_reusable(const struct page *page) { return !page->pfmemalloc; }

/* DMA API backed by persistent `dry_dma_arena` (`0` dynamic IOMMU syscalls) */
enum dma_data_direction {
	DMA_BIDIRECTIONAL = 0,
	DMA_TO_DEVICE = 1,
	DMA_FROM_DEVICE = 2,
	DMA_NONE = 3,
};

#define DMA_ATTR_SKIP_CPU_SYNC 0x10UL
#define DMA_ATTR_WEAK_ORDERING 0x20UL
#define DMA_BIT_MASK(n) (((n) == 64) ? ~0ULL : ((1ULL << ((n) & 63)) - 1ULL))
#define DMA_MAPPING_ERROR (~(dma_addr_t)0)

#define DEFINE_DMA_UNMAP_ADDR(ADDR_NAME) dma_addr_t ADDR_NAME
#define DEFINE_DMA_UNMAP_LEN(LEN_NAME)   __u32 LEN_NAME
#define dma_unmap_addr(PTR, ADDR_NAME)           ((PTR)->ADDR_NAME)
#define dma_unmap_addr_set(PTR, ADDR_NAME, VAL)  (((PTR)->ADDR_NAME) = (VAL))
#define dma_unmap_len(PTR, LEN_NAME)             ((PTR)->LEN_NAME)
#define dma_unmap_len_set(PTR, LEN_NAME, VAL)    (((PTR)->LEN_NAME) = (VAL))

static inline int dma_set_mask_and_coherent(struct device *dev, u64 mask)
{
	(void)dev;
	return (mask >= DMA_BIT_MASK(32)) ? 0 : -EIO;
}
static inline int dma_mapping_error(struct device *dev, dma_addr_t dma_addr)
{
	(void)dev;
	return (dma_addr == DMA_MAPPING_ERROR) ? -ENOMEM : 0;
}

void *dma_alloc_coherent(struct device *dev, size_t size,
			 dma_addr_t *dma_handle, gfp_t gfp);
void dma_free_coherent(struct device *dev, size_t size,
		       void *cpu_addr, dma_addr_t dma_handle);
dma_addr_t dma_map_single_attrs(struct device *dev, void *ptr, size_t size,
				enum dma_data_direction dir, unsigned long attrs);
void dma_unmap_single_attrs(struct device *dev, dma_addr_t addr, size_t size,
			    enum dma_data_direction dir, unsigned long attrs);
dma_addr_t dma_map_page_attrs(struct device *dev, struct page *page,
			      size_t offset, size_t size,
			      enum dma_data_direction dir, unsigned long attrs);
void dma_unmap_page_attrs(struct device *dev, dma_addr_t addr, size_t size,
			  enum dma_data_direction dir, unsigned long attrs);

#define dma_map_single(d, a, s, r)     dma_map_single_attrs(d, a, s, r, 0)
#define dma_unmap_single(d, a, s, r)   dma_unmap_single_attrs(d, a, s, r, 0)
#define dma_map_page(d, p, o, s, r)    dma_map_page_attrs(d, p, o, s, r, 0)
#define dma_unmap_page(d, a, s, r)     dma_unmap_page_attrs(d, a, s, r, 0)

static inline void dma_sync_single_for_cpu(struct device *dev, dma_addr_t addr,
					   size_t size, enum dma_data_direction dir)
{
	(void)dev; (void)addr; (void)size; (void)dir;
	dma_rmb();
}
static inline void dma_sync_single_for_device(struct device *dev, dma_addr_t addr,
					      size_t size, enum dma_data_direction dir)
{
	(void)dev; (void)addr; (void)size; (void)dir;
	dma_wmb();
}
static inline void dma_sync_single_range_for_cpu(struct device *dev, dma_addr_t addr,
						 unsigned long offset, size_t size,
						 enum dma_data_direction dir)
{
	(void)offset;
	dma_sync_single_for_cpu(dev, addr, size, dir);
}
static inline void dma_sync_single_range_for_device(struct device *dev, dma_addr_t addr,
						    unsigned long offset, size_t size,
						    enum dma_data_direction dir)
{
	(void)offset;
	dma_sync_single_for_device(dev, addr, size, dir);
}

/* MMIO BAR0 helpers routed through `dry_mmio_read32` / `dry_mmio_write32` */
static inline u32 readl(const volatile void __iomem *addr)
{
	return dry_mmio_read32((const volatile void *)addr);
}
static inline void writel(u32 val, volatile void __iomem *addr)
{
	dry_mmio_write32(val, (volatile void *)addr);
}
void __iomem *ioremap(resource_size_t offset, unsigned long size);
void iounmap(volatile void __iomem *addr);
void memcpy_fromio(void *to, const volatile void __iomem *from, size_t count);

#endif /* LX_EMUL_MM_DMA_H */

