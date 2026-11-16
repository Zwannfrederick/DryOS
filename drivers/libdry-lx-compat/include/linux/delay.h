/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_DELAY_H
#define _DRY_LINUX_DELAY_H

#include <linux/jiffies.h>

void udelay(unsigned long usecs);
void mdelay(unsigned long msecs);
long schedule_timeout_uninterruptible(long timeout_jiffies);

#endif /* _DRY_LINUX_DELAY_H */
