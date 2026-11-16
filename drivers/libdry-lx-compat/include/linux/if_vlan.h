/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_IF_VLAN_H
#define _DRY_LINUX_IF_VLAN_H

#include <linux/skbuff.h>

#define ETH_P_8021Q 0x8100U

static inline bool skb_vlan_tag_present(const struct sk_buff *skb) {
    return skb->vlan_present;
}

static inline u16 skb_vlan_tag_get(const struct sk_buff *skb) {
    return skb->vlan_tci;
}

static inline void __vlan_hwaccel_put_tag(struct sk_buff *skb, __be16 vlan_proto, u16 vlan_tci) {
    skb->vlan_present = true;
    skb->vlan_proto = vlan_proto;
    skb->vlan_tci = vlan_tci;
}

static inline u64 vlan_features_check(const struct sk_buff *skb, u64 features) {
    (void)skb;
    return features;
}

#endif /* _DRY_LINUX_IF_VLAN_H */
