/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_PM_RUNTIME_H
#define _DRY_LINUX_PM_RUNTIME_H

#include <linux/compat_base.h>

struct device;

enum rpm_status {
    RPM_ACTIVE = 0,
    RPM_RESUMING = 1,
    RPM_SUSPENDED = 2,
    RPM_SUSPENDING = 3,
};

void pm_runtime_enable(struct device *dev);
void pm_runtime_disable(struct device *dev);
int pm_runtime_get_sync(struct device *dev);
int pm_runtime_put_sync(struct device *dev);
bool pm_runtime_suspended(const struct device *dev);
bool pm_runtime_active(const struct device *dev);
enum rpm_status dry_lx_pm_runtime_status(const struct device *dev);
int device_set_wakeup_enable(struct device *dev, bool enable);

#endif /* _DRY_LINUX_PM_RUNTIME_H */
