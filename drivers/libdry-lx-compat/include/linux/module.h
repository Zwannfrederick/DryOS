/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_MODULE_H
#define _DRY_LINUX_MODULE_H

#include <linux/compat_base.h>

#define module_param_named(name, value, type, perm)
#define module_param(name, type, perm)
#define MODULE_PARM_DESC(parm, desc)
#define MODULE_DEVICE_TABLE(type, name)
#define MODULE_AUTHOR(author)
#define MODULE_DESCRIPTION(desc)
#define MODULE_LICENSE(lic)

#endif
