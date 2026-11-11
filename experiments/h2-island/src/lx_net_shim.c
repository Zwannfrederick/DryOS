/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * DRY OS — H2 Networking Semantic Adapter (`sk_buff`, `NAPI`, `net_device`, PI Locks, `IoRing`)
 */
#include "../include/lx_emul/lx_net.h"

static pthread_mutex_t g_rtnl_mutex = PTHREAD_MUTEX_INITIALIZER;

void lx_sync_lock_init(pthread_mutex_t *m, bool *init_flag)
{
	pthread_mutexattr_t attr;
	pthread_mutexattr_init(&attr);
	pthread_mutexattr_setprotocol(&attr, PTHREAD_PRIO_INHERIT);
	pthread_mutexattr_settype(&attr, PTHREAD_MUTEX_RECURSIVE);
	pthread_mutex_init(m, &attr);
	pthread_mutexattr_destroy(&attr);
	*init_flag = true;
}

void lx_sync_lock(pthread_mutex_t *m, bool *init_flag)
{
	if (!*init_flag)
		lx_sync_lock_init(m, init_flag);
	pthread_mutex_lock(m);
}

void lx_sync_unlock(pthread_mutex_t *m)
{
	pthread_mutex_unlock(m);
}

void rtnl_lock(void)
{
	pthread_mutex_lock(&g_rtnl_mutex);
}

void rtnl_unlock(void)
{
	pthread_mutex_unlock(&g_rtnl_mutex);
}

struct net_device *alloc_etherdev_mqs(int sizeof_priv, unsigned int txqs, unsigned int rxqs)
{
	size_t total = ALIGN(sizeof(struct net_device), 64) + (size_t)sizeof_priv;
	uint8_t *raw = calloc(1, total);
	if (!raw)
		return NULL;
	struct net_device *dev = (struct net_device *)raw;
	dev->priv = raw + ALIGN(sizeof(struct net_device), 64);
	dev->addr_len = ETH_ALEN;
	dev->hard_header_len = ETH_HLEN;
	dev->mtu = ETH_DATA_LEN;
	dev->min_mtu = ETH_MIN_MTU;
	dev->max_mtu = 9216;
	dev->present = true;
	dev->num_tx_queues = txqs < 16 ? txqs : 16;
	dev->real_num_tx_queues = dev->num_tx_queues;
	dev->real_num_rx_queues = rxqs;
	INIT_LIST_HEAD(&dev->uc.list);
	INIT_LIST_HEAD(&dev->mc.list);
	for (unsigned int i = 0; i < 16; i++)
		spin_lock_init(&dev->_tx[i]._xmit_lock);
	strscpy(dev->name, "dryeth0", sizeof(dev->name));
	return dev;
}

void free_netdev(struct net_device *dev)
{
	free(dev);
}

int register_netdev(struct net_device *dev)
{
	if (strcmp(dev->name, "eth%d") == 0)
		strscpy(dev->name, "dryeth0", sizeof(dev->name));
	g_dry_island.registered_netdev = dev;
	return 0;
}

void unregister_netdev(struct net_device *dev)
{
	if (g_dry_island.registered_netdev == dev)
		g_dry_island.registered_netdev = NULL;
}

struct sk_buff *alloc_skb(unsigned int size, gfp_t priority)
{
	(void)priority;
	unsigned int aligned = SKB_DATA_ALIGN(size);
	size_t buf_bytes = aligned + SKB_DATA_ALIGN(sizeof(struct skb_shared_info));
	struct sk_buff *skb = calloc(1, sizeof(*skb));
	if (!skb)
		return NULL;
	skb->head = calloc(1, buf_bytes);
	if (!skb->head) {
		free(skb);
		return NULL;
	}
	skb->data = skb->head;
	skb->tail = 0;
	skb->end = aligned;
	skb->truesize = (unsigned int)(sizeof(*skb) + buf_bytes);
	skb->head_is_slab = 1;
	atomic_store(&skb->users, 1);
	return skb;
}

struct sk_buff *netdev_alloc_skb_ip_align(struct net_device *dev, unsigned int length)
{
	struct sk_buff *skb = alloc_skb(length + NET_SKB_PAD + NET_IP_ALIGN, GFP_ATOMIC);
	if (!skb)
		return NULL;
	skb->dev = dev;
	skb_reserve(skb, NET_SKB_PAD + NET_IP_ALIGN);
	return skb;
}

struct sk_buff *napi_alloc_skb(struct napi_struct *napi, unsigned int length)
{
	return netdev_alloc_skb_ip_align(napi ? napi->dev : NULL, length);
}

struct sk_buff *napi_build_skb(void *data, unsigned int frag_size)
{
	if (!data || frag_size <= SKB_DATA_ALIGN(sizeof(struct skb_shared_info)))
		return NULL;
	struct sk_buff *skb = calloc(1, sizeof(*skb));
	if (!skb)
		return NULL;
	unsigned int end_off = frag_size - (unsigned int)SKB_DATA_ALIGN(sizeof(struct skb_shared_info));
	skb->head = (unsigned char *)data;
	skb->data = (unsigned char *)data;
	skb->tail = 0;
	skb->end = end_off;
	skb->truesize = frag_size;
	skb->head_is_slab = 0;
	atomic_store(&skb->users, 1);
	memset(skb_shinfo(skb), 0, sizeof(struct skb_shared_info));
	return skb;
}

void dev_kfree_skb_any(struct sk_buff *skb)
{
	if (!skb)
		return;
	if (atomic_fetch_sub_explicit(&skb->users, 1, memory_order_acq_rel) > 1)
		return;
	struct skb_shared_info *sh = skb_shinfo(skb);
	for (u8 i = 0; i < sh->nr_frags; i++) {
		if (sh->frags[i].bv_page)
			put_page(sh->frags[i].bv_page);
	}
	if (skb->head_is_slab) {
		free(skb->head);
	} else if (skb->head) {
		struct page *pg = virt_to_page(skb->head);
		if (pg)
			put_page(pg);
	}
	free(skb);
}

void netif_napi_add(struct net_device *dev, struct napi_struct *napi,
		    int (*poll)(struct napi_struct *, int))
{
	memset(napi, 0, sizeof(*napi));
	INIT_LIST_HEAD(&napi->poll_list);
	napi->dev = dev;
	napi->poll = poll;
	napi->napi_id = dev->napi_count + 1;
	if (dev->napi_count < 16)
		dev->napi_list[dev->napi_count++] = napi;
}

void netif_napi_del(struct napi_struct *napi)
{
	napi->enabled = false;
	napi->scheduled = false;
}

bool napi_complete_done(struct napi_struct *napi, int work_done)
{
	(void)work_done;
	if (napi->missed) {
		napi->missed = false;
		return false;
	}
	napi->scheduled = false;
	return true;
}

/* Linearize `sk_buff` (mac header + linear data + paged frags) into `dry_io_ring` */
int napi_gro_receive(struct napi_struct *napi, struct sk_buff *skb)
{
	(void)napi;
	if (!skb)
		return 0;

	struct dry_io_ring *ring = &g_dry_island.ioring;
	uint32_t next_head = (ring->rx_head + 1U) % DRY_IORING_CAP;
	if (next_head != ring->rx_tail) {
		struct dry_pkt_slot *slot = &ring->rx_pkts[ring->rx_head];
		unsigned char *frame_start = skb_mac_header(skb);
		size_t mac_len = (frame_start <= skb->data) ? (size_t)(skb->data - frame_start) : 0;
		size_t head_len = skb_headlen(skb);
		size_t copied = 0;

		if (mac_len + head_len <= DRY_MAX_PKT) {
			memcpy(slot->data, frame_start, mac_len + head_len);
			copied = mac_len + head_len;
		}
		struct skb_shared_info *sh = skb_shinfo(skb);
		for (u8 i = 0; i < sh->nr_frags; i++) {
			size_t flen = skb_frag_size(&sh->frags[i]);
			if (copied + flen <= DRY_MAX_PKT) {
				memcpy(slot->data + copied, skb_frag_address(&sh->frags[i]), flen);
				copied += flen;
			}
		}
		slot->len = (uint16_t)copied;
		slot->queue_idx = skb->queue_mapping;
		slot->protocol_be = skb->protocol;
		ring->rx_head = next_head;
		ring->rx_total_packets++;
		ring->rx_total_bytes += copied;
	}
	dev_kfree_skb_any(skb);
	return 0;
}

bool dry_ioring_pop_rx(struct dry_io_ring *ring, struct dry_pkt_slot *out)
{
	if (ring->rx_tail == ring->rx_head)
		return false;
	*out = ring->rx_pkts[ring->rx_tail];
	ring->rx_tail = (ring->rx_tail + 1U) % DRY_IORING_CAP;
	return true;
}

__be16 eth_type_trans(struct sk_buff *skb, struct net_device *dev)
{
	skb->dev = dev;
	skb_reset_mac_header(skb);
	const struct ethhdr *eth = (const struct ethhdr *)skb->data;
	__skb_pull(skb, ETH_HLEN);
	skb_reset_network_header(skb);
	return eth->h_proto;
}

unsigned int eth_get_headlen(const struct net_device *dev, const void *data, unsigned int max_len)
{
	(void)dev;
	if (max_len < ETH_HLEN)
		return max_len;
	const struct ethhdr *eth = (const struct ethhdr *)data;
	if (eth->h_proto == htons(ETH_P_IP) && max_len >= ETH_HLEN + sizeof(struct iphdr))
		return ETH_HLEN + sizeof(struct iphdr);
	if (eth->h_proto == htons(ETH_P_IPV6) && max_len >= ETH_HLEN + sizeof(struct ipv6hdr))
		return ETH_HLEN + sizeof(struct ipv6hdr);
	return ETH_HLEN;
}

__wsum csum_partial(const void *buff, int len, __wsum wsum)
{
	uint64_t sum = (uint32_t)wsum;
	const uint16_t *p = (const uint16_t *)buff;
	while (len > 1) {
		sum += *p++;
		len -= 2;
	}
	if (len == 1)
		sum += *(const uint8_t *)p;
	while (sum >> 16)
		sum = (sum & 0xffffULL) + (sum >> 16);
	return (__wsum)sum;
}

int ipv6_skip_exthdr(const struct sk_buff *skb, int start, u8 *nexthdrp, __be16 *frag_offp)
{
	(void)skb; (void)nexthdrp;
	if (frag_offp)
		*frag_offp = 0;
	return start;
}
