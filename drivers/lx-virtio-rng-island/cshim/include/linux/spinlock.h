/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef _DRY_LX_SPINLOCK_H
#define _DRY_LX_SPINLOCK_H

#include <linux/completion.h>
#include <linux/types.h>

typedef struct {
    volatile uint32_t lock;
} spinlock_t;

static inline void spin_lock_init(spinlock_t *l) {
    l->lock = 0;
}

static inline void spin_lock(spinlock_t *l) {
    while (__atomic_exchange_n(&l->lock, 1U, __ATOMIC_ACQUIRE) != 0U) {
        __builtin_ia32_pause();
    }
}

static inline void spin_unlock(spinlock_t *l) {
    __atomic_store_n(&l->lock, 0U, __ATOMIC_RELEASE);
}

#endif /* _DRY_LX_SPINLOCK_H */
