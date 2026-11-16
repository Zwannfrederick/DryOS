/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
#include <linux/crc32.h>
#include <linux/dma-mapping.h>
#include <linux/etherdevice.h>
#include <linux/ethtool.h>
#include <linux/in.h>
#include <linux/interrupt.h>
#include <linux/netdevice.h>
#include <linux/pci.h>
#include <linux/udp.h>
#include <linux/workqueue.h>

#define MAX_SKB_SLOTS 128U
#define SKB_SLOT_BYTES 2048U
#define QEMU_SLIRP_GUEST_IP 0x0a00020fU /* 10.0.2.15 */
#define QEMU_SLIRP_GW_IP    0x0a000202U /* 10.0.2.2 */
#define DRY_UDP_ECHO_PORT   7777U

struct arp_pkt {
    __be16 htype;
    __be16 ptype;
    u8 hlen;
    u8 plen;
    __be16 oper;
    u8 sha[6];
    __be32 spa;
    u8 tha[6];
    __be32 tpa;
} __attribute__((packed));

extern const struct pci_driver *dry_lx_registered_pci_driver(void);
extern void dry_lx_devm_reset(void);

static struct sk_buff g_skb_pool[MAX_SKB_SLOTS];
static uint8_t g_skb_used[MAX_SKB_SLOTS];
static uint8_t g_netdev_storage[4096] __attribute__((aligned(64)));
static struct net_device *g_registered_netdev = NULL;
static struct pci_dev g_net_pdev;

static u8 g_gw_mac[6];
static u8 g_gw_mac_known = 0U;
static u64 g_arp_replies = 0ULL;
static u64 g_udp_echo_pkts = 0ULL;
static u16 g_ip_id = 1U;

u32 ether_crc(int length, unsigned char *data) {
    u32 crc = ~0U;
    for (int i = 0; i < length; i++) {
        crc ^= data[i];
        for (int b = 0; b < 8; b++) {
            crc = (crc >> 1) ^ ((crc & 1U) ? 0xedb88320U : 0U);
        }
    }
    return ~crc;
}

u32 ethtool_op_get_link(struct net_device *dev) {
    return (dev && netif_carrier_ok(dev)) ? 1U : 0U;
}

struct net_device *alloc_etherdev_mqs(int sizeof_priv, unsigned int txqs, unsigned int rxqs) {
    (void)txqs; (void)rxqs;
    size_t net_sz = ALIGN(sizeof(struct net_device), 64U);
    if (net_sz + (size_t)sizeof_priv > sizeof(g_netdev_storage)) {
        return NULL;
    }
    dry_lx_memset(g_netdev_storage, 0, net_sz + (size_t)sizeof_priv);
    struct net_device *dev = (struct net_device *)g_netdev_storage;
    dev->priv = g_netdev_storage + net_sz;
    dev->addr_len = ETH_ALEN;
    dev->mtu = ETH_DATA_LEN;
    dev->min_mtu = 60U;
    dev->max_mtu = 4096U;
    dev->present = true;
    strscpy(dev->name, "eth0", sizeof(dev->name));
    return dev;
}

void free_netdev(struct net_device *dev) {
    if (g_registered_netdev == dev) {
        g_registered_netdev = NULL;
    }
}

int register_netdev(struct net_device *dev) {
    if (!dev) return -EINVAL;
    if (dev->name[0] == '\0') {
        strscpy(dev->name, "eth0", sizeof(dev->name));
    }
    g_registered_netdev = dev;
    return 0;
}

void unregister_netdev(struct net_device *dev) {
    if (g_registered_netdev == dev) {
        if (dev->napi_primary) {
            dry_lx_wq_unregister_napi(dev->napi_primary);
        }
        g_registered_netdev = NULL;
    }
}

void netdev_sent_queue(struct net_device *dev, unsigned int bytes) {
    (void)dev; (void)bytes;
}

void netdev_completed_queue(struct net_device *dev, unsigned int pkts, unsigned int bytes) {
    (void)dev; (void)pkts; (void)bytes;
}

void netdev_reset_queue(struct net_device *dev) {
    (void)dev;
}

struct sk_buff *alloc_skb(unsigned int size, gfp_t priority) {
    (void)priority;
    if (size > SKB_SLOT_BYTES) {
        return NULL;
    }
    for (u32 i = 0U; i < MAX_SKB_SLOTS; i++) {
        if (g_skb_used[i] == 0U) {
            void *buf = dry_lx_dma_alloc_skbuf(SKB_SLOT_BYTES, NULL);
            if (!buf) {
                return NULL;
            }
            g_skb_used[i] = 1U;
            struct sk_buff *skb = &g_skb_pool[i];
            dry_lx_memset(skb, 0, sizeof(*skb));
            dry_lx_memset(buf, 0, SKB_SLOT_BYTES);
            skb->head = (unsigned char *)buf;
            skb->data = (unsigned char *)buf;
            skb->pool_idx = (int)i;
            return skb;
        }
    }
    return NULL;
}

struct sk_buff *netdev_alloc_skb_ip_align(struct net_device *dev, unsigned int length) {
    struct sk_buff *skb = alloc_skb(length + NET_SKB_PAD + NET_IP_ALIGN, GFP_ATOMIC);
    if (!skb) {
        return NULL;
    }
    skb->dev = dev;
    skb_reserve(skb, NET_SKB_PAD + NET_IP_ALIGN);
    return skb;
}

struct sk_buff *napi_alloc_skb(struct napi_struct *napi, unsigned int length) {
    return netdev_alloc_skb_ip_align(napi ? napi->dev : NULL, length);
}

void dev_kfree_skb_any(struct sk_buff *skb) {
    if (!skb) return;
    if (skb->head) {
        dry_lx_dma_free_skbuf(skb->head);
    }
    int idx = skb->pool_idx;
    dry_lx_memset(skb, 0, sizeof(*skb));
    if (idx >= 0 && idx < (int)MAX_SKB_SLOTS) {
        g_skb_used[idx] = 0U;
    }
}

void kfree_skb(struct sk_buff *skb) { dev_kfree_skb_any(skb); }
void dev_consume_skb_irq(struct sk_buff *skb) { dev_kfree_skb_any(skb); }

void netif_napi_add_weight(struct net_device *dev, struct napi_struct *napi,
                           int (*poll)(struct napi_struct *, int), int weight) {
    dry_lx_memset(napi, 0, sizeof(*napi));
    napi->dev = dev;
    napi->poll = poll;
    napi->weight = weight;
    dev->napi_primary = napi;
    (void)dry_lx_wq_register_napi(napi);
}

void napi_enable(struct napi_struct *napi) {
    if (napi) napi->enabled = true;
}

void napi_disable(struct napi_struct *napi) {
    if (napi) {
        napi->enabled = false;
        napi->scheduled = false;
    }
}

bool napi_schedule_prep(struct napi_struct *napi) {
    if (!napi || !napi->enabled) return false;
    if (napi->scheduled) {
        napi->missed = true;
        return false;
    }
    return true;
}

void __napi_schedule(struct napi_struct *napi) {
    if (napi) napi->scheduled = true;
}

void napi_schedule_irqoff(struct napi_struct *napi) {
    if (napi && napi->enabled) napi->scheduled = true;
}

bool napi_complete_done(struct napi_struct *napi, int work_done) {
    (void)work_done;
    if (!napi) return true;
    if (napi->missed) {
        napi->missed = false;
        return false;
    }
    napi->scheduled = false;
    return true;
}

__be16 eth_type_trans(struct sk_buff *skb, struct net_device *dev) {
    skb->dev = dev;
    skb_reset_mac_header(skb);
    const struct ethhdr *eth = (const struct ethhdr *)skb->data;
    skb_pull(skb, ETH_HLEN);
    skb_reset_network_header(skb);
    return eth->h_proto;
}

static u16 inet_csum_fold(u32 sum) {
    while (sum >> 16) {
        sum = (sum & 0xffffU) + (sum >> 16);
    }
    return (u16)(~sum);
}

static u32 csum_add_bytes(u32 sum, const u8 *buf, size_t len) {
    for (size_t i = 0; i + 1 < len; i += 2) {
        sum += ((u32)buf[i] << 8) | (u32)buf[i + 1];
    }
    if ((len & 1U) != 0U) {
        sum += (u32)buf[len - 1] << 8;
    }
    return sum;
}

static int tx_raw_frame(struct net_device *dev, const u8 *frame, size_t len) {
    if (!dev || !dev->netdev_ops || !dev->netdev_ops->ndo_start_xmit || len < 14U) {
        return -EINVAL;
    }
    size_t tx_len = (len < 60U) ? 60U : len;
    struct sk_buff *tx_skb = alloc_skb((unsigned int)tx_len, GFP_ATOMIC);
    if (!tx_skb) {
        return -ENOMEM;
    }
    tx_skb->dev = dev;
    tx_skb->ip_summed = CHECKSUM_NONE;
    dry_lx_memcpy(skb_put(tx_skb, (unsigned int)tx_len), frame, len);
    if (dev->netdev_ops->ndo_start_xmit(tx_skb, dev) != NETDEV_TX_OK) {
        dev_kfree_skb_any(tx_skb);
        return -EBUSY;
    }
    return 0;
}

static void handle_arp_rx(struct net_device *dev, const struct ethhdr *eth, const u8 *payload, size_t plen) {
    if (plen < sizeof(struct arp_pkt)) return;
    const struct arp_pkt *arp = (const struct arp_pkt *)payload;
    if (arp->htype != htons(1U) || arp->ptype != htons(ETH_P_IP) || arp->hlen != 6U || arp->plen != 4U) {
        return;
    }
    if (arp->oper == htons(2U) && arp->spa == htonl(QEMU_SLIRP_GW_IP)) {
        dry_lx_memcpy(g_gw_mac, arp->sha, ETH_ALEN);
        g_gw_mac_known = 1U;
        g_arp_replies++;
        return;
    }
    if (arp->oper == htons(1U) && arp->tpa == htonl(QEMU_SLIRP_GUEST_IP)) {
        if (arp->spa == htonl(QEMU_SLIRP_GW_IP)) {
            dry_lx_memcpy(g_gw_mac, arp->sha, ETH_ALEN);
            g_gw_mac_known = 1U;
        }
        u8 reply[60];
        dry_lx_memset(reply, 0, sizeof(reply));
        struct ethhdr *reth = (struct ethhdr *)reply;
        struct arp_pkt *rarp = (struct arp_pkt *)(reply + ETH_HLEN);
        dry_lx_memcpy(reth->h_dest, eth->h_source, ETH_ALEN);
        dry_lx_memcpy(reth->h_source, dev->dev_addr, ETH_ALEN);
        reth->h_proto = htons(ETH_P_ARP);
        rarp->htype = htons(1U);
        rarp->ptype = htons(ETH_P_IP);
        rarp->hlen = 6U;
        rarp->plen = 4U;
        rarp->oper = htons(2U);
        dry_lx_memcpy(rarp->sha, dev->dev_addr, ETH_ALEN);
        rarp->spa = htonl(QEMU_SLIRP_GUEST_IP);
        dry_lx_memcpy(rarp->tha, arp->sha, ETH_ALEN);
        rarp->tpa = arp->spa;
        if (tx_raw_frame(dev, reply, sizeof(reply)) == 0) {
            g_arp_replies++;
        }
    }
}

static void handle_ipv4_udp_rx(struct net_device *dev, const struct ethhdr *eth, const u8 *payload, size_t plen) {
    if (plen < sizeof(struct iphdr)) return;
    const struct iphdr *iph = (const struct iphdr *)payload;
    u8 ver = iph->ihl_version >> 4;
    size_t ihl = (size_t)(iph->ihl_version & 0x0fU) * 4U;
    if (ver != 4U || ihl < sizeof(struct iphdr) || iph->protocol != IPPROTO_UDP) return;
    size_t tot_len = ntohs(iph->tot_len);
    if (tot_len < ihl + sizeof(struct udphdr) || tot_len > plen) return;
    if (iph->daddr != htonl(QEMU_SLIRP_GUEST_IP)) return;

    const struct udphdr *uh = (const struct udphdr *)(payload + ihl);
    u16 dport = ntohs(uh->dest);
    u16 udp_len = ntohs(uh->len);
    if (dport != DRY_UDP_ECHO_PORT || udp_len < sizeof(struct udphdr) || ihl + udp_len > tot_len) {
        return;
    }

    size_t frame_len = ETH_HLEN + sizeof(struct iphdr) + (size_t)udp_len;
    if (frame_len > 1514U) return;
    u8 reply[1514];
    dry_lx_memset(reply, 0, 64U);

    struct ethhdr *reth = (struct ethhdr *)reply;
    struct iphdr *riph = (struct iphdr *)(reply + ETH_HLEN);
    struct udphdr *ruh = (struct udphdr *)(reply + ETH_HLEN + sizeof(struct iphdr));
    u8 *rpay = reply + ETH_HLEN + sizeof(struct iphdr) + sizeof(struct udphdr);

    dry_lx_memcpy(reth->h_dest, eth->h_source, ETH_ALEN);
    dry_lx_memcpy(reth->h_source, dev->dev_addr, ETH_ALEN);
    reth->h_proto = htons(ETH_P_IP);

    riph->ihl_version = 0x45U;
    riph->tos = 0U;
    riph->tot_len = htons((u16)(sizeof(struct iphdr) + udp_len));
    riph->id = htons(g_ip_id++);
    riph->frag_off = 0U;
    riph->ttl = 64U;
    riph->protocol = IPPROTO_UDP;
    riph->check = 0U;
    riph->saddr = iph->daddr;
    riph->daddr = iph->saddr;
    riph->check = htons(inet_csum_fold(csum_add_bytes(0U, (const u8 *)riph, sizeof(struct iphdr))));

    ruh->source = uh->dest;
    ruh->dest = uh->source;
    ruh->len = htons(udp_len);
    ruh->check = 0U;
    size_t data_len = (size_t)udp_len - sizeof(struct udphdr);
    if (data_len > 0U) {
        dry_lx_memcpy(rpay, payload + ihl + sizeof(struct udphdr), data_len);
    }

    u32 ucsum = csum_add_bytes(0U, (const u8 *)&riph->saddr, 4U);
    ucsum = csum_add_bytes(ucsum, (const u8 *)&riph->daddr, 4U);
    ucsum += (u32)IPPROTO_UDP + (u32)udp_len;
    ucsum = csum_add_bytes(ucsum, (const u8 *)ruh, (size_t)udp_len);
    u16 final_ucsum = inet_csum_fold(ucsum);
    ruh->check = htons(final_ucsum ? final_ucsum : 0xffffU);

    if (tx_raw_frame(dev, reply, frame_len) == 0) {
        g_udp_echo_pkts++;
    }
}

int netif_receive_skb(struct sk_buff *skb) {
    if (!skb) return 0;
    struct net_device *dev = skb->dev ? skb->dev : g_registered_netdev;
    if (dev) {
        const struct ethhdr *eth = (const struct ethhdr *)skb_mac_header(skb);
        if (skb->protocol == htons(ETH_P_ARP)) {
            handle_arp_rx(dev, eth, skb->data, skb->len);
        } else if (skb->protocol == htons(ETH_P_IP)) {
            handle_ipv4_udp_rx(dev, eth, skb->data, skb->len);
        }
    }
    dev_kfree_skb_any(skb);
    return 0;
}

int napi_gro_receive(struct napi_struct *napi, struct sk_buff *skb) {
    (void)napi;
    return netif_receive_skb(skb);
}

int32_t dry_lx_net_probe(u16 vendor_id, u16 device_id, u16 subvendor, u16 subdevice,
                         u16 bdf, u8 revision, u16 dev_flags,
                         const struct dry_lx_pci_bar_init *bars,
                         struct dry_lx_net_probe_info *out) {
    const struct pci_driver *drv = dry_lx_registered_pci_driver();
    if (!drv || !drv->probe) {
        return -ENODEV;
    }
    dry_lx_devm_reset();
    dry_lx_memset(g_skb_used, 0, sizeof(g_skb_used));
    dry_lx_memset(g_gw_mac, 0, sizeof(g_gw_mac));
    g_registered_netdev = NULL;
    g_gw_mac_known = 0U;
    g_arp_replies = 0ULL;
    g_udp_echo_pkts = 0ULL;

    dry_lx_pci_init_device(&g_net_pdev, vendor_id, device_id, subvendor, subdevice,
                           bdf, revision, dev_flags, bars);
    const struct pci_device_id *matched = dry_lx_match_pci_id_table(drv->id_table, &g_net_pdev);
    if (!matched) {
        return -ENODEV;
    }
    int rc = drv->probe(&g_net_pdev, matched);
    if (rc != 0 || !g_registered_netdev) {
        return rc != 0 ? rc : -ENODEV;
    }
    g_registered_netdev->running = true;
    if (g_registered_netdev->netdev_ops && g_registered_netdev->netdev_ops->ndo_open) {
        rc = g_registered_netdev->netdev_ops->ndo_open(g_registered_netdev);
        if (rc != 0) {
            g_registered_netdev->running = false;
            return rc;
        }
    }
    if (out) {
        dry_lx_memcpy(out->mac, g_registered_netdev->dev_addr, ETH_ALEN);
        out->link_up = netif_carrier_ok(g_registered_netdev) ? 1U : 0U;
        out->bar1_mapped = (dry_lx_pci_mapped_bar_mask() & (1U << 1)) ? 1U : 0U;
        out->bar1_suboff = bars ? bars[1].page_suboff : 0U;
        out->irq_is_msi = dry_lx_irq_is_msi() ? 1U : 0U;
        out->mtu = g_registered_netdev->mtu;
    }
    return 0;
}

int32_t dry_lx_net_send_arp_who_has(u32 target_ip_be) {
    struct net_device *dev = g_registered_netdev;
    if (!dev || !dev->running) {
        return -ENODEV;
    }
    u8 pkt[60];
    dry_lx_memset(pkt, 0, sizeof(pkt));
    struct ethhdr *eth = (struct ethhdr *)pkt;
    struct arp_pkt *arp = (struct arp_pkt *)(pkt + ETH_HLEN);
    dry_lx_memset(eth->h_dest, 0xff, ETH_ALEN);
    dry_lx_memcpy(eth->h_source, dev->dev_addr, ETH_ALEN);
    eth->h_proto = htons(ETH_P_ARP);
    arp->htype = htons(1U);
    arp->ptype = htons(ETH_P_IP);
    arp->hlen = 6U;
    arp->plen = 4U;
    arp->oper = htons(1U);
    dry_lx_memcpy(arp->sha, dev->dev_addr, ETH_ALEN);
    arp->spa = htonl(QEMU_SLIRP_GUEST_IP);
    dry_lx_memset(arp->tha, 0x00, ETH_ALEN);
    arp->tpa = target_ip_be ? target_ip_be : htonl(QEMU_SLIRP_GW_IP);
    return tx_raw_frame(dev, pkt, sizeof(pkt));
}

int32_t dry_lx_net_poll_step(int budget, struct dry_lx_net_stats *out) {
    if (!g_registered_netdev) {
        return -ENODEV;
    }
    (void)dry_lx_irq_poll();
    int work = dry_lx_wq_run_tick(budget);
    dry_lx_net_get_stats(out);
    return work;
}

void dry_lx_net_get_stats(struct dry_lx_net_stats *out) {
    if (!out) return;
    dry_lx_memset(out, 0, sizeof(*out));
    if (g_registered_netdev) {
        out->tx_packets = g_registered_netdev->stats.tx_packets;
        out->rx_packets = g_registered_netdev->stats.rx_packets;
        out->tx_bytes = g_registered_netdev->stats.tx_bytes;
        out->rx_bytes = g_registered_netdev->stats.rx_bytes;
        dry_lx_memcpy(out->mac, g_registered_netdev->dev_addr, ETH_ALEN);
        out->link_up = netif_carrier_ok(g_registered_netdev) ? 1U : 0U;
    }
    out->arp_replies = g_arp_replies;
    out->udp_echo_pkts = g_udp_echo_pkts;
    out->irqs_handled = dry_lx_irq_handled_count();
    out->gw_mac_known = g_gw_mac_known;
}

void dry_lx_net_shutdown(void) {
    const struct pci_driver *drv = dry_lx_registered_pci_driver();
    if (g_registered_netdev) {
        if (g_registered_netdev->running && g_registered_netdev->netdev_ops &&
            g_registered_netdev->netdev_ops->ndo_stop) {
            g_registered_netdev->netdev_ops->ndo_stop(g_registered_netdev);
            g_registered_netdev->running = false;
        }
        if (drv && drv->remove) {
            drv->remove(&g_net_pdev);
        }
    }
}
