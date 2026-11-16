/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_JIFFIES_H
#define _DRY_LINUX_JIFFIES_H

#include <linux/compat_base.h>

#define HZ 1000UL

unsigned long dry_lx_jiffies(void);
#define jiffies dry_lx_jiffies()

static inline unsigned long msecs_to_jiffies(const unsigned int m) {
    return (unsigned long)m;
}

static inline bool time_after(unsigned long a, unsigned long b) {
    return (long)(b - a) < 0;
}

#endif /* _DRY_LINUX_JIFFIES_H */
