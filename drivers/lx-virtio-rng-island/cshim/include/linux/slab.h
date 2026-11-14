/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef _DRY_LX_SLAB_H
#define _DRY_LX_SLAB_H

#include <linux/kernel.h>
#include <linux/types.h>

struct ida {
    uint32_t bitmap;
};

#define DEFINE_IDA(name) struct ida name = { .bitmap = 0 }

static inline int ida_alloc(struct ida *ida, gfp_t gfp) {
    (void)gfp;
    for (int i = 0; i < 32; i++) {
        uint32_t bit = 1U << (unsigned int)i;
        if ((ida->bitmap & bit) == 0U) {
            ida->bitmap |= bit;
            return i;
        }
    }
    return -12; /* -ENOMEM */
}

static inline void ida_free(struct ida *ida, unsigned int id) {
    if (id < 32U) {
        ida->bitmap &= ~(1U << id);
    }
}

void *kzalloc(size_t size, gfp_t flags);
void kfree(const void *ptr);

#endif /* _DRY_LX_SLAB_H */
