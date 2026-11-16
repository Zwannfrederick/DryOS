/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_MUTEX_H
#define _DRY_LINUX_MUTEX_H

#include <linux/spinlock.h>

struct mutex {
    volatile uint32_t locked;
    uint32_t contentions;
};

void mutex_init(struct mutex *lock);
void mutex_lock(struct mutex *lock);
int mutex_trylock(struct mutex *lock);
void mutex_unlock(struct mutex *lock);

#endif /* _DRY_LINUX_MUTEX_H */
