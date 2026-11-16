/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_IP_H
#define _DRY_LINUX_IP_H

#include <linux/types.h>

struct iphdr {
    u8 ihl_version;
    u8 tos;
    __be16 tot_len;
    __be16 id;
    __be16 frag_off;
    u8 ttl;
    u8 protocol;
    __be16 check;
    __be32 saddr;
    __be32 daddr;
} __attribute__((packed));

#endif
