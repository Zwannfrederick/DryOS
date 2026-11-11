/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * DRY OS — H2 Userspace BAR0 MMIO, PCI Driver Registry, and `eventfd` MSI-X IRQ Loop
 */
#include <sys/eventfd.h>
#include <sys/mman.h>
#include "../include/lx_emul/lx_net.h"

struct dry_island_ctx g_dry_island;
bool g_dry_fast_delays = true;

int dry_mmio_init_sim(struct dry_mmio_region *mmio, size_t size,
		      dry_mmio_read32_fn rd_fn, dry_mmio_write32_fn wr_fn,
		      void *opaque)
{
	memset(mmio, 0, sizeof(*mmio));
	void *mem = mmap(NULL, size, PROT_READ | PROT_WRITE,
			 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (mem == MAP_FAILED)
		return -ENOMEM;
	mmio->vaddr = (uint8_t *)mem;
	mmio->size = size;
	mmio->backing_is_mmap = false;
	mmio->sim_read32 = rd_fn;
	mmio->sim_write32 = wr_fn;
	mmio->sim_opaque = opaque;
	return 0;
}

void dry_mmio_destroy(struct dry_mmio_region *mmio)
{
	if (mmio->vaddr) {
		munmap(mmio->vaddr, mmio->size);
		mmio->vaddr = NULL;
	}
}

uint32_t dry_mmio_read32(const volatile void *addr)
{
	struct dry_mmio_region *bar = &g_dry_island.bar0;
	bar->read_ops++;
	if (bar->vaddr &&
	    (const uint8_t *)addr >= bar->vaddr &&
	    (const uint8_t *)addr + 4 <= bar->vaddr + bar->size) {
		uint32_t reg = (uint32_t)((const uint8_t *)addr - bar->vaddr);
		if (!bar->backing_is_mmap && bar->sim_read32)
			return bar->sim_read32(bar->sim_opaque, reg);
	}
	return *(const volatile uint32_t *)addr;
}

void dry_mmio_write32(uint32_t val, volatile void *addr)
{
	struct dry_mmio_region *bar = &g_dry_island.bar0;
	bar->write_ops++;
	if (bar->vaddr &&
	    (volatile uint8_t *)addr >= bar->vaddr &&
	    (volatile uint8_t *)addr + 4 <= bar->vaddr + bar->size) {
		uint32_t reg = (uint32_t)((volatile uint8_t *)addr - bar->vaddr);
		*(volatile uint32_t *)addr = val;
		if (!bar->backing_is_mmap && bar->sim_write32) {
			bar->sim_write32(bar->sim_opaque, reg, val);
			return;
		}
		return;
	}
	*(volatile uint32_t *)addr = val;
}

void __iomem *ioremap(resource_size_t offset, unsigned long size)
{
	(void)offset;
	if (g_dry_island.bar0.vaddr && size <= g_dry_island.bar0.size)
		return (void __iomem *)g_dry_island.bar0.vaddr;
	return NULL;
}

void iounmap(volatile void __iomem *addr)
{
	(void)addr;
}

void memcpy_fromio(void *to, const volatile void __iomem *from, size_t count)
{
	uint8_t *dst = (uint8_t *)to;
	const volatile uint8_t *src = (const volatile uint8_t *)from;
	for (size_t i = 0; i < count; i++)
		dst[i] = src[i];
}

int pci_register_driver(struct pci_driver *drv)
{
	g_dry_island.registered_drv = drv;
	return 0;
}

void pci_unregister_driver(struct pci_driver *drv)
{
	if (g_dry_island.registered_drv == drv)
		g_dry_island.registered_drv = NULL;
}

int dry_irq_alloc_vectors(unsigned int count, unsigned int base_irq)
{
	if (count > DRY_MAX_MSIX)
		count = DRY_MAX_MSIX;
	for (unsigned int i = 0; i < count; i++) {
		int efd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
		if (efd < 0)
			return -errno;
		g_dry_island.irqs[i].allocated = true;
		g_dry_island.irqs[i].irq_num = base_irq + i;
		g_dry_island.irqs[i].event_fd = efd;
		g_dry_island.irqs[i].primary_fn = NULL;
		g_dry_island.irqs[i].thread_fn = NULL;
		g_dry_island.irqs[i].dev_id = NULL;
		g_dry_island.irqs[i].trigger_count = 0;
	}
	return (int)count;
}

void dry_irq_free_all(void)
{
	for (unsigned int i = 0; i < DRY_MAX_MSIX; i++) {
		if (g_dry_island.irqs[i].allocated) {
			if (g_dry_island.irqs[i].event_fd >= 0)
				close(g_dry_island.irqs[i].event_fd);
			memset(&g_dry_island.irqs[i], 0, sizeof(g_dry_island.irqs[i]));
			g_dry_island.irqs[i].event_fd = -1;
		}
	}
}

int pci_enable_msix_range(struct pci_dev *dev, struct msix_entry *entries,
			  int minvec, int maxvec)
{
	if (maxvec < minvec || minvec <= 0)
		return -EINVAL;
	int nvec = dry_irq_alloc_vectors((unsigned int)maxvec, 100U);
	if (nvec < minvec)
		return -ENOSPC;
	for (int i = 0; i < nvec; i++)
		entries[i].vector = g_dry_island.irqs[i].irq_num;
	dev->msix_enabled = true;
	return nvec;
}

void pci_disable_msix(struct pci_dev *dev)
{
	dev->msix_enabled = false;
	dry_irq_free_all();
}

int pci_enable_msi(struct pci_dev *dev)
{
	int nvec = dry_irq_alloc_vectors(1, 90U);
	if (nvec < 1)
		return nvec;
	dev->irq = g_dry_island.irqs[0].irq_num;
	dev->msi_enabled = true;
	return 0;
}

void pci_disable_msi(struct pci_dev *dev)
{
	dev->msi_enabled = false;
	dry_irq_free_all();
}

int request_threaded_irq(unsigned int irq, irq_handler_t handler,
			 irq_handler_t thread_fn, unsigned long flags,
			 const char *name, void *dev)
{
	(void)flags;
	for (unsigned int i = 0; i < DRY_MAX_MSIX; i++) {
		if (g_dry_island.irqs[i].allocated && g_dry_island.irqs[i].irq_num == irq) {
			g_dry_island.irqs[i].primary_fn = handler;
			g_dry_island.irqs[i].thread_fn = thread_fn;
			g_dry_island.irqs[i].dev_id = dev;
			if (name)
				strscpy(g_dry_island.irqs[i].name, name, sizeof(g_dry_island.irqs[i].name));
			return 0;
		}
	}
	return -ENOENT;
}

const void *free_irq(unsigned int irq, void *dev_id)
{
	for (unsigned int i = 0; i < DRY_MAX_MSIX; i++) {
		if (g_dry_island.irqs[i].allocated &&
		    g_dry_island.irqs[i].irq_num == irq &&
		    g_dry_island.irqs[i].dev_id == dev_id) {
			g_dry_island.irqs[i].primary_fn = NULL;
			g_dry_island.irqs[i].thread_fn = NULL;
			g_dry_island.irqs[i].dev_id = NULL;
			return dev_id;
		}
	}
	return NULL;
}

void synchronize_irq(unsigned int irq)
{
	(void)irq;
}

int dry_irq_trigger(unsigned int irq_num)
{
	for (unsigned int i = 0; i < DRY_MAX_MSIX; i++) {
		if (g_dry_island.irqs[i].allocated && g_dry_island.irqs[i].irq_num == irq_num) {
			uint64_t val = 1;
			ssize_t wr = write(g_dry_island.irqs[i].event_fd, &val, sizeof(val));
			return (wr == (ssize_t)sizeof(val)) ? 0 : -EIO;
		}
	}
	return -ENOENT;
}

int dry_island_poll_once(int budget)
{
	int total_work = 0;

	/* Step 1: Drain `eventfd` MSI-X notifications and invoke IRQ handlers */
	for (unsigned int i = 0; i < DRY_MAX_MSIX; i++) {
		struct dry_irq_slot *slot = &g_dry_island.irqs[i];
		if (!slot->allocated || slot->event_fd < 0)
			continue;
		uint64_t hits = 0;
		if (read(slot->event_fd, &hits, sizeof(hits)) == (ssize_t)sizeof(hits) && hits > 0) {
			slot->trigger_count += hits;
			int ret = IRQ_NONE;
			if (slot->primary_fn)
				ret = slot->primary_fn((int)slot->irq_num, slot->dev_id);
			if (ret == IRQ_WAKE_THREAD && slot->thread_fn)
				slot->thread_fn((int)slot->irq_num, slot->dev_id);
		}
	}

	/* Step 2: Run scheduled NAPI poll loops on registered net_device */
	struct net_device *ndev = g_dry_island.registered_netdev;
	if (ndev) {
		for (unsigned int i = 0; i < ndev->napi_count; i++) {
			struct napi_struct *napi = ndev->napi_list[i];
			if (napi && napi->enabled && napi->scheduled && napi->poll) {
				napi->missed = false;
				total_work += napi->poll(napi, budget);
			}
		}
	}
	return total_work;
}
