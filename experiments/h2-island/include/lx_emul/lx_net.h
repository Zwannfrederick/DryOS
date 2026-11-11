/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * DRY OS — H2 Shadow Networking (`sk_buff`, `napi_struct`, `net_device`, `IoRing`)
 */
#ifndef LX_EMUL_NET_H
#define LX_EMUL_NET_H

#include "lx_subsys.h"

#define IFNAMSIZ       16
#define MAX_SKB_FRAGS  17
#define NET_SKB_PAD    64
#define NET_IP_ALIGN   2
#define SKB_DATA_ALIGN(X) ALIGN((X), SMP_CACHE_BYTES)
#define SKB_WITH_OVERHEAD(X) ((X) - SKB_DATA_ALIGN(sizeof(struct skb_shared_info)))

typedef int netdev_tx_t;
#define NETDEV_TX_OK   0
#define NETDEV_TX_BUSY 0x10

#define CHECKSUM_NONE        0
#define CHECKSUM_UNNECESSARY 1
#define CHECKSUM_COMPLETE    2
#define CHECKSUM_PARTIAL     3

enum pkt_hash_types {
	PKT_HASH_TYPE_NONE = 0,
	PKT_HASH_TYPE_L2,
	PKT_HASH_TYPE_L3,
	PKT_HASH_TYPE_L4,
};

#define SKBTX_HW_TSTAMP            (1 << 0)
#define SKBTX_IN_PROGRESS          (1 << 2)
#define SKBTX_HW_TSTAMP_USE_CYCLES (1 << 3)
#define SKBTX_HW_TSTAMP_NETDEV     (1 << 7)

#define SKB_GSO_TCPV4    (1 << 0)
#define SKB_GSO_UDP_L4   (1 << 17)
#define SKB_GSO_TCPV6    (1 << 4)

struct napi_struct;

struct sock {
	u32 sk_pacing_status;
	u32 sk_tsflags;
};
enum { SK_PACING_NONE = 0, SK_PACING_NEEDED = 1, SK_PACING_FQ = 2 };

typedef struct skb_frag {
	struct page *bv_page;
	u32 bv_offset;
	u32 bv_len;
} skb_frag_t;

struct skb_shared_hwtstamps {
	union {
		ktime_t hwtstamp;
		void *netdev_data;
	};
};

struct skb_shared_info {
	u8 tx_flags;
	u8 nr_frags;
	u16 gso_size;
	u16 gso_segs;
	unsigned int gso_type;
	skb_frag_t frags[MAX_SKB_FRAGS];
	struct skb_shared_hwtstamps hwtstamps;
};

struct sk_buff {
	struct net_device *dev;
	struct sock *sk;
	unsigned char *head;
	unsigned char *data;
	unsigned int tail;
	unsigned int end;
	unsigned int len;
	unsigned int data_len;
	unsigned int truesize;
	u16 queue_mapping;
	u16 mac_header;
	u16 network_header;
	u16 transport_header;
	u16 vlan_tci;
	__be16 vlan_proto;
	__be16 protocol;
	u8 ip_summed:2;
	u8 no_fcs:1;
	u8 encapsulation:1;
	u8 xmit_more:1;
	u8 vlan_present:1;
	u8 head_is_slab:1;
	u16 csum_start;
	u16 csum_offset;
	u32 hash;
	ktime_t tstamp;
	_Atomic int users;
};

static inline struct skb_shared_info *skb_shinfo(const struct sk_buff *skb)
{
	return (struct skb_shared_info *)(skb->head + skb->end);
}
static inline struct skb_shared_hwtstamps *skb_hwtstamps(struct sk_buff *skb)
{
	return &skb_shinfo(skb)->hwtstamps;
}
static inline unsigned char *skb_end_pointer(const struct sk_buff *skb)
{
	return skb->head + skb->end;
}
static inline unsigned char *skb_tail_pointer(const struct sk_buff *skb)
{
	return skb->head + skb->tail;
}
static inline unsigned int skb_headlen(const struct sk_buff *skb)
{
	return skb->len - skb->data_len;
}
static inline unsigned int skb_headroom(const struct sk_buff *skb)
{
	return (unsigned int)(skb->data - skb->head);
}
static inline void skb_reserve(struct sk_buff *skb, int len)
{
	skb->data += len;
	skb->tail += (unsigned int)len;
}
static inline void *skb_put(struct sk_buff *skb, unsigned int len)
{
	void *tmp = skb_tail_pointer(skb);
	skb->tail += len;
	skb->len += len;
	return tmp;
}
static inline void *__skb_put(struct sk_buff *skb, unsigned int len) { return skb_put(skb, len); }
static inline void *skb_put_zero(struct sk_buff *skb, unsigned int len)
{
	void *tmp = skb_put(skb, len);
	memset(tmp, 0, len);
	return tmp;
}
static inline void *__skb_pull(struct sk_buff *skb, unsigned int len)
{
	skb->len -= len;
	return skb->data += len;
}
static inline void skb_reset_mac_header(struct sk_buff *skb)
{
	skb->mac_header = (u16)(skb->data - skb->head);
}
static inline void skb_reset_network_header(struct sk_buff *skb)
{
	skb->network_header = (u16)(skb->data - skb->head);
}
static inline unsigned char *skb_mac_header(const struct sk_buff *skb)
{
	return skb->head + skb->mac_header;
}
static inline unsigned char *skb_network_header(const struct sk_buff *skb)
{
	return skb->head + skb->network_header;
}
static inline unsigned char *skb_transport_header(const struct sk_buff *skb)
{
	return skb->head + skb->transport_header;
}
static inline unsigned char *skb_checksum_start(const struct sk_buff *skb)
{
	return skb->head + skb->csum_start;
}
static inline int skb_network_offset(const struct sk_buff *skb)
{
	return (int)(skb_network_header(skb) - skb->data);
}
static inline int skb_checksum_start_offset(const struct sk_buff *skb)
{
	return (int)(skb->csum_start - (skb->data - skb->head));
}
static inline struct iphdr *ip_hdr(const struct sk_buff *skb)
{
	return (struct iphdr *)skb_network_header(skb);
}
static inline struct ipv6hdr *ipv6_hdr(const struct sk_buff *skb)
{
	return (struct ipv6hdr *)skb_network_header(skb);
}
static inline struct tcphdr *tcp_hdr(const struct sk_buff *skb)
{
	return (struct tcphdr *)skb_transport_header(skb);
}
static inline unsigned int tcp_hdrlen(const struct sk_buff *skb)
{
	return (unsigned int)tcp_hdr(skb)->doff * 4U;
}
static inline struct iphdr *inner_ip_hdr(const struct sk_buff *skb) { return ip_hdr(skb); }
static inline struct ipv6hdr *inner_ipv6_hdr(const struct sk_buff *skb) { return ipv6_hdr(skb); }
static inline struct tcphdr *inner_tcp_hdr(const struct sk_buff *skb) { return tcp_hdr(skb); }
static inline unsigned int inner_tcp_hdrlen(const struct sk_buff *skb) { return tcp_hdrlen(skb); }
static inline unsigned char *skb_inner_network_header(const struct sk_buff *skb) { return skb_network_header(skb); }
static inline int skb_inner_network_offset(const struct sk_buff *skb) { return skb_network_offset(skb); }

static inline bool skb_is_gso(const struct sk_buff *skb) { return skb_shinfo(skb)->gso_size != 0; }
static inline bool skb_is_gso_v6(const struct sk_buff *skb) { return (skb_shinfo(skb)->gso_type & SKB_GSO_TCPV6) != 0; }
static inline bool skb_csum_is_sctp(struct sk_buff *skb) { (void)skb; return false; }
static inline void skb_tx_timestamp(struct sk_buff *skb) { (void)skb; }
static inline void skb_tstamp_tx(struct sk_buff *orig_skb, struct skb_shared_hwtstamps *hwtstamps)
{
	(void)orig_skb; (void)hwtstamps;
}
static inline void skb_checksum_none_assert(const struct sk_buff *skb) { (void)skb; }
static inline void skb_metadata_set(struct sk_buff *skb, u8 meta_len) { (void)skb; (void)meta_len; }
static inline void skb_record_rx_queue(struct sk_buff *skb, u16 rx_queue) { skb->queue_mapping = rx_queue; }
static inline void skb_set_hash(struct sk_buff *skb, u32 hash, enum pkt_hash_types type)
{
	(void)type;
	skb->hash = hash;
}
static inline bool skb_vlan_tag_present(const struct sk_buff *skb) { return skb->vlan_present; }
static inline u16 skb_vlan_tag_get(const struct sk_buff *skb) { return skb->vlan_tci; }
static inline void __vlan_hwaccel_put_tag(struct sk_buff *skb, __be16 vlan_proto, u16 vlan_tci)
{
	skb->vlan_proto = vlan_proto;
	skb->vlan_tci = vlan_tci;
	skb->vlan_present = 1;
}
static inline __be16 vlan_get_protocol(struct sk_buff *skb) { return skb->protocol; }
static inline struct sk_buff *skb_get(struct sk_buff *skb)
{
	atomic_fetch_add_explicit(&skb->users, 1, memory_order_relaxed);
	return skb;
}
static inline int skb_cow_head(struct sk_buff *skb, unsigned int headroom)
{
	(void)skb; (void)headroom;
	return 0;
}
static inline int skb_padto(struct sk_buff *skb, unsigned int len)
{
	if (likely(skb->len >= len))
		return 0;
	memset(skb->data + skb->len, 0, len - skb->len);
	return 0;
}
static inline int eth_skb_pad(struct sk_buff *skb) { return skb_padto(skb, ETH_ZLEN); }
static inline bool netdev_xmit_more(void) { return false; }

static inline unsigned int skb_frag_size(const skb_frag_t *frag) { return frag->bv_len; }
static inline void *skb_frag_address(const skb_frag_t *frag)
{
	return (uint8_t *)page_address(frag->bv_page) + frag->bv_offset;
}
static inline dma_addr_t skb_frag_dma_map(struct device *dev, const skb_frag_t *frag,
					  size_t offset, size_t size,
					  enum dma_data_direction dir)
{
	return dma_map_page(dev, frag->bv_page, frag->bv_offset + offset, size, dir);
}
static inline void skb_add_rx_frag(struct sk_buff *skb, int i, struct page *page,
				   int off, int size, unsigned int truesize)
{
	struct skb_shared_info *sh = skb_shinfo(skb);
	sh->frags[i].bv_page = page;
	sh->frags[i].bv_offset = (u32)off;
	sh->frags[i].bv_len = (u32)size;
	sh->nr_frags = (u8)(i + 1);
	skb->len += (unsigned int)size;
	skb->data_len += (unsigned int)size;
	skb->truesize += truesize;
}

struct sk_buff *alloc_skb(unsigned int size, gfp_t priority);
struct sk_buff *netdev_alloc_skb_ip_align(struct net_device *dev, unsigned int length);
struct sk_buff *napi_alloc_skb(struct napi_struct *napi, unsigned int length);
struct sk_buff *napi_build_skb(void *data, unsigned int frag_size);
void dev_kfree_skb_any(struct sk_buff *skb);
static inline void consume_skb(struct sk_buff *skb) { dev_kfree_skb_any(skb); }
static inline void dev_kfree_skb(struct sk_buff *skb) { dev_kfree_skb_any(skb); }
static inline void dev_consume_skb_any(struct sk_buff *skb) { dev_kfree_skb_any(skb); }
static inline void napi_consume_skb(struct sk_buff *skb, int budget) { (void)budget; dev_kfree_skb_any(skb); }

/* Checksum helpers */
__wsum csum_partial(const void *buff, int len, __wsum wsum);
static inline __sum16 csum_fold(__wsum sum)
{
	u32 s = (u32)sum;
	s = (s & 0xffffU) + (s >> 16);
	s = (s & 0xffffU) + (s >> 16);
	return (__sum16)~s;
}
static inline void csum_replace_by_diff(__sum16 *sum, __wsum diff)
{
	*sum = csum_fold(csum_partial(&diff, sizeof(diff), ~(*sum) & 0xffffU));
}
static inline __sum16 csum_tcpudp_magic(__be32 saddr, __be32 daddr, __u32 len,
					__u8 proto, __wsum sum)
{
	u64 s = (u32)saddr + (u32)daddr + htonl(len + proto) + (u32)sum;
	while (s >> 16)
		s = (s & 0xffffULL) + (s >> 16);
	return (__sum16)~s;
}
static inline __sum16 csum_ipv6_magic(const struct in6_addr *saddr,
				      const struct in6_addr *daddr,
				      __u32 len, __u8 proto, __wsum csum)
{
	u64 s = (u32)csum + htonl(len) + htonl(proto);
	for (int i = 0; i < 4; i++)
		s += (u32)saddr->in6_u.u6_addr32[i] + (u32)daddr->in6_u.u6_addr32[i];
	while (s >> 16)
		s = (s & 0xffffULL) + (s >> 16);
	return (__sum16)~s;
}
static inline int skb_checksum_help(struct sk_buff *skb) { (void)skb; return 0; }
int ipv6_skip_exthdr(const struct sk_buff *skb, int start, u8 *nexthdrp, __be16 *frag_offp);

/* NAPI & net_device */
#define NAPI_POLL_WEIGHT 64
struct napi_struct {
	struct list_head poll_list;
	struct net_device *dev;
	int (*poll)(struct napi_struct *, int);
	unsigned int napi_id;
	int irq;
	bool scheduled;
	bool enabled;
	bool missed;
};

void netif_napi_add(struct net_device *dev, struct napi_struct *napi,
		    int (*poll)(struct napi_struct *, int));
void netif_napi_del(struct napi_struct *napi);
static inline void netif_napi_set_irq(struct napi_struct *napi, int irq) { napi->irq = irq; }
static inline void napi_enable(struct napi_struct *napi) { napi->enabled = true; }
static inline void napi_disable(struct napi_struct *napi) { napi->enabled = false; napi->scheduled = false; }
static inline void napi_synchronize(const struct napi_struct *napi) { (void)napi; }
static inline void napi_schedule(struct napi_struct *napi)
{
	if (napi->enabled)
		napi->scheduled = true;
}
static inline void napi_schedule_irqoff(struct napi_struct *napi) { napi_schedule(napi); }
static inline bool napi_if_scheduled_mark_missed(struct napi_struct *napi)
{
	if (napi->scheduled) {
		napi->missed = true;
		return true;
	}
	return false;
}
bool napi_complete_done(struct napi_struct *napi, int work_done);
int napi_gro_receive(struct napi_struct *napi, struct sk_buff *skb);
static inline int netif_rx(struct sk_buff *skb) { return napi_gro_receive(NULL, skb); }

/* Netdevice flags & features */
#define IFF_UP          BIT(0)
#define IFF_PROMISC     BIT(8)
#define IFF_ALLMULTI    BIT(9)

#define NETIF_F_SG                  BIT_ULL(0)
#define NETIF_F_HW_CSUM             BIT_ULL(3)
#define NETIF_F_HIGHDMA             BIT_ULL(5)
#define NETIF_F_HW_VLAN_CTAG_TX     BIT_ULL(7)
#define NETIF_F_HW_VLAN_CTAG_RX     BIT_ULL(8)
#define NETIF_F_HW_VLAN_CTAG_FILTER BIT_ULL(9)
#define NETIF_F_TSO                 BIT_ULL(11)
#define NETIF_F_GSO_PARTIAL         BIT_ULL(13)
#define NETIF_F_TSO_ECN             BIT_ULL(14)
#define NETIF_F_TSO6                BIT_ULL(16)
#define NETIF_F_TSO_MANGLEID        BIT_ULL(18)
#define NETIF_F_GSO_GRE             BIT_ULL(19)
#define NETIF_F_GSO_GRE_CSUM        BIT_ULL(20)
#define NETIF_F_GSO_IPXIP4          BIT_ULL(21)
#define NETIF_F_GSO_IPXIP6          BIT_ULL(22)
#define NETIF_F_GSO_UDP_TUNNEL      BIT_ULL(23)
#define NETIF_F_GSO_UDP_TUNNEL_CSUM BIT_ULL(24)
#define NETIF_F_SCTP_CRC            BIT_ULL(25)
#define NETIF_F_NTUPLE              BIT_ULL(27)
#define NETIF_F_RXHASH              BIT_ULL(28)
#define NETIF_F_RXCSUM              BIT_ULL(29)
#define NETIF_F_HW_TC               BIT_ULL(30)
#define NETIF_F_RXALL               BIT_ULL(31)

#define NETIF_MSG_DRV   0x0001
#define NETIF_MSG_PROBE 0x0002
#define NETIF_MSG_LINK  0x0004
#define NETIF_MSG_HW    0x0080
static inline u32 netif_msg_init(int debug_value, int default_msg_enable_bits)
{
	return (debug_value < 0) ? (u32)default_msg_enable_bits : (u32)debug_value;
}

enum netdev_queue_type { NETDEV_QUEUE_TYPE_RX = 0, NETDEV_QUEUE_TYPE_TX = 1 };

struct netdev_queue {
	spinlock_t _xmit_lock;
	int xmit_lock_owner;
	unsigned long trans_start;
	bool stopped;
	unsigned int bytes_queued;
};

struct netdev_hw_addr {
	struct list_head list;
	unsigned char addr[ETH_ALEN];
};
struct netdev_hw_addr_list {
	struct list_head list;
	int count;
};
#define netdev_for_each_mc_addr(ha, dev) \
	list_for_each_entry(ha, &(dev)->mc.list, list)
static inline bool netdev_mc_empty(const struct net_device *dev);
static inline int netdev_uc_count(const struct net_device *dev);

struct netdev_tc_txq { u16 count; u16 offset; };

struct rtnl_link_stats64 {
	u64 rx_packets, tx_packets;
	u64 rx_bytes, tx_bytes;
	u64 rx_errors, tx_errors;
	u64 rx_dropped, tx_dropped;
	u64 multicast, collisions;
	u64 rx_length_errors, rx_over_errors, rx_crc_errors, rx_frame_errors;
	u64 rx_fifo_errors, rx_missed_errors;
	u64 tx_aborted_errors, tx_carrier_errors, tx_fifo_errors, tx_heartbeat_errors, tx_window_errors;
};

struct sockaddr {
	unsigned short sa_family;
	char sa_data[14];
};

struct net_device_ops {
	int (*ndo_open)(struct net_device *dev);
	int (*ndo_stop)(struct net_device *dev);
	netdev_tx_t (*ndo_start_xmit)(struct sk_buff *skb, struct net_device *dev);
	void (*ndo_set_rx_mode)(struct net_device *dev);
	int (*ndo_set_mac_address)(struct net_device *dev, void *addr);
	int (*ndo_validate_addr)(struct net_device *dev);
	int (*ndo_eth_ioctl)(struct net_device *dev, struct ifreq *ifr, int cmd);
	int (*ndo_change_mtu)(struct net_device *dev, int new_mtu);
	void (*ndo_tx_timeout)(struct net_device *dev, unsigned int txqueue);
	void (*ndo_get_stats64)(struct net_device *dev, struct rtnl_link_stats64 *storage);
	netdev_features_t (*ndo_fix_features)(struct net_device *dev, netdev_features_t features);
	int (*ndo_set_features)(struct net_device *dev, netdev_features_t features);
	netdev_features_t (*ndo_features_check)(struct sk_buff *skb, struct net_device *dev,
						netdev_features_t features);
	int (*ndo_setup_tc)(struct net_device *dev, enum tc_setup_type type, void *type_data);
	int (*ndo_bpf)(struct net_device *dev, struct netdev_bpf *bpf);
	int (*ndo_xdp_xmit)(struct net_device *dev, int n, struct xdp_frame **xdp, u32 flags);
	int (*ndo_xsk_wakeup)(struct net_device *dev, u32 queue_id, u32 flags);
	ktime_t (*ndo_get_tstamp)(struct net_device *dev, const struct skb_shared_hwtstamps *shhwtstamps,
				  bool cycles);
	int (*ndo_hwtstamp_get)(struct net_device *dev, struct kernel_hwtstamp_config *kernel_config);
	int (*ndo_hwtstamp_set)(struct net_device *dev, struct kernel_hwtstamp_config *kernel_config,
				struct netlink_ext_ack *extack);
};

struct net_device {
	char name[IFNAMSIZ];
	struct device dev;
	const struct net_device_ops *netdev_ops;
	const struct ethtool_ops *ethtool_ops;
	const struct xdp_metadata_ops *xdp_metadata_ops;
	const struct xsk_tx_metadata_ops *xsk_tx_metadata_ops;
	unsigned long state;
	unsigned int flags;
	netdev_features_t features;
	netdev_features_t hw_features;
	netdev_features_t vlan_features;
	netdev_features_t hw_enc_features;
	netdev_features_t mpls_features;
	netdev_features_t gso_partial_features;
	u64 xdp_features;
	unsigned int mtu;
	unsigned int min_mtu;
	unsigned int max_mtu;
	unsigned short hard_header_len;
	unsigned char addr_len;
	unsigned char dev_addr[ETH_ALEN];
	unsigned char perm_addr[ETH_ALEN];
	unsigned long mem_start;
	unsigned long mem_end;
	int watchdog_timeo;
	bool dismantle;
	bool carrier_up;
	bool present;
	bool running;
	u8 num_tc;
	struct netdev_tc_txq tc_to_txq[TC_QOPT_MAX_QUEUE];
	struct netdev_hw_addr_list uc;
	struct netdev_hw_addr_list mc;
	unsigned int num_tx_queues;
	unsigned int real_num_tx_queues;
	unsigned int real_num_rx_queues;
	struct netdev_queue _tx[16];
	struct napi_struct *napi_list[16];
	unsigned int napi_count;
	void *priv;
};

#define SET_NETDEV_DEV(net, pdev) do { (net)->dev.parent = (pdev); } while (0)
static inline void *netdev_priv(const struct net_device *dev) { return dev->priv; }
static inline const char *netdev_name(const struct net_device *dev) { return dev->name; }
static inline struct netdev_queue *netdev_get_tx_queue(const struct net_device *dev, unsigned int index)
{
	return (struct netdev_queue *)&dev->_tx[index < 16 ? index : 0];
}
static inline bool netdev_mc_empty(const struct net_device *dev) { return list_empty(&dev->mc.list); }
static inline int netdev_mc_count(const struct net_device *dev) { return dev->mc.count; }
static inline int netdev_uc_count(const struct net_device *dev) { return dev->uc.count; }

struct net_device *alloc_etherdev_mqs(int sizeof_priv, unsigned int txqs, unsigned int rxqs);
#define alloc_etherdev_mq(sizeof_priv, count) alloc_etherdev_mqs((sizeof_priv), (count), (count))
void free_netdev(struct net_device *dev);
int register_netdev(struct net_device *dev);
void unregister_netdev(struct net_device *dev);

static inline bool netif_running(const struct net_device *dev) { return dev->running; }
static inline bool netif_device_present(const struct net_device *dev) { return dev->present; }
static inline void netif_device_detach(struct net_device *dev) { dev->present = false; }
static inline void netif_device_attach(struct net_device *dev) { dev->present = true; }
static inline bool netif_carrier_ok(const struct net_device *dev) { return dev->carrier_up; }
static inline void netif_carrier_on(struct net_device *dev) { dev->carrier_up = true; }
static inline void netif_carrier_off(struct net_device *dev) { dev->carrier_up = false; }

static inline void netif_tx_start_all_queues(struct net_device *dev)
{
	for (unsigned int i = 0; i < dev->num_tx_queues; i++)
		dev->_tx[i].stopped = false;
}
static inline void netif_tx_stop_all_queues(struct net_device *dev)
{
	for (unsigned int i = 0; i < dev->num_tx_queues; i++)
		dev->_tx[i].stopped = true;
}
static inline void netif_stop_subqueue(struct net_device *dev, u16 q) { netdev_get_tx_queue(dev, q)->stopped = true; }
static inline void netif_wake_subqueue(struct net_device *dev, u16 q) { netdev_get_tx_queue(dev, q)->stopped = false; }
static inline void netif_tx_wake_queue(struct netdev_queue *q) { q->stopped = false; }
static inline bool __netif_subqueue_stopped(const struct net_device *dev, u16 q) { return netdev_get_tx_queue(dev, q)->stopped; }
static inline bool netif_xmit_stopped(const struct netdev_queue *q) { return q->stopped; }
static inline void __netif_tx_lock(struct netdev_queue *nq, int cpu) { spin_lock(&nq->_xmit_lock); nq->xmit_lock_owner = cpu; }
static inline void __netif_tx_unlock(struct netdev_queue *nq) { nq->xmit_lock_owner = -1; spin_unlock(&nq->_xmit_lock); }
static inline void txq_trans_cond_update(struct netdev_queue *nq) { nq->trans_start = jiffies; }
static inline void netif_trans_update(struct net_device *dev) { dev->_tx[0].trans_start = jiffies; }

static inline int netif_set_real_num_tx_queues(struct net_device *dev, unsigned int txq) { dev->real_num_tx_queues = txq; return 0; }
static inline int netif_set_real_num_rx_queues(struct net_device *dev, unsigned int rxq) { dev->real_num_rx_queues = rxq; return 0; }
static inline int netif_set_real_num_queues(struct net_device *dev, unsigned int txq, unsigned int rxq)
{
	dev->real_num_tx_queues = txq;
	dev->real_num_rx_queues = rxq;
	return 0;
}
static inline void netif_queue_set_napi(struct net_device *dev, unsigned int queue_index,
					enum netdev_queue_type type, struct napi_struct *napi)
{
	(void)dev; (void)queue_index; (void)type; (void)napi;
}
static inline void netdev_reset_tc(struct net_device *dev) { dev->num_tc = 0; }
static inline int netdev_set_num_tc(struct net_device *dev, u8 num_tc) { dev->num_tc = num_tc; return 0; }
static inline int netdev_set_tc_queue(struct net_device *dev, u8 tc, u16 count, u16 offset)
{
	if (tc < TC_QOPT_MAX_QUEUE) {
		dev->tc_to_txq[tc].count = count;
		dev->tc_to_txq[tc].offset = offset;
	}
	return 0;
}

static inline void netdev_tx_sent_queue(struct netdev_queue *q, unsigned int bytes) { q->bytes_queued += bytes; }
static inline void netdev_tx_completed_queue(struct netdev_queue *q, unsigned int pkts, unsigned int bytes)
{
	(void)pkts;
	q->bytes_queued = (q->bytes_queued > bytes) ? (q->bytes_queued - bytes) : 0;
}
static inline void netdev_tx_reset_queue(struct netdev_queue *q) { q->bytes_queued = 0; }
static inline void netdev_rss_key_fill(void *buffer, size_t len) { memset(buffer, 0x6d, len); }

static inline int __dev_uc_sync(struct net_device *dev,
				int (*sync)(struct net_device *, const unsigned char *),
				int (*unsync)(struct net_device *, const unsigned char *))
{
	(void)dev; (void)sync; (void)unsync;
	return 0;
}
static inline void __dev_uc_unsync(struct net_device *dev,
				   int (*unsync)(struct net_device *, const unsigned char *))
{
	(void)dev; (void)unsync;
}

#define netdev_err(dev, fmt, ...)       fprintf(stderr, "[%s ERR] " fmt, (dev)->name, ##__VA_ARGS__)
#define netdev_err_once(dev, fmt, ...)  fprintf(stderr, "[%s ERR] " fmt, (dev)->name, ##__VA_ARGS__)
#define netdev_warn(dev, fmt, ...)      do { (void)(dev); } while (0)
#define netdev_warn_once(dev, fmt, ...) do { (void)(dev); } while (0)
#define netdev_info(dev, fmt, ...)      do { (void)(dev); } while (0)
#define netdev_dbg(dev, fmt, ...)       do { (void)(dev); } while (0)
#define netif_dbg(priv, type, dev, fmt, ...) do { (void)(priv); (void)(dev); } while (0)

/* Ethernet address & header helpers */
static inline bool is_zero_ether_addr(const u8 *addr)
{
	return (addr[0] | addr[1] | addr[2] | addr[3] | addr[4] | addr[5]) == 0;
}
static inline bool is_multicast_ether_addr(const u8 *addr) { return (addr[0] & 0x01) != 0; }
static inline bool is_valid_ether_addr(const u8 *addr)
{
	return !is_multicast_ether_addr(addr) && !is_zero_ether_addr(addr);
}
static inline bool ether_addr_equal(const u8 *a, const u8 *b) { return memcmp(a, b, ETH_ALEN) == 0; }
static inline void eth_hw_addr_set(struct net_device *dev, const u8 *addr)
{
	memcpy(dev->dev_addr, addr, ETH_ALEN);
}
static inline int eth_platform_get_mac_address(struct device *dev, u8 *mac_addr)
{
	if (dev && dev->has_mac) {
		memcpy(mac_addr, dev->mac_addr, ETH_ALEN);
		return 0;
	}
	return -ENODEV;
}
__be16 eth_type_trans(struct sk_buff *skb, struct net_device *dev);
unsigned int eth_get_headlen(const struct net_device *dev, const void *data, unsigned int max_len);
static inline netdev_features_t passthru_features_check(struct sk_buff *skb, struct net_device *dev,
							netdev_features_t features)
{
	(void)skb; (void)dev;
	return features;
}

#include "lx_xdp_stubs.h"

#endif /* LX_EMUL_NET_H */
