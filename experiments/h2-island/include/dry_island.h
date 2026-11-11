/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * DRY OS — H2 Userspace Linux Driver Island Core Runtime Interface
 *
 * Isolates a source-recompiled GPL-2.0 Linux PCIe NIC driver (`igc`) inside an
 * unprivileged userspace process backed by:
 *   1. Persistent pre-mapped `dry_dma_arena` + linear shadow `vmemmap`
 *      (`0` dynamic IOMMU map/unmap syscalls on packet TX/RX hot path).
 *   2. Capability-equivalent BAR0 MMIO region (`mmap` VFIO BAR or simulated HW).
 *   3. `eventfd`-backed MSI-X interrupt vectors driving bounded NAPI polling.
 *   4. Bounded shared-memory `dry_io_ring` packet RX/TX queues.
 */
#ifndef DRY_ISLAND_H
#define DRY_ISLAND_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define DRY_PAGE_SHIFT 12U
#define DRY_PAGE_SIZE  (1UL << DRY_PAGE_SHIFT)
#define DRY_MAX_MSIX   16U
#define DRY_IORING_CAP 256U
#define DRY_MAX_PKT    2048U

/* Forward declaration of shadow Linux `struct page` in linear shadow vmemmap */
struct page;
struct pci_driver;
struct pci_dev;
struct net_device;

enum dry_dma_perm {
	DRY_DMA_NONE = 0,
	DRY_DMA_READ = 1,       /* Device reads (TX buffers) */
	DRY_DMA_WRITE = 2,      /* Device writes (RX pages) */
	DRY_DMA_BIDIR = 3,      /* Coherent descriptor rings */
};

struct dry_dma_stats {
	uint64_t coherent_allocs;
	uint64_t rx_page_allocs;
	uint64_t tx_arena_maps;
	uint64_t tx_bounce_copies;
	uint64_t rejected_oob_maps;
	uint64_t dynamic_iommu_syscalls; /* Invariant: stays 0 after init */
};

struct dry_dma_arena {
	uint8_t *base;
	uint64_t base_iova;
	size_t total_pages;
	size_t coherent_off_pages;
	size_t coherent_pages;
	size_t coherent_cursor;
	size_t rx_off_pages;
	size_t rx_pages;
	size_t rx_cursor;
	size_t tx_bounce_off_pages;
	size_t tx_bounce_pages;
	size_t tx_bounce_cursor;
	struct page *vmemmap;
	uint8_t *page_perms;
	struct dry_dma_stats stats;
};

typedef uint32_t (*dry_mmio_read32_fn)(void *opaque, uint32_t reg);
typedef void (*dry_mmio_write32_fn)(void *opaque, uint32_t reg, uint32_t val);

struct dry_mmio_region {
	uint8_t *vaddr;
	size_t size;
	bool backing_is_mmap;
	dry_mmio_read32_fn sim_read32;
	dry_mmio_write32_fn sim_write32;
	void *sim_opaque;
	uint64_t read_ops;
	uint64_t write_ops;
};

struct dry_irq_slot {
	bool allocated;
	unsigned int irq_num;
	int event_fd;
	int (*primary_fn)(int irq, void *dev_id);
	int (*thread_fn)(int irq, void *dev_id);
	void *dev_id;
	char name[32];
	uint64_t trigger_count;
};

struct dry_pkt_slot {
	uint16_t len;
	uint16_t queue_idx;
	uint16_t protocol_be;
	uint8_t data[DRY_MAX_PKT];
};

struct dry_io_ring {
	struct dry_pkt_slot rx_pkts[DRY_IORING_CAP];
	uint32_t rx_head;
	uint32_t rx_tail;
	uint64_t rx_total_packets;
	uint64_t rx_total_bytes;
};

struct dry_island_ctx {
	struct dry_dma_arena arena;
	struct dry_mmio_region bar0;
	struct dry_irq_slot irqs[DRY_MAX_MSIX];
	struct dry_io_ring ioring;
	struct pci_driver *registered_drv;
	struct net_device *registered_netdev;
};

extern struct dry_island_ctx g_dry_island;

int dry_dma_arena_init(struct dry_dma_arena *arena, uint64_t base_iova,
		       size_t coherent_pages, size_t rx_pages,
		       size_t tx_bounce_pages);
void dry_dma_arena_destroy(struct dry_dma_arena *arena);
void *dry_dma_iova_to_va(struct dry_dma_arena *arena, uint64_t iova,
			 size_t len, enum dry_dma_perm required_perm);

int dry_mmio_init_sim(struct dry_mmio_region *mmio, size_t size,
		      dry_mmio_read32_fn rd_fn, dry_mmio_write32_fn wr_fn,
		      void *opaque);
void dry_mmio_destroy(struct dry_mmio_region *mmio);
uint32_t dry_mmio_read32(const volatile void *addr);
void dry_mmio_write32(uint32_t val, volatile void *addr);

int dry_irq_alloc_vectors(unsigned int count, unsigned int base_irq);
void dry_irq_free_all(void);
int dry_irq_trigger(unsigned int irq_num);
int dry_island_poll_once(int budget);

bool dry_ioring_pop_rx(struct dry_io_ring *ring, struct dry_pkt_slot *out);

#ifdef __cplusplus
}
#endif

#endif /* DRY_ISLAND_H */

