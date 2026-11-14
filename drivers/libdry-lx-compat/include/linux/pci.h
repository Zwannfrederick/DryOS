/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_PCI_H
#define _DRY_LINUX_PCI_H

#include <linux/compat_base.h>

#define IORESOURCE_IO 0x00000100UL
#define IORESOURCE_MEM 0x00000200UL

#define PCI_SUBVENDOR_ID_REDHAT_QUMRANET 0x1af4U
#define PCI_SUBDEVICE_ID_QEMU 0x1100U
#define PCI_ANY_ID (~0U)

struct resource {
    u64 start;
    u64 end;
    unsigned long flags;
};

struct device {
    void *driver_data;
    u16 bdf;
};

struct pci_dev {
    struct device dev;
    u16 vendor;
    u16 device;
    u16 subsystem_vendor;
    u16 subsystem_device;
    u8 revision;
    struct resource resource[6];
};

struct pci_device_id {
    u32 vendor;
    u32 device;
    u32 subvendor;
    u32 subdevice;
    u32 class;
    u32 class_mask;
    unsigned long driver_data;
};

struct dev_pm_ops {
    int reserved;
};

#define SET_SYSTEM_SLEEP_PM_OPS(suspend_fn, resume_fn) .reserved = 0

struct pci_driver {
    const char *name;
    const struct pci_device_id *id_table;
    int (*probe)(struct pci_dev *dev, const struct pci_device_id *id);
    void (*remove)(struct pci_dev *dev);
    void (*shutdown)(struct pci_dev *dev);
    struct {
        const struct dev_pm_ops *pm;
    } driver;
};

#define to_pci_dev(n) container_of(n, struct pci_dev, dev)

static inline unsigned long pci_resource_start(const struct pci_dev *pdev, int bar) {
    return (unsigned long)pdev->resource[bar].start;
}

static inline unsigned long pci_resource_len(const struct pci_dev *pdev, int bar) {
    if (pdev->resource[bar].end < pdev->resource[bar].start || pdev->resource[bar].start == 0) {
        return 0;
    }
    return (unsigned long)(pdev->resource[bar].end - pdev->resource[bar].start + 1);
}

static inline int pcim_enable_device(struct pci_dev *pdev) {
    return pdev ? 0 : -ENODEV;
}

static inline void pci_set_drvdata(struct pci_dev *pdev, void *data) {
    pdev->dev.driver_data = data;
}

static inline void *pci_get_drvdata(struct pci_dev *pdev) {
    return pdev->dev.driver_data;
}

static inline void *dev_get_drvdata(const struct device *dev) {
    return dev->driver_data;
}

void *devm_request_mem_region(struct device *dev, unsigned long start, unsigned long n, const char *name);
void *devm_request_region(struct device *dev, unsigned long start, unsigned long n, const char *name);
void __iomem *devm_ioremap(struct device *dev, unsigned long offset, unsigned long size);
void __iomem *devm_ioremap_wc(struct device *dev, unsigned long offset, unsigned long size);

#endif
