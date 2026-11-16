/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_ETHERDEVICE_H
#define _DRY_LINUX_ETHERDEVICE_H

#include <linux/netdevice.h>

struct ethhdr {
    unsigned char h_dest[ETH_ALEN];
    unsigned char h_source[ETH_ALEN];
    __be16 h_proto;
} __attribute__((packed));

struct net_device *alloc_etherdev_mqs(int sizeof_priv, unsigned int txqs, unsigned int rxqs);
#define alloc_etherdev(sizeof_priv) alloc_etherdev_mqs((sizeof_priv), 1, 1)

static inline void eth_hw_addr_set(struct net_device *dev, const void *addr) {
    dry_lx_memcpy(dev->dev_addr, addr, ETH_ALEN);
}

static inline bool is_valid_ether_addr(const void *addr_ptr) {
    const u8 *addr = (const u8 *)addr_ptr;
    u8 or_all = addr[0] | addr[1] | addr[2] | addr[3] | addr[4] | addr[5];
    bool is_mcast = (addr[0] & 0x01U) != 0U;
    return or_all != 0U && !is_mcast;
}

static inline int eth_validate_addr(struct net_device *dev) {
    return is_valid_ether_addr(dev->dev_addr) ? 0 : -EADDRNOTAVAIL;
}

__be16 eth_type_trans(struct sk_buff *skb, struct net_device *dev);

#endif /* _DRY_LINUX_ETHERDEVICE_H */
