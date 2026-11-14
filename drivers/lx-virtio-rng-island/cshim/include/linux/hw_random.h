/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef _DRY_LX_HW_RANDOM_H
#define _DRY_LX_HW_RANDOM_H

#include <linux/types.h>

struct hwrng {
    const char *name;
    int (*init)(struct hwrng *rng);
    void (*cleanup)(struct hwrng *rng);
    int (*read)(struct hwrng *rng, void *data, size_t max, bool wait);
    unsigned long priv;
};

int hwrng_register(struct hwrng *rng);
void hwrng_unregister(struct hwrng *rng);

#endif /* _DRY_LX_HW_RANDOM_H */
