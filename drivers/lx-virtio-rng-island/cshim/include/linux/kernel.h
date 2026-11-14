/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef _DRY_LX_KERNEL_H
#define _DRY_LX_KERNEL_H

#include <linux/types.h>

#define min_t(type, x, y) ({        \
    type __min1 = (x);              \
    type __min2 = (y);              \
    __min1 < __min2 ? __min1 : __min2; \
})

int sprintf(char *buf, const char *fmt, ...);
void *memcpy(void *dst, const void *src, size_t n);
void *memset(void *dst, int c, size_t n);

#endif /* _DRY_LX_KERNEL_H */
