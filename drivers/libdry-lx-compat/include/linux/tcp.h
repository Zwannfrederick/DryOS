/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_TCP_H
#define _DRY_LINUX_TCP_H

#include <linux/types.h>

struct tcphdr {
    __be16 source;
    __be16 dest;
    __be32 seq;
    __be32 ack_seq;
    u16 flags;
    __be16 window;
    __be16 check;
    __be16 urg_ptr;
} __attribute__((packed));

#endif
