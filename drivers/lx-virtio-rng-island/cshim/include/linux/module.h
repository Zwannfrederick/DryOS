/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef _DRY_LX_MODULE_H
#define _DRY_LX_MODULE_H

#include <linux/pm.h>

#ifndef KBUILD_MODNAME
#define KBUILD_MODNAME "virtio_rng"
#endif

#define MODULE_DEVICE_TABLE(type, name)
#define MODULE_DESCRIPTION(desc)
#define MODULE_LICENSE(lic)

#endif /* _DRY_LX_MODULE_H */
