/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_SPINLOCK_H
#define _DRY_LINUX_SPINLOCK_H

#include <linux/compat_base.h>

typedef struct {
    volatile uint32_t lock;
} spinlock_t;

void dry_lx_spin_lock_init(spinlock_t *l);
void dry_lx_spin_lock(spinlock_t *l);
void dry_lx_spin_unlock(spinlock_t *l);

#define spin_lock_init(l) dry_lx_spin_lock_init(l)
#define spin_lock(l) dry_lx_spin_lock(l)
#define spin_unlock(l) dry_lx_spin_unlock(l)
#define spin_lock_irq(l) dry_lx_spin_lock(l)
#define spin_unlock_irq(l) dry_lx_spin_unlock(l)
#define spin_lock_irqsave(l, flags) do { (flags) = 0UL; dry_lx_spin_lock(l); } while (0)
#define spin_unlock_irqrestore(l, flags) do { (void)(flags); dry_lx_spin_unlock(l); } while (0)

#endif /* _DRY_LINUX_SPINLOCK_H */
