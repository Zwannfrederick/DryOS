/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * DRY OS — H2 Protocol Headers, Pruned Subsystems (PTP/Ethtool/TC), and
 * Unsupported Behavior Stubs (in-kernel eBPF/XDP/AF_XDP)
 */
#ifndef LX_EMUL_SUBSYS_H
#define LX_EMUL_SUBSYS_H

#include "lx_pci_irq.h"

struct net_device;
struct sk_buff;

/* Netlink extack */
struct netlink_ext_ack {
  const char *_msg;
};
#define NL_SET_ERR_MSG_MOD(extack, msg)                                        \
  do {                                                                         \
    if (extack)                                                                \
      (extack)->_msg = (msg);                                                  \
  } while (0)
#define NL_SET_ERR_MSG_FMT_MOD(extack, fmt, ...)                               \
  do {                                                                         \
    if (extack)                                                                \
      (extack)->_msg = (fmt);                                                  \
  } while (0)

/* Ethernet, VLAN, IPv4, IPv6, TCP, UDP, SCTP, MDIO */
#define ETH_ALEN 6
#define ETH_TLEN 2
#define ETH_HLEN 14
#define ETH_ZLEN 60
#define ETH_DATA_LEN 1500
#define ETH_FRAME_LEN 1514
#define ETH_FCS_LEN 4
#define ETH_MIN_MTU 68

#define ETH_P_IP 0x0800
#define ETH_P_IPV6 0x86DD
#define ETH_P_8021Q 0x8100
#define ETH_P_8021AD 0x88A8
#define ETH_P_ARP 0x0806
#define ETH_P_1588 0x88F7

#define VLAN_HLEN 4
#define VLAN_ETH_HLEN 18
#define VLAN_VID_MASK 0x0fff
#define VLAN_PRIO_MASK 0xe000
#define VLAN_PRIO_SHIFT 13

#define IPPROTO_TCP 6
#define IPPROTO_UDP 17
#define IPPROTO_SCTP 132
#define IPPROTO_ICMPV6 58
#define NEXTHDR_TCP 6
#define NEXTHDR_UDP 17
#define NEXTHDR_SCTP 132

struct ethhdr {
  unsigned char h_dest[ETH_ALEN];
  unsigned char h_source[ETH_ALEN];
  __be16 h_proto;
} __packed;

struct vlan_hdr {
  __be16 h_vlan_TCI;
  __be16 h_vlan_encapsulated_proto;
} __packed;

struct vlan_ethhdr {
  unsigned char h_dest[ETH_ALEN];
  unsigned char h_source[ETH_ALEN];
  __be16 h_vlan_proto;
  __be16 h_vlan_TCI;
  __be16 h_vlan_encapsulated_proto;
} __packed;

struct iphdr {
  u8 ihl : 4, version : 4;
  u8 tos;
  __be16 tot_len;
  __be16 id;
  __be16 frag_off;
  u8 ttl;
  u8 protocol;
  __sum16 check;
  __be32 saddr;
  __be32 daddr;
} __packed;

struct in6_addr {
  union {
    __u8 u6_addr8[16];
    __be16 u6_addr16[8];
    __be32 u6_addr32[4];
  } in6_u;
};

struct ipv6hdr {
  u8 priority : 4, version : 4;
  u8 flow_lbl[3];
  __be16 payload_len;
  u8 nexthdr;
  u8 hop_limit;
  struct in6_addr saddr;
  struct in6_addr daddr;
} __packed;

struct tcphdr {
  __be16 source;
  __be16 dest;
  __be32 seq;
  __be32 ack_seq;
  u16 res1 : 4, doff : 4, fin : 1, syn : 1, rst : 1, psh : 1, ack : 1, urg : 1,
      ece : 1, cwr : 1;
  __be16 window;
  __sum16 check;
  __be16 urg_ptr;
} __packed;

struct udphdr {
  __be16 source;
  __be16 dest;
  __be16 len;
  __sum16 check;
} __packed;

struct sctphdr {
  __be16 source;
  __be16 dest;
  __be32 vtag;
  __le32 checksum;
} __packed;

/* MDIO / MII */
#define MII_BMCR 0x00
#define MII_BMSR 0x01
#define MII_PHYSID1 0x02
#define MII_PHYSID2 0x03
#define MII_ADVERTISE 0x04
#define MII_LPA 0x05
#define MII_CTRL1000 0x09
#define MII_STAT1000 0x0a
#define MDIO_MMD_PMAPMD 1
#define MDIO_MMD_PCS 3
#define MDIO_MMD_AN 7
#define MDIO_MMD_VEND1 30
#define MDIO_MMD_VEND2 31
#define MDIO_AN_EEE_ADV 60
#define MDIO_AN_EEE_LPABLE 61
#define MDIO_AN_EEE_ADV2 62
#define MDIO_AN_EEE_LPABLE2 63
#define MDIO_PMA_2_5G_STAT 130

#define DUPLEX_HALF 0x00
#define DUPLEX_FULL 0x01
#define DUPLEX_UNKNOWN 0xff
#define SPEED_10 10
#define SPEED_100 100
#define SPEED_1000 1000
#define SPEED_2500 2500
#define SPEED_UNKNOWN -1

/* Ethtool & 802.3br FPE MMSV (feature_prune) */
struct ethtool_ops {
  int dummy;
};
struct ethtool_keee {
  u32 tx_lpi_timer;
  bool eee_enabled;
  bool eee_active;
  bool tx_lpi_enabled;
};
enum ethtool_mmsv_event {
  ETHTOOL_MMSV_LD_SENT_VERIFY_MPACKET = 0,
  ETHTOOL_MMSV_LP_SENT_VERIFY_MPACKET = 1,
  ETHTOOL_MMSV_LP_SENT_RESPONSE_MPACKET = 2,
  ETHTOOL_MMSV_LD_VERIFICATION_SUCCEEDED = 3,
};
struct ethtool_mmsv {
  bool pmac_enabled;
  bool tx_enabled;
};
static inline void ethtool_mmsv_event_handle(struct ethtool_mmsv *mmsv,
                                             enum ethtool_mmsv_event ev) {
  (void)mmsv;
  (void)ev;
}
static inline void ethtool_mmsv_link_state_handle(struct ethtool_mmsv *mmsv,
                                                  bool up) {
  (void)mmsv;
  (void)up;
}
static inline void ethtool_mmsv_stop(struct ethtool_mmsv *mmsv) { (void)mmsv; }

/* PTP / Timecounter / HWTSTAMP (feature_prune) */
#define SIOCSHWTSTAMP 0x89b0
#define SIOCGHWTSTAMP 0x89b1
#define SOF_TIMESTAMPING_BIND_PHC (1 << 15)

struct hwtstamp_config {
  int flags;
  int tx_type;
  int rx_filter;
};
struct kernel_hwtstamp_config {
  int flags;
  int tx_type;
  int rx_filter;
  struct ifreq *ifr;
  bool copied_to_user;
  int source;
};
struct ifreq {
  union {
    char ifr_name[16];
  } ifr_ifrn;
  union {
    void *ifru_data;
  } ifr_ifru;
};
#define ifr_data ifr_ifru.ifru_data

enum ptp_pin_function {
  PTP_PF_NONE = 0,
  PTP_PF_EXTTS,
  PTP_PF_PEROUT,
  PTP_PF_PHYSYNC
};
struct ptp_pin_desc {
  char name[64];
  unsigned int index;
  enum ptp_pin_function func;
  unsigned int chan;
  unsigned int rsv[5];
};
struct ptp_clock;
struct ptp_clock_info {
  void *owner;
  char name[32];
  s32 max_adj;
  int n_alarm;
  int n_ext_ts;
  int n_per_out;
  int n_pins;
  int pps;
  struct ptp_pin_desc *pin_config;
};
#define PTP_CLOCK_EXTTS 1
#define PTP_CLOCK_PPS 2
struct ptp_clock_event {
  int type;
  int index;
  s64 timestamp;
};
static inline void ptp_clock_event(struct ptp_clock *ptp,
                                   struct ptp_clock_event *ev) {
  (void)ptp;
  (void)ev;
}
struct cyclecounter {
  u64 (*read)(const struct cyclecounter *cc);
  u64 mask;
  u32 mult;
  u32 shift;
};
struct timecounter {
  const struct cyclecounter *cc;
  u64 cycle_last;
  u64 nsec;
};
struct hwtstamp_provider {
  int source;
  int qualifier;
};
struct igc_led_classdev;

/* Traffic Control / TSN Qdisc Offload structs (feature_prune) */
enum tc_setup_type {
  TC_QUERY_CAPS = 0,
  TC_SETUP_QDISC_MQPRIO,
  TC_SETUP_QDISC_CBS,
  TC_SETUP_QDISC_ETF,
  TC_SETUP_QDISC_TAPRIO,
};
struct tc_query_caps_base {
  enum tc_setup_type type;
  void *caps;
};
struct tc_mqprio_caps {
  bool validate_queue_counts;
};
struct tc_taprio_caps {
  bool supports_queue_max_sdu;
  bool gate_mask_per_txq;
  bool broken_mqprio;
};

#define TC_QOPT_MAX_QUEUE 16
#define TC_MQPRIO_HW_OFFLOAD_TCS 1
struct tc_mqprio_qopt {
  __u8 num_tc;
  __u8 prio_tc_map[TC_QOPT_MAX_QUEUE];
  __u8 hw;
  __u16 count[TC_QOPT_MAX_QUEUE];
  __u16 offset[TC_QOPT_MAX_QUEUE];
};
struct tc_mqprio_qopt_offload {
  struct tc_mqprio_qopt qopt;
  struct netlink_ext_ack *extack;
  u32 preemptible_tcs;
};
struct tc_cbs_qopt_offload {
  u8 enable;
  s32 queue;
  s32 hicredit;
  s32 locredit;
  s32 idleslope;
  s32 sendslope;
};
struct tc_etf_qopt_offload {
  u8 enable;
  s32 queue;
};
enum tc_taprio_qopt_cmd {
  TAPRIO_CMD_REPLACE = 0,
  TAPRIO_CMD_DESTROY,
  TAPRIO_CMD_STATS,
  TAPRIO_CMD_QUEUE_STATS,
};
#define TC_TAPRIO_CMD_SET_GATES 0x00
struct tc_taprio_sched_entry {
  u8 command;
  u32 gate_mask;
  u32 interval;
};
struct tc_taprio_qopt_stats {
  u64 window_drops;
  u64 tx_overruns;
};
struct tc_taprio_qopt_queue_stats {
  int queue;
  struct tc_taprio_qopt_stats stats;
};
struct tc_taprio_qopt_offload {
  enum tc_taprio_qopt_cmd cmd;
  struct netlink_ext_ack *extack;
  ktime_t base_time;
  u64 cycle_time;
  u64 cycle_time_extension;
  u32 max_sdu[TC_QOPT_MAX_QUEUE];
  struct tc_mqprio_qopt_offload mqprio;
  struct tc_taprio_qopt_stats stats;
  struct tc_taprio_qopt_queue_stats queue_stats;
  size_t num_entries;
  struct tc_taprio_sched_entry entries[];
};

/*
 * Unsupported monolithic-kernel behaviors (`bpf_prog`, `xdp_*`, `xsk_*`):
 * Kept ONLY as dead/unreachable compile-time type stubs (`adapter->xdp_prog ==
 * NULL`, `ring->xsk_pool == NULL`). Invoking XDP/AF_XDP setup returns
 * `-EOPNOTSUPP`.
 */
struct bpf_prog {
  int unused;
};
enum xdp_action { XDP_ABORTED = 0, XDP_DROP, XDP_PASS, XDP_TX, XDP_REDIRECT };
enum xdp_mem_type { MEM_TYPE_PAGE_SHARED = 0, MEM_TYPE_XSK_BUFF_POOL = 2 };
enum xdp_rss_hash_type {
  XDP_RSS_TYPE_NONE = 0,
  XDP_RSS_TYPE_L2,
  XDP_RSS_TYPE_L3_IPV4,
  XDP_RSS_TYPE_L3_IPV6,
  XDP_RSS_TYPE_L3_IPV6_EX,
  XDP_RSS_TYPE_L4_IPV4_TCP,
  XDP_RSS_TYPE_L4_IPV4_UDP,
  XDP_RSS_TYPE_L4_IPV6_TCP,
  XDP_RSS_TYPE_L4_IPV6_TCP_EX,
  XDP_RSS_TYPE_L4_IPV6_UDP,
  XDP_RSS_TYPE_L4_IPV6_UDP_EX,
};

#define XDP_PACKET_HEADROOM 256
#define XDP_XMIT_FLUSH BIT(0)
#define XDP_XMIT_FLAGS_MASK XDP_XMIT_FLUSH
#define NETDEV_XDP_ACT_BASIC BIT(0)
#define NETDEV_XDP_ACT_REDIRECT BIT(1)
#define NETDEV_XDP_ACT_XSK_ZEROCOPY BIT(3)

struct xdp_rxq_info {
  struct net_device *dev;
  u32 queue_index;
  bool reg;
};
struct xdp_md {
  u32 data;
  u32 data_end;
};
struct xdp_buff {
  void *data;
  void *data_end;
  void *data_meta;
  void *data_hard_start;
  struct xdp_rxq_info *rxq;
  u32 frame_sz;
  u32 flags;
};
struct xdp_frame {
  void *data;
  u16 len;
  u16 headroom;
  u32 metasize : 8;
  u32 frame_sz : 24;
  u32 flags;
};
struct xdp_desc {
  u64 addr;
  u32 len;
  u32 options;
};
struct xsk_buff_pool {
  struct device *dev;
};
struct xsk_tx_metadata {
  u64 flags;
  union {
    struct {
      u64 launch_time;
    } request;
  };
};
struct xsk_tx_metadata_compl {
  void *priv;
};
struct xsk_tx_metadata_ops {
  void (*tmo_request_timestamp)(void *priv);
  u64 (*tmo_fill_timestamp)(void *priv);
  void (*tmo_request_launch_time)(u64 launch_time, void *priv);
};
struct xdp_metadata_ops {
  int (*xmo_rx_timestamp)(const struct xdp_md *ctx, u64 *timestamp);
  int (*xmo_rx_hash)(const struct xdp_md *ctx, u32 *hash,
                     enum xdp_rss_hash_type *rss_type);
};
#define XSK_CHECK_PRIV_TYPE(t) _Static_assert(sizeof(t) > 0, "xsk priv type")

enum bpf_netdev_command { XDP_SETUP_PROG = 0, XDP_SETUP_XSK_POOL = 1 };
struct netdev_bpf {
  enum bpf_netdev_command command;
  struct netlink_ext_ack *extack;
  union {
    struct {
      u32 flags;
      struct bpf_prog *prog;
    };
    struct {
      struct xsk_buff_pool *pool;
      u16 queue_id;
    } xsk;
  };
};

#endif /* LX_EMUL_SUBSYS_H */
