# SPDX-License-Identifier: GPL-2.0-only
CC      := clang
CFLAGS  := -std=gnu11 -O2 -g -Wall -Wextra -Wno-unused-parameter -Wno-sign-compare \
           -Wno-division-by-zero -Wno-pointer-sign -Wno-unused-but-set-variable \
           -Wno-unused-const-variable -pthread
LDFLAGS := -pthread

ROOT_DIR := ../..
V618_DIR := $(ROOT_DIR)/vendor/linux-drivers/v6.18/igc
V612_DIR := $(ROOT_DIR)/vendor/linux-drivers/v6.12/igc

ISLAND_SRCS := src/dma_arena.c src/lx_mmio_irq.c src/lx_net_shim.c src/igc_pruned_stubs.c
IGC_TUS     := igc_base.c igc_mac.c igc_nvm.c igc_phy.c igc_i225.c igc_main.c

V618_IGC_SRCS := $(addprefix $(V618_DIR)/,$(IGC_TUS))
V612_IGC_SRCS := $(addprefix $(V612_DIR)/,$(IGC_TUS))

KVER := 6.18.60-1-lts
KMOD := /lib/modules/$(KVER)/kernel

.PHONY: all test vfio-test clean

all: build/test_igc_618 build/test_igc_612 build/vfio_e1000e_live

build/test_igc_618: $(ISLAND_SRCS) tests/test_h2_igc_island.c $(V618_IGC_SRCS)
	@mkdir -p build
	$(CC) $(CFLAGS) -Iinclude -I$(V618_DIR) $(ISLAND_SRCS) $(V618_IGC_SRCS) tests/test_h2_igc_island.c $(LDFLAGS) -o $@

build/test_igc_612: $(ISLAND_SRCS) tests/test_h2_igc_island.c $(V612_IGC_SRCS)
	@mkdir -p build
	$(CC) $(CFLAGS) -Iinclude -I$(V612_DIR) $(ISLAND_SRCS) $(V612_IGC_SRCS) tests/test_h2_igc_island.c $(LDFLAGS) -o $@

build/vfio_e1000e_live: tests/vfio_e1000e_live.c
	@mkdir -p build
	$(CC) -std=gnu11 -O2 -Wall -Wextra -Werror -static tests/vfio_e1000e_live.c -o $@

build/vfio_initramfs.cpio: build/vfio_e1000e_live
	@rm -rf build/vfio_root && mkdir -p build/vfio_root/modules
	cp build/vfio_e1000e_live build/vfio_root/init
	chmod +x build/vfio_root/init
	zstd -d -f -q $(KMOD)/drivers/iommu/iommufd/iommufd.ko.zst -o build/vfio_root/modules/iommufd.ko
	zstd -d -f -q $(KMOD)/virt/lib/irqbypass.ko.zst -o build/vfio_root/modules/irqbypass.ko
	zstd -d -f -q $(KMOD)/drivers/vfio/vfio.ko.zst -o build/vfio_root/modules/vfio.ko
	zstd -d -f -q $(KMOD)/drivers/vfio/vfio_iommu_type1.ko.zst -o build/vfio_root/modules/vfio_iommu_type1.ko
	zstd -d -f -q $(KMOD)/drivers/vfio/pci/vfio-pci-core.ko.zst -o build/vfio_root/modules/vfio-pci-core.ko
	zstd -d -f -q $(KMOD)/drivers/vfio/pci/vfio-pci.ko.zst -o build/vfio_root/modules/vfio-pci.ko
	cd build/vfio_root && find . -print0 | cpio --null -o -H newc --quiet > ../vfio_initramfs.cpio

vfio-test: build/vfio_initramfs.cpio
	qemu-system-x86_64 \
	  -machine q35,accel=kvm,kernel-irqchip=split \
	  -cpu host -smp 2 -m 512M \
	  -device intel-iommu,intremap=on,caching-mode=on \
	  -netdev hubport,id=n0,hubid=0 \
	  -netdev hubport,id=n1,hubid=0 \
	  -device pcie-root-port,id=rp1,slot=1,chassis=1 \
	  -device pcie-root-port,id=rp2,slot=2,chassis=2 \
	  -device e1000e,netdev=n0,bus=rp1,mac=02:00:00:00:00:01 \
	  -device e1000e,netdev=n1,bus=rp2,mac=02:00:00:00:00:02 \
	  -kernel /lib/modules/$(KVER)/vmlinuz \
	  -initrd build/vfio_initramfs.cpio \
	  -append "console=ttyS0 intel_iommu=on iommu=pt loglevel=4 panic=-1" \
	  -display none -serial stdio -no-reboot

test: build/test_igc_618 build/test_igc_612
	./build/test_igc_618
	./build/test_igc_612

clean:
	rm -rf build
