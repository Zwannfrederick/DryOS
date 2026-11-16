/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_FIRMWARE_H
#define _DRY_LINUX_FIRMWARE_H

#include <linux/compat_base.h>

struct device;

struct firmware {
    size_t size;
    const u8 *data;
    void *priv;
};

int dry_lx_firmware_register(const char *name, const u8 *data, size_t size);
int request_firmware(const struct firmware **fw, const char *name, struct device *device);
void release_firmware(const struct firmware *fw);

#endif /* _DRY_LINUX_FIRMWARE_H */
