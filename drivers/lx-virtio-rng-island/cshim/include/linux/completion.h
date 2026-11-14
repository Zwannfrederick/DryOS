/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef _DRY_LX_COMPLETION_H
#define _DRY_LX_COMPLETION_H

#include <linux/types.h>

struct completion {
    volatile unsigned int done;
};

static inline void init_completion(struct completion *x) {
    x->done = 0;
}

static inline void reinit_completion(struct completion *x) {
    __atomic_store_n(&x->done, 0U, __ATOMIC_RELEASE);
}

static inline void complete(struct completion *x) {
    __atomic_store_n(&x->done, 1U, __ATOMIC_RELEASE);
}

int wait_for_completion_killable(struct completion *x);

#endif /* _DRY_LX_COMPLETION_H */
