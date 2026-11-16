/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#ifndef _DRY_LINUX_SKBUFF_H
#define _DRY_LINUX_SKBUFF_H

#include <linux/compat_base.h>
#include <linux/gfp.h>
#include <linux/ip.h>

#define MAX_SKB_FRAGS 17
#define NET_SKB_PAD 16
#define NET_IP_ALIGN 2

#define CHECKSUM_NONE 0
#define CHECKSUM_UNNECESSARY 1
#define CHECKSUM_COMPLETE 2
#define CHECKSUM_PARTIAL 3

typedef struct {
    struct page *bv_page;
    unsigned int bv_len;
    unsigned int bv_offset;
} skb_frag_t;

struct skb_shared_info {
    unsigned char nr_frags;
    unsigned short gso_size;
    skb_frag_t frags[MAX_SKB_FRAGS];
};

struct net_device;

struct sk_buff {
    struct net_device *dev;
    unsigned char *head;
    unsigned char *data;
    unsigned int len;
    unsigned int mac_header;
    unsigned int network_header;
    __be16 protocol;
    u8 ip_summed;
    bool vlan_present;
    __be16 vlan_proto;
    u16 vlan_tci;
    int pool_idx;
    struct skb_shared_info shinfo;
};

static inline struct skb_shared_info *skb_shinfo(const struct sk_buff *skb) {
    return (struct skb_shared_info *)&skb->shinfo;
}

static inline unsigned int skb_headlen(const struct sk_buff *skb) {
    return skb->len;
}

static inline unsigned int skb_frag_size(const skb_frag_t *frag) {
    return frag->bv_len;
}

static inline void *skb_frag_address(const skb_frag_t *frag) {
    return (frag->bv_page && frag->bv_page->virt_addr)
               ? (u8 *)frag->bv_page->virt_addr + frag->bv_offset
               : NULL;
}

static inline void skb_reserve(struct sk_buff *skb, int len) {
    skb->data += len;
}

static inline unsigned char *skb_put(struct sk_buff *skb, unsigned int len) {
    unsigned char *tmp = skb->data + skb->len;
    skb->len += len;
    return tmp;
}

static inline unsigned char *skb_push(struct sk_buff *skb, unsigned int len) {
    skb->data -= len;
    skb->len += len;
    return skb->data;
}

static inline unsigned char *skb_pull(struct sk_buff *skb, unsigned int len) {
    skb->len -= len;
    return skb->data += len;
}

static inline void skb_reset_mac_header(struct sk_buff *skb) {
    skb->mac_header = (unsigned int)(skb->data - skb->head);
}

static inline unsigned char *skb_mac_header(const struct sk_buff *skb) {
    return skb->head + skb->mac_header;
}

static inline void skb_reset_network_header(struct sk_buff *skb) {
    skb->network_header = (unsigned int)(skb->data - skb->head);
}

static inline struct iphdr *ip_hdr(const struct sk_buff *skb) {
    return (struct iphdr *)(skb->head + skb->network_header);
}

static inline void skb_checksum_none_assert(const struct sk_buff *skb) {
    (void)skb;
}

struct sk_buff *alloc_skb(unsigned int size, gfp_t priority);
struct sk_buff *netdev_alloc_skb_ip_align(struct net_device *dev, unsigned int length);
void kfree_skb(struct sk_buff *skb);
void dev_kfree_skb_any(struct sk_buff *skb);
void dev_consume_skb_irq(struct sk_buff *skb);

#endif /* _DRY_LINUX_SKBUFF_H */
