/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_COMPLETION_H
#define _DRY_LINUX_COMPLETION_H

#include <linux/compat_base.h>

struct completion {
    volatile unsigned int done;
};

void init_completion(struct completion *x);
void reinit_completion(struct completion *x);
void complete(struct completion *x);
unsigned long wait_for_completion_timeout(struct completion *x, unsigned long timeout_jiffies);
int wait_for_completion_killable(struct completion *x);

#endif /* _DRY_LINUX_COMPLETION_H */
