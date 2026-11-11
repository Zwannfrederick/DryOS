/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * DRY OS — H2 Persistent `DmaArena` & Linear Shadow `vmemmap` Implementation
 */
#include <sys/mman.h>
#include "../include/lx_emul/lx_net.h"

int dry_dma_arena_init(struct dry_dma_arena *arena, uint64_t base_iova,
		       size_t coherent_pages, size_t rx_pages,
		       size_t tx_bounce_pages)
{
	size_t total = coherent_pages + rx_pages + tx_bounce_pages;
	if (total == 0)
		return -EINVAL;

	memset(arena, 0, sizeof(*arena));
	void *mem = mmap(NULL, total * DRY_PAGE_SIZE, PROT_READ | PROT_WRITE,
			 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (mem == MAP_FAILED)
		return -ENOMEM;

	arena->vmemmap = calloc(total, sizeof(struct page));
	arena->page_perms = calloc(total, sizeof(uint8_t));
	if (!arena->vmemmap || !arena->page_perms) {
		munmap(mem, total * DRY_PAGE_SIZE);
		free(arena->vmemmap);
		free(arena->page_perms);
		return -ENOMEM;
	}

	arena->base = (uint8_t *)mem;
	arena->base_iova = base_iova;
	arena->total_pages = total;
	arena->coherent_off_pages = 0;
	arena->coherent_pages = coherent_pages;
	arena->rx_off_pages = coherent_pages;
	arena->rx_pages = rx_pages;
	arena->tx_bounce_off_pages = coherent_pages + rx_pages;
	arena->tx_bounce_pages = tx_bounce_pages;

	for (size_t i = 0; i < total; i++) {
		arena->vmemmap[i].arena_pfn = (uint32_t)i;
		if (i < coherent_pages)
			arena->page_perms[i] = DRY_DMA_BIDIR;
		else if (i < coherent_pages + rx_pages)
			arena->page_perms[i] = DRY_DMA_WRITE;
		else
			arena->page_perms[i] = DRY_DMA_READ;
	}
	return 0;
}

void dry_dma_arena_destroy(struct dry_dma_arena *arena)
{
	if (arena->base) {
		munmap(arena->base, arena->total_pages * DRY_PAGE_SIZE);
		arena->base = NULL;
	}
	free(arena->vmemmap);
	free(arena->page_perms);
	arena->vmemmap = NULL;
	arena->page_perms = NULL;
}

static bool ptr_in_arena(const struct dry_dma_arena *arena, const void *ptr, size_t len)
{
	if (!arena->base || !ptr)
		return false;
	uintptr_t start = (uintptr_t)arena->base;
	uintptr_t end = start + arena->total_pages * DRY_PAGE_SIZE;
	uintptr_t p = (uintptr_t)ptr;
	return p >= start && len <= (end - p);
}

void *page_address(const struct page *page)
{
	struct dry_dma_arena *arena = &g_dry_island.arena;
	if (!page || !arena->vmemmap)
		return NULL;
	ptrdiff_t idx = page - arena->vmemmap;
	if (idx < 0 || (size_t)idx >= arena->total_pages)
		return NULL;
	return arena->base + ((size_t)idx << DRY_PAGE_SHIFT);
}

struct page *virt_to_page(const void *addr)
{
	struct dry_dma_arena *arena = &g_dry_island.arena;
	if (!ptr_in_arena(arena, addr, 1))
		return NULL;
	size_t idx = ((uintptr_t)addr - (uintptr_t)arena->base) >> DRY_PAGE_SHIFT;
	return &arena->vmemmap[idx];
}

struct page *lx_alloc_arena_pages(gfp_t gfp, unsigned int order)
{
	(void)gfp;
	struct dry_dma_arena *arena = &g_dry_island.arena;
	size_t count = 1UL << order;
	if (!arena->base || count > arena->rx_pages)
		return NULL;

	for (size_t scanned = 0; scanned < arena->rx_pages; scanned++) {
		size_t rel = (arena->rx_cursor + scanned) % arena->rx_pages;
		if ((rel & (count - 1)) != 0 || rel + count > arena->rx_pages)
			continue;
		size_t pfn = arena->rx_off_pages + rel;
		bool free_block = true;
		for (size_t k = 0; k < count; k++) {
			if (arena->vmemmap[pfn + k].in_use) {
				free_block = false;
				break;
			}
		}
		if (free_block) {
			for (size_t k = 0; k < count; k++) {
				arena->vmemmap[pfn + k].in_use = true;
				arena->vmemmap[pfn + k].order = order;
				arena->vmemmap[pfn + k].pfmemalloc = false;
				atomic_store(&arena->vmemmap[pfn + k].refcount, (k == 0) ? 1 : 0);
			}
			arena->rx_cursor = (rel + count) % arena->rx_pages;
			arena->stats.rx_page_allocs++;
			memset(arena->base + (pfn << DRY_PAGE_SHIFT), 0, count * DRY_PAGE_SIZE);
			return &arena->vmemmap[pfn];
		}
	}
	return NULL;
}

void lx_free_arena_pages(struct page *page, unsigned int order)
{
	struct dry_dma_arena *arena = &g_dry_island.arena;
	if (!page || !arena->vmemmap)
		return;
	ptrdiff_t pfn = page - arena->vmemmap;
	size_t count = 1UL << order;
	if (pfn < (ptrdiff_t)arena->rx_off_pages ||
	    (size_t)pfn + count > arena->rx_off_pages + arena->rx_pages)
		return;
	for (size_t k = 0; k < count; k++) {
		arena->vmemmap[(size_t)pfn + k].in_use = false;
		atomic_store(&arena->vmemmap[(size_t)pfn + k].refcount, 0);
	}
}

void *dma_alloc_coherent(struct device *dev, size_t size,
			 dma_addr_t *dma_handle, gfp_t gfp)
{
	(void)dev; (void)gfp;
	struct dry_dma_arena *arena = &g_dry_island.arena;
	size_t pages = DIV_ROUND_UP(size ? size : 1, DRY_PAGE_SIZE);
	if (!arena->base || arena->coherent_cursor + pages > arena->coherent_pages) {
		if (dma_handle)
			*dma_handle = DMA_MAPPING_ERROR;
		return NULL;
	}
	size_t pfn = arena->coherent_off_pages + arena->coherent_cursor;
	arena->coherent_cursor += pages;
	arena->stats.coherent_allocs++;
	void *va = arena->base + (pfn << DRY_PAGE_SHIFT);
	memset(va, 0, pages * DRY_PAGE_SIZE);
	if (dma_handle)
		*dma_handle = arena->base_iova + ((uint64_t)pfn << DRY_PAGE_SHIFT);
	return va;
}

void dma_free_coherent(struct device *dev, size_t size,
		       void *cpu_addr, dma_addr_t dma_handle)
{
	(void)dev; (void)size; (void)cpu_addr; (void)dma_handle;
}

dma_addr_t dma_map_page_attrs(struct device *dev, struct page *page,
			      size_t offset, size_t size,
			      enum dma_data_direction dir, unsigned long attrs)
{
	(void)dev; (void)attrs;
	struct dry_dma_arena *arena = &g_dry_island.arena;
	if (!page || !arena->vmemmap) {
		arena->stats.rejected_oob_maps++;
		return DMA_MAPPING_ERROR;
	}
	ptrdiff_t pfn = page - arena->vmemmap;
	if (pfn < 0 || (size_t)pfn >= arena->total_pages) {
		arena->stats.rejected_oob_maps++;
		return DMA_MAPPING_ERROR;
	}
	uint8_t *va = arena->base + ((size_t)pfn << DRY_PAGE_SHIFT) + offset;
	if (!ptr_in_arena(arena, va, size)) {
		arena->stats.rejected_oob_maps++;
		return DMA_MAPPING_ERROR;
	}
	enum dry_dma_perm need = (dir == DMA_FROM_DEVICE) ? DRY_DMA_WRITE : DRY_DMA_READ;
	if ((arena->page_perms[pfn] & need) != need) {
		arena->stats.rejected_oob_maps++;
		return DMA_MAPPING_ERROR;
	}
	return arena->base_iova + (uint64_t)((uintptr_t)va - (uintptr_t)arena->base);
}

void dma_unmap_page_attrs(struct device *dev, dma_addr_t addr, size_t size,
			  enum dma_data_direction dir, unsigned long attrs)
{
	(void)dev; (void)addr; (void)size; (void)dir; (void)attrs;
}

dma_addr_t dma_map_single_attrs(struct device *dev, void *ptr, size_t size,
				enum dma_data_direction dir, unsigned long attrs)
{
	(void)dev; (void)attrs;
	struct dry_dma_arena *arena = &g_dry_island.arena;
	if (!ptr || size == 0 || size > DRY_PAGE_SIZE) {
		arena->stats.rejected_oob_maps++;
		return DMA_MAPPING_ERROR;
	}
	if (ptr_in_arena(arena, ptr, size)) {
		size_t pfn = ((uintptr_t)ptr - (uintptr_t)arena->base) >> DRY_PAGE_SHIFT;
		enum dry_dma_perm need = (dir == DMA_FROM_DEVICE) ? DRY_DMA_WRITE : DRY_DMA_READ;
		if ((arena->page_perms[pfn] & need) != need) {
			arena->stats.rejected_oob_maps++;
			return DMA_MAPPING_ERROR;
		}
		arena->stats.tx_arena_maps++;
		return arena->base_iova + (uint64_t)((uintptr_t)ptr - (uintptr_t)arena->base);
	}
	/* Out-of-arena heap pointer: NEVER grant device write access (`DMA_FROM_DEVICE`)! */
	if (dir != DMA_TO_DEVICE || arena->tx_bounce_pages == 0) {
		arena->stats.rejected_oob_maps++;
		return DMA_MAPPING_ERROR;
	}
	/* Copy linear TX header into read-only-for-device (`DRY_DMA_READ`) bounce page */
	size_t slot_pfn = arena->tx_bounce_off_pages + (arena->tx_bounce_cursor % arena->tx_bounce_pages);
	arena->tx_bounce_cursor++;
	uint8_t *dst = arena->base + (slot_pfn << DRY_PAGE_SHIFT);
	memcpy(dst, ptr, size);
	arena->stats.tx_bounce_copies++;
	return arena->base_iova + ((uint64_t)slot_pfn << DRY_PAGE_SHIFT);
}

void dma_unmap_single_attrs(struct device *dev, dma_addr_t addr, size_t size,
			    enum dma_data_direction dir, unsigned long attrs)
{
	(void)dev; (void)addr; (void)size; (void)dir; (void)attrs;
}

void *dry_dma_iova_to_va(struct dry_dma_arena *arena, uint64_t iova,
			 size_t len, enum dry_dma_perm required_perm)
{
	if (!arena->base || iova < arena->base_iova)
		return NULL;
	uint64_t off = iova - arena->base_iova;
	size_t total_bytes = arena->total_pages * DRY_PAGE_SIZE;
	if (off >= total_bytes || len > total_bytes - off)
		return NULL;
	size_t start_pfn = (size_t)(off >> DRY_PAGE_SHIFT);
	size_t end_pfn = (size_t)((off + (len ? len - 1 : 0)) >> DRY_PAGE_SHIFT);
	for (size_t p = start_pfn; p <= end_pfn; p++) {
		if ((arena->page_perms[p] & required_perm) != required_perm)
			return NULL;
	}
	return arena->base + off;
}
