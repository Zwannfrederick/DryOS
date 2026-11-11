/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * DRY OS — H2 Shadow PCI, Device Model, Power Management, and MSI-X IRQ Layer
 */
#ifndef LX_EMUL_PCI_IRQ_H
#define LX_EMUL_PCI_IRQ_H

#include "lx_mm_dma.h"

#define __init
#define __exit

#define module_init(initfn) \
	int dry_lx_module_init(void) { return initfn(); }
#define module_exit(exitfn) \
	void dry_lx_module_exit(void) { exitfn(); }

int dry_lx_module_init(void);
void dry_lx_module_exit(void);

/* Generic device & PM ops */
struct device {
	struct device *parent;
	void *driver_data;
	u8 mac_addr[6];
	bool has_mac;
};

static inline void dev_set_drvdata(struct device *dev, void *data) { dev->driver_data = data; }
static inline void *dev_get_drvdata(const struct device *dev) { return dev->driver_data; }

#define dev_err(dev, fmt, ...)  do { (void)(dev); fprintf(stderr, "[dev ERR] " fmt, ##__VA_ARGS__); } while (0)
#define dev_warn(dev, fmt, ...) do { (void)(dev); } while (0)
#define dev_info(dev, fmt, ...) do { (void)(dev); } while (0)
#define dev_dbg(dev, fmt, ...)  do { (void)(dev); } while (0)

#define DPM_FLAG_NO_DIRECT_COMPLETE BIT(1)
static inline void dev_pm_set_driver_flags(struct device *dev, u32 flags) { (void)dev; (void)flags; }
static inline int device_set_wakeup_enable(struct device *dev, bool enable) { (void)dev; (void)enable; return 0; }
static inline bool device_may_wakeup(struct device *dev) { (void)dev; return false; }

struct dev_pm_ops {
	int (*suspend)(struct device *dev);
	int (*resume)(struct device *dev);
	int (*runtime_suspend)(struct device *dev);
	int (*runtime_resume)(struct device *dev);
	int (*runtime_idle)(struct device *dev);
};

#define _DEFINE_DEV_PM_OPS(name, suspend_fn, resume_fn, rt_susp, rt_res, rt_idle) \
	const struct dev_pm_ops name = { \
		.suspend = (suspend_fn), \
		.resume = (resume_fn), \
		.runtime_suspend = (rt_susp), \
		.runtime_resume = (rt_res), \
		.runtime_idle = (rt_idle), \
	}
#define pm_ptr(ptr) (ptr)

static inline void pm_runtime_put_noidle(struct device *dev) { (void)dev; }
static inline void pm_runtime_get_noresume(struct device *dev) { (void)dev; }
static inline int pm_runtime_get_sync(struct device *dev) { (void)dev; return 0; }
static inline int pm_runtime_put_sync(struct device *dev) { (void)dev; return 0; }
static inline int pm_runtime_put(struct device *dev) { (void)dev; return 0; }
static inline int pm_runtime_resume(struct device *dev) { (void)dev; return 0; }
static inline int pm_schedule_suspend(struct device *dev, unsigned int delay) { (void)dev; (void)delay; return 0; }

struct device_driver {
	const char *name;
	const struct dev_pm_ops *pm;
};

/* PCI subsystem */
#define PCI_VENDOR_ID_INTEL 0x8086
#define PCI_ANY_ID          (~0U)
#define PCI_VDEVICE(vend, dev) \
	.vendor = PCI_VENDOR_ID_##vend, .device = (dev), \
	.subvendor = PCI_ANY_ID, .subdevice = PCI_ANY_ID, 0, 0

#define PCI_SLOT(devfn) (((devfn) >> 3) & 0x1f)
#define PCI_FUNC(devfn) ((devfn) & 0x07)

#define PCI_COMMAND         0x04
#define PCI_STATUS          0x06
#define PCI_EXP_LNKSTA      18
#define PCI_EXP_LNKSTA_CLS  0x000f
#define PCI_EXP_LNKSTA_NLW  0x03f0
#define PCI_EXP_LNKCTL2     48
#define PCIE_LINK_STATE_L0S  BIT(0)
#define PCIE_LINK_STATE_L1   BIT(1)
#define PCIE_LINK_STATE_L1_1 BIT(3)
#define PCIE_LINK_STATE_L1_2 BIT(4)
#define PCIE_LINK_STATE_CLKPM BIT(2)

typedef int pci_power_t;
#define PCI_D0     0
#define PCI_D3hot  3
#define PCI_D3cold 4

typedef unsigned int pci_ers_result_t;
typedef int pci_channel_state_t;
#define pci_channel_io_normal      1
#define pci_channel_io_frozen      2
#define pci_channel_io_perm_failure 3
#define PCI_ERS_RESULT_NONE        1
#define PCI_ERS_RESULT_CAN_RECOVER 2
#define PCI_ERS_RESULT_NEED_RESET  3
#define PCI_ERS_RESULT_DISCONNECT  4
#define PCI_ERS_RESULT_RECOVERED   5

struct resource {
	resource_size_t start;
	resource_size_t end;
	unsigned long flags;
};
static inline resource_size_t resource_size(const struct resource *res)
{
	return res->end - res->start + 1;
}

struct pci_bus {
	unsigned char number;
};

struct pci_dev {
	struct device dev;
	struct pci_bus *bus;
	unsigned int devfn;
	unsigned short vendor;
	unsigned short device;
	unsigned short subsystem_vendor;
	unsigned short subsystem_device;
	u8 revision;
	unsigned int irq;
	struct resource resource[6];
	u16 config16[64];
	bool msi_enabled;
	bool msix_enabled;
};

struct pci_device_id {
	u32 vendor, device;
	u32 subvendor, subdevice;
	u32 class, class_mask;
	unsigned long driver_data;
};

struct pci_error_handlers {
	pci_ers_result_t (*error_detected)(struct pci_dev *dev, pci_channel_state_t error);
	pci_ers_result_t (*slot_reset)(struct pci_dev *dev);
	void (*resume)(struct pci_dev *dev);
};

struct pci_driver {
	const char *name;
	const struct pci_device_id *id_table;
	int (*probe)(struct pci_dev *dev, const struct pci_device_id *id);
	void (*remove)(struct pci_dev *dev);
	void (*shutdown)(struct pci_dev *dev);
	const struct pci_error_handlers *err_handler;
	struct device_driver driver;
};

struct msix_entry {
	u32 vector;
	u16 entry;
};

#define to_pci_dev(n) container_of((n), struct pci_dev, dev)

enum system_states { SYSTEM_RUNNING = 0, SYSTEM_POWER_OFF = 1 };
#define system_state SYSTEM_RUNNING

#define pci_resource_start(dev, bar) ((dev)->resource[(bar)].start)
#define pci_resource_end(dev, bar)   ((dev)->resource[(bar)].end)
#define pci_resource_len(dev, bar)   \
	((pci_resource_start((dev), (bar)) == 0 && pci_resource_end((dev), (bar)) == 0) ? 0 : \
	 resource_size(&(dev)->resource[(bar)]))

static inline void pci_set_drvdata(struct pci_dev *pdev, void *data) { dev_set_drvdata(&pdev->dev, data); }
static inline void *pci_get_drvdata(struct pci_dev *pdev) { return dev_get_drvdata(&pdev->dev); }
static inline int pci_enable_device_mem(struct pci_dev *pdev) { (void)pdev; return 0; }
static inline void pci_disable_device(struct pci_dev *pdev) { (void)pdev; }
static inline int pci_request_mem_regions(struct pci_dev *pdev, const char *name) { (void)pdev; (void)name; return 0; }
static inline void pci_release_mem_regions(struct pci_dev *pdev) { (void)pdev; }
static inline void pci_set_master(struct pci_dev *pdev) { (void)pdev; }
static inline void pci_clear_master(struct pci_dev *pdev) { (void)pdev; }
static inline int pci_save_state(struct pci_dev *pdev) { (void)pdev; return 0; }
static inline void pci_restore_state(struct pci_dev *pdev) { (void)pdev; }
static inline int pci_enable_wake(struct pci_dev *pdev, pci_power_t state, bool enable) { (void)pdev; (void)state; (void)enable; return 0; }
static inline int pci_wake_from_d3(struct pci_dev *pdev, bool enable) { (void)pdev; (void)enable; return 0; }
static inline int pci_set_power_state(struct pci_dev *pdev, pci_power_t state) { (void)pdev; (void)state; return 0; }
static inline int pci_enable_ptm(struct pci_dev *pdev, u8 *granularity) { (void)pdev; (void)granularity; return -EOPNOTSUPP; }
static inline void pci_disable_ptm(struct pci_dev *pdev) { (void)pdev; }
static inline bool pcie_ptm_enabled(struct pci_dev *pdev) { (void)pdev; return false; }
static inline void pcie_print_link_status(const struct pci_dev *pdev) { (void)pdev; }
static inline bool pci_device_is_present(struct pci_dev *pdev) { (void)pdev; return true; }
static inline bool pci_channel_offline(struct pci_dev *pdev) { (void)pdev; return false; }
static inline void pci_iounmap(struct pci_dev *dev, void __iomem *addr) { (void)dev; iounmap(addr); }
static inline int pci_disable_link_state(struct pci_dev *pdev, int state) { (void)pdev; (void)state; return 0; }
static inline int pci_disable_link_state_locked(struct pci_dev *pdev, int state) { (void)pdev; (void)state; return 0; }
static inline bool pci_is_pcie(struct pci_dev *dev) { (void)dev; return true; }

static inline int pci_read_config_word(const struct pci_dev *dev, int where, u16 *val)
{
	*val = ((where / 2) < 64) ? dev->config16[where / 2] : 0;
	return 0;
}
static inline int pci_write_config_word(struct pci_dev *dev, int where, u16 val)
{
	if ((where / 2) < 64)
		dev->config16[where / 2] = val;
	return 0;
}
static inline int pcie_capability_read_word(struct pci_dev *dev, int pos, u16 *val)
{
	return pci_read_config_word(dev, pos, val);
}
static inline int pcie_capability_write_word(struct pci_dev *dev, int pos, u16 val)
{
	return pci_write_config_word(dev, pos, val);
}

int pci_register_driver(struct pci_driver *drv);
void pci_unregister_driver(struct pci_driver *drv);
int pci_enable_msix_range(struct pci_dev *dev, struct msix_entry *entries,
			  int minvec, int maxvec);
void pci_disable_msix(struct pci_dev *dev);
int pci_enable_msi(struct pci_dev *dev);
void pci_disable_msi(struct pci_dev *dev);

/* IRQ subsystem */
typedef int irqreturn_t;
#define IRQ_NONE        0
#define IRQ_HANDLED     1
#define IRQ_WAKE_THREAD 2

#define IRQF_SHARED   0x00000080U
#define IRQF_ONESHOT  0x00002000U

typedef irqreturn_t (*irq_handler_t)(int, void *);

int request_threaded_irq(unsigned int irq, irq_handler_t handler,
			 irq_handler_t thread_fn, unsigned long flags,
			 const char *name, void *dev);
static inline int request_irq(unsigned int irq, irq_handler_t handler,
			      unsigned long flags, const char *name, void *dev)
{
	return request_threaded_irq(irq, handler, NULL, flags, name, dev);
}
const void *free_irq(unsigned int irq, void *dev_id);
void synchronize_irq(unsigned int irq);

#endif /* LX_EMUL_PCI_IRQ_H */

