/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef _DRY_LX_SCATTERLIST_H
#define _DRY_LX_SCATTERLIST_H

#include <linux/types.h>

struct scatterlist {
    void *addr;
    unsigned int length;
    uint64_t dma_addr;
};

void sg_init_one(struct scatterlist *sg, const void *buf, unsigned int buflen);

#endif /* _DRY_LX_SCATTERLIST_H */
