/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * DRY OS — H2 Userspace `igc` Driver Island Adapter & Simulated I225 Verification
 *
 * Exercises:
 *   1. Persistent `DmaArena` + linear shadow `vmemmap` bijection, TX/RX permission
 *      separation, heap TX bounce-copy, and out-of-bounds / heap-RX rejection.
 *   2. Userspace BAR0 MMIO read/write interceptor + `eventfd` MSI-X IRQ & NAPI loop.
 *   3. Full unmodified upstream Linux `igc` (`v6.12` and `v6.18`) lifecycle:
 *      `module_init` -> `igc_probe` -> `igc_open` -> `ndo_start_xmit` (`igc_xmit_frame`)
 *      -> simulated Intel I225 DMA loopback + MSI-X `eventfd` -> `igc_msix_ring`
 *      -> `igc_poll` -> `napi_gro_receive` -> `dry_io_ring` RX verification
 *      -> `igc_close` -> `igc_remove` (`0` dynamic IOMMU syscalls).
 */
#include <assert.h>
#include "igc.h"

#define SIM_BAR0_SIZE 0x20000U

struct sim_i225_hw {
	uint32_t regs[SIM_BAR0_SIZE / 4];
	uint16_t phy_regs[32];
	uint64_t tx_frames_looped;
	uint64_t dma_perm_violations;
};

static uint32_t sim_i225_read32(void *opaque, uint32_t reg)
{
	struct sim_i225_hw *sim = (struct sim_i225_hw *)opaque;
	if (reg >= SIM_BAR0_SIZE)
		return 0;

	switch (reg) {
	case IGC_STATUS:
		/* Link Up + Full Duplex + 2500Mb/s; GIO_MASTER_ENABLE cleared */
		return IGC_STATUS_LU | IGC_STATUS_FD | IGC_STATUS_SPEED_2500;
	case IGC_I225_PHPM:
		return IGC_PHY_RST_COMP;
	case IGC_EECD:
		/* Flash not detected (skip 64KB NVM checksum scan) + AUTO_RD done */
		return IGC_EECD_AUTO_RD;
	case IGC_FWSM:
		return 0;
	default:
		return sim->regs[reg / 4];
	}
}

static void sim_i225_process_tx_queue0(struct sim_i225_hw *sim)
{
	uint64_t tdbal = sim->regs[IGC_TDBAL(0) / 4];
	uint64_t tdbah = sim->regs[IGC_TDBAH(0) / 4];
	uint32_t tdlen = sim->regs[IGC_TDLEN(0) / 4];
	uint32_t tdh   = sim->regs[IGC_TDH(0) / 4];
	uint32_t tdt   = sim->regs[IGC_TDT(0) / 4];
	if (tdlen == 0)
		return;
	uint32_t tcount = tdlen / sizeof(union igc_adv_tx_desc);
	uint64_t td_base_iova = (tdbah << 32) | (tdbal & ~0xFULL);

	uint64_t rdbal = sim->regs[IGC_RDBAL(0) / 4];
	uint64_t rdbah = sim->regs[IGC_RDBAH(0) / 4];
	uint32_t rdlen = sim->regs[IGC_RDLEN(0) / 4];
	uint32_t rdh   = sim->regs[IGC_RDH(0) / 4];
	uint32_t rdt   = sim->regs[IGC_RDT(0) / 4];
	uint32_t rcount = rdlen ? (rdlen / sizeof(union igc_adv_rx_desc)) : 0;
	uint64_t rd_base_iova = (rdbah << 32) | (rdbal & ~0xFULL);

	uint8_t frame_buf[DRY_MAX_PKT];
	size_t frame_len = 0;

	while (tdh != tdt) {
		union igc_adv_tx_desc *txd = dry_dma_iova_to_va(
			&g_dry_island.arena,
			td_base_iova + (uint64_t)tdh * sizeof(*txd),
			sizeof(*txd),
			DRY_DMA_BIDIR);
		if (!txd) {
			sim->dma_perm_violations++;
			break;
		}

		uint32_t cmd = le32_to_cpu(txd->read.cmd_type_len);
		bool is_data = (cmd & IGC_ADVTXD_DTYP_DATA) == IGC_ADVTXD_DTYP_DATA;
		if (is_data) {
			uint16_t dlen = (uint16_t)(cmd & 0xFFFFU);
			uint64_t buf_iova = le64_to_cpu(txd->read.buffer_addr);
			const uint8_t *src = dry_dma_iova_to_va(
				&g_dry_island.arena, buf_iova, dlen, DRY_DMA_READ);
			if (!src) {
				sim->dma_perm_violations++;
			} else if (frame_len + dlen <= sizeof(frame_buf)) {
				memcpy(frame_buf + frame_len, src, dlen);
				frame_len += dlen;
			}
		}

		/* Mark TX descriptor Done */
		txd->wb.status = cpu_to_le32(IGC_TXD_STAT_DD);

		if (is_data && (cmd & IGC_ADVTXD_DCMD_EOP) && frame_len > 0 && rcount > 0 && rdh != rdt) {
			union igc_adv_rx_desc *rxd = dry_dma_iova_to_va(
				&g_dry_island.arena,
				rd_base_iova + (uint64_t)rdh * sizeof(*rxd),
				sizeof(*rxd),
				DRY_DMA_BIDIR);
			if (rxd) {
				uint64_t rx_buf_iova = le64_to_cpu(rxd->read.pkt_addr);
				uint8_t *rx_dst = dry_dma_iova_to_va(
					&g_dry_island.arena, rx_buf_iova, frame_len, DRY_DMA_WRITE);
				if (rx_dst) {
					memcpy(rx_dst, frame_buf, frame_len);
					memset(rxd, 0, sizeof(*rxd));
					rxd->wb.upper.length = cpu_to_le16((uint16_t)frame_len);
					rxd->wb.upper.status_error =
						cpu_to_le32(IGC_RXD_STAT_DD | IGC_RXD_STAT_EOP);
					rdh = (rdh + 1U) % rcount;
					sim->regs[IGC_RDH(0) / 4] = rdh;
					sim->tx_frames_looped++;
					/* Fire MSI-X vector 1 (`q_vector[0]` TxRx-0 = IRQ 101) */
					dry_irq_trigger(101U);
				} else {
					sim->dma_perm_violations++;
				}
			}
			frame_len = 0;
		}
		tdh = (tdh + 1U) % tcount;
	}
	sim->regs[IGC_TDH(0) / 4] = tdh;
}

static void sim_i225_write32(void *opaque, uint32_t reg, uint32_t val)
{
	struct sim_i225_hw *sim = (struct sim_i225_hw *)opaque;
	if (reg >= SIM_BAR0_SIZE)
		return;

	if (reg == IGC_MDIC) {
		uint32_t phy_reg = (val & IGC_MDIC_REG_MASK) >> IGC_MDIC_REG_SHIFT;
		if (val & IGC_MDIC_OP_READ) {
			uint16_t data = (phy_reg < 32) ? sim->phy_regs[phy_reg] : 0;
			sim->regs[IGC_MDIC / 4] = (val & ~0xFFFFU) | data | IGC_MDIC_READY;
		} else {
			if (phy_reg < 32)
				sim->phy_regs[phy_reg] = (uint16_t)(val & 0xFFFFU);
			sim->regs[IGC_MDIC / 4] = val | IGC_MDIC_READY;
		}
		return;
	}

	if (reg == IGC_EERD) {
		sim->regs[IGC_EERD / 4] = val | IGC_NVM_RW_REG_DONE;
		return;
	}

	sim->regs[reg / 4] = val;

	if (reg == IGC_TDT(0))
		sim_i225_process_tx_queue0(sim);
}

static void test_1_dma_arena_and_shadow_vmemmap(void)
{
	struct dry_dma_arena arena;
	assert(dry_dma_arena_init(&arena, 0x10000000ULL, 16, 32, 8) == 0);
	g_dry_island.arena = arena;

	/* 1a. Bijection `lx_alloc_arena_pages` <-> `page_address` <-> `virt_to_page` */
	struct page *pg = alloc_pages(GFP_KERNEL, 0);
	assert(pg != NULL);
	void *va = page_address(pg);
	assert(va != NULL);
	assert(virt_to_page(va) == pg);
	assert(virt_to_page((uint8_t *)va + 2047) == pg);

	/* 1b. RX page maps for `DMA_FROM_DEVICE` (`DRY_DMA_WRITE`), rejects `DMA_TO_DEVICE` */
	dma_addr_t rx_iova = dma_map_page(NULL, pg, 0, 2048, DMA_FROM_DEVICE);
	assert(rx_iova != DMA_MAPPING_ERROR);
	assert(dry_dma_iova_to_va(&g_dry_island.arena, rx_iova, 2048, DRY_DMA_WRITE) == va);
	assert(dry_dma_iova_to_va(&g_dry_island.arena, rx_iova, 2048, DRY_DMA_READ) == NULL);

	/* 1c. Arbitrary heap pointer rejected for `DMA_FROM_DEVICE`, bounced safely for `DMA_TO_DEVICE` */
	uint8_t *heap_buf = malloc(128);
	assert(heap_buf != NULL);
	memset(heap_buf, 0xA5, 128);
	assert(virt_to_page(heap_buf) == NULL);
	assert(dma_map_single(NULL, heap_buf, 128, DMA_FROM_DEVICE) == DMA_MAPPING_ERROR);

	dma_addr_t tx_iova = dma_map_single(NULL, heap_buf, 128, DMA_TO_DEVICE);
	assert(tx_iova != DMA_MAPPING_ERROR);
	const uint8_t *bounced = dry_dma_iova_to_va(&g_dry_island.arena, tx_iova, 128, DRY_DMA_READ);
	assert(bounced != NULL && memcmp(bounced, heap_buf, 128) == 0);
	assert(dry_dma_iova_to_va(&g_dry_island.arena, tx_iova, 128, DRY_DMA_WRITE) == NULL);

	/* 1d. Fake stack `struct page` outside `vmemmap` rejected */
	struct page fake_pg = { 0 };
	assert(dma_map_page(NULL, &fake_pg, 0, 64, DMA_FROM_DEVICE) == DMA_MAPPING_ERROR);
	assert(g_dry_island.arena.stats.rejected_oob_maps == 2);
	assert(g_dry_island.arena.stats.tx_bounce_copies == 1);
	assert(g_dry_island.arena.stats.dynamic_iommu_syscalls == 0);

	free(heap_buf);
	put_page(pg);
	dry_dma_arena_destroy(&g_dry_island.arena);
	memset(&g_dry_island, 0, sizeof(g_dry_island));
	printf("[PASS] Test 1: DmaArena + shadow vmemmap bijection, permissions, & OOB rejection\n");
}

static irqreturn_t dummy_irq_handler(int irq, void *dev_id)
{
	(void)irq;
	struct napi_struct *napi = (struct napi_struct *)dev_id;
	napi_schedule(napi);
	return IRQ_HANDLED;
}

static int dummy_napi_poll(struct napi_struct *napi, int budget)
{
	int *polled = (int *)napi->dev->priv;
	(*polled)++;
	napi_complete_done(napi, 1);
	return (budget > 0) ? 1 : 0;
}

static void test_2_mmio_and_eventfd_irq_napi_adapter(void)
{
	struct sim_i225_hw sim = { 0 };
	assert(dry_mmio_init_sim(&g_dry_island.bar0, SIM_BAR0_SIZE,
				 sim_i225_read32, sim_i225_write32, &sim) == 0);

	void __iomem *bar = ioremap(0xFE000000ULL, SIM_BAR0_SIZE);
	assert(bar != NULL);
	writel(0xDEADBEEFU, (uint8_t __iomem *)bar + IGC_CTRL);
	assert(readl((uint8_t __iomem *)bar + IGC_CTRL) == 0xDEADBEEFU);
	assert(g_dry_island.bar0.write_ops == 1 && g_dry_island.bar0.read_ops == 1);

	struct net_device *ndev = alloc_etherdev_mqs(sizeof(int), 1, 1);
	register_netdev(ndev);
	struct napi_struct napi;
	netif_napi_add(ndev, &napi, dummy_napi_poll);
	napi_enable(&napi);

	assert(dry_irq_alloc_vectors(2, 200U) == 2);
	assert(request_irq(200U, dummy_irq_handler, 0, "test-msix0", &napi) == 0);
	assert(dry_irq_trigger(200U) == 0);

	int work = dry_island_poll_once(64);
	assert(work == 1);
	assert(*(int *)ndev->priv == 1);
	assert(!napi.scheduled);

	dry_irq_free_all();
	unregister_netdev(ndev);
	free_netdev(ndev);
	dry_mmio_destroy(&g_dry_island.bar0);
	memset(&g_dry_island, 0, sizeof(g_dry_island));
	printf("[PASS] Test 2: BAR0 MMIO interceptor + eventfd MSI-X -> NAPI poll adapter\n");
}

static void test_3_upstream_igc_full_probe_open_tx_rx_loopback(void)
{
	struct sim_i225_hw sim = { 0 };
	/* Intel I225/I226 internal GPY PHY ID (`0x67C9DC00`) & Link Up in MII status */
	sim.phy_regs[MII_PHYSID1] = 0x67C9U;
	sim.phy_regs[MII_PHYSID2] = 0xDC00U;
	sim.phy_regs[MII_BMSR]    = 0x796DU;

	assert(dry_dma_arena_init(&g_dry_island.arena, 0x80000000ULL, 128, 1024, 64) == 0);
	assert(dry_mmio_init_sim(&g_dry_island.bar0, SIM_BAR0_SIZE,
				 sim_i225_read32, sim_i225_write32, &sim) == 0);

	/* 3a. Run real `igc_init_module` (`pci_register_driver(&igc_driver)`) */
	assert(dry_lx_module_init() == 0);
	assert(g_dry_island.registered_drv != NULL);

	struct pci_bus bus = { .number = 1 };
	struct pci_dev pdev = {
		.bus = &bus,
		.devfn = 0,
		.vendor = PCI_VENDOR_ID_INTEL,
		.device = IGC_DEV_ID_I225_V,
		.subsystem_vendor = PCI_VENDOR_ID_INTEL,
		.subsystem_device = 0x0001,
		.revision = 3,
		.resource = { [0] = { .start = 0xFE200000ULL, .end = 0xFE200000ULL + SIM_BAR0_SIZE - 1 } },
	};
	const u8 test_mac[ETH_ALEN] = { 0x02, 0x44, 0x52, 0x59, 0x22, 0x55 };
	memcpy(pdev.dev.mac_addr, test_mac, ETH_ALEN);
	pdev.dev.has_mac = true;

	/* Match `IGC_DEV_ID_I225_V` in upstream `igc_pci_tbl` and call `igc_probe` */
	const struct pci_device_id *id = g_dry_island.registered_drv->id_table;
	while (id->vendor && id->device != IGC_DEV_ID_I225_V)
		id++;
	assert(id->device == IGC_DEV_ID_I225_V);

	int err = g_dry_island.registered_drv->probe(&pdev, id);
	assert(err == 0);
	struct net_device *ndev = g_dry_island.registered_netdev;
	assert(ndev != NULL);
	assert(memcmp(ndev->dev_addr, test_mac, ETH_ALEN) == 0);

	/* 3b. Open interface (`igc_open`) -> allocates coherent TX/RX rings + RX arena pages */
	err = ndev->netdev_ops->ndo_open(ndev);
	assert(err == 0);
	ndev->running = true;
	netif_carrier_on(ndev);

	/* 3c. Transmit 3 Ethernet frames via real `ndo_start_xmit` (`igc_xmit_frame`) */
	for (unsigned int pkt_i = 0; pkt_i < 3; pkt_i++) {
		struct sk_buff *skb = netdev_alloc_skb_ip_align(ndev, 128);
		assert(skb != NULL);
		uint8_t *frame = skb_put(skb, 64);
		memset(frame, 0, 64);
		memcpy(frame + 0, test_mac, ETH_ALEN);
		memcpy(frame + 6, test_mac, ETH_ALEN);
		frame[12] = 0x08;
		frame[13] = 0x00; /* ETH_P_IP */
		for (size_t b = 14; b < 64; b++)
			frame[b] = (uint8_t)(0x30U + pkt_i + (b & 0x1FU));
		skb->protocol = htons(ETH_P_IP);
		skb_reset_mac_header(skb);
		skb->network_header = skb->mac_header + ETH_HLEN;

		netdev_tx_t tx_rc = ndev->netdev_ops->ndo_start_xmit(skb, ndev);
		assert(tx_rc == NETDEV_TX_OK);

		/* Drain MSI-X `eventfd` -> `igc_msix_ring` -> `igc_poll` -> `dry_io_ring` */
		int polled = dry_island_poll_once(64);
		assert(polled >= 1);

		struct dry_pkt_slot rx_pkt;
		assert(dry_ioring_pop_rx(&g_dry_island.ioring, &rx_pkt));
		assert(rx_pkt.len == 64);
		assert(rx_pkt.protocol_be == htons(ETH_P_IP));
		assert(rx_pkt.data[14] == (uint8_t)(0x30U + pkt_i + (14U & 0x1FU)));
	}

	assert(sim.tx_frames_looped == 3);
	assert(sim.dma_perm_violations == 0);
	assert(g_dry_island.ioring.rx_total_packets == 3);
	assert(g_dry_island.arena.stats.dynamic_iommu_syscalls == 0);

	uint64_t coherent_allocs = g_dry_island.arena.stats.coherent_allocs;
	uint64_t rx_page_allocs  = g_dry_island.arena.stats.rx_page_allocs;
	uint64_t tx_bounces      = g_dry_island.arena.stats.tx_bounce_copies;
	uint64_t mmio_reads      = g_dry_island.bar0.read_ops;
	uint64_t mmio_writes     = g_dry_island.bar0.write_ops;

	/* 3d. Clean teardown: `ndo_stop` (`igc_close`) + `remove` (`igc_remove`) + `module_exit` */
	assert(ndev->netdev_ops->ndo_stop(ndev) == 0);
	ndev->running = false;
	g_dry_island.registered_drv->remove(&pdev);
	dry_lx_module_exit();

	dry_irq_free_all();
	dry_mmio_destroy(&g_dry_island.bar0);
	dry_dma_arena_destroy(&g_dry_island.arena);

	printf("[PASS] Test 3: Upstream igc probe+open+3x(TX->DMA->MSIX->NAPI->IoRing RX)+close "
	       "(coherent=%lu rx_pages=%lu tx_bounce=%lu mmio_r=%lu mmio_w=%lu dyn_iommu=0)\n",
	       (unsigned long)coherent_allocs, (unsigned long)rx_page_allocs,
	       (unsigned long)tx_bounces, (unsigned long)mmio_reads,
	       (unsigned long)mmio_writes);
}

int main(void)
{
	test_1_dma_arena_and_shadow_vmemmap();
	test_2_mmio_and_eventfd_irq_napi_adapter();
	test_3_upstream_igc_full_probe_open_tx_rx_loopback();
	return 0;
}
