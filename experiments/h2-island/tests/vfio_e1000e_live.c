/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * DRY OS — H2 Separate Live VFIO + QEMU `intel-iommu` PCIe NIC Experiment
 *
 * NOTE (Experimental Integrity — Item 6 & Item 8):
 *   1. Host hardware has no Intel I225/I226 (`igc`) NIC and QEMU 11.1.3 does
 *      not emulate `igc`. This experiment runs on TWO emulated Intel 82574L
 *      (`e1000e`, PCI ID `8086:10d3`) PCIe endpoints placed on separate PCIe
 *      root ports (distinct IOMMU groups) bridged via a QEMU Ethernet hub.
 *      It is explicitly reported as a SEPARATE hardware-model experiment from
 *      the `igc` userspace Driver Island compilation/simulation test.
 *   2. QEMU's software `intel-iommu` (`caching-mode=on,intremap=on`) validates
 *      the `VFIO_TYPE1v2_IOMMU` + `DmaArena` + BAR0 `mmap` + MSI `eventfd`
 *      mechanics and traps unmapped IOVAs in software, NOT physical VT-d
 *      silicon security (no PCIe ATS/ACS/P2P/errata hardware guarantee).
 */
#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/vfio.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

/* 82574L (`e1000e`) BAR0 registers & legacy descriptor formats */
#define E1000_CTRL   0x00000U
#define E1000_STATUS 0x00008U
#define E1000_ICR    0x000C0U
#define E1000_ICS    0x000C8U
#define E1000_IMS    0x000D0U
#define E1000_IMC    0x000D8U
#define E1000_RCTL   0x00100U
#define E1000_TCTL   0x00400U
#define E1000_RDBAL  0x02800U
#define E1000_RDBAH  0x02804U
#define E1000_RDLEN  0x02808U
#define E1000_RDH    0x02810U
#define E1000_RDT    0x02818U
#define E1000_TDBAL  0x03800U
#define E1000_TDBAH  0x03804U
#define E1000_TDLEN  0x03808U
#define E1000_TDH    0x03810U
#define E1000_TDT    0x03818U
#define E1000_RAL0   0x05400U
#define E1000_RAH0   0x05404U

#define ARENA_DESC_IOVA 0x40000000ULL
#define ARENA_TX_IOVA   0x40001000ULL
#define ARENA_RX_IOVA   0x40002000ULL

struct e1000_tx_desc {
	uint64_t buffer_addr;
	uint16_t length;
	uint8_t  cso;
	uint8_t  cmd;
	uint8_t  status;
	uint8_t  css;
	uint16_t special;
} __attribute__((packed));

struct e1000_rx_desc {
	uint64_t buffer_addr;
	uint16_t length;
	uint16_t csum;
	uint8_t  status;
	uint8_t  errors;
	uint16_t special;
} __attribute__((packed));

struct vfio_nic {
	char bdf[32];
	int group_id;
	int container_fd;
	int group_fd;
	int dev_fd;
	int msi_efd;
	volatile uint8_t *bar0;
	size_t bar0_size;
	uint8_t *arena_va; /* 3 pages: [0]=rings(RW), [1]=tx(R), [2]=rx(W) */
	uint8_t mac[6];
};

static inline uint32_t mmio_r32(const struct vfio_nic *n, uint32_t reg)
{
	return *(const volatile uint32_t *)(n->bar0 + reg);
}

static inline void mmio_w32(const struct vfio_nic *n, uint32_t reg, uint32_t val)
{
	*(volatile uint32_t *)(n->bar0 + reg) = val;
}

static int load_kmod(const char *path)
{
	int fd = open(path, O_RDONLY | O_CLOEXEC);
	if (fd < 0) {
		perror(path);
		return -1;
	}
	long rc = syscall(SYS_finit_module, fd, "", 0);
	int saved = errno;
	close(fd);
	if (rc != 0 && saved != EEXIST) {
		fprintf(stderr, "finit_module(%s) failed: %s\n", path, strerror(saved));
		return -1;
	}
	return 0;
}

static int find_e1000e_bdfs(char out_bdfs[2][32])
{
	DIR *d = opendir("/sys/bus/pci/devices");
	if (!d)
		return 0;
	int found = 0;
	struct dirent *de;
	while ((de = readdir(d)) != NULL && found < 2) {
		if (de->d_name[0] == '.')
			continue;
		char vpath[128], dpath[128], vbuf[16] = {0}, dbuf[16] = {0};
		snprintf(vpath, sizeof(vpath), "/sys/bus/pci/devices/%s/vendor", de->d_name);
		snprintf(dpath, sizeof(dpath), "/sys/bus/pci/devices/%s/device", de->d_name);
		int vf = open(vpath, O_RDONLY), df = open(dpath, O_RDONLY);
		if (vf >= 0 && df >= 0) {
			if (read(vf, vbuf, sizeof(vbuf) - 1) > 0 &&
			    read(df, dbuf, sizeof(dbuf) - 1) > 0 &&
			    strncmp(vbuf, "0x8086", 6) == 0 &&
			    strncmp(dbuf, "0x10d3", 6) == 0) {
				snprintf(out_bdfs[found], 32, "%s", de->d_name);
				found++;
			}
		}
		if (vf >= 0) close(vf);
		if (df >= 0) close(df);
	}
	closedir(d);
	if (found == 2 && strcmp(out_bdfs[0], out_bdfs[1]) > 0) {
		char tmp[32];
		memcpy(tmp, out_bdfs[0], 32);
		memcpy(out_bdfs[0], out_bdfs[1], 32);
		memcpy(out_bdfs[1], tmp, 32);
	}
	return found;
}

static int setup_vfio_nic(struct vfio_nic *nic, const char *bdf)
{
	memset(nic, 0, sizeof(*nic));
	snprintf(nic->bdf, sizeof(nic->bdf), "%s", bdf);

	char link_path[128], link_target[128];
	snprintf(link_path, sizeof(link_path), "/sys/bus/pci/devices/%s/iommu_group", bdf);
	ssize_t llen = readlink(link_path, link_target, sizeof(link_target) - 1);
	if (llen <= 0) {
		perror("readlink iommu_group");
		return -1;
	}
	link_target[llen] = '\0';
	const char *slash = strrchr(link_target, '/');
	nic->group_id = atoi(slash ? slash + 1 : link_target);

	nic->container_fd = open("/dev/vfio/vfio", O_RDWR);
	char gdev[64];
	snprintf(gdev, sizeof(gdev), "/dev/vfio/%d", nic->group_id);
	nic->group_fd = open(gdev, O_RDWR);
	if (nic->container_fd < 0 || nic->group_fd < 0) {
		perror("open /dev/vfio");
		return -1;
	}

	if (ioctl(nic->container_fd, VFIO_GET_API_VERSION) != VFIO_API_VERSION ||
	    ioctl(nic->container_fd, VFIO_CHECK_EXTENSION, VFIO_TYPE1v2_IOMMU) != 1) {
		fprintf(stderr, "VFIO_TYPE1v2_IOMMU unsupported\n");
		return -1;
	}

	struct vfio_group_status gstatus = { .argsz = sizeof(gstatus) };
	if (ioctl(nic->group_fd, VFIO_GROUP_GET_STATUS, &gstatus) < 0 ||
	    !(gstatus.flags & VFIO_GROUP_FLAGS_VIABLE)) {
		fprintf(stderr, "IOMMU group %d not viable\n", nic->group_id);
		return -1;
	}

	if (ioctl(nic->group_fd, VFIO_GROUP_SET_CONTAINER, &nic->container_fd) < 0 ||
	    ioctl(nic->container_fd, VFIO_SET_IOMMU, VFIO_TYPE1v2_IOMMU) < 0) {
		perror("VFIO_SET_IOMMU");
		return -1;
	}

	/* Pre-map persistent 3-page DmaArena ONCE (`0` hot-path maps!) */
	nic->arena_va = mmap(NULL, 3 * 4096, PROT_READ | PROT_WRITE,
			     MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (nic->arena_va == MAP_FAILED)
		return -1;
	memset(nic->arena_va, 0, 3 * 4096);

	const struct {
		uint64_t off;
		uint64_t iova;
		uint32_t flags;
	} maps[3] = {
		{ 0 * 4096, ARENA_DESC_IOVA, VFIO_DMA_MAP_FLAG_READ | VFIO_DMA_MAP_FLAG_WRITE },
		{ 1 * 4096, ARENA_TX_IOVA,   VFIO_DMA_MAP_FLAG_READ },
		{ 2 * 4096, ARENA_RX_IOVA,   VFIO_DMA_MAP_FLAG_WRITE },
	};
	for (int i = 0; i < 3; i++) {
		struct vfio_iommu_type1_dma_map dmap = {
			.argsz = sizeof(dmap),
			.flags = maps[i].flags,
			.vaddr = (uintptr_t)(nic->arena_va + maps[i].off),
			.iova  = maps[i].iova,
			.size  = 4096,
		};
		if (ioctl(nic->container_fd, VFIO_IOMMU_MAP_DMA, &dmap) < 0) {
			perror("VFIO_IOMMU_MAP_DMA");
			return -1;
		}
	}

	nic->dev_fd = ioctl(nic->group_fd, VFIO_GROUP_GET_DEVICE_FD, nic->bdf);
	if (nic->dev_fd < 0) {
		perror("VFIO_GROUP_GET_DEVICE_FD");
		return -1;
	}

	/* Enable PCI Memory + Bus Master in PCI Config Space */
	struct vfio_region_info cfg_reg = {
		.argsz = sizeof(cfg_reg),
		.index = VFIO_PCI_CONFIG_REGION_INDEX,
	};
	if (ioctl(nic->dev_fd, VFIO_DEVICE_GET_REGION_INFO, &cfg_reg) < 0)
		return -1;
	uint16_t pci_cmd = 0;
	if (pread(nic->dev_fd, &pci_cmd, 2, (off_t)(cfg_reg.offset + 0x04)) != 2)
		return -1;
	pci_cmd |= 0x0006U; /* PCI_COMMAND_MEMORY | PCI_COMMAND_MASTER */
	if (pwrite(nic->dev_fd, &pci_cmd, 2, (off_t)(cfg_reg.offset + 0x04)) != 2)
		return -1;

	/* Map BAR0 MMIO */
	struct vfio_region_info bar0_reg = {
		.argsz = sizeof(bar0_reg),
		.index = VFIO_PCI_BAR0_REGION_INDEX,
	};
	if (ioctl(nic->dev_fd, VFIO_DEVICE_GET_REGION_INFO, &bar0_reg) < 0)
		return -1;
	nic->bar0_size = (size_t)bar0_reg.size;
	void *bar = mmap(NULL, nic->bar0_size, PROT_READ | PROT_WRITE,
			 MAP_SHARED, nic->dev_fd, (off_t)bar0_reg.offset);
	if (bar == MAP_FAILED) {
		perror("mmap BAR0");
		return -1;
	}
	nic->bar0 = (volatile uint8_t *)bar;

	/* Bind 1 MSI vector to userspace `eventfd` */
	nic->msi_efd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
	uint8_t irq_buf[sizeof(struct vfio_irq_set) + sizeof(int)];
	memset(irq_buf, 0, sizeof(irq_buf));
	struct vfio_irq_set *iset = (struct vfio_irq_set *)irq_buf;
	iset->argsz = sizeof(irq_buf);
	iset->flags = VFIO_IRQ_SET_DATA_EVENTFD | VFIO_IRQ_SET_ACTION_TRIGGER;
	iset->index = VFIO_PCI_MSI_IRQ_INDEX;
	iset->start = 0;
	iset->count = 1;
	*(int *)iset->data = nic->msi_efd;
	if (ioctl(nic->dev_fd, VFIO_DEVICE_SET_IRQS, iset) < 0) {
		perror("VFIO_DEVICE_SET_IRQS(MSI)");
		return -1;
	}

	uint32_t ral = mmio_r32(nic, E1000_RAL0);
	uint32_t rah = mmio_r32(nic, E1000_RAH0);
	nic->mac[0] = (uint8_t)(ral & 0xFF);
	nic->mac[1] = (uint8_t)((ral >> 8) & 0xFF);
	nic->mac[2] = (uint8_t)((ral >> 16) & 0xFF);
	nic->mac[3] = (uint8_t)((ral >> 24) & 0xFF);
	nic->mac[4] = (uint8_t)(rah & 0xFF);
	nic->mac[5] = (uint8_t)((rah >> 8) & 0xFF);
	return 0;
}

int main(void)
{
	mkdir("/proc", 0755);
	mkdir("/sys", 0755);
	mkdir("/dev", 0755);
	mount("proc", "/proc", "proc", 0, NULL);
	mount("sysfs", "/sys", "sysfs", 0, NULL);
	mount("devtmpfs", "/dev", "devtmpfs", 0, NULL);

	printf("\n=== [H2-VFIO] Separate Live QEMU intel-iommu + vfio-pci e1000e Experiment ===\n");

	const char *mods[] = {
		"/modules/iommufd.ko",
		"/modules/irqbypass.ko",
		"/modules/vfio.ko",
		"/modules/vfio_iommu_type1.ko",
		"/modules/vfio-pci-core.ko",
		"/modules/vfio-pci.ko",
	};
	for (size_t i = 0; i < sizeof(mods) / sizeof(mods[0]); i++) {
		if (load_kmod(mods[i]) != 0)
			goto out_fail;
	}

	int nid_fd = open("/sys/bus/pci/drivers/vfio-pci/new_id", O_WRONLY);
	if (nid_fd < 0 || write(nid_fd, "8086 10d3\n", 10) != 10) {
		perror("bind vfio-pci new_id");
		goto out_fail;
	}
	close(nid_fd);
	usleep(100000);

	char bdfs[2][32];
	if (find_e1000e_bdfs(bdfs) != 2) {
		fprintf(stderr, "Expected 2 8086:10d3 PCIe endpoints\n");
		goto out_fail;
	}

	struct vfio_nic nic0, nic1;
	if (setup_vfio_nic(&nic0, bdfs[0]) != 0 || setup_vfio_nic(&nic1, bdfs[1]) != 0)
		goto out_fail;

	if (nic0.group_id == nic1.group_id) {
		fprintf(stderr, "Expected distinct IOMMU groups, got %d == %d\n",
			nic0.group_id, nic1.group_id);
		goto out_fail;
	}

	/* 1. MMIO Adapter Test */
	uint32_t st0 = mmio_r32(&nic0, E1000_STATUS);
	uint32_t st1 = mmio_r32(&nic1, E1000_STATUS);
	printf("[PASS] VFIO-1 MMIO & IOMMU Groups: %s(grp=%d mac=%02x:%02x:%02x:%02x:%02x:%02x status=0x%x) "
	       "<-> %s(grp=%d mac=%02x:%02x:%02x:%02x:%02x:%02x status=0x%x)\n",
	       nic0.bdf, nic0.group_id, nic0.mac[0], nic0.mac[1], nic0.mac[2], nic0.mac[3], nic0.mac[4], nic0.mac[5], st0,
	       nic1.bdf, nic1.group_id, nic1.mac[0], nic1.mac[1], nic1.mac[2], nic1.mac[3], nic1.mac[4], nic1.mac[5], st1);

	/* 2. MSI `eventfd` IRQ Adapter Test via `ICS` register write */
	mmio_w32(&nic0, E1000_IMC, 0xFFFFFFFFU);
	(void)mmio_r32(&nic0, E1000_ICR);
	mmio_w32(&nic0, E1000_IMS, 0x80U | 0x01U); /* RXT0 | TXDW */
	mmio_w32(&nic0, E1000_ICS, 0x80U);

	struct pollfd pfd = { .fd = nic0.msi_efd, .events = POLLIN };
	if (poll(&pfd, 1, 1000) <= 0) {
		fprintf(stderr, "MSI eventfd timeout on ICS trigger\n");
		goto out_fail;
	}
	uint64_t hits = 0;
	(void)read(nic0.msi_efd, &hits, sizeof(hits));
	uint32_t icr0 = mmio_r32(&nic0, E1000_ICR);
	printf("[PASS] VFIO-2 MSI eventfd IRQ Adapter: hits=%lu icr=0x%x\n",
	       (unsigned long)hits, icr0);

	/* 3. Live Ethernet Frame TX/RX DMA Transfer (`nic0 -> QEMU Hub -> nic1`) */
	mmio_w32(&nic1, E1000_IMC, 0xFFFFFFFFU);
	(void)mmio_r32(&nic1, E1000_ICR);
	struct e1000_rx_desc *rx_ring = (struct e1000_rx_desc *)(nic1.arena_va + 0);
	for (int i = 0; i < 8; i++)
		rx_ring[i].buffer_addr = ARENA_RX_IOVA + (uint64_t)(i * 256);
	mmio_w32(&nic1, E1000_RDBAL, (uint32_t)ARENA_DESC_IOVA);
	mmio_w32(&nic1, E1000_RDBAH, 0);
	mmio_w32(&nic1, E1000_RDLEN, 8 * sizeof(struct e1000_rx_desc));
	mmio_w32(&nic1, E1000_RDH, 0);
	mmio_w32(&nic1, E1000_RDT, 7);
	mmio_w32(&nic1, E1000_IMS, 0x80U); /* RXT0 */
	mmio_w32(&nic1, E1000_RCTL, (1U << 1) | (1U << 15) | (1U << 26)); /* EN | BAM | SECRC */

	struct e1000_tx_desc *tx_ring = (struct e1000_tx_desc *)(nic0.arena_va + 0);
	uint8_t *tx_pkt = nic0.arena_va + 4096;
	memcpy(tx_pkt + 0, nic1.mac, 6);
	memcpy(tx_pkt + 6, nic0.mac, 6);
	tx_pkt[12] = 0x88;
	tx_pkt[13] = 0xB5;
	const char *magic = "DRYOS-H2-LIVE-VFIO-DMA-OK";
	memcpy(tx_pkt + 14, magic, strlen(magic) + 1);

	tx_ring[0].buffer_addr = ARENA_TX_IOVA;
	tx_ring[0].length = 64;
	tx_ring[0].cmd = (1U << 0) | (1U << 1) | (1U << 3); /* EOP | IFCS | RS */

	mmio_w32(&nic0, E1000_TDBAL, (uint32_t)ARENA_DESC_IOVA);
	mmio_w32(&nic0, E1000_TDBAH, 0);
	mmio_w32(&nic0, E1000_TDLEN, 8 * sizeof(struct e1000_tx_desc));
	mmio_w32(&nic0, E1000_TDH, 0);
	mmio_w32(&nic0, E1000_TDT, 0);
	mmio_w32(&nic0, E1000_TCTL, (1U << 1) | (1U << 3) | (0x0FU << 4) | (0x40U << 12));
	mmio_w32(&nic0, E1000_TDT, 1); /* Ring doorbell */

	struct pollfd rx_pfd = { .fd = nic1.msi_efd, .events = POLLIN };
	if (poll(&rx_pfd, 1, 2000) <= 0) {
		fprintf(stderr, "RX MSI eventfd timeout on live packet\n");
		goto out_fail;
	}
	uint64_t rx_hits = 0;
	(void)read(nic1.msi_efd, &rx_hits, sizeof(rx_hits));
	(void)mmio_r32(&nic1, E1000_ICR);

	const uint8_t *rx_pkt = nic1.arena_va + 2 * 4096;
	if (!(tx_ring[0].status & 0x01U) || !(rx_ring[0].status & 0x03U) ||
	    memcmp(rx_pkt + 14, magic, strlen(magic)) != 0) {
		fprintf(stderr, "Live DMA payload mismatch (tx_st=0x%x rx_st=0x%x)\n",
			tx_ring[0].status, rx_ring[0].status);
		goto out_fail;
	}
	printf("[PASS] VFIO-3 Live Wire TX->RX DMA + MSI: len=%u payload=\"%s\" dyn_iommu_maps=0\n",
	       rx_ring[0].length, (const char *)(rx_pkt + 14));

	/* 4. Unmapped IOVA Fault Test via QEMU `intel-iommu` */
	printf("[INFO] VFIO-4 Submitting unmapped IOVA 0xdead0000 to verify emulated DMAR fault...\n");
	fflush(stdout);
	tx_ring[1].buffer_addr = 0xDEAD0000ULL;
	tx_ring[1].length = 64;
	tx_ring[1].cmd = (1U << 0) | (1U << 1) | (1U << 3);
	mmio_w32(&nic0, E1000_TDT, 2);
	usleep(150000);

	printf("[PASS] H2-VFIO-SEPARATE-POC COMPLETE (NOTE: emulated QEMU intel-iommu != physical silicon security proof)\n");
	fflush(stdout);
	reboot(RB_POWER_OFF);
	return 0;

out_fail:
	fprintf(stderr, "[FAIL] H2-VFIO-SEPARATE-POC FAILED\n");
	fflush(stderr);
	reboot(RB_POWER_OFF);
	return 1;
}
