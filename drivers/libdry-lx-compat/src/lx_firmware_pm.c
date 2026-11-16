/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#include <linux/firmware.h>
#include <linux/pci.h>
#include <linux/pm_runtime.h>

#define MAX_FW_ENTRIES 16U

struct fw_slot {
    bool in_use;
    char name[64];
    struct firmware fw;
};

static struct fw_slot g_fw_slots[MAX_FW_ENTRIES];

__attribute__((weak)) int32_t dry_lx_rust_load_firmware(
    const char *name, const uint8_t **data_out, size_t *size_out) {
    (void)name;
    (void)data_out;
    (void)size_out;
    return -ENOENT;
}

static bool streq(const char *a, const char *b) {
    if (!a || !b) return false;
    size_t i = 0;
    while (a[i] != '\0' && b[i] != '\0') {
        if (a[i] != b[i]) return false;
        i++;
    }
    return a[i] == b[i];
}

int dry_lx_firmware_register(const char *name, const u8 *data, size_t size) {
    if (!name || !data || size == 0U) {
        return -EINVAL;
    }
    for (size_t i = 0; i < MAX_FW_ENTRIES; i++) {
        if (g_fw_slots[i].in_use && streq(g_fw_slots[i].name, name)) {
            g_fw_slots[i].fw.data = data;
            g_fw_slots[i].fw.size = size;
            return 0;
        }
    }
    for (size_t i = 0; i < MAX_FW_ENTRIES; i++) {
        if (!g_fw_slots[i].in_use) {
            g_fw_slots[i].in_use = true;
            strscpy(g_fw_slots[i].name, name, sizeof(g_fw_slots[i].name));
            g_fw_slots[i].fw.data = data;
            g_fw_slots[i].fw.size = size;
            g_fw_slots[i].fw.priv = &g_fw_slots[i];
            return 0;
        }
    }
    return -ENOMEM;
}

int request_firmware(const struct firmware **fw, const char *name, struct device *device) {
    (void)device;
    if (!fw || !name) {
        return -EINVAL;
    }
    *fw = NULL;
    for (size_t i = 0; i < MAX_FW_ENTRIES; i++) {
        if (g_fw_slots[i].in_use && streq(g_fw_slots[i].name, name)) {
            *fw = &g_fw_slots[i].fw;
            return 0;
        }
    }
    const uint8_t *data = NULL;
    size_t sz = 0U;
    if (dry_lx_rust_load_firmware(name, &data, &sz) == 0 && data && sz > 0U) {
        if (dry_lx_firmware_register(name, data, sz) == 0) {
            for (size_t i = 0; i < MAX_FW_ENTRIES; i++) {
                if (g_fw_slots[i].in_use && streq(g_fw_slots[i].name, name)) {
                    *fw = &g_fw_slots[i].fw;
                    return 0;
                }
            }
        }
    }
    return -ENOENT;
}

void release_firmware(const struct firmware *fw) {
    (void)fw;
}

void pm_runtime_enable(struct device *dev) {
    if (dev) {
        dev->pm_runtime_enabled = true;
        dev->rpm_state = RPM_ACTIVE;
    }
}

void pm_runtime_disable(struct device *dev) {
    if (dev) {
        dev->pm_runtime_enabled = false;
    }
}

int pm_runtime_get_sync(struct device *dev) {
    if (!dev) return -ENODEV;
    if (dev->rpm_state == RPM_SUSPENDED) {
        dev->rpm_state = RPM_RESUMING;
        if (dev->pm_ops && dev->pm_ops->runtime_resume) {
            int rc = dev->pm_ops->runtime_resume(dev);
            if (rc != 0) {
                dev->rpm_state = RPM_SUSPENDED;
                return rc;
            }
        } else if (dev->pm_ops && dev->pm_ops->resume) {
            int rc = dev->pm_ops->resume(dev);
            if (rc != 0) {
                dev->rpm_state = RPM_SUSPENDED;
                return rc;
            }
        }
        dev->rpm_state = RPM_ACTIVE;
    }
    return 0;
}

int pm_runtime_put_sync(struct device *dev) {
    if (!dev) return -ENODEV;
    if (dev->pm_runtime_enabled && dev->rpm_state == RPM_ACTIVE) {
        dev->rpm_state = RPM_SUSPENDING;
        if (dev->pm_ops && dev->pm_ops->runtime_suspend) {
            int rc = dev->pm_ops->runtime_suspend(dev);
            if (rc != 0) {
                dev->rpm_state = RPM_ACTIVE;
                return rc;
            }
        } else if (dev->pm_ops && dev->pm_ops->suspend) {
            int rc = dev->pm_ops->suspend(dev);
            if (rc != 0) {
                dev->rpm_state = RPM_ACTIVE;
                return rc;
            }
        }
        dev->rpm_state = RPM_SUSPENDED;
    }
    return 0;
}

bool pm_runtime_suspended(const struct device *dev) {
    return dev && dev->rpm_state == RPM_SUSPENDED;
}

bool pm_runtime_active(const struct device *dev) {
    return dev && dev->rpm_state == RPM_ACTIVE;
}

enum rpm_status dry_lx_pm_runtime_status(const struct device *dev) {
    return dev ? dev->rpm_state : RPM_SUSPENDED;
}

int device_set_wakeup_enable(struct device *dev, bool enable) {
    if (!dev) return -ENODEV;
    dev->wakeup_enabled = enable;
    return 0;
}
