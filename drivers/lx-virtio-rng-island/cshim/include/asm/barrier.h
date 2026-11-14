/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef _DRY_ASM_BARRIER_H
#define _DRY_ASM_BARRIER_H

#include <linux/types.h>

#define smp_mb()  __atomic_thread_fence(__ATOMIC_SEQ_CST)
#define smp_rmb() __atomic_thread_fence(__ATOMIC_ACQUIRE)
#define smp_wmb() __atomic_thread_fence(__ATOMIC_RELEASE)

#define smp_store_release(p, v) __atomic_store_n((p), (v), __ATOMIC_RELEASE)
#define smp_load_acquire(p)     __atomic_load_n((p), __ATOMIC_ACQUIRE)

#endif /* _DRY_ASM_BARRIER_H */
