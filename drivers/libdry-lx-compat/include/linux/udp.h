/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_UDP_H
#define _DRY_LINUX_UDP_H

#include <linux/types.h>

struct udphdr {
    __be16 source;
    __be16 dest;
    __be16 len;
    __be16 check;
} __attribute__((packed));

#endif
