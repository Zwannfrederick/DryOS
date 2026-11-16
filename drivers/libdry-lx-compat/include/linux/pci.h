/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_PCI_H
#define _DRY_LINUX_PCI_H

#include <linux/compat_base.h>
#include <linux/pm_runtime.h>

#define IORESOURCE_IO 0x00000100UL
#define IORESOURCE_MEM 0x00000200UL
#define IORESOURCE_PREFETCH 0x00002000UL

#define PCI_VENDOR_ID_REALTEK 0x10ecU
#define PCI_DEVICE_ID_REALTEK_8139 0x8139U
#define PCI_VENDOR_ID_TTTECH 0x0357U
#define PCI_DEVICE_ID_TTTECH_MC322 0x000aU
#define PCI_SUBVENDOR_ID_REDHAT_QUMRANET 0x1af4U
#define PCI_SUBDEVICE_ID_QEMU 0x1100U
#define PCI_ANY_ID (~0U)

#define PCI_STATUS 0x06U
#define PCI_D0 0
#define PCI_D3hot 3

#define PCI_DEVICE(vend, dev) \
    .vendor = (vend), .device = (dev), .subvendor = PCI_ANY_ID, .subdevice = PCI_ANY_ID

struct resource {
    u64 start;
    u64 end;
    unsigned long flags;
    u16 page_suboff;
};

struct dev_pm_ops;

struct device {
    void *driver_data;
    const struct dev_pm_ops *pm_ops;
    u16 bdf;
    u64 dma_mask;
    u64 coherent_dma_mask;
    bool pm_runtime_enabled;
    enum rpm_status rpm_state;
    bool wakeup_enabled;
};

struct pci_dev {
    struct device dev;
    u16 vendor;
    u16 device;
    u16 subsystem_vendor;
    u16 subsystem_device;
    u8 revision;
    int irq;
    u16 dev_flags;
    bool enabled;
    bool bus_master;
    bool mwi;
    bool wake_enabled;
    int current_state;
    char slot_name[16];
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
    int (*suspend)(struct device *dev);
    int (*resume)(struct device *dev);
    int (*runtime_suspend)(struct device *dev);
    int (*runtime_resume)(struct device *dev);
};

#define SET_SYSTEM_SLEEP_PM_OPS(suspend_fn, resume_fn) .suspend = NULL, .resume = NULL
#define SIMPLE_DEV_PM_OPS(name, suspend_fn, resume_fn) \
    const struct dev_pm_ops name = { .suspend = (suspend_fn), .resume = (resume_fn) }

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

#define module_pci_driver(drv_sym)                                      \
    const struct pci_driver *dry_lx_registered_pci_driver(void) {      \
        return &(drv_sym);                                              \
    }

#define to_pci_dev(n) container_of(n, struct pci_dev, dev)

static inline unsigned long pci_resource_start(const struct pci_dev *pdev, int bar) {
    return (bar >= 0 && bar < 6) ? (unsigned long)pdev->resource[bar].start : 0UL;
}

static inline unsigned long pci_resource_len(const struct pci_dev *pdev, int bar) {
    if (bar < 0 || bar >= 6 || pdev->resource[bar].end < pdev->resource[bar].start ||
        pdev->resource[bar].start == 0) {
        return 0;
    }
    return (unsigned long)(pdev->resource[bar].end - pdev->resource[bar].start + 1);
}

static inline const char *pci_name(const struct pci_dev *pdev) {
    return pdev ? pdev->slot_name : "0000:00:00.0";
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

struct dry_lx_pci_bar_init {
    u64 size_bytes;
    u32 flags;
    u16 page_suboff;
};

void dry_lx_pci_init_device(struct pci_dev *pdev, u16 vendor_id, u16 device_id,
                            u16 subvendor, u16 subdevice, u16 bdf, u8 revision,
                            u16 dev_flags, const struct dry_lx_pci_bar_init *bars);
const struct pci_device_id *dry_lx_match_pci_id_table(
    const struct pci_device_id *tbl, const struct pci_dev *pdev);
uint32_t dry_lx_pci_mapped_bar_mask(void);

int pci_enable_device(struct pci_dev *pdev);
int pcim_enable_device(struct pci_dev *pdev);
void pci_disable_device(struct pci_dev *pdev);
void pci_set_master(struct pci_dev *pdev);
int pci_set_mwi(struct pci_dev *pdev);
void pci_clear_mwi(struct pci_dev *pdev);
int pci_request_regions(struct pci_dev *pdev, const char *res_name);
void pci_release_regions(struct pci_dev *pdev);
int pci_read_config_word(const struct pci_dev *pdev, int where, u16 *val);
int pci_write_config_word(const struct pci_dev *pdev, int where, u16 val);
int pci_enable_wake(struct pci_dev *pdev, int state, bool enable);
int pci_set_power_state(struct pci_dev *pdev, int state);

void *devm_request_mem_region(struct device *dev, unsigned long start, unsigned long n, const char *name);
void *devm_request_region(struct device *dev, unsigned long start, unsigned long n, const char *name);
void __iomem *pci_iomap(struct pci_dev *pdev, int bar, unsigned long maxlen);
void __iomem *ioremap(resource_size_t offset, unsigned long size);
void iounmap(volatile void __iomem *addr);
void __iomem *devm_ioremap(struct device *dev, unsigned long offset, unsigned long size);
void __iomem *devm_ioremap_wc(struct device *dev, unsigned long offset, unsigned long size);
void memcpy_fromio(void *to, const volatile void __iomem *from, size_t count);

#endif /* _DRY_LINUX_PCI_H */
